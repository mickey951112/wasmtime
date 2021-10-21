//! An `Instance` contains all the runtime state used by execution of a
//! wasm module (except its callstack and register state). An
//! `InstanceHandle` is a reference-counting handle for an `Instance`.

use crate::export::Export;
use crate::externref::VMExternRefActivationsTable;
use crate::memory::{Memory, RuntimeMemoryCreator};
use crate::table::{Table, TableElement, TableElementType};
use crate::traphandlers::Trap;
use crate::vmcontext::{
    VMCallerCheckedAnyfunc, VMContext, VMFunctionImport, VMGlobalDefinition, VMGlobalImport,
    VMInterrupts, VMMemoryDefinition, VMMemoryImport, VMTableDefinition, VMTableImport,
};
use crate::{ExportFunction, ExportGlobal, ExportMemory, ExportTable, Store};
use memoffset::offset_of;
use more_asserts::assert_lt;
use std::alloc::Layout;
use std::any::Any;
use std::convert::TryFrom;
use std::hash::Hash;
use std::ops::Range;
use std::ptr::NonNull;
use std::sync::Arc;
use std::{mem, ptr, slice};
use wasmtime_environ::{
    packed_option::ReservedValue, DataIndex, DefinedGlobalIndex, DefinedMemoryIndex,
    DefinedTableIndex, ElemIndex, EntityIndex, EntityRef, EntitySet, FuncIndex, GlobalIndex,
    HostPtr, MemoryIndex, Module, PrimaryMap, TableIndex, TrapCode, VMOffsets, WasmType,
};

mod allocator;

pub use allocator::*;

/// A type that roughly corresponds to a WebAssembly instance, but is also used
/// for host-defined objects.
///
/// This structure is is never allocated directly but is instead managed through
/// an `InstanceHandle`. This structure ends with a `VMContext` which has a
/// dynamic size corresponding to the `module` configured within. Memory
/// management of this structure is always externalized.
///
/// Instances here can correspond to actual instantiated modules, but it's also
/// used ubiquitously for host-defined objects. For example creating a
/// host-defined memory will have a `module` that looks like it exports a single
/// memory (and similar for other constructs).
///
/// This `Instance` type is used as a ubiquitous representation for WebAssembly
/// values, whether or not they were created on the host or through a module.
#[repr(C)] // ensure that the vmctx field is last.
pub(crate) struct Instance {
    /// The `Module` this `Instance` was instantiated from.
    module: Arc<Module>,

    /// Offsets in the `vmctx` region, precomputed from the `module` above.
    offsets: VMOffsets<HostPtr>,

    /// WebAssembly linear memory data.
    ///
    /// This is where all runtime information about defined linear memories in
    /// this module lives.
    memories: PrimaryMap<DefinedMemoryIndex, Memory>,

    /// WebAssembly table data.
    ///
    /// Like memories, this is only for defined tables in the module and
    /// contains all of their runtime state.
    tables: PrimaryMap<DefinedTableIndex, Table>,

    /// Stores the dropped passive element segments in this instantiation by index.
    /// If the index is present in the set, the segment has been dropped.
    dropped_elements: EntitySet<ElemIndex>,

    /// Stores the dropped passive data segments in this instantiation by index.
    /// If the index is present in the set, the segment has been dropped.
    dropped_data: EntitySet<DataIndex>,

    /// A slice pointing to all data that is referenced by this instance. This
    /// data is managed externally so this is effectively an unsafe reference,
    /// and this does not live for the `'static` lifetime so the API boundaries
    /// here are careful to never hand out static references.
    wasm_data: &'static [u8],

    /// Hosts can store arbitrary per-instance information here.
    ///
    /// Most of the time from Wasmtime this is `Box::new(())`, a noop
    /// allocation, but some host-defined objects will store their state here.
    host_state: Box<dyn Any + Send + Sync>,

    /// Additional context used by compiled wasm code. This field is last, and
    /// represents a dynamically-sized array that extends beyond the nominal
    /// end of the struct (similar to a flexible array member).
    vmctx: VMContext,
}

#[allow(clippy::cast_ptr_alignment)]
impl Instance {
    /// Helper function to access various locations offset from our `*mut
    /// VMContext` object.
    unsafe fn vmctx_plus_offset<T>(&self, offset: u32) -> *mut T {
        (self.vmctx_ptr() as *mut u8)
            .add(usize::try_from(offset).unwrap())
            .cast()
    }

