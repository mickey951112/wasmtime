//! Support for a calling of an imported function.

use super::create_handle::create_handle;
use crate::{FuncType, Store, Trap};
use anyhow::{bail, Result};
use std::any::Any;
use std::cmp;
use std::collections::HashMap;
use std::mem;
use std::panic::{self, AssertUnwindSafe};
use wasmtime_environ::entity::PrimaryMap;
use wasmtime_environ::isa::TargetIsa;
use wasmtime_environ::{
    ir, settings, CompiledFunction, CompiledFunctionUnwindInfo, Export, Module,
};
use wasmtime_jit::trampoline::ir::{
    ExternalName, Function, InstBuilder, MemFlags, StackSlotData, StackSlotKind,
};
use wasmtime_jit::trampoline::{
    binemit, pretty_error, Context, FunctionBuilder, FunctionBuilderContext,
};
use wasmtime_jit::{native, CodeMemory};
use wasmtime_runtime::{InstanceHandle, VMContext, VMFunctionBody, VMTrampoline};

struct TrampolineState {
    func: Box<dyn Fn(*mut VMContext, *mut u128) -> Result<(), Trap>>,
    #[allow(dead_code)]
    code_memory: CodeMemory,
}

unsafe extern "C" fn stub_fn(
    vmctx: *mut VMContext,
    caller_vmctx: *mut VMContext,
    values_vec: *mut u128,
) {
    // Here we are careful to use `catch_unwind` to ensure Rust panics don't
    // unwind past us. The primary reason for this is that Rust considers it UB
    // to unwind past an `extern "C"` function. Here we are in an `extern "C"`
    // function and the cross into wasm was through an `extern "C"` function at
    // the base of the stack as well. We'll need to wait for assorted RFCs and
    // language features to enable this to be done in a sound and stable fashion
    // before avoiding catching the panic here.
    //
    // Also note that there are intentionally no local variables on this stack
    // frame. The reason for that is that some of the "raise" functions we have
    // below will trigger a longjmp, which won't run local destructors if we
    // have any. To prevent leaks we avoid having any local destructors by
    // avoiding local variables.
    let result = panic::catch_unwind(AssertUnwindSafe(|| {
        call_stub(vmctx, caller_vmctx, values_vec)
    }));

    match result {
        Ok(Ok(())) => {}

        // If a trap was raised (an error returned from the imported function)
        // then we smuggle the trap through `Box<dyn Error>` through to the
        // call-site, which gets unwrapped in `Trap::from_jit` later on as we
        // convert from the internal `Trap` type to our own `Trap` type in this
        // crate.
        Ok(Err(trap)) => wasmtime_runtime::raise_user_trap(Box::new(trap)),

        // And finally if the imported function panicked, then we trigger the
        // form of unwinding that's safe to jump over wasm code on all
        // platforms.
        Err(panic) => wasmtime_runtime::resume_panic(panic),
    }

    unsafe fn call_stub(
        vmctx: *mut VMContext,
        caller_vmctx: *mut VMContext,
        values_vec: *mut u128,
    ) -> Result<(), Trap> {
        let instance = InstanceHandle::from_vmctx(vmctx);
        let state = &instance
            .host_state()
            .downcast_ref::<TrampolineState>()
            .expect("state");
        (state.func)(caller_vmctx, values_vec)
    }
}

/// Create a trampoline for invoking a function.
fn make_trampoline(
    isa: &dyn TargetIsa,
    code_memory: &mut CodeMemory,
    fn_builder_ctx: &mut FunctionBuilderContext,
    signature: &ir::Signature,
) -> *mut [VMFunctionBody] {
    // Mostly reverse copy of the similar method from wasmtime's
    // wasmtime-jit/src/compiler.rs.
    let pointer_type = isa.pointer_type();
    let mut stub_sig = ir::Signature::new(isa.frontend_config().default_call_conv);

    // Add the caller/callee `vmctx` parameters.
    stub_sig.params.push(ir::AbiParam::special(
        pointer_type,
        ir::ArgumentPurpose::VMContext,
    ));

    // Add the caller `vmctx` parameter.
    stub_sig.params.push(ir::AbiParam::new(pointer_type));

    // Add the `values_vec` parameter.
    stub_sig.params.push(ir::AbiParam::new(pointer_type));

    // Compute the size of the values vector. The vmctx and caller vmctx are passed separately.
    let value_size = mem::size_of::<u128>();
    let values_vec_len = ((value_size as usize)
        * cmp::max(signature.params.len() - 2, signature.returns.len()))
        as u32;

    let mut context = Context::new();
    context.func = Function::with_name_signature(ExternalName::user(0, 0), signature.clone());
    context.func.collect_frame_layout_info();

    let ss = context.func.create_stack_slot(StackSlotData::new(
        StackSlotKind::ExplicitSlot,
        values_vec_len,
    ));

    {
        let mut builder = FunctionBuilder::new(&mut context.func, fn_builder_ctx);
        let block0 = builder.create_block();

        builder.append_block_params_for_function_params(block0);
        builder.switch_to_block(block0);
        builder.seal_block(block0);

        let values_vec_ptr_val = builder.ins().stack_addr(pointer_type, ss, 0);
        let mflags = MemFlags::trusted();
        for i in 2..signature.params.len() {
            if i == 0 {
                continue;
            }

            let val = builder.func.dfg.block_params(block0)[i];
            builder.ins().store(
                mflags,
                val,
                values_vec_ptr_val,
                ((i - 2) * value_size) as i32,
            );
        }

        let block_params = builder.func.dfg.block_params(block0);
        let vmctx_ptr_val = block_params[0];
        let caller_vmctx_ptr_val = block_params[1];

        let callee_args = vec![vmctx_ptr_val, caller_vmctx_ptr_val, values_vec_ptr_val];

        let new_sig = builder.import_signature(stub_sig);

        let callee_value = builder
            .ins()
            .iconst(pointer_type, stub_fn as *const VMFunctionBody as i64);
        builder
            .ins()
            .call_indirect(new_sig, callee_value, &callee_args);

        let mflags = MemFlags::trusted();
        let mut results = Vec::new();
        for (i, r) in signature.returns.iter().enumerate() {
            let load = builder.ins().load(
                r.value_type,
                mflags,
                values_vec_ptr_val,
                (i * value_size) as i32,
            );
            results.push(load);
        }
        builder.ins().return_(&results);
        builder.finalize()
    }

    let mut code_buf: Vec<u8> = Vec::new();
    let mut reloc_sink = binemit::TrampolineRelocSink {};
    let mut trap_sink = binemit::NullTrapSink {};
    let mut stackmap_sink = binemit::NullStackmapSink {};
    context
        .compile_and_emit(
            isa,
            &mut code_buf,
            &mut reloc_sink,
            &mut trap_sink,
            &mut stackmap_sink,
        )
        .map_err(|error| pretty_error(&context.func, Some(isa), error))
        .expect("compile_and_emit");

    let unwind_info = CompiledFunctionUnwindInfo::new(isa, &context);

    code_memory
        .allocate_for_function(&CompiledFunction {
            body: code_buf,
            jt_offsets: context.func.jt_offsets,
            unwind_info,
        })
        .expect("allocate_for_function")
}

