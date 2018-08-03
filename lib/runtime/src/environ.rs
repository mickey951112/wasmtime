use cranelift_codegen::cursor::FuncCursor;
use cranelift_codegen::ir;
use cranelift_codegen::ir::immediates::Offset32;
use cranelift_codegen::ir::types::*;
use cranelift_codegen::ir::{
    AbiParam, ArgumentExtension, ArgumentLoc, ArgumentPurpose, ExtFuncData, ExternalName, FuncRef,
    Function, InstBuilder, Signature,
};
use cranelift_codegen::isa;
use cranelift_codegen::settings;
use cranelift_wasm;
use cranelift_wasm::{
    FunctionIndex, Global, GlobalIndex, GlobalVariable, Memory, MemoryIndex, SignatureIndex, Table,
    TableIndex, WasmResult,
};
use module;
use module::Module;
use target_lexicon::Triple;
use ModuleTranslation;

/// Compute a `ir::ExternalName` for a given wasm function index.
pub fn get_func_name(func_index: FunctionIndex) -> ir::ExternalName {
    debug_assert!(func_index as u32 as FunctionIndex == func_index);
    ir::ExternalName::user(0, func_index as u32)
}

/// A data initializer for linear memory.
pub struct DataInitializer<'data> {
    /// The index of the memory to initialize.
    pub memory_index: MemoryIndex,
    /// Optionally a globalvar base to initialize at.
    pub base: Option<GlobalIndex>,
    /// A constant offset to initialize at.
    pub offset: usize,
    /// The initialization data.
    pub data: &'data [u8],
}

/// References to the input wasm data buffer to be decoded and processed later.
/// separately from the main module translation.
pub struct LazyContents<'data> {
    /// References to the function bodies.
    pub function_body_inputs: Vec<&'data [u8]>,

    /// References to the data initializers.
    pub data_initializers: Vec<DataInitializer<'data>>,
}

impl<'data> LazyContents<'data> {
    fn new() -> Self {
        Self {
            function_body_inputs: Vec::new(),
            data_initializers: Vec::new(),
        }
    }
}

/// Object containing the standalone runtime information. To be passed after creation as argument
/// to `cranelift_wasm::translatemodule`.
pub struct ModuleEnvironment<'data, 'module> {
    /// Compilation setting flags.
    pub isa: &'module isa::TargetIsa,

    /// Module information.
    pub module: &'module mut Module,

    /// References to information to be decoded later.
    pub lazy: LazyContents<'data>,
}

impl<'data, 'module> ModuleEnvironment<'data, 'module> {
    /// Allocates the runtime data structures with the given isa.
    pub fn new(isa: &'module isa::TargetIsa, module: &'module mut Module) -> Self {
        Self {
            isa,
            module,
            lazy: LazyContents::new(),
        }
    }

    fn func_env(&self) -> FuncEnvironment {
        FuncEnvironment::new(self.isa, &self.module)
    }

    fn pointer_type(&self) -> ir::Type {
        use cranelift_wasm::FuncEnvironment;
        self.func_env().pointer_type()
    }

    /// Declare that translation of the module is complete. This consumes the
    /// `ModuleEnvironment` with its mutable reference to the `Module` and
    /// produces a `ModuleTranslation` with an immutable reference to the
    /// `Module`.
    pub fn finish_translation(self) -> ModuleTranslation<'data, 'module> {
        ModuleTranslation {
            isa: self.isa,
            module: self.module,
            lazy: self.lazy,
        }
    }
}

/// The FuncEnvironment implementation for use by the `ModuleEnvironment`.
pub struct FuncEnvironment<'module_environment> {
    /// Compilation setting flags.
    isa: &'module_environment isa::TargetIsa,

    /// The module-level environment which this function-level environment belongs to.
    pub module: &'module_environment Module,

    /// The Cranelift global holding the base address of the memories vector.
    pub memories_base: Option<ir::GlobalValue>,

    /// The Cranelift global holding the base address of the globals vector.
    pub globals_base: Option<ir::GlobalValue>,

    /// The external function declaration for implementing wasm's `current_memory`.
    pub current_memory_extfunc: Option<FuncRef>,

    /// The external function declaration for implementing wasm's `grow_memory`.
    pub grow_memory_extfunc: Option<FuncRef>,
}

