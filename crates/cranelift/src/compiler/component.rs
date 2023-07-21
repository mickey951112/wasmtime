//! Compilation support for the component model.

use crate::compiler::{Compiler, NativeRet};
use anyhow::Result;
use cranelift_codegen::ir::{self, InstBuilder, MemFlags};
use cranelift_codegen::isa::CallConv;
use cranelift_frontend::FunctionBuilder;
use std::any::Any;
use wasmtime_cranelift_shared::{ALWAYS_TRAP_CODE, CANNOT_ENTER_CODE};
use wasmtime_environ::component::*;
use wasmtime_environ::{PtrSize, WasmFuncType, WasmType};

#[derive(Copy, Clone)]
enum Abi {
    Wasm,
    Native,
    Array,
}

impl Compiler {
    fn compile_lowered_trampoline_for_abi(
        &self,
        component: &Component,
        lowering: &LowerImport,
        types: &ComponentTypes,
        abi: Abi,
    ) -> Result<Box<dyn Any + Send>> {
        let wasm_func_ty = &types[lowering.canonical_abi];
        let isa = &*self.isa;
        let pointer_type = isa.pointer_type();
        let offsets = VMComponentOffsets::new(isa.pointer_bytes(), component);

        let mut compiler = self.function_compiler();

        let func = self.func(wasm_func_ty, abi);
        let (mut builder, block0) = compiler.builder(func);
        let args = builder.func.dfg.block_params(block0).to_vec();
        let vmctx = args[0];

        // More handling is necessary here if this changes
        assert!(matches!(
            NativeRet::classify(pointer_type, wasm_func_ty),
            NativeRet::Bare
        ));

        // Start off by spilling all the wasm arguments into a stack slot to be
        // passed to the host function.
        let (values_vec_ptr, values_vec_len) = match abi {
            Abi::Wasm | Abi::Native => {
                let (ptr, len) = self.allocate_stack_array_and_spill_args(
                    wasm_func_ty,
                    &mut builder,
                    &args[2..],
                );
                let len = builder.ins().iconst(pointer_type, i64::from(len));
                (ptr, len)
            }
            Abi::Array => {
                let params = builder.func.dfg.block_params(block0);
                (params[2], params[3])
            }
        };

        self.abi_preamble(&mut builder, &offsets, vmctx, abi);

        // Below this will incrementally build both the signature of the host
        // function we're calling as well as the list of arguments since the
        // list is somewhat long.
        let mut callee_args = Vec::new();
        let mut host_sig = ir::Signature::new(CallConv::triple_default(isa.triple()));

        let CanonicalOptions {
            instance,
            memory,
            realloc,
            post_return,
            string_encoding,
        } = lowering.options;

        // vmctx: *mut VMComponentContext
        host_sig.params.push(ir::AbiParam::new(pointer_type));
        callee_args.push(vmctx);

        // data: *mut u8,
        host_sig.params.push(ir::AbiParam::new(pointer_type));
        callee_args.push(builder.ins().load(
            pointer_type,
            MemFlags::trusted(),
            vmctx,
            i32::try_from(offsets.lowering_data(lowering.index)).unwrap(),
        ));

        // ty: TypeFuncIndex,
        let ty = lowering.lower_ty;
        host_sig.params.push(ir::AbiParam::new(ir::types::I32));
        callee_args.push(builder.ins().iconst(ir::types::I32, i64::from(ty.as_u32())));

        // flags: *mut VMGlobalDefinition
        host_sig.params.push(ir::AbiParam::new(pointer_type));
        callee_args.push(
            builder
                .ins()
                .iadd_imm(vmctx, i64::from(offsets.instance_flags(instance))),
        );

        // memory: *mut VMMemoryDefinition
        host_sig.params.push(ir::AbiParam::new(pointer_type));
        callee_args.push(match memory {
            Some(idx) => builder.ins().load(
                pointer_type,
                MemFlags::trusted(),
                vmctx,
                i32::try_from(offsets.runtime_memory(idx)).unwrap(),
            ),
            None => builder.ins().iconst(pointer_type, 0),
        });

        // realloc: *mut VMFuncRef
        host_sig.params.push(ir::AbiParam::new(pointer_type));
        callee_args.push(match realloc {
            Some(idx) => builder.ins().load(
                pointer_type,
                MemFlags::trusted(),
                vmctx,
                i32::try_from(offsets.runtime_realloc(idx)).unwrap(),
            ),
            None => builder.ins().iconst(pointer_type, 0),
        });

        // A post-return option is only valid on `canon.lift`'d functions so no
        // valid component should have this specified for a lowering which this
        // trampoline compiler is interested in.
        assert!(post_return.is_none());

        // string_encoding: StringEncoding
        host_sig.params.push(ir::AbiParam::new(ir::types::I8));
        callee_args.push(
            builder
                .ins()
                .iconst(ir::types::I8, i64::from(string_encoding as u8)),
        );

        // storage: *mut ValRaw
        host_sig.params.push(ir::AbiParam::new(pointer_type));
        callee_args.push(values_vec_ptr);

        // storage_len: usize
        host_sig.params.push(ir::AbiParam::new(pointer_type));
        callee_args.push(values_vec_len);

        // Load host function pointer from the vmcontext and then call that
        // indirect function pointer with the list of arguments.
        let host_fn = builder.ins().load(
            pointer_type,
            MemFlags::trusted(),
            vmctx,
            i32::try_from(offsets.lowering_callee(lowering.index)).unwrap(),
        );
        let host_sig = builder.import_signature(host_sig);
        builder.ins().call_indirect(host_sig, host_fn, &callee_args);

        match abi {
            Abi::Wasm | Abi::Native => {
                // After the host function has returned the results are loaded from
                // `values_vec_ptr` and then returned.
                let results = self.load_values_from_array(
                    wasm_func_ty.returns(),
                    &mut builder,
                    values_vec_ptr,
                    values_vec_len,
                );
                builder.ins().return_(&results);
            }
            Abi::Array => {
                builder.ins().return_(&[]);
            }
        }
        builder.finalize();

        Ok(Box::new(compiler.finish()?))
    }

