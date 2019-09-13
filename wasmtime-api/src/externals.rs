use crate::callable::{Callable, NativeCallable, WasmtimeFn, WrappedCallable};
use crate::runtime::Store;
use crate::trampoline::{generate_global_export, generate_memory_export, generate_table_export};
use crate::trap::Trap;
use crate::types::{ExternType, FuncType, GlobalType, MemoryType, TableType, ValType};
use crate::values::{from_checked_anyfunc, into_checked_anyfunc, AnyRef, Val};
use std::cell::RefCell;
use std::rc::Rc;
use std::result::Result;
use wasmtime_runtime::InstanceHandle;

// Externals

pub enum Extern {
    Func(Rc<RefCell<Func>>),
    Global(Rc<RefCell<Global>>),
    Table(Rc<RefCell<Table>>),
    Memory(Rc<RefCell<Memory>>),
}

impl Extern {
    pub fn func(&self) -> &Rc<RefCell<Func>> {
        match self {
            Extern::Func(func) => func,
            _ => panic!("Extern::Func expected"),
        }
    }
    pub fn global(&self) -> &Rc<RefCell<Global>> {
        match self {
            Extern::Global(global) => global,
            _ => panic!("Extern::Global expected"),
        }
    }
    pub fn table(&self) -> &Rc<RefCell<Table>> {
        match self {
            Extern::Table(table) => table,
            _ => panic!("Extern::Table expected"),
        }
    }
    pub fn memory(&self) -> &Rc<RefCell<Memory>> {
        match self {
            Extern::Memory(memory) => memory,
            _ => panic!("Extern::Memory expected"),
        }
    }

    pub fn r#type(&self) -> ExternType {
        match self {
            Extern::Func(ft) => ExternType::ExternFunc(ft.borrow().r#type().clone()),
            Extern::Memory(ft) => ExternType::ExternMemory(ft.borrow().r#type().clone()),
            Extern::Table(tt) => ExternType::ExternTable(tt.borrow().r#type().clone()),
            Extern::Global(gt) => ExternType::ExternGlobal(gt.borrow().r#type().clone()),
        }
    }

    pub(crate) fn get_wasmtime_export(&mut self) -> wasmtime_runtime::Export {
        match self {
            Extern::Func(f) => f.borrow().wasmtime_export().clone(),
            Extern::Global(g) => g.borrow().wasmtime_export().clone(),
            Extern::Memory(m) => m.borrow().wasmtime_export().clone(),
            Extern::Table(t) => t.borrow().wasmtime_export().clone(),
        }
    }

    pub(crate) fn from_wasmtime_export(
        store: Rc<RefCell<Store>>,
        instance_handle: InstanceHandle,
        export: wasmtime_runtime::Export,
    ) -> Extern {
        match export {
            wasmtime_runtime::Export::Function { .. } => Extern::Func(Rc::new(RefCell::new(
                Func::from_wasmtime_function(export, store, instance_handle),
            ))),
            wasmtime_runtime::Export::Memory { .. } => Extern::Memory(Rc::new(RefCell::new(
                Memory::from_wasmtime_memory(export, store, instance_handle),
            ))),
            wasmtime_runtime::Export::Global { .. } => Extern::Global(Rc::new(RefCell::new(
                Global::from_wasmtime_global(export, store),
            ))),
            wasmtime_runtime::Export::Table { .. } => Extern::Table(Rc::new(RefCell::new(
                Table::from_wasmtime_table(export, store, instance_handle),
            ))),
        }
    }
}

pub struct Func {
    _store: Rc<RefCell<Store>>,
    callable: Rc<dyn WrappedCallable + 'static>,
    r#type: FuncType,
}

impl Func {
    pub fn new(
        store: Rc<RefCell<Store>>,
        ty: FuncType,
        callable: Rc<dyn Callable + 'static>,
    ) -> Self {
        let callable = Rc::new(NativeCallable::new(callable, &ty, &store));
        Func::from_wrapped(store, ty, callable)
    }

    fn from_wrapped(
        store: Rc<RefCell<Store>>,
        r#type: FuncType,
        callable: Rc<dyn WrappedCallable + 'static>,
    ) -> Func {
        Func {
            _store: store,
            callable,
            r#type,
        }
    }

    pub fn r#type(&self) -> &FuncType {
        &self.r#type
    }

    #[cfg(feature = "wasm-c-api")]
    pub(crate) fn callable(&self) -> &Rc<dyn WrappedCallable + 'static> {
        &self.callable
    }

    pub fn param_arity(&self) -> usize {
        self.r#type.params().len()
    }

    pub fn result_arity(&self) -> usize {
        self.r#type.results().len()
    }

    pub fn call(&self, params: &[Val]) -> Result<Box<[Val]>, Rc<RefCell<Trap>>> {
        let mut results = vec![Val::default(); self.result_arity()];
        self.callable.call(params, &mut results)?;
        Ok(results.into_boxed_slice())
    }

    fn wasmtime_export(&self) -> &wasmtime_runtime::Export {
        self.callable.wasmtime_export()
    }

    fn from_wasmtime_function(
        export: wasmtime_runtime::Export,
        store: Rc<RefCell<Store>>,
        instance_handle: InstanceHandle,
    ) -> Self {
        let ty = if let wasmtime_runtime::Export::Function { signature, .. } = &export {
            FuncType::from_cranelift_signature(signature.clone())
        } else {
            panic!("expected function export")
        };
        let callable = WasmtimeFn::new(store.clone(), instance_handle, export.clone());
        Func::from_wrapped(store, ty, Rc::new(callable))
    }
}

pub struct Global {
    _store: Rc<RefCell<Store>>,
    r#type: GlobalType,
    wasmtime_export: wasmtime_runtime::Export,
    #[allow(dead_code)]
    wasmtime_state: Option<crate::trampoline::GlobalState>,
}

impl Global {
    pub fn new(store: Rc<RefCell<Store>>, r#type: GlobalType, val: Val) -> Global {
        let (wasmtime_export, wasmtime_state) =
            generate_global_export(&r#type, val).expect("generated global");
        Global {
            _store: store,
            r#type,
            wasmtime_export,
            wasmtime_state: Some(wasmtime_state),
        }
    }

    pub fn r#type(&self) -> &GlobalType {
        &self.r#type
    }