impl<'module_environment> FuncEnvironment<'module_environment> {
    pub fn new(
        isa: &'module_environment isa::TargetIsa,
        module: &'module_environment Module,
    ) -> Self {
        Self {
            isa,
            module,
            memories_base: None,
            globals_base: None,
            current_memory_extfunc: None,
            grow_memory_extfunc: None,
        }
    }

    /// Transform the call argument list in preparation for making a call.
    fn get_real_call_args(func: &Function, call_args: &[ir::Value]) -> Vec<ir::Value> {
        let mut real_call_args = Vec::with_capacity(call_args.len() + 1);
        real_call_args.extend_from_slice(call_args);
        real_call_args.push(func.special_param(ArgumentPurpose::VMContext).unwrap());
        real_call_args
    }

    fn ptr_size(&self) -> usize {
        usize::from(self.isa.pointer_bytes())
    }
}

/// This trait is useful for
/// `cranelift_wasm::translatemodule` because it
/// tells how to translate runtime-dependent wasm instructions. These functions should not be
/// called by the user.
impl<'data, 'module> cranelift_wasm::ModuleEnvironment<'data>
    for ModuleEnvironment<'data, 'module>
{
    fn get_func_name(&self, func_index: FunctionIndex) -> ir::ExternalName {
        get_func_name(func_index)
    }

    fn flags(&self) -> &settings::Flags {
        self.isa.flags()
    }

    fn declare_signature(&mut self, sig: &ir::Signature) {
        let mut sig = sig.clone();
        sig.params.push(AbiParam {
            value_type: self.pointer_type(),
            purpose: ArgumentPurpose::VMContext,
            extension: ArgumentExtension::None,
            location: ArgumentLoc::Unassigned,
        });
        // TODO: Deduplicate signatures.
        self.module.signatures.push(sig);
    }

    fn get_signature(&self, sig_index: SignatureIndex) -> &ir::Signature {
        &self.module.signatures[sig_index]
    }

    fn declare_func_import(&mut self, sig_index: SignatureIndex, module: &str, field: &str) {
        debug_assert_eq!(
            self.module.functions.len(),
            self.module.imported_funcs.len(),
            "Imported functions must be declared first"
        );
        self.module.functions.push(sig_index);

        self.module
            .imported_funcs
            .push((String::from(module), String::from(field)));
    }

    fn get_num_func_imports(&self) -> usize {
        self.module.imported_funcs.len()
    }

    fn declare_func_type(&mut self, sig_index: SignatureIndex) {
        self.module.functions.push(sig_index);
    }

    fn get_func_type(&self, func_index: FunctionIndex) -> SignatureIndex {
        self.module.functions[func_index]
    }

    fn declare_global(&mut self, global: Global) {
        self.module.globals.push(global);
    }

    fn get_global(&self, global_index: GlobalIndex) -> &cranelift_wasm::Global {
        &self.module.globals[global_index]
    }

    fn declare_table(&mut self, table: Table) {
        self.module.tables.push(table);
    }

    fn declare_table_elements(
        &mut self,
        table_index: TableIndex,
        base: Option<GlobalIndex>,
        offset: usize,
        elements: Vec<FunctionIndex>,
    ) {
        debug_assert!(base.is_none(), "global-value offsets not supported yet");
        self.module.table_elements.push(module::TableElements {
            table_index,
            base,
            offset,
            elements,
        });
    }

    fn declare_memory(&mut self, memory: Memory) {
        self.module.memories.push(memory);
    }

    fn declare_data_initialization(
        &mut self,
        memory_index: MemoryIndex,
        base: Option<GlobalIndex>,
        offset: usize,
        data: &'data [u8],
    ) {
        debug_assert!(base.is_none(), "global-value offsets not supported yet");
        self.lazy.data_initializers.push(DataInitializer {
            memory_index,
            base,
            offset,
            data,
        });
    }

    fn declare_func_export(&mut self, func_index: FunctionIndex, name: &str) {
        self.module
            .exports
            .insert(String::from(name), module::Export::Function(func_index));
    }

    fn declare_table_export(&mut self, table_index: TableIndex, name: &str) {
        self.module
            .exports
            .insert(String::from(name), module::Export::Table(table_index));
    }

    fn declare_memory_export(&mut self, memory_index: MemoryIndex, name: &str) {
        self.module
            .exports
            .insert(String::from(name), module::Export::Memory(memory_index));
    }

    fn declare_global_export(&mut self, global_index: GlobalIndex, name: &str) {
        self.module
            .exports
            .insert(String::from(name), module::Export::Global(global_index));
    }

    fn declare_start_func(&mut self, func_index: FunctionIndex) {
        debug_assert!(self.module.start_func.is_none());
        self.module.start_func = Some(func_index);
    }

    fn define_function_body(&mut self, body_bytes: &'data [u8]) -> WasmResult<()> {
        self.lazy.function_body_inputs.push(body_bytes);
        Ok(())
    }
}