    pub(crate) fn module(&self) -> &Arc<Module> {
        &self.module
    }

    /// Return the indexed `VMFunctionImport`.
    fn imported_function(&self, index: FuncIndex) -> &VMFunctionImport {
        unsafe { &*self.vmctx_plus_offset(self.offsets.vmctx_vmfunction_import(index)) }
    }

    /// Return the index `VMTableImport`.
    fn imported_table(&self, index: TableIndex) -> &VMTableImport {
        unsafe { &*self.vmctx_plus_offset(self.offsets.vmctx_vmtable_import(index)) }
    }

    /// Return the indexed `VMMemoryImport`.
    fn imported_memory(&self, index: MemoryIndex) -> &VMMemoryImport {
        unsafe { &*self.vmctx_plus_offset(self.offsets.vmctx_vmmemory_import(index)) }
    }

    /// Return the indexed `VMGlobalImport`.
    fn imported_global(&self, index: GlobalIndex) -> &VMGlobalImport {
        unsafe { &*self.vmctx_plus_offset(self.offsets.vmctx_vmglobal_import(index)) }
    }

    /// Return the indexed `VMTableDefinition`.
    #[allow(dead_code)]
    fn table(&self, index: DefinedTableIndex) -> VMTableDefinition {
        unsafe { *self.table_ptr(index) }
    }

    /// Updates the value for a defined table to `VMTableDefinition`.
    fn set_table(&self, index: DefinedTableIndex, table: VMTableDefinition) {
        unsafe {
            *self.table_ptr(index) = table;
        }
    }

    /// Return the indexed `VMTableDefinition`.
    fn table_ptr(&self, index: DefinedTableIndex) -> *mut VMTableDefinition {
        unsafe { self.vmctx_plus_offset(self.offsets.vmctx_vmtable_definition(index)) }
    }

    /// Get a locally defined or imported memory.
    pub(crate) fn get_memory(&self, index: MemoryIndex) -> VMMemoryDefinition {
        if let Some(defined_index) = self.module.defined_memory_index(index) {
            self.memory(defined_index)
        } else {
            let import = self.imported_memory(index);
            *unsafe { import.from.as_ref().unwrap() }
        }
    }

    /// Return the indexed `VMMemoryDefinition`.
    fn memory(&self, index: DefinedMemoryIndex) -> VMMemoryDefinition {
        unsafe { *self.memory_ptr(index) }
    }

    /// Set the indexed memory to `VMMemoryDefinition`.
    fn set_memory(&self, index: DefinedMemoryIndex, mem: VMMemoryDefinition) {
        unsafe {
            *self.memory_ptr(index) = mem;
        }
    }

    /// Return the indexed `VMMemoryDefinition`.
    fn memory_ptr(&self, index: DefinedMemoryIndex) -> *mut VMMemoryDefinition {
        unsafe { self.vmctx_plus_offset(self.offsets.vmctx_vmmemory_definition(index)) }
    }

    /// Return the indexed `VMGlobalDefinition`.
    fn global(&self, index: DefinedGlobalIndex) -> &VMGlobalDefinition {
        unsafe { &*self.global_ptr(index) }
    }

    /// Return the indexed `VMGlobalDefinition`.
    fn global_ptr(&self, index: DefinedGlobalIndex) -> *mut VMGlobalDefinition {
        unsafe { self.vmctx_plus_offset(self.offsets.vmctx_vmglobal_definition(index)) }
    }

    /// Get a raw pointer to the global at the given index regardless whether it
    /// is defined locally or imported from another module.
    ///
    /// Panics if the index is out of bound or is the reserved value.
    pub(crate) fn defined_or_imported_global_ptr(
        &self,
        index: GlobalIndex,
    ) -> *mut VMGlobalDefinition {
        if let Some(index) = self.module.defined_global_index(index) {
            self.global_ptr(index)
        } else {
            self.imported_global(index).from
        }
    }

    /// Return a pointer to the interrupts structure
    pub fn interrupts(&self) -> *mut *const VMInterrupts {
        unsafe { self.vmctx_plus_offset(self.offsets.vmctx_interrupts()) }
    }

