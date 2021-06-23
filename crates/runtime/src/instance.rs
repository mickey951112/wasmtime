//! An `Instance` contains all the runtime state used by execution of a
//! wasm module (except its callstack and register state). An
//! `InstanceHandle` is a reference-counting handle for an `Instance`.

use crate::export::Export;
use crate::externref::VMExternRefActivationsTable;
use crate::memory::{Memory, RuntimeMemoryCreator};
use crate::table::{Table, TableElement};
use crate::traphandlers::Trap;
use crate::vmcontext::{
    VMBuiltinFunctionsArray, VMCallerCheckedAnyfunc, VMContext, VMFunctionImport,
    VMGlobalDefinition, VMGlobalImport, VMInterrupts, VMMemoryDefinition, VMMemoryImport,
    VMSharedSignatureIndex, VMTableDefinition, VMTableImport,
};
use crate::{ExportFunction, ExportGlobal, ExportMemory, ExportTable, Store};
use memoffset::offset_of;
use more_asserts::assert_lt;
use std::alloc::Layout;
use std::any::Any;
use std::collections::HashMap;
use std::convert::TryFrom;
use std::hash::Hash;
use std::ptr::NonNull;
use std::sync::Arc;
use std::{mem, ptr, slice};
use wasmtime_environ::entity::{packed_option::ReservedValue, EntityRef, EntitySet, PrimaryMap};
use wasmtime_environ::wasm::{
    DataIndex, DefinedGlobalIndex, DefinedMemoryIndex, DefinedTableIndex, ElemIndex, EntityIndex,
    FuncIndex, GlobalIndex, MemoryIndex, TableElementType, TableIndex, WasmType,
};
use wasmtime_environ::{ir, Module, VMOffsets};

mod allocator;

pub use allocator::*;

/// Value returned by [`ResourceLimiter::instances`] default method
pub const DEFAULT_INSTANCE_LIMIT: usize = 10000;
/// Value returned by [`ResourceLimiter::tables`] default method
pub const DEFAULT_TABLE_LIMIT: usize = 10000;
/// Value returned by [`ResourceLimiter::memories`] default method
pub const DEFAULT_MEMORY_LIMIT: usize = 10000;

/// Used by hosts to limit resource consumption of instances.
///
/// An instance can be created with a resource limiter so that hosts can take into account
/// non-WebAssembly resource usage to determine if a linear memory or table should grow.
pub trait ResourceLimiter {
    /// Notifies the resource limiter that an instance's linear memory has been requested to grow.
    ///
    /// * `current` is the current size of the linear memory in WebAssembly page units.
    /// * `desired` is the desired size of the linear memory in WebAssembly page units.
    /// * `maximum` is either the linear memory's maximum or a maximum from an instance allocator,
    ///   also in WebAssembly page units. A value of `None` indicates that the linear memory is
    ///   unbounded.
    ///
    /// This function should return `true` to indicate that the growing operation is permitted or
    /// `false` if not permitted. Returning `true` when a maximum has been exceeded will have no
    /// effect as the linear memory will not grow.
    fn memory_growing(&mut self, current: u32, desired: u32, maximum: Option<u32>) -> bool;

    /// Notifies the resource limiter that an instance's table has been requested to grow.
    ///
    /// * `current` is the current number of elements in the table.
    /// * `desired` is the desired number of elements in the table.
    /// * `maximum` is either the table's maximum or a maximum from an instance allocator.
    ///   A value of `None` indicates that the table is unbounded.
    ///
    /// This function should return `true` to indicate that the growing operation is permitted or
    /// `false` if not permitted. Returning `true` when a maximum has been exceeded will have no
    /// effect as the table will not grow.
    fn table_growing(&mut self, current: u32, desired: u32, maximum: Option<u32>) -> bool;

    /// The maximum number of instances that can be created for a `Store`.
    ///
    /// Module instantiation will fail if this limit is exceeded.
    ///
    /// This value defaults to 10,000.
    fn instances(&self) -> usize {
        DEFAULT_INSTANCE_LIMIT
    }

    /// The maximum number of tables that can be created for a `Store`.
    ///
    /// Module instantiation will fail if this limit is exceeded.
    ///
    /// This value defaults to 10,000.
    fn tables(&self) -> usize {
        DEFAULT_TABLE_LIMIT
    }