impl<'module_environment> cranelift_wasm::FuncEnvironment for FuncEnvironment<'module_environment> {
    fn flags(&self) -> &settings::Flags {
        &self.isa.flags()
    }

    fn triple(&self) -> &Triple {
        self.isa.triple()
    }

    fn make_global(&mut self, func: &mut ir::Function, index: GlobalIndex) -> GlobalVariable {
        let ptr_size = self.ptr_size();
        let globals_base = self.globals_base.unwrap_or_else(|| {
            let new_base = func.create_global_value(ir::GlobalValueData::VMContext {
                offset: Offset32::new(0),
            });
            self.globals_base = Some(new_base);
            new_base
        });
        let offset = index as usize * ptr_size;
        let offset32 = offset as i32;
        debug_assert_eq!(offset32 as usize, offset);
        let gv = func.create_global_value(ir::GlobalValueData::Deref {
            base: globals_base,
            offset: Offset32::new(offset32),
        });
        GlobalVariable::Memory {
            gv,
            ty: self.module.globals[index].ty,
        }
    }

    fn make_heap(&mut self, func: &mut ir::Function, index: MemoryIndex) -> ir::Heap {
        let ptr_size = self.ptr_size();
        let memories_base = self.memories_base.unwrap_or_else(|| {
            let new_base = func.create_global_value(ir::GlobalValueData::VMContext {
                offset: Offset32::new(ptr_size as i32),
            });
            self.globals_base = Some(new_base);
            new_base
        });
        let offset = index as usize * ptr_size;
        let offset32 = offset as i32;
        debug_assert_eq!(offset32 as usize, offset);
        let heap_base_addr = func.create_global_value(ir::GlobalValueData::Deref {
            base: memories_base,
            offset: Offset32::new(offset32),
        });
        let heap_base = func.create_global_value(ir::GlobalValueData::Deref {
            base: heap_base_addr,
            offset: Offset32::new(0),
        });
        func.create_heap(ir::HeapData {
            base: heap_base,
            min_size: 0.into(),
            guard_size: 0x8000_0000.into(),
            style: ir::HeapStyle::Static {
                bound: 0x1_0000_0000.into(),
            },
        })
    }

    fn make_table(&mut self, _func: &mut ir::Function, _index: TableIndex) -> ir::Table {
        unimplemented!("make_table");
    }

    fn make_indirect_sig(&mut self, func: &mut ir::Function, index: SignatureIndex) -> ir::SigRef {
        func.import_signature(self.module.signatures[index].clone())
    }