    fn compile_always_trap_for_abi(
        &self,
        ty: &WasmFuncType,
        abi: Abi,
    ) -> Result<Box<dyn Any + Send>> {
        let mut compiler = self.function_compiler();
        let func = self.func(ty, abi);
        let (mut builder, _block0) = compiler.builder(func);
        builder.ins().trap(ir::TrapCode::User(ALWAYS_TRAP_CODE));
        builder.finalize();

        Ok(Box::new(compiler.finish()?))
    }

    fn compile_transcoder_for_abi(
        &self,
        component: &Component,
        transcoder: &Transcoder,
        types: &ComponentTypes,
        abi: Abi,
    ) -> Result<Box<dyn Any + Send>> {
        let ty = &types[transcoder.signature];
        let isa = &*self.isa;
        let offsets = VMComponentOffsets::new(isa.pointer_bytes(), component);
        let mut compiler = self.function_compiler();
        let func = self.func(ty, abi);
        let (mut builder, block0) = compiler.builder(func);

        match abi {
            Abi::Wasm => {
                self.translate_transcode(&mut builder, &offsets, transcoder, block0);
            }
            // Transcoders can only actually be called by Wasm, so let's assert
            // that here.
            Abi::Native | Abi::Array => {
                builder
                    .ins()
                    .trap(ir::TrapCode::User(crate::DEBUG_ASSERT_TRAP_CODE));
            }
        }

        builder.finalize();
        Ok(Box::new(compiler.finish()?))
    }

    fn compile_resource_new_for_abi(
        &self,
        component: &Component,
        resource: &ResourceNew,
        types: &ComponentTypes,
        abi: Abi,
    ) -> Result<Box<dyn Any + Send>> {
        let ty = &types[resource.signature];
        let isa = &*self.isa;
        let offsets = VMComponentOffsets::new(isa.pointer_bytes(), component);
        let mut compiler = self.function_compiler();
        let func = self.func(ty, abi);
        let (mut builder, block0) = compiler.builder(func);

        let args = self.abi_load_params(&mut builder, ty, block0, abi);
        let vmctx = args[0];

        self.abi_preamble(&mut builder, &offsets, vmctx, abi);

        // The arguments this shim passes along to the libcall are:
        //
        //   * the vmctx
        //   * a constant value for this `ResourceNew` intrinsic
        //   * the wasm argument to wrap
        let mut host_args = Vec::new();
        host_args.push(vmctx);
        host_args.push(
            builder
                .ins()
                .iconst(ir::types::I32, i64::from(resource.resource.as_u32())),
        );
        host_args.push(args[2]);

        // Currently this only support resources represented by `i32`
        assert_eq!(ty.params()[0], WasmType::I32);
        let (host_sig, offset) = host::resource_new32(self, &mut builder.func);

        let host_fn = self.load_libcall(&mut builder, &offsets, vmctx, offset);
        let call = builder.ins().call_indirect(host_sig, host_fn, &host_args);
        let result = builder.func.dfg.inst_results(call)[0];
        self.abi_store_results(&mut builder, ty, block0, &[result], abi);

        builder.finalize();
        Ok(Box::new(compiler.finish()?))
    }