    /// Return a pointer to the `VMExternRefActivationsTable`.
    pub fn externref_activations_table(&self) -> *mut *mut VMExternRefActivationsTable {
        unsafe { self.vmctx_plus_offset(self.offsets.vmctx_externref_activations_table()) }
    }

    /// Gets a pointer to this instance's `Store` which was originally
    /// configured on creation.
    ///
    /// # Panics
    ///
    /// This will panic if the originally configured store was `None`. That can
    /// happen for host functions so host functions can't be queried what their
    /// original `Store` was since it's just retained as null (since host
    /// functions are shared amongst threads and don't all share the same
    /// store).
    #[inline]
    pub fn store(&self) -> *mut dyn Store {
        let ptr = unsafe { *self.vmctx_plus_offset::<*mut dyn Store>(self.offsets.vmctx_store()) };
        assert!(!ptr.is_null());
        ptr
    }

    pub unsafe fn set_store(&mut self, store: *mut dyn Store) {
        *self.vmctx_plus_offset(self.offsets.vmctx_store()) = store;
    }

    /// Return a reference to the vmctx used by compiled wasm code.
    #[inline]
    pub fn vmctx(&self) -> &VMContext {
        &self.vmctx
    }

    /// Return a raw pointer to the vmctx used by compiled wasm code.
    #[inline]
    pub fn vmctx_ptr(&self) -> *mut VMContext {
        self.vmctx() as *const VMContext as *mut VMContext
    }

    /// Lookup an export with the given export declaration.
    pub fn lookup_by_declaration(&self, export: &EntityIndex) -> Export {
        match export {
            EntityIndex::Function(index) => {
                let anyfunc = self.get_caller_checked_anyfunc(*index).unwrap();
                let anyfunc =
                    NonNull::new(anyfunc as *const VMCallerCheckedAnyfunc as *mut _).unwrap();
                ExportFunction { anyfunc }.into()
            }
            EntityIndex::Table(index) => {
                let (definition, vmctx) =
                    if let Some(def_index) = self.module.defined_table_index(*index) {
                        (self.table_ptr(def_index), self.vmctx_ptr())
                    } else {
                        let import = self.imported_table(*index);
                        (import.from, import.vmctx)
                    };
                ExportTable {
                    definition,
                    vmctx,
                    table: self.module.table_plans[*index].clone(),
                }
                .into()
            }
            EntityIndex::Memory(index) => {
                let (definition, vmctx) =
                    if let Some(def_index) = self.module.defined_memory_index(*index) {
                        (self.memory_ptr(def_index), self.vmctx_ptr())
                    } else {
                        let import = self.imported_memory(*index);
                        (import.from, import.vmctx)
                    };
                ExportMemory {
                    definition,
                    vmctx,
                    memory: self.module.memory_plans[*index].clone(),
                }
                .into()
            }
            EntityIndex::Global(index) => ExportGlobal {
                definition: if let Some(def_index) = self.module.defined_global_index(*index) {
                    self.global_ptr(def_index)
                } else {
                    self.imported_global(*index).from
                },
                vmctx: self.vmctx_ptr(),
                global: self.module.globals[*index],
            }
            .into(),

            EntityIndex::Instance(_) | EntityIndex::Module(_) => {
                panic!("can't use this api for modules/instances")
            }
        }
    }

    /// Return an iterator over the exports of this instance.
    ///
    /// Specifically, it provides access to the key-value pairs, where the keys
    /// are export names, and the values are export declarations which can be
    /// resolved `lookup_by_declaration`.
    pub fn exports(&self) -> indexmap::map::Iter<String, EntityIndex> {
        self.module.exports.iter()
    }

    /// Return a reference to the custom state attached to this instance.
    #[inline]
    pub fn host_state(&self) -> &dyn Any {
        &*self.host_state
    }

    /// Return the offset from the vmctx pointer to its containing Instance.
    #[inline]
    pub(crate) fn vmctx_offset() -> isize {
        offset_of!(Self, vmctx) as isize
    }

    /// Return the table index for the given `VMTableDefinition`.
    unsafe fn table_index(&self, table: &VMTableDefinition) -> DefinedTableIndex {
        let index = DefinedTableIndex::new(
            usize::try_from(
                (table as *const VMTableDefinition)
                    .offset_from(self.table_ptr(DefinedTableIndex::new(0))),
            )
            .unwrap(),
        );
        assert_lt!(index.index(), self.tables.len());
        index
    }