    fn wasmtime_global_definition(&self) -> *mut wasmtime_runtime::VMGlobalDefinition {
        match self.wasmtime_export {
            wasmtime_runtime::Export::Global { definition, .. } => definition,
            _ => panic!("global definition not found"),
        }
    }

    pub fn get(&self) -> Val {
        let definition = unsafe { &mut *self.wasmtime_global_definition() };
        unsafe {
            match self.r#type().content() {
                ValType::I32 => Val::from(*definition.as_i32()),
                ValType::I64 => Val::from(*definition.as_i64()),
                ValType::F32 => Val::from_f32_bits(*definition.as_u32()),
                ValType::F64 => Val::from_f64_bits(*definition.as_u64()),
                _ => unimplemented!("Global::get for {:?}", self.r#type().content()),
            }
        }
    }

    pub fn set(&mut self, val: Val) {
        if val.r#type() != *self.r#type().content() {
            panic!(
                "global of type {:?} cannot be set to {:?}",
                self.r#type().content(),
                val.r#type()
            );
        }
        let definition = unsafe { &mut *self.wasmtime_global_definition() };
        unsafe {
            match val {
                Val::I32(i) => *definition.as_i32_mut() = i,
                Val::I64(i) => *definition.as_i64_mut() = i,
                Val::F32(f) => *definition.as_u32_mut() = f,
                Val::F64(f) => *definition.as_u64_mut() = f,
                _ => unimplemented!("Global::set for {:?}", val.r#type()),
            }
        }
    }

    pub(crate) fn wasmtime_export(&self) -> &wasmtime_runtime::Export {
        &self.wasmtime_export
    }

    pub(crate) fn from_wasmtime_global(
        export: wasmtime_runtime::Export,
        store: Rc<RefCell<Store>>,
    ) -> Global {
        let global = if let wasmtime_runtime::Export::Global { ref global, .. } = export {
            global
        } else {
            panic!("wasmtime export is not memory")
        };
        let ty = GlobalType::from_cranelift_global(global.clone());
        Global {
            _store: store,
            r#type: ty,
            wasmtime_export: export,
            wasmtime_state: None,
        }
    }
}

pub struct Table {
    store: Rc<RefCell<Store>>,
    r#type: TableType,
    wasmtime_handle: InstanceHandle,
    wasmtime_export: wasmtime_runtime::Export,
}

fn get_table_item(
    handle: &InstanceHandle,
    store: &Rc<RefCell<Store>>,
    table_index: cranelift_wasm::DefinedTableIndex,
    item_index: u32,
) -> Val {
    if let Some(item) = handle.table_get(table_index, item_index) {
        from_checked_anyfunc(item, store)
    } else {
        AnyRef::null().into()
    }
}

fn set_table_item(
    handle: &mut InstanceHandle,
    store: &Rc<RefCell<Store>>,
    table_index: cranelift_wasm::DefinedTableIndex,
    item_index: u32,
    val: Val,
) -> bool {
    let item = into_checked_anyfunc(val, store);
    if let Some(item_ref) = handle.table_get_mut(table_index, item_index) {
        *item_ref = item;
        true
    } else {
        false
    }
}

impl Table {
    pub fn new(store: Rc<RefCell<Store>>, r#type: TableType, init: Val) -> Table {
        match r#type.element() {
            ValType::FuncRef => (),
            _ => panic!("table is not for funcref"),
        }
        let (mut wasmtime_handle, wasmtime_export) =
            generate_table_export(&r#type).expect("generated table");

        // Initialize entries with the init value.
        match wasmtime_export {
            wasmtime_runtime::Export::Table { definition, .. } => {
                let index = wasmtime_handle.table_index(unsafe { &*definition });
                let len = unsafe { (*definition).current_elements };
                for i in 0..len {
                    let _success =
                        set_table_item(&mut wasmtime_handle, &store, index, i, init.clone());
                    assert!(_success);
                }
            }
            _ => panic!("global definition not found"),
        }

        Table {
            store,
            r#type,
            wasmtime_handle,
            wasmtime_export,
        }
    }