    fn compile_resource_rep_for_abi(
        &self,
        component: &Component,
        resource: &ResourceRep,
        types: &ComponentTypes,
        abi: Abi,
    ) -> Result<Box<dyn Any + Send>> {
        let ty = &types[resource.signature];
        let isa = &*self.isa;
        let offsets = VMComponentOffsets::new(isa.pointer_bytes(), component);
        let mut compiler = self.function_compiler();
        let func = self.func(ty, abi);
        let (mut builder, block0) = compiler.builder(func);

        let args = self.abi_load_params(&mut builder, ty, block0, abi);
        let vmctx = args[0];

        self.abi_preamble(&mut builder, &offsets, vmctx, abi);

        // The arguments this shim passes along to the libcall are:
        //
        //   * the vmctx
        //   * a constant value for this `ResourceRep` intrinsic
        //   * the wasm argument to unwrap
        let mut host_args = Vec::new();
        host_args.push(vmctx);
        host_args.push(
            builder
                .ins()
                .iconst(ir::types::I32, i64::from(resource.resource.as_u32())),
        );
        host_args.push(args[2]);

        // Currently this only support resources represented by `i32`
        assert_eq!(ty.returns()[0], WasmType::I32);
        let (host_sig, offset) = host::resource_rep32(self, &mut builder.func);

        let host_fn = self.load_libcall(&mut builder, &offsets, vmctx, offset);
        let call = builder.ins().call_indirect(host_sig, host_fn, &host_args);
        let result = builder.func.dfg.inst_results(call)[0];
        self.abi_store_results(&mut builder, ty, block0, &[result], abi);

        builder.finalize();
        Ok(Box::new(compiler.finish()?))
    }