    /// Return the memory index for the given `VMMemoryDefinition`.
    unsafe fn memory_index(&self, memory: &VMMemoryDefinition) -> DefinedMemoryIndex {
        let index = DefinedMemoryIndex::new(
            usize::try_from(
                (memory as *const VMMemoryDefinition)
                    .offset_from(self.memory_ptr(DefinedMemoryIndex::new(0))),
            )
            .unwrap(),
        );
        assert_lt!(index.index(), self.memories.len());
        index
    }

    /// Grow memory by the specified amount of pages.
    ///
    /// Returns `None` if memory can't be grown by the specified amount
    /// of pages. Returns `Some` with the old size in bytes if growth was
    /// successful.
    pub(crate) fn memory_grow(&mut self, index: MemoryIndex, delta: u64) -> Option<usize> {
        let (idx, instance) = if let Some(idx) = self.module.defined_memory_index(index) {
            (idx, self)
        } else {
            let import = self.imported_memory(index);
            unsafe {
                let foreign_instance = (*import.vmctx).instance_mut();
                let foreign_memory_def = &*import.from;
                let foreign_memory_index = foreign_instance.memory_index(foreign_memory_def);
                (foreign_memory_index, foreign_instance)
            }
        };
        let store = unsafe { &mut *instance.store() };
        let memory = &mut instance.memories[idx];

        let result = unsafe { memory.grow(delta, store) };
        let vmmemory = memory.vmmemory();

        // Update the state used by wasm code in case the base pointer and/or
        // the length changed.
        instance.set_memory(idx, vmmemory);

        result
    }

    pub(crate) fn table_element_type(&mut self, table_index: TableIndex) -> TableElementType {
        unsafe { (*self.get_table(table_index)).element_type() }
    }

    /// Grow table by the specified amount of elements, filling them with
    /// `init_value`.
    ///
    /// Returns `None` if table can't be grown by the specified amount of
    /// elements, or if `init_value` is the wrong type of table element.
    pub(crate) fn table_grow(
        &mut self,
        table_index: TableIndex,
        delta: u32,
        init_value: TableElement,
    ) -> Option<u32> {
        let (defined_table_index, instance) =
            self.get_defined_table_index_and_instance(table_index);
        instance.defined_table_grow(defined_table_index, delta, init_value)
    }

    fn defined_table_grow(
        &mut self,
        table_index: DefinedTableIndex,
        delta: u32,
        init_value: TableElement,
    ) -> Option<u32> {
        let store = unsafe { &mut *self.store() };
        let table = self
            .tables
            .get_mut(table_index)
            .unwrap_or_else(|| panic!("no table for index {}", table_index.index()));

        let result = unsafe { table.grow(delta, init_value, store) };

        // Keep the `VMContext` pointers used by compiled Wasm code up to
        // date.
        self.set_table(table_index, self.tables[table_index].vmtable());

        result
    }

    fn alloc_layout(&self) -> Layout {
        let size = mem::size_of_val(self)
            .checked_add(usize::try_from(self.offsets.size_of_vmctx()).unwrap())
            .unwrap();
        let align = mem::align_of_val(self);
        Layout::from_size_align(size, align).unwrap()
    }

    /// Get a `&VMCallerCheckedAnyfunc` for the given `FuncIndex`.
    ///
    /// Returns `None` if the index is the reserved index value.
    ///
    /// The returned reference is a stable reference that won't be moved and can
    /// be passed into JIT code.
    pub(crate) fn get_caller_checked_anyfunc(
        &self,
        index: FuncIndex,
    ) -> Option<&VMCallerCheckedAnyfunc> {
        if index == FuncIndex::reserved_value() {
            return None;
        }

        unsafe { Some(&*self.vmctx_plus_offset(self.offsets.vmctx_anyfunc(index))) }
    }

    unsafe fn anyfunc_base(&self) -> *mut VMCallerCheckedAnyfunc {
        self.vmctx_plus_offset(self.offsets.vmctx_anyfuncs_begin())
    }