    fn make_direct_func(&mut self, func: &mut ir::Function, index: FunctionIndex) -> ir::FuncRef {
        let sigidx = self.module.functions[index];
        let signature = func.import_signature(self.module.signatures[sigidx].clone());
        let name = get_func_name(index);
        // We currently allocate all code segments independently, so nothing
        // is colocated.
        let colocated = false;
        func.import_function(ir::ExtFuncData {
            name,
            signature,
            colocated,
        })
    }

    fn translate_call_indirect(
        &mut self,
        mut pos: FuncCursor,
        table_index: TableIndex,
        _table: ir::Table,
        _sig_index: SignatureIndex,
        sig_ref: ir::SigRef,
        callee: ir::Value,
        call_args: &[ir::Value],
    ) -> WasmResult<ir::Inst> {
        // TODO: Cranelift's call_indirect doesn't implement bounds checking
        // or signature checking, so we need to implement it ourselves.
        debug_assert_eq!(table_index, 0, "non-default tables not supported yet");
        let real_call_args = FuncEnvironment::get_real_call_args(pos.func, call_args);
        Ok(pos.ins().call_indirect(sig_ref, callee, &real_call_args))
    }

    fn translate_call(
        &mut self,
        mut pos: FuncCursor,
        _callee_index: FunctionIndex,
        callee: ir::FuncRef,
        call_args: &[ir::Value],
    ) -> WasmResult<ir::Inst> {
        let real_call_args = FuncEnvironment::get_real_call_args(pos.func, call_args);
        Ok(pos.ins().call(callee, &real_call_args))
    }

    fn translate_memory_grow(
        &mut self,
        mut pos: FuncCursor,
        index: MemoryIndex,
        _heap: ir::Heap,
        val: ir::Value,
    ) -> WasmResult<ir::Value> {
        debug_assert_eq!(index, 0, "non-default memories not supported yet");
        let grow_mem_func = self.grow_memory_extfunc.unwrap_or_else(|| {
            let sig_ref = pos.func.import_signature(Signature {
                call_conv: self.isa.flags().call_conv(),
                argument_bytes: None,
                params: vec![AbiParam::new(I32)],
                returns: vec![AbiParam::new(I32)],
            });
            // We currently allocate all code segments independently, so nothing
            // is colocated.
            let colocated = false;
            // FIXME: Use a real ExternalName system.
            pos.func.import_function(ExtFuncData {
                name: ExternalName::testcase("grow_memory"),
                signature: sig_ref,
                colocated,
            })
        });
        self.grow_memory_extfunc = Some(grow_mem_func);
        let call_inst = pos.ins().call(grow_mem_func, &[val]);
        Ok(*pos.func.dfg.inst_results(call_inst).first().unwrap())
    }

    fn translate_memory_size(
        &mut self,
        mut pos: FuncCursor,
        index: MemoryIndex,
        _heap: ir::Heap,
    ) -> WasmResult<ir::Value> {
        debug_assert_eq!(index, 0, "non-default memories not supported yet");
        let cur_mem_func = self.current_memory_extfunc.unwrap_or_else(|| {
            let sig_ref = pos.func.import_signature(Signature {
                call_conv: self.isa.flags().call_conv(),
                argument_bytes: None,
                params: Vec::new(),
                returns: vec![AbiParam::new(I32)],
            });
            // We currently allocate all code segments independently, so nothing
            // is colocated.
            let colocated = false;
            // FIXME: Use a real ExternalName system.
            pos.func.import_function(ExtFuncData {
                name: ExternalName::testcase("current_memory"),
                signature: sig_ref,
                colocated,
            })
        });
        self.current_memory_extfunc = Some(cur_mem_func);
        let call_inst = pos.ins().call(cur_mem_func, &[]);
        Ok(*pos.func.dfg.inst_results(call_inst).first().unwrap())
    }
}