    fn compile_resource_drop_for_abi(
        &self,
        component: &Component,
        resource: &ResourceDrop,
        types: &ComponentTypes,
        abi: Abi,
    ) -> Result<Box<dyn Any + Send>> {
        let pointer_type = self.isa.pointer_type();
        let ty = &types[resource.signature];
        let isa = &*self.isa;
        let offsets = VMComponentOffsets::new(isa.pointer_bytes(), component);
        let mut compiler = self.function_compiler();
        let func = self.func(ty, abi);
        let (mut builder, block0) = compiler.builder(func);

        let args = self.abi_load_params(&mut builder, ty, block0, abi);
        let vmctx = args[0];
        let caller_vmctx = args[1];

        self.abi_preamble(&mut builder, &offsets, vmctx, abi);

        // The arguments this shim passes along to the libcall are:
        //
        //   * the vmctx
        //   * a constant value for this `ResourceDrop` intrinsic
        //   * the wasm handle index to drop
        let mut host_args = Vec::new();
        host_args.push(vmctx);
        host_args.push(
            builder
                .ins()
                .iconst(ir::types::I32, i64::from(resource.resource.as_u32())),
        );
        host_args.push(args[2]);

        let (host_sig, offset) = host::resource_drop(self, &mut builder.func);
        let host_fn = self.load_libcall(&mut builder, &offsets, vmctx, offset);
        let call = builder.ins().call_indirect(host_sig, host_fn, &host_args);
        let should_run_destructor = builder.func.dfg.inst_results(call)[0];

        let resource_ty = types[resource.resource].ty;
        let resource_def = component.defined_resource_index(resource_ty).map(|idx| {
            component
                .initializers
                .iter()
                .filter_map(|i| match i {
                    GlobalInitializer::Resource(r) if r.index == idx => Some(r),
                    _ => None,
                })
                .next()
                .unwrap()
        });
        let has_destructor = match resource_def {
            Some(def) => def.dtor.is_some(),
            None => true,
        };
        // Synthesize the following:
        //
        //      ...
        //      brif should_run_destructor, run_destructor_block, return_block
        //
        //    run_destructor_block:
        //      ;; test may_enter, but only if the component instances
        //      ;; differ
        //      flags = load.i32 vmctx+$offset
        //      masked = band flags, $FLAG_MAY_ENTER
        //      trapz masked, CANNOT_ENTER_CODE
        //
        //      ;; ============================================================
        //      ;; this is conditionally emitted based on whether the resource
        //      ;; has a destructor or not, and can be statically omitted
        //      ;; because that information is known at compile time here.
        //      rep = ushr.i64 rep, 1
        //      rep = ireduce.i32 rep
        //      dtor = load.ptr vmctx+$offset
        //      func_addr = load.ptr dtor+$offset
        //      callee_vmctx = load.ptr dtor+$offset
        //      call_indirect func_addr, callee_vmctx, vmctx, rep
        //      ;; ============================================================
        //
        //      jump return_block
        //
        //    return_block:
        //      return
        //
        // This will decode `should_run_destructor` and run the destructor
        // funcref if one is specified for this resource. Note that not all
        // resources have destructors, hence the null check.
        builder.ensure_inserted_block();
        let current_block = builder.current_block().unwrap();
        let run_destructor_block = builder.create_block();
        builder.insert_block_after(run_destructor_block, current_block);
        let return_block = builder.create_block();
        builder.insert_block_after(return_block, run_destructor_block);

        builder.ins().brif(
            should_run_destructor,
            run_destructor_block,
            &[],
            return_block,
            &[],
        );

        let trusted = ir::MemFlags::trusted().with_readonly();

        builder.switch_to_block(run_destructor_block);

        // If this is a defined resource within the component itself then a
        // check needs to be emitted for the `may_enter` flag. Note though
        // that this check can be elided if the resource table resides in
        // the same component instance that defined the resource as the
        // component is calling itself.
        if let Some(def) = resource_def {
            if types[resource.resource].instance != def.instance {
                let flags = builder.ins().load(
                    ir::types::I32,
                    trusted,
                    vmctx,
                    i32::try_from(offsets.instance_flags(def.instance)).unwrap(),
                );
                let masked = builder.ins().band_imm(flags, i64::from(FLAG_MAY_ENTER));
                builder
                    .ins()
                    .trapz(masked, ir::TrapCode::User(CANNOT_ENTER_CODE));
            }
        }

        // Conditionally emit destructor-execution code based on whether we
        // statically know that a destructor exists or not.
        if has_destructor {
            let rep = builder.ins().ushr_imm(should_run_destructor, 1);
            let rep = builder.ins().ireduce(ir::types::I32, rep);
            let index = types[resource.resource].ty;
            // NB: despite the vmcontext storing nullable funcrefs for function
            // pointers we know this is statically never null due to the
            // `has_destructor` check above.
            let dtor_func_ref = builder.ins().load(
                pointer_type,
                trusted,
                vmctx,
                i32::try_from(offsets.resource_destructor(index)).unwrap(),
            );
            if cfg!(debug_assertions) {
                builder.ins().trapz(
                    dtor_func_ref,
                    ir::TrapCode::User(crate::DEBUG_ASSERT_TRAP_CODE),
                );
            }
            let func_addr = builder.ins().load(
                pointer_type,
                trusted,
                dtor_func_ref,
                i32::from(offsets.ptr.vm_func_ref_wasm_call()),
            );
            let callee_vmctx = builder.ins().load(
                pointer_type,
                trusted,
                dtor_func_ref,
                i32::from(offsets.ptr.vm_func_ref_vmctx()),
            );
            let sig = crate::wasm_call_signature(isa, &types[resource.signature]);
            let sig_ref = builder.import_signature(sig);
            // NB: note that the "caller" vmctx here is the caller of this
            // intrinsic itself, not the `VMComponentContext`. This effectively
            // takes ourselves out of the chain here but that's ok since the
            // caller is only used for store/limits and that same info is
            // stored, but elsewhere, in the component context.
            builder
                .ins()
                .call_indirect(sig_ref, func_addr, &[callee_vmctx, caller_vmctx, rep]);
        }
        builder.ins().jump(return_block, &[]);
        builder.seal_block(run_destructor_block);

        builder.switch_to_block(return_block);
        builder.ins().return_(&[]);
        builder.seal_block(return_block);

        builder.finalize();
        Ok(Box::new(compiler.finish()?))
    }