    /// The `table.init` operation: initializes a portion of a table with a
    /// passive element.
    ///
    /// # Errors
    ///
    /// Returns a `Trap` error when the range within the table is out of bounds
    /// or the range within the passive element is out of bounds.
    pub(crate) fn table_init(
        &mut self,
        table_index: TableIndex,
        elem_index: ElemIndex,
        dst: u32,
        src: u32,
        len: u32,
    ) -> Result<(), Trap> {
        // TODO: this `clone()` shouldn't be necessary but is used for now to
        // inform `rustc` that the lifetime of the elements here are
        // disconnected from the lifetime of `self`.
        let module = self.module.clone();

        let elements = match module.passive_elements_map.get(&elem_index) {
            Some(index) if !self.dropped_elements.contains(elem_index) => {
                module.passive_elements[*index].as_ref()
            }
            _ => &[],
        };
        self.table_init_segment(table_index, elements, dst, src, len)
    }

    pub(crate) fn table_init_segment(
        &mut self,
        table_index: TableIndex,
        elements: &[FuncIndex],
        dst: u32,
        src: u32,
        len: u32,
    ) -> Result<(), Trap> {
        // https://webassembly.github.io/bulk-memory-operations/core/exec/instructions.html#exec-table-init

        let table = unsafe { &mut *self.get_table(table_index) };

        let elements = match elements
            .get(usize::try_from(src).unwrap()..)
            .and_then(|s| s.get(..usize::try_from(len).unwrap()))
        {
            Some(elements) => elements,
            None => return Err(Trap::wasm(TrapCode::TableOutOfBounds)),
        };

        match table.element_type() {
            TableElementType::Func => unsafe {
                let base = self.anyfunc_base();
                table.init_funcs(
                    dst,
                    elements.iter().map(|idx| {
                        if *idx == FuncIndex::reserved_value() {
                            ptr::null_mut()
                        } else {
                            debug_assert!(idx.as_u32() < self.offsets.num_defined_functions);
                            base.add(usize::try_from(idx.as_u32()).unwrap())
                        }
                    }),
                )?;
            },

            TableElementType::Extern => {
                debug_assert!(elements.iter().all(|e| *e == FuncIndex::reserved_value()));
                table.fill(dst, TableElement::ExternRef(None), len)?;
            }
        }
        Ok(())
    }

    /// Drop an element.
    pub(crate) fn elem_drop(&mut self, elem_index: ElemIndex) {
        // https://webassembly.github.io/reference-types/core/exec/instructions.html#exec-elem-drop

        self.dropped_elements.insert(elem_index);

        // Note that we don't check that we actually removed a segment because
        // dropping a non-passive segment is a no-op (not a trap).
    }

    /// Get a locally-defined memory.
    pub(crate) fn get_defined_memory(&mut self, index: DefinedMemoryIndex) -> *mut Memory {
        ptr::addr_of_mut!(self.memories[index])
    }

    /// Do a `memory.copy`
    ///
    /// # Errors
    ///
    /// Returns a `Trap` error when the source or destination ranges are out of
    /// bounds.
    pub(crate) fn memory_copy(
        &mut self,
        dst_index: MemoryIndex,
        dst: u64,
        src_index: MemoryIndex,
        src: u64,
        len: u64,
    ) -> Result<(), Trap> {
        // https://webassembly.github.io/reference-types/core/exec/instructions.html#exec-memory-copy

        let src_mem = self.get_memory(src_index);
        let dst_mem = self.get_memory(dst_index);

        let src = self.validate_inbounds(src_mem.current_length, src, len)?;
        let dst = self.validate_inbounds(dst_mem.current_length, dst, len)?;

        // Bounds and casts are checked above, by this point we know that
        // everything is safe.
        unsafe {
            let dst = dst_mem.base.add(dst);
            let src = src_mem.base.add(src);
            ptr::copy(src, dst, len as usize);
        }

        Ok(())
    }

    fn validate_inbounds(&self, max: usize, ptr: u64, len: u64) -> Result<usize, Trap> {
        let oob = || Trap::wasm(TrapCode::HeapOutOfBounds);
        let end = ptr
            .checked_add(len)
            .and_then(|i| usize::try_from(i).ok())
            .ok_or_else(oob)?;
        if end > max {
            Err(oob())
        } else {
            Ok(ptr as usize)
        }
    }