    /// The maximum number of linear memories that can be created for a `Store`
    ///
    /// Instantiation will fail with an error if this limit is exceeded.
    ///
    /// This value defaults to 10,000.
    fn memories(&self) -> usize {
        DEFAULT_MEMORY_LIMIT
    }
}

/// A WebAssembly instance.
///
/// This is repr(C) to ensure that the vmctx field is last.
#[repr(C)]
pub(crate) struct Instance {
    /// The `Module` this `Instance` was instantiated from.
    module: Arc<Module>,

    /// Offsets in the `vmctx` region.
    offsets: VMOffsets,

    /// WebAssembly linear memory data.
    memories: PrimaryMap<DefinedMemoryIndex, Memory>,

    /// WebAssembly table data.
    tables: PrimaryMap<DefinedTableIndex, Table>,

    /// Stores the dropped passive element segments in this instantiation by index.
    /// If the index is present in the set, the segment has been dropped.
    dropped_elements: EntitySet<ElemIndex>,

    /// Stores the dropped passive data segments in this instantiation by index.
    /// If the index is present in the set, the segment has been dropped.
    dropped_data: EntitySet<DataIndex>,

    /// Hosts can store arbitrary per-instance information here.
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

    /// Return a pointer to the `VMSharedSignatureIndex`s.
    fn signature_ids_ptr(&self) -> *mut VMSharedSignatureIndex {
        unsafe { self.vmctx_plus_offset(self.offsets.vmctx_signature_ids_begin()) }
    }

    /// Return the indexed `VMFunctionImport`.
    fn imported_function(&self, index: FuncIndex) -> &VMFunctionImport {
        let index = usize::try_from(index.as_u32()).unwrap();
        unsafe { &*self.imported_functions_ptr().add(index) }
    }

    /// Return a pointer to the `VMFunctionImport`s.
    fn imported_functions_ptr(&self) -> *mut VMFunctionImport {
        unsafe { self.vmctx_plus_offset(self.offsets.vmctx_imported_functions_begin()) }
    }

    /// Return the index `VMTableImport`.
    fn imported_table(&self, index: TableIndex) -> &VMTableImport {
        let index = usize::try_from(index.as_u32()).unwrap();
        unsafe { &*self.imported_tables_ptr().add(index) }
    }

    /// Return a pointer to the `VMTableImports`s.
    fn imported_tables_ptr(&self) -> *mut VMTableImport {
        unsafe { self.vmctx_plus_offset(self.offsets.vmctx_imported_tables_begin()) }
    }

    /// Return the indexed `VMMemoryImport`.
    fn imported_memory(&self, index: MemoryIndex) -> &VMMemoryImport {
        let index = usize::try_from(index.as_u32()).unwrap();
        unsafe { &*self.imported_memories_ptr().add(index) }
    }

    /// Return a pointer to the `VMMemoryImport`s.
    fn imported_memories_ptr(&self) -> *mut VMMemoryImport {
        unsafe { self.vmctx_plus_offset(self.offsets.vmctx_imported_memories_begin()) }
    }

    /// Return the indexed `VMGlobalImport`.
    fn imported_global(&self, index: GlobalIndex) -> &VMGlobalImport {
        let index = usize::try_from(index.as_u32()).unwrap();
        unsafe { &*self.imported_globals_ptr().add(index) }
    }

    /// Return a pointer to the `VMGlobalImport`s.
    fn imported_globals_ptr(&self) -> *mut VMGlobalImport {
        unsafe { self.vmctx_plus_offset(self.offsets.vmctx_imported_globals_begin()) }
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
        let index = usize::try_from(index.as_u32()).unwrap();
        unsafe { self.tables_ptr().add(index) }
    }

    /// Return a pointer to the `VMTableDefinition`s.
    fn tables_ptr(&self) -> *mut VMTableDefinition {
        unsafe { self.vmctx_plus_offset(self.offsets.vmctx_tables_begin()) }
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
        let index = usize::try_from(index.as_u32()).unwrap();
        unsafe { self.memories_ptr().add(index) }
    }

    /// Return a pointer to the `VMMemoryDefinition`s.
    fn memories_ptr(&self) -> *mut VMMemoryDefinition {
        unsafe { self.vmctx_plus_offset(self.offsets.vmctx_memories_begin()) }
    }