    fn func(&self, ty: &WasmFuncType, abi: Abi) -> ir::Function {
        let isa = &*self.isa;
        ir::Function::with_name_signature(
            ir::UserFuncName::user(0, 0),
            match abi {
                Abi::Wasm => crate::wasm_call_signature(isa, ty),
                Abi::Native => crate::native_call_signature(isa, ty),
                Abi::Array => crate::array_call_signature(isa),
            },
        )
    }

    fn compile_func_ref(
        &self,
        compile: impl Fn(Abi) -> Result<Box<dyn Any + Send>>,
    ) -> Result<AllCallFunc<Box<dyn Any + Send>>> {
        Ok(AllCallFunc {
            wasm_call: compile(Abi::Wasm)?,
            array_call: compile(Abi::Array)?,
            native_call: compile(Abi::Native)?,
        })
    }

    /// Loads a host function pointer for a libcall stored at the `offset`
    /// provided in the libcalls array.
    ///
    /// The offset is calculated in the `host` module below.
    fn load_libcall(
        &self,
        builder: &mut FunctionBuilder<'_>,
        offsets: &VMComponentOffsets<u8>,
        vmctx: ir::Value,
        offset: u32,
    ) -> ir::Value {
        let pointer_type = self.isa.pointer_type();
        // First load the pointer to the libcalls structure which is static
        // per-process.
        let libcalls_array = builder.ins().load(
            pointer_type,
            MemFlags::trusted().with_readonly(),
            vmctx,
            i32::try_from(offsets.libcalls()).unwrap(),
        );
        // Next load the function pointer at `offset` and return that.
        builder.ins().load(
            pointer_type,
            MemFlags::trusted().with_readonly(),
            libcalls_array,
            i32::try_from(offset * u32::from(offsets.ptr.size())).unwrap(),
        )
    }

    fn abi_load_params(
        &self,
        builder: &mut FunctionBuilder<'_>,
        ty: &WasmFuncType,
        block0: ir::Block,
        abi: Abi,
    ) -> Vec<ir::Value> {
        let mut block0_params = builder.func.dfg.block_params(block0).to_vec();
        match abi {
            // Wasm and native ABIs pass parameters as normal function
            // parameters.
            Abi::Wasm | Abi::Native => block0_params,

            // The array ABI passes a pointer/length as the 3rd/4th arguments
            // and those are used to load the actual wasm parameters.
            Abi::Array => {
                let results = self.load_values_from_array(
                    ty.params(),
                    builder,
                    block0_params[2],
                    block0_params[3],
                );
                block0_params.truncate(2);
                block0_params.extend(results);
                block0_params
            }
        }
    }

    fn abi_store_results(
        &self,
        builder: &mut FunctionBuilder<'_>,
        ty: &WasmFuncType,
        block0: ir::Block,
        results: &[ir::Value],
        abi: Abi,
    ) {
        match abi {
            // Wasm/native ABIs return values as usual.
            Abi::Wasm | Abi::Native => {
                builder.ins().return_(results);
            }

            // The array ABI stores all results in the pointer/length passed
            // as arguments to this function, which contractually are required
            // to have enough space for the results.
            Abi::Array => {
                let block0_params = builder.func.dfg.block_params(block0);
                self.store_values_to_array(
                    builder,
                    ty.returns(),
                    results,
                    block0_params[2],
                    block0_params[3],
                );
                builder.ins().return_(&[]);
            }
        }
    }

    fn abi_preamble(
        &self,
        builder: &mut FunctionBuilder<'_>,
        offsets: &VMComponentOffsets<u8>,
        vmctx: ir::Value,
        abi: Abi,
    ) {
        let pointer_type = self.isa.pointer_type();
        // If we are crossing the Wasm-to-native boundary, we need to save the
        // exit FP and return address for stack walking purposes. However, we
        // always debug assert that our vmctx is a component context, regardless
        // whether we are actually crossing that boundary because it should
        // always hold.
        super::debug_assert_vmctx_kind(
            &*self.isa,
            builder,
            vmctx,
            wasmtime_environ::component::VMCOMPONENT_MAGIC,
        );
        if let Abi::Wasm = abi {
            let limits = builder.ins().load(
                pointer_type,
                MemFlags::trusted(),
                vmctx,
                i32::try_from(offsets.limits()).unwrap(),
            );
            super::save_last_wasm_exit_fp_and_pc(builder, pointer_type, &offsets.ptr, limits);
        }
    }
}