pub fn create_handle_with_function(
    ft: &FuncType,
    func: Box<dyn Fn(*mut VMContext, *mut u128) -> Result<(), Trap>>,
    store: &Store,
) -> Result<(InstanceHandle, VMTrampoline)> {
    let isa = {
        let isa_builder = native::builder();
        let flag_builder = settings::builder();
        isa_builder.finish(settings::Flags::new(flag_builder))
    };

    let pointer_type = isa.pointer_type();
    let sig = match ft.get_wasmtime_signature(pointer_type) {
        Some(sig) => sig.clone(),
        None => bail!("not a supported core wasm signature {:?}", ft),
    };

    let mut fn_builder_ctx = FunctionBuilderContext::new();
    let mut module = Module::new();
    let mut finished_functions = PrimaryMap::new();
    let mut trampolines = HashMap::new();
    let mut code_memory = CodeMemory::new();

    // First up we manufacture a trampoline which has the ABI specified by `ft`
    // and calls into `stub_fn`...
    let sig_id = module.local.signatures.push(sig.clone());
    let func_id = module.local.functions.push(sig_id);
    module
        .exports
        .insert("trampoline".to_string(), Export::Function(func_id));
    let trampoline = make_trampoline(isa.as_ref(), &mut code_memory, &mut fn_builder_ctx, &sig);
    finished_functions.push(trampoline);

    // ... and then we also need a trampoline with the standard "trampoline ABI"
    // which enters into the ABI specified by `ft`. Note that this is only used
    // if `Func::call` is called on an object created by `Func::new`.
    let (trampoline, relocations) = wasmtime_jit::make_trampoline(
        &*isa,
        &mut code_memory,
        &mut fn_builder_ctx,
        &sig,
        mem::size_of::<u128>(),
    )?;
    assert!(relocations.is_empty());
    let sig_id = store.compiler().signatures().register(&sig);
    trampolines.insert(sig_id, trampoline);

    // Next up we wrap everything up into an `InstanceHandle` by publishing our
    // code memory (makes it executable) and ensuring all our various bits of
    // state make it into the instance constructors.
    code_memory.publish();
    let trampoline_state = TrampolineState { func, code_memory };
    create_handle(
        module,
        store,
        finished_functions,
        trampolines,
        Box::new(trampoline_state),
    )
    .map(|instance| (instance, trampoline))
}

pub unsafe fn create_handle_with_raw_function(
    ft: &FuncType,
    func: *mut [VMFunctionBody],
    trampoline: VMTrampoline,
    store: &Store,
    state: Box<dyn Any>,
) -> Result<InstanceHandle> {
    let isa = {
        let isa_builder = native::builder();
        let flag_builder = settings::builder();
        isa_builder.finish(settings::Flags::new(flag_builder))
    };

    let pointer_type = isa.pointer_type();
    let sig = match ft.get_wasmtime_signature(pointer_type) {
        Some(sig) => sig.clone(),
        None => bail!("not a supported core wasm signature {:?}", ft),
    };

    let mut module = Module::new();
    let mut finished_functions = PrimaryMap::new();
    let mut trampolines = HashMap::new();

    let sig_id = module.local.signatures.push(sig.clone());
    let func_id = module.local.functions.push(sig_id);
    module
        .exports
        .insert("trampoline".to_string(), Export::Function(func_id));
    finished_functions.push(func);
    let sig_id = store.compiler().signatures().register(&sig);
    trampolines.insert(sig_id, trampoline);

    create_handle(module, store, finished_functions, trampolines, state)
}