    /// Perform the `memory.fill` operation on a locally defined memory.
    ///
    /// # Errors
    ///
    /// Returns a `Trap` error if the memory range is out of bounds.
    pub(crate) fn memory_fill(
        &mut self,
        memory_index: MemoryIndex,
        dst: u64,
        val: u8,
        len: u64,
    ) -> Result<(), Trap> {
        let memory = self.get_memory(memory_index);
        let dst = self.validate_inbounds(memory.current_length, dst, len)?;

        // Bounds and casts are checked above, by this point we know that
        // everything is safe.
        unsafe {
            let dst = memory.base.add(dst);
            ptr::write_bytes(dst, val, len as usize);
        }

        Ok(())
    }

    /// Performs the `memory.init` operation.
    ///
    /// # Errors
    ///
    /// Returns a `Trap` error if the destination range is out of this module's
    /// memory's bounds or if the source range is outside the data segment's
    /// bounds.
    pub(crate) fn memory_init(
        &mut self,
        memory_index: MemoryIndex,
        data_index: DataIndex,
        dst: u64,
        src: u32,
        len: u32,
    ) -> Result<(), Trap> {
        let range = match self.module.passive_data_map.get(&data_index).cloned() {
            Some(range) if !self.dropped_data.contains(data_index) => range,
            _ => 0..0,
        };
        self.memory_init_segment(memory_index, range, dst, src, len)
    }

    pub(crate) fn wasm_data(&self, range: Range<u32>) -> &[u8] {
        &self.wasm_data[range.start as usize..range.end as usize]
    }

    pub(crate) fn memory_init_segment(
        &mut self,
        memory_index: MemoryIndex,
        range: Range<u32>,
        dst: u64,
        src: u32,
        len: u32,
    ) -> Result<(), Trap> {
        // https://webassembly.github.io/bulk-memory-operations/core/exec/instructions.html#exec-memory-init

        let memory = self.get_memory(memory_index);
        let data = self.wasm_data(range);
        let dst = self.validate_inbounds(memory.current_length, dst, len.into())?;
        let src = self.validate_inbounds(data.len(), src.into(), len.into())?;
        let len = len as usize;

        let src_slice = &data[src..(src + len)];

        unsafe {
            let dst_start = memory.base.add(dst);
            let dst_slice = slice::from_raw_parts_mut(dst_start, len);
            dst_slice.copy_from_slice(src_slice);
        }

        Ok(())
    }

    /// Drop the given data segment, truncating its length to zero.
    pub(crate) fn data_drop(&mut self, data_index: DataIndex) {
        self.dropped_data.insert(data_index);

        // Note that we don't check that we actually removed a segment because
        // dropping a non-passive segment is a no-op (not a trap).
    }

    /// Get a table by index regardless of whether it is locally-defined or an
    /// imported, foreign table.
    pub(crate) fn get_table(&mut self, table_index: TableIndex) -> *mut Table {
        let (idx, instance) = self.get_defined_table_index_and_instance(table_index);
        ptr::addr_of_mut!(instance.tables[idx])
    }

    /// Get a locally-defined table.
    pub(crate) fn get_defined_table(&mut self, index: DefinedTableIndex) -> *mut Table {
        ptr::addr_of_mut!(self.tables[index])
    }

    pub(crate) fn get_defined_table_index_and_instance(
        &mut self,
        index: TableIndex,
    ) -> (DefinedTableIndex, &mut Instance) {
        if let Some(defined_table_index) = self.module.defined_table_index(index) {
            (defined_table_index, self)
        } else {
            let import = self.imported_table(index);
            unsafe {
                let foreign_instance = (*import.vmctx).instance_mut();
                let foreign_table_def = &*import.from;
                let foreign_table_index = foreign_instance.table_index(foreign_table_def);
                (foreign_table_index, foreign_instance)
            }
        }
    }

    fn drop_globals(&mut self) {
        for (idx, global) in self.module.globals.iter() {
            let idx = match self.module.defined_global_index(idx) {
                Some(idx) => idx,
                None => continue,
            };
            match global.wasm_ty {
                // For now only externref gloabls need to get destroyed
                WasmType::ExternRef => {}
                _ => continue,
            }
            unsafe {
                drop((*self.global_ptr(idx)).as_externref_mut().take());
            }
        }
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        self.drop_globals();
    }
}