impl ComponentCompiler for Compiler {
    fn compile_lowered_trampoline(
        &self,
        component: &Component,
        lowering: &LowerImport,
        types: &ComponentTypes,
    ) -> Result<AllCallFunc<Box<dyn Any + Send>>> {
        self.compile_func_ref(|abi| {
            self.compile_lowered_trampoline_for_abi(component, lowering, types, abi)
        })
    }

    fn compile_always_trap(&self, ty: &WasmFuncType) -> Result<AllCallFunc<Box<dyn Any + Send>>> {
        self.compile_func_ref(|abi| self.compile_always_trap_for_abi(ty, abi))
    }

    fn compile_transcoder(
        &self,
        component: &Component,
        transcoder: &Transcoder,
        types: &ComponentTypes,
    ) -> Result<AllCallFunc<Box<dyn Any + Send>>> {
        self.compile_func_ref(|abi| {
            self.compile_transcoder_for_abi(component, transcoder, types, abi)
        })
    }

    fn compile_resource_new(
        &self,
        component: &Component,
        resource: &ResourceNew,
        types: &ComponentTypes,
    ) -> Result<AllCallFunc<Box<dyn Any + Send>>> {
        self.compile_func_ref(|abi| {
            self.compile_resource_new_for_abi(component, resource, types, abi)
        })
    }

    fn compile_resource_rep(
        &self,
        component: &Component,
        resource: &ResourceRep,
        types: &ComponentTypes,
    ) -> Result<AllCallFunc<Box<dyn Any + Send>>> {
        self.compile_func_ref(|abi| {
            self.compile_resource_rep_for_abi(component, resource, types, abi)
        })
    }

    fn compile_resource_drop(
        &self,
        component: &Component,
        resource: &ResourceDrop,
        types: &ComponentTypes,
    ) -> Result<AllCallFunc<Box<dyn Any + Send>>> {
        self.compile_func_ref(|abi| {
            self.compile_resource_drop_for_abi(component, resource, types, abi)
        })
    }
}

impl Compiler {
    fn translate_transcode(
        &self,
        builder: &mut FunctionBuilder<'_>,
        offsets: &VMComponentOffsets<u8>,
        transcoder: &Transcoder,
        block: ir::Block,
    ) {
        let pointer_type = self.isa.pointer_type();
        let vmctx = builder.func.dfg.block_params(block)[0];

        self.abi_preamble(builder, offsets, vmctx, Abi::Wasm);

        // Determine the static signature of the host libcall for this transcode
        // operation and additionally calculate the static offset within the
        // transode libcalls array.
        let func = &mut builder.func;
        let (sig, offset) = match transcoder.op {
            Transcode::Copy(FixedEncoding::Utf8) => host::utf8_to_utf8(self, func),
            Transcode::Copy(FixedEncoding::Utf16) => host::utf16_to_utf16(self, func),
            Transcode::Copy(FixedEncoding::Latin1) => host::latin1_to_latin1(self, func),
            Transcode::Latin1ToUtf16 => host::latin1_to_utf16(self, func),
            Transcode::Latin1ToUtf8 => host::latin1_to_utf8(self, func),
            Transcode::Utf16ToCompactProbablyUtf16 => {
                host::utf16_to_compact_probably_utf16(self, func)
            }
            Transcode::Utf16ToCompactUtf16 => host::utf16_to_compact_utf16(self, func),
            Transcode::Utf16ToLatin1 => host::utf16_to_latin1(self, func),
            Transcode::Utf16ToUtf8 => host::utf16_to_utf8(self, func),
            Transcode::Utf8ToCompactUtf16 => host::utf8_to_compact_utf16(self, func),
            Transcode::Utf8ToLatin1 => host::utf8_to_latin1(self, func),
            Transcode::Utf8ToUtf16 => host::utf8_to_utf16(self, func),
        };

        let libcall = self.load_libcall(builder, offsets, vmctx, offset);

        // Load the base pointers for the from/to linear memories.
        let from_base = self.load_runtime_memory_base(builder, vmctx, offsets, transcoder.from);
        let to_base = self.load_runtime_memory_base(builder, vmctx, offsets, transcoder.to);

        // Helper function to cast a core wasm input to a host pointer type
        // which will go into the host libcall.
        let cast_to_pointer = |builder: &mut FunctionBuilder<'_>, val: ir::Value, is64: bool| {
            let host64 = pointer_type == ir::types::I64;
            if is64 == host64 {
                val
            } else if !is64 {
                assert!(host64);
                builder.ins().uextend(pointer_type, val)
            } else {
                assert!(!host64);
                builder.ins().ireduce(pointer_type, val)
            }
        };