    pub fn r#type(&self) -> &TableType {
        &self.r#type
    }

    fn wasmtime_table_index(&self) -> cranelift_wasm::DefinedTableIndex {
        match self.wasmtime_export {
            wasmtime_runtime::Export::Table { definition, .. } => {
                self.wasmtime_handle.table_index(unsafe { &*definition })
            }
            _ => panic!("global definition not found"),
        }
    }

    pub fn get(&self, index: u32) -> Val {
        let table_index = self.wasmtime_table_index();
        get_table_item(&self.wasmtime_handle, &self.store, table_index, index)
    }

    pub fn set(&self, index: u32, val: Val) -> bool {
        let table_index = self.wasmtime_table_index();
        let mut wasmtime_handle = self.wasmtime_handle.clone();
        set_table_item(&mut wasmtime_handle, &self.store, table_index, index, val)
    }

    pub fn size(&self) -> u32 {
        match self.wasmtime_export {
            wasmtime_runtime::Export::Table { definition, .. } => unsafe {
                (*definition).current_elements
            },
            _ => panic!("global definition not found"),
        }
    }

    pub fn grow(&mut self, delta: u32, init: Val) -> bool {
        let index = self.wasmtime_table_index();
        if let Some(len) = self.wasmtime_handle.table_grow(index, delta) {
            let mut wasmtime_handle = self.wasmtime_handle.clone();
            for i in 0..delta {
                let i = len - (delta - i);
                let _success =
                    set_table_item(&mut wasmtime_handle, &self.store, index, i, init.clone());
                assert!(_success);
            }
            true
        } else {
            false
        }
    }

    pub(crate) fn wasmtime_export(&self) -> &wasmtime_runtime::Export {
        &self.wasmtime_export
    }

    pub(crate) fn from_wasmtime_table(
        export: wasmtime_runtime::Export,
        store: Rc<RefCell<Store>>,
        instance_handle: wasmtime_runtime::InstanceHandle,
    ) -> Table {
        let table = if let wasmtime_runtime::Export::Table { ref table, .. } = export {
            table
        } else {
            panic!("wasmtime export is not table")
        };
        let ty = TableType::from_cranelift_table(table.table.clone());
        Table {
            store,
            r#type: ty,
            wasmtime_handle: instance_handle,
            wasmtime_export: export,
        }
    }
}

pub struct Memory {
    _store: Rc<RefCell<Store>>,
    r#type: MemoryType,
    wasmtime_handle: InstanceHandle,
    wasmtime_export: wasmtime_runtime::Export,
}

impl Memory {
    pub fn new(store: Rc<RefCell<Store>>, r#type: MemoryType) -> Memory {
        let (wasmtime_handle, wasmtime_export) =
            generate_memory_export(&r#type).expect("generated memory");
        Memory {
            _store: store,
            r#type,
            wasmtime_handle,
            wasmtime_export,
        }
    }

    pub fn r#type(&self) -> &MemoryType {
        &self.r#type
    }

    fn wasmtime_memory_definition(&self) -> *mut wasmtime_runtime::VMMemoryDefinition {
        match self.wasmtime_export {
            wasmtime_runtime::Export::Memory { definition, .. } => definition,
            _ => panic!("memory definition not found"),
        }
    }

    pub fn data(&self) -> *mut u8 {
        unsafe { (*self.wasmtime_memory_definition()).base }
    }

    pub fn data_size(&self) -> usize {
        unsafe { (*self.wasmtime_memory_definition()).current_length }
    }

    pub fn size(&self) -> u32 {
        (self.data_size() / wasmtime_environ::WASM_PAGE_SIZE as usize) as u32
    }

    pub fn grow(&mut self, delta: u32) -> bool {
        match self.wasmtime_export {
            wasmtime_runtime::Export::Memory { definition, .. } => {
                let definition = unsafe { &(*definition) };
                let index = self.wasmtime_handle.memory_index(definition);
                self.wasmtime_handle.memory_grow(index, delta).is_some()
            }
            _ => panic!("memory definition not found"),
        }
    }

    pub(crate) fn wasmtime_export(&self) -> &wasmtime_runtime::Export {
        &self.wasmtime_export
    }

    pub(crate) fn from_wasmtime_memory(
        export: wasmtime_runtime::Export,
        store: Rc<RefCell<Store>>,
        instance_handle: wasmtime_runtime::InstanceHandle,
    ) -> Memory {
        let memory = if let wasmtime_runtime::Export::Memory { ref memory, .. } = export {
            memory
        } else {
            panic!("wasmtime export is not memory")
        };
        let ty = MemoryType::from_cranelift_memory(memory.memory.clone());
        Memory {
            _store: store,
            r#type: ty,
            wasmtime_handle: instance_handle,
            wasmtime_export: export,
        }
    }
}