/// A handle holding an `Instance` of a WebAssembly module.
#[derive(Hash, PartialEq, Eq)]
pub struct InstanceHandle {
    instance: *mut Instance,
}

// These are only valid if the `Instance` type is send/sync, hence the
// assertion below.
unsafe impl Send for InstanceHandle {}
unsafe impl Sync for InstanceHandle {}

fn _assert_send_sync() {
    fn _assert<T: Send + Sync>() {}
    _assert::<Instance>();
}

impl InstanceHandle {
    /// Create a new `InstanceHandle` pointing at the instance
    /// pointed to by the given `VMContext` pointer.
    ///
    /// # Safety
    /// This is unsafe because it doesn't work on just any `VMContext`, it must
    /// be a `VMContext` allocated as part of an `Instance`.
    #[inline]
    pub unsafe fn from_vmctx(vmctx: *mut VMContext) -> Self {
        let instance = (&mut *vmctx).instance();
        Self {
            instance: instance as *const Instance as *mut Instance,
        }
    }

    /// Return a reference to the vmctx used by compiled wasm code.
    pub fn vmctx(&self) -> &VMContext {
        self.instance().vmctx()
    }

    /// Return a raw pointer to the vmctx used by compiled wasm code.
    #[inline]
    pub fn vmctx_ptr(&self) -> *mut VMContext {
        self.instance().vmctx_ptr()
    }

    /// Return a reference to a module.
    pub fn module(&self) -> &Arc<Module> {
        self.instance().module()
    }

    /// Lookup an export with the given export declaration.
    pub fn lookup_by_declaration(&self, export: &EntityIndex) -> Export {
        self.instance().lookup_by_declaration(export)
    }

    /// Return an iterator over the exports of this instance.
    ///
    /// Specifically, it provides access to the key-value pairs, where the keys
    /// are export names, and the values are export declarations which can be
    /// resolved `lookup_by_declaration`.
    pub fn exports(&self) -> indexmap::map::Iter<String, EntityIndex> {
        self.instance().exports()
    }

    /// Return a reference to the custom state attached to this instance.
    pub fn host_state(&self) -> &dyn Any {
        self.instance().host_state()
    }

    /// Return the memory index for the given `VMMemoryDefinition` in this instance.
    pub unsafe fn memory_index(&self, memory: &VMMemoryDefinition) -> DefinedMemoryIndex {
        self.instance().memory_index(memory)
    }

    /// Get a memory defined locally within this module.
    pub fn get_defined_memory(&mut self, index: DefinedMemoryIndex) -> *mut Memory {
        self.instance_mut().get_defined_memory(index)
    }

    /// Return the table index for the given `VMTableDefinition` in this instance.
    pub unsafe fn table_index(&self, table: &VMTableDefinition) -> DefinedTableIndex {
        self.instance().table_index(table)
    }

    /// Get a table defined locally within this module.
    pub fn get_defined_table(&mut self, index: DefinedTableIndex) -> *mut Table {
        self.instance_mut().get_defined_table(index)
    }

    /// Return a reference to the contained `Instance`.
    #[inline]
    pub(crate) fn instance(&self) -> &Instance {
        unsafe { &*(self.instance as *const Instance) }
    }

    pub(crate) fn instance_mut(&mut self) -> &mut Instance {
        unsafe { &mut *self.instance }
    }

    /// Returns the `Store` pointer that was stored on creation
    #[inline]
    pub fn store(&self) -> *mut dyn Store {
        self.instance().store()
    }

    /// Configure the `*mut dyn Store` internal pointer after-the-fact.
    ///
    /// This is provided for the original `Store` itself to configure the first
    /// self-pointer after the original `Box` has been initialized.
    pub unsafe fn set_store(&mut self, store: *mut dyn Store) {
        self.instance_mut().set_store(store);
    }

    /// Returns a clone of this instance.
    ///
    /// This is unsafe because the returned handle here is just a cheap clone
    /// of the internals, there's no lifetime tracking around its validity.
    /// You'll need to ensure that the returned handles all go out of scope at
    /// the same time.
    #[inline]
    pub unsafe fn clone(&self) -> InstanceHandle {
        InstanceHandle {
            instance: self.instance,
        }
    }
}