        // Helper function to cast an input parameter to the host pointer type.
        let len_param = |builder: &mut FunctionBuilder<'_>, param: usize, is64: bool| {
            let val = builder.func.dfg.block_params(block)[2 + param];
            cast_to_pointer(builder, val, is64)
        };

        // Helper function to interpret an input parameter as a pointer into
        // linear memory. This will cast the input parameter to the host integer
        // type and then add that value to the base.
        //
        // Note that bounds-checking happens in adapter modules, and this
        // trampoline is simply calling the host libcall.
        let ptr_param =
            |builder: &mut FunctionBuilder<'_>, param: usize, is64: bool, base: ir::Value| {
                let val = len_param(builder, param, is64);
                builder.ins().iadd(base, val)
            };

        let Transcoder { to64, from64, .. } = *transcoder;
        let mut args = Vec::new();

        let uses_retptr = match transcoder.op {
            Transcode::Utf16ToUtf8
            | Transcode::Latin1ToUtf8
            | Transcode::Utf8ToLatin1
            | Transcode::Utf16ToLatin1 => true,
            _ => false,
        };

        // Most transcoders share roughly the same signature despite doing very
        // different things internally, so most libcalls are lumped together
        // here.
        match transcoder.op {
            Transcode::Copy(_)
            | Transcode::Latin1ToUtf16
            | Transcode::Utf16ToCompactProbablyUtf16
            | Transcode::Utf8ToLatin1
            | Transcode::Utf16ToLatin1
            | Transcode::Utf8ToUtf16 => {
                args.push(ptr_param(builder, 0, from64, from_base));
                args.push(len_param(builder, 1, from64));
                args.push(ptr_param(builder, 2, to64, to_base));
            }

            Transcode::Utf16ToUtf8 | Transcode::Latin1ToUtf8 => {
                args.push(ptr_param(builder, 0, from64, from_base));
                args.push(len_param(builder, 1, from64));
                args.push(ptr_param(builder, 2, to64, to_base));
                args.push(len_param(builder, 3, to64));
            }

            Transcode::Utf8ToCompactUtf16 | Transcode::Utf16ToCompactUtf16 => {
                args.push(ptr_param(builder, 0, from64, from_base));
                args.push(len_param(builder, 1, from64));
                args.push(ptr_param(builder, 2, to64, to_base));
                args.push(len_param(builder, 3, to64));
                args.push(len_param(builder, 4, to64));
            }
        };
        if uses_retptr {
            let slot = builder.func.create_sized_stack_slot(ir::StackSlotData::new(
                ir::StackSlotKind::ExplicitSlot,
                pointer_type.bytes(),
            ));
            args.push(builder.ins().stack_addr(pointer_type, slot, 0));
        }
        let call = builder.ins().call_indirect(sig, libcall, &args);
        let mut results = builder.func.dfg.inst_results(call).to_vec();
        if uses_retptr {
            results.push(builder.ins().load(
                pointer_type,
                ir::MemFlags::trusted(),
                *args.last().unwrap(),
                0,
            ));
        }
        let mut raw_results = Vec::new();

        // Helper to cast a host pointer integer type to the destination type.
        let cast_from_pointer = |builder: &mut FunctionBuilder<'_>, val: ir::Value, is64: bool| {
            let host64 = pointer_type == ir::types::I64;
            if is64 == host64 {
                val
            } else if !is64 {
                assert!(host64);
                builder.ins().ireduce(ir::types::I32, val)
            } else {
                assert!(!host64);
                builder.ins().uextend(ir::types::I64, val)
            }
        };

        // Like the arguments the results are fairly similar across libcalls, so
        // they're lumped into various buckets here.
        match transcoder.op {
            Transcode::Copy(_) | Transcode::Latin1ToUtf16 => {}

            Transcode::Utf8ToUtf16
            | Transcode::Utf16ToCompactProbablyUtf16
            | Transcode::Utf8ToCompactUtf16
            | Transcode::Utf16ToCompactUtf16 => {
                raw_results.push(cast_from_pointer(builder, results[0], to64));
            }

            Transcode::Latin1ToUtf8
            | Transcode::Utf16ToUtf8
            | Transcode::Utf8ToLatin1
            | Transcode::Utf16ToLatin1 => {
                raw_results.push(cast_from_pointer(builder, results[0], from64));
                raw_results.push(cast_from_pointer(builder, results[1], to64));
            }
        };

        builder.ins().return_(&raw_results);
    }

    fn load_runtime_memory_base(
        &self,
        builder: &mut FunctionBuilder<'_>,
        vmctx: ir::Value,
        offsets: &VMComponentOffsets<u8>,
        mem: RuntimeMemoryIndex,
    ) -> ir::Value {
        let pointer_type = self.isa.pointer_type();
        let from_vmmemory_definition = builder.ins().load(
            pointer_type,
            MemFlags::trusted(),
            vmctx,
            i32::try_from(offsets.runtime_memory(mem)).unwrap(),
        );
        builder.ins().load(
            pointer_type,
            MemFlags::trusted(),
            from_vmmemory_definition,
            i32::from(offsets.ptr.vmmemory_definition_base()),
        )
    }
}