    /// Return the indexed `VMGlobalDefinition`.
    fn global(&self, index: DefinedGlobalIndex) -> &VMGlobalDefinition {
        unsafe { &*self.global_ptr(index) }
    }

    /// Return the indexed `VMGlobalDefinition`.
    fn global_ptr(&self, index: DefinedGlobalIndex) -> *mut VMGlobalDefinition {
        let index = usize::try_from(index.as_u32()).unwrap();
        unsafe { self.globals_ptr().add(index) }
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

    /// Return a pointer to the `VMGlobalDefinition`s.
    fn globals_ptr(&self) -> *mut VMGlobalDefinition {
        unsafe { self.vmctx_plus_offset(self.offsets.vmctx_globals_begin()) }
    }

    /// Return a pointer to the `VMBuiltinFunctionsArray`.
    fn builtin_functions_ptr(&self) -> *mut VMBuiltinFunctionsArray {
        unsafe { self.vmctx_plus_offset(self.offsets.vmctx_builtin_functions_begin()) }
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
    pub(crate) fn table_index(&self, table: &VMTableDefinition) -> DefinedTableIndex {
        let offsets = &self.offsets;
        let begin = unsafe {
            (&self.vmctx as *const VMContext as *const u8)
                .add(usize::try_from(offsets.vmctx_tables_begin()).unwrap())
        } as *const VMTableDefinition;
        let end: *const VMTableDefinition = table;
        // TODO: Use `offset_from` once it stablizes.
        let index = DefinedTableIndex::new(
            (end as usize - begin as usize) / mem::size_of::<VMTableDefinition>(),
        );
        assert_lt!(index.index(), self.tables.len());
        index
    }

    /// Return the memory index for the given `VMMemoryDefinition`.
    pub(crate) fn memory_index(&self, memory: &VMMemoryDefinition) -> DefinedMemoryIndex {
        let offsets = &self.offsets;
        let begin = unsafe {
            (&self.vmctx as *const VMContext as *const u8)
                .add(usize::try_from(offsets.vmctx_memories_begin()).unwrap())
        } as *const VMMemoryDefinition;
        let end: *const VMMemoryDefinition = memory;
        // TODO: Use `offset_from` once it stablizes.
        let index = DefinedMemoryIndex::new(
            (end as usize - begin as usize) / mem::size_of::<VMMemoryDefinition>(),
        );
        assert_lt!(index.index(), self.memories.len());
        index
    }

    /// Grow memory by the specified amount of pages.
    ///
    /// Returns `None` if memory can't be grown by the specified amount
    /// of pages.
    pub(crate) fn memory_grow(
        &mut self,
        memory_index: DefinedMemoryIndex,
        delta: u32,
    ) -> Option<u32> {
        let limiter = unsafe { (*self.store()).limiter() };
        let memory = self
            .memories
            .get_mut(memory_index)
            .unwrap_or_else(|| panic!("no memory for index {}", memory_index.index()));

        let result = unsafe { memory.grow(delta, limiter) };

        // Keep current the VMContext pointers used by compiled wasm code.
        self.set_memory(memory_index, self.memories[memory_index].vmmemory());

        result
    }

    /// Grow imported memory by the specified amount of pages.
    ///
    /// Returns `None` if memory can't be grown by the specified amount
    /// of pages.
    ///
    /// # Safety
    /// This and `imported_memory_size` are currently unsafe because they
    /// dereference the memory import's pointers.
    pub(crate) unsafe fn imported_memory_grow(
        &mut self,
        memory_index: MemoryIndex,
        delta: u32,
    ) -> Option<u32> {
        let import = self.imported_memory(memory_index);
        let foreign_instance = (*import.vmctx).instance_mut();
        let foreign_memory = &*import.from;
        let foreign_index = foreign_instance.memory_index(foreign_memory);

        foreign_instance.memory_grow(foreign_index, delta)
    }

    /// Returns the number of allocated wasm pages.
    pub(crate) fn memory_size(&self, memory_index: DefinedMemoryIndex) -> u32 {
        self.memories
            .get(memory_index)
            .unwrap_or_else(|| panic!("no memory for index {}", memory_index.index()))
            .size()
    }

    /// Returns the number of allocated wasm pages in an imported memory.
    ///
    /// # Safety
    /// This and `imported_memory_grow` are currently unsafe because they
    /// dereference the memory import's pointers.
    pub(crate) unsafe fn imported_memory_size(&self, memory_index: MemoryIndex) -> u32 {
        let import = self.imported_memory(memory_index);
        let foreign_instance = (&mut *import.vmctx).instance();
        let foreign_memory = &mut *import.from;
        let foreign_index = foreign_instance.memory_index(foreign_memory);

        foreign_instance.memory_size(foreign_index)
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
        let limiter = unsafe { (*self.store()).limiter() };
        let table = self
            .tables
            .get_mut(table_index)
            .unwrap_or_else(|| panic!("no table for index {}", table_index.index()));

        let result = unsafe { table.grow(delta, init_value, limiter) };

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

    fn find_passive_segment<'a, I, D, T>(
        index: I,
        index_map: &HashMap<I, usize>,
        data: &'a Vec<D>,
        dropped: &EntitySet<I>,
    ) -> &'a [T]
    where
        D: AsRef<[T]>,
        I: EntityRef + Hash,
    {
        match index_map.get(&index) {
            Some(index) if !dropped.contains(I::new(*index)) => data[*index].as_ref(),
            _ => &[],
        }
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
        let elements = Self::find_passive_segment(
            elem_index,
            &module.passive_elements_map,
            &module.passive_elements,
            &self.dropped_elements,
        );
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
            None => return Err(Trap::wasm(ir::TrapCode::TableOutOfBounds)),
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

            TableElementType::Val(_) => {
                debug_assert!(elements.iter().all(|e| *e == FuncIndex::reserved_value()));
                table.fill(dst, TableElement::ExternRef(None), len)?;
            }
        }
        Ok(())
    }

    /// Drop an element.
    pub(crate) fn elem_drop(&mut self, elem_index: ElemIndex) {
        // https://webassembly.github.io/reference-types/core/exec/instructions.html#exec-elem-drop

        if let Some(index) = self.module.passive_elements_map.get(&elem_index) {
            self.dropped_elements.insert(ElemIndex::new(*index));
        }

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
        dst: u32,
        src_index: MemoryIndex,
        src: u32,
        len: u32,
    ) -> Result<(), Trap> {
        // https://webassembly.github.io/reference-types/core/exec/instructions.html#exec-memory-copy

        let src_mem = self.get_memory(src_index);
        let dst_mem = self.get_memory(dst_index);

        if src
            .checked_add(len)
            .map_or(true, |n| n > src_mem.current_length)
            || dst
                .checked_add(len)
                .map_or(true, |m| m > dst_mem.current_length)
        {
            return Err(Trap::wasm(ir::TrapCode::HeapOutOfBounds));
        }

        let dst = usize::try_from(dst).unwrap();
        let src = usize::try_from(src).unwrap();

        // Bounds and casts are checked above, by this point we know that
        // everything is safe.
        unsafe {
            let dst = dst_mem.base.add(dst);
            let src = src_mem.base.add(src);
            ptr::copy(src, dst, len as usize);
        }

        Ok(())
    }

    /// Perform the `memory.fill` operation on a locally defined memory.
    ///
    /// # Errors
    ///
    /// Returns a `Trap` error if the memory range is out of bounds.
    pub(crate) fn defined_memory_fill(
        &self,
        memory_index: DefinedMemoryIndex,
        dst: u32,
        val: u32,
        len: u32,
    ) -> Result<(), Trap> {
        let memory = self.memory(memory_index);

        if dst
            .checked_add(len)
            .map_or(true, |m| m > memory.current_length)
        {
            return Err(Trap::wasm(ir::TrapCode::HeapOutOfBounds));
        }

        let dst = isize::try_from(dst).unwrap();
        let val = val as u8;

        // Bounds and casts are checked above, by this point we know that
        // everything is safe.
        unsafe {
            let dst = memory.base.offset(dst);
            ptr::write_bytes(dst, val, len as usize);
        }

        Ok(())
    }

    /// Perform the `memory.fill` operation on an imported memory.
    ///
    /// # Errors
    ///
    /// Returns a `Trap` error if the memory range is out of bounds.
    pub(crate) fn imported_memory_fill(
        &self,
        memory_index: MemoryIndex,
        dst: u32,
        val: u32,
        len: u32,
    ) -> Result<(), Trap> {
        let import = self.imported_memory(memory_index);
        unsafe {
            let foreign_instance = (&*import.vmctx).instance();
            let foreign_memory = &*import.from;
            let foreign_index = foreign_instance.memory_index(foreign_memory);
            foreign_instance.defined_memory_fill(foreign_index, dst, val, len)
        }
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
        dst: u32,
        src: u32,
        len: u32,
    ) -> Result<(), Trap> {
        // TODO: this `clone()` shouldn't be necessary but is used for now to
        // inform `rustc` that the lifetime of the elements here are
        // disconnected from the lifetime of `self`.
        let module = self.module.clone();
        let data = Self::find_passive_segment(
            data_index,
            &module.passive_data_map,
            &module.passive_data,
            &self.dropped_data,
        );
        self.memory_init_segment(memory_index, &data, dst, src, len)
    }

    pub(crate) fn memory_init_segment(
        &mut self,
        memory_index: MemoryIndex,
        data: &[u8],
        dst: u32,
        src: u32,
        len: u32,
    ) -> Result<(), Trap> {
        // https://webassembly.github.io/bulk-memory-operations/core/exec/instructions.html#exec-memory-init

        let memory = self.get_memory(memory_index);

        if src
            .checked_add(len)
            .map_or(true, |n| n as usize > data.len())
            || dst
                .checked_add(len)
                .map_or(true, |m| m > memory.current_length)
        {
            return Err(Trap::wasm(ir::TrapCode::HeapOutOfBounds));
        }

        let src_slice = &data[src as usize..(src + len) as usize];

        unsafe {
            let dst_start = memory.base.add(dst as usize);
            let dst_slice = slice::from_raw_parts_mut(dst_start, len as usize);
            dst_slice.copy_from_slice(src_slice);
        }

        Ok(())
    }

    /// Drop the given data segment, truncating its length to zero.
    pub(crate) fn data_drop(&mut self, data_index: DataIndex) {
        if let Some(index) = self.module.passive_data_map.get(&data_index) {
            self.dropped_data.insert(DataIndex::new(*index));
        }

        // Note that we don't check that we actually removed a segment because
        // dropping a non-passive segment is a no-op (not a trap).
    }

    /// Get a table by index regardless of whether it is locally-defined or an
    /// imported, foreign table.
    pub(crate) fn get_table(&mut self, table_index: TableIndex) -> *mut Table {
        if let Some(defined_table_index) = self.module.defined_table_index(table_index) {
            self.get_defined_table(defined_table_index)
        } else {
            self.get_foreign_table(table_index)
        }
    }

    /// Get a locally-defined table.
    pub(crate) fn get_defined_table(&mut self, index: DefinedTableIndex) -> *mut Table {
        ptr::addr_of_mut!(self.tables[index])
    }

    /// Get an imported, foreign table.
    pub(crate) fn get_foreign_table(&mut self, index: TableIndex) -> *mut Table {
        let import = self.imported_table(index);
        let foreign_instance = unsafe { (*import.vmctx).instance_mut() };
        let foreign_table = unsafe { &*import.from };
        let foreign_index = foreign_instance.table_index(foreign_table);
        ptr::addr_of_mut!(foreign_instance.tables[foreign_index])
    }

    pub(crate) fn get_defined_table_index_and_instance(
        &mut self,
        index: TableIndex,
    ) -> (DefinedTableIndex, &mut Instance) {
        if let Some(defined_table_index) = self.module.defined_table_index(index) {
            (defined_table_index, self)
        } else {
            let import = self.imported_table(index);
            let foreign_instance = unsafe { (*import.vmctx).instance_mut() };
            let foreign_table_def = unsafe { &*import.from };
            let foreign_table_index = foreign_instance.table_index(foreign_table_def);
            (foreign_table_index, foreign_instance)
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
    pub fn memory_index(&self, memory: &VMMemoryDefinition) -> DefinedMemoryIndex {
        self.instance().memory_index(memory)
    }

    /// Get a memory defined locally within this module.
    pub fn get_defined_memory(&mut self, index: DefinedMemoryIndex) -> *mut Memory {
        self.instance_mut().get_defined_memory(index)
    }

    /// Return the table index for the given `VMTableDefinition` in this instance.
    pub fn table_index(&self, table: &VMTableDefinition) -> DefinedTableIndex {
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