/// Module with macro-generated contents that will return the signature and
/// offset for each of the host transcoder functions.
///
/// Note that a macro is used here to keep this in sync with the actual
/// transcoder functions themselves which are also defined via a macro.
mod host {
    use crate::compiler::Compiler;
    use cranelift_codegen::ir::{self, AbiParam};
    use cranelift_codegen::isa::CallConv;

    macro_rules! define {
        (
            $(
                $( #[$attr:meta] )*
                $name:ident( $( $pname:ident: $param:ident ),* ) $( -> $result:ident )?;
            )*
        ) => {
            $(
                pub(super) fn $name(compiler: &Compiler, func: &mut ir::Function) -> (ir::SigRef, u32) {
                    let pointer_type = compiler.isa.pointer_type();
                    let params = vec![
                        $( AbiParam::new(define!(@ty pointer_type $param)) ),*
                    ];
                    let returns = vec![
                        $( AbiParam::new(define!(@ty pointer_type $result)) )?
                    ];
                    let sig = func.import_signature(ir::Signature {
                        params,
                        returns,
                        call_conv: CallConv::triple_default(compiler.isa.triple()),
                    });

                    (sig, offsets::$name)
                }
            )*
        };

        (@ty $ptr:ident size) => ($ptr);
        (@ty $ptr:ident ptr_u8) => ($ptr);
        (@ty $ptr:ident ptr_u16) => ($ptr);
        (@ty $ptr:ident ptr_size) => ($ptr);
        (@ty $ptr:ident u32) => (ir::types::I32);
        (@ty $ptr:ident u64) => (ir::types::I64);
        (@ty $ptr:ident vmctx) => ($ptr);
    }

    wasmtime_environ::foreach_transcoder!(define);
    wasmtime_environ::foreach_builtin_component_function!(define);

    mod offsets {
        macro_rules! offsets {
            (
                $(
                    $( #[$attr:meta] )*
                    $name:ident($($t:tt)*) $( -> $result:ident )?;
                )*
            ) => {
                offsets!(@declare (0) $($name)*);
            };

            (@declare ($n:expr)) => (const LAST_BUILTIN: u32 = $n;);
            (@declare ($n:expr) $name:ident $($rest:tt)*) => (
                pub const $name: u32 = $n;
                offsets!(@declare ($n + 1) $($rest)*);
            );
        }

        wasmtime_environ::foreach_builtin_component_function!(offsets);

        macro_rules! transcode_offsets {
            (
                $(
                    $( #[$attr:meta] )*
                    $name:ident($($t:tt)*) $( -> $result:ident )?;
                )*
            ) => {
                transcode_offsets!(@declare (0) $($name)*);
            };

            (@declare ($n:expr)) => ();
            (@declare ($n:expr) $name:ident $($rest:tt)*) => (
                pub const $name: u32 = LAST_BUILTIN + $n;
                transcode_offsets!(@declare ($n + 1) $($rest)*);
            );
        }

        wasmtime_environ::foreach_transcoder!(transcode_offsets);
    }
}
