//! WebAssembly trap handling, which is built on top of the lower-level
//! signalhandling mechanisms.

use crate::trap_registry::get_trap_registry;
use crate::trap_registry::TrapDescription;
use crate::vmcontext::{VMContext, VMFunctionBody};
use alloc::string::{String, ToString};
use core::cell::Cell;
use core::ptr;
use cranelift_codegen::ir;

extern "C" {
    fn WasmtimeCallTrampoline(
        vmctx: *mut u8,
        callee: *const VMFunctionBody,
        values_vec: *mut u8,
    ) -> i32;
    fn WasmtimeCall(vmctx: *mut u8, callee: *const VMFunctionBody) -> i32;
}

thread_local! {
    static RECORDED_TRAP: Cell<Option<TrapDescription>> = Cell::new(None);
    static JMP_BUF: Cell<*const u8> = Cell::new(ptr::null());
}

/// Check if there is a trap at given PC
#[doc(hidden)]
#[allow(non_snake_case)]
#[no_mangle]
pub extern "C" fn CheckIfTrapAtAddress(_pc: *const u8) -> i8 {
    // TODO: stack overflow can happen at any random time (i.e. in malloc() in memory.grow)
    // and it's really hard to determine if the cause was stack overflow and if it happened
    // in WebAssembly module.
    // So, let's assume that any untrusted code called from WebAssembly doesn't trap.
    // Then, if we have called some WebAssembly code, it means the trap is stack overflow.
    JMP_BUF.with(|ptr| !ptr.get().is_null()) as i8
}

/// Record the Trap code and wasm bytecode offset in TLS somewhere
#[doc(hidden)]
#[allow(non_snake_case)]
#[no_mangle]
pub extern "C" fn RecordTrap(pc: *const u8) {
    // TODO: please see explanation in CheckIfTrapAtAddress.
    let registry = get_trap_registry();
    let trap_desc = registry
        .get_trap(pc as usize)
        .unwrap_or_else(|| TrapDescription {
            source_loc: ir::SourceLoc::default(),
            trap_code: ir::TrapCode::StackOverflow,
        });
    RECORDED_TRAP.with(|data| {
        assert_eq!(
            data.get(),
            None,
            "Only one trap per thread can be recorded at a moment!"
        );
        data.set(Some(trap_desc))
    });
}

#[doc(hidden)]
#[allow(non_snake_case)]
#[no_mangle]
pub extern "C" fn EnterScope(ptr: *const u8) -> *const u8 {
    JMP_BUF.with(|buf| buf.replace(ptr))
}

#[doc(hidden)]
#[allow(non_snake_case)]
#[no_mangle]
pub extern "C" fn GetScope() -> *const u8 {
    JMP_BUF.with(|buf| buf.get())
}

#[doc(hidden)]
#[allow(non_snake_case)]
#[no_mangle]
pub extern "C" fn LeaveScope(ptr: *const u8) {
    JMP_BUF.with(|buf| buf.set(ptr))
}

fn trap_message() -> String {
    let trap_desc = RECORDED_TRAP
        .with(|data| data.replace(None))
        .expect("trap_message must be called after trap occurred");

    format!(
        "wasm trap: {}, source location: {}",
        trap_code_to_expected_string(trap_desc.trap_code),
        trap_desc.source_loc,
    )
}

fn trap_code_to_expected_string(trap_code: ir::TrapCode) -> String {
    use ir::TrapCode::*;
    match trap_code {
        StackOverflow => "call stack exhausted".to_string(),
        HeapOutOfBounds => "out of bounds memory access".to_string(),
        TableOutOfBounds => "undefined element".to_string(),
        OutOfBounds => "out of bounds".to_string(), // Note: not covered by the test suite
        IndirectCallToNull => "uninitialized element".to_string(),
        BadSignature => "indirect call type mismatch".to_string(),
        IntegerOverflow => "integer overflow".to_string(),
        IntegerDivisionByZero => "integer divide by zero".to_string(),
        BadConversionToInteger => "invalid conversion to integer".to_string(),
        UnreachableCodeReached => "unreachable".to_string(),
        Interrupt => "interrupt".to_string(), // Note: not covered by the test suite
        User(x) => format!("user trap {}", x), // Note: not covered by the test suite
    }
}

/// Call the wasm function pointed to by `callee`. `values_vec` points to
/// a buffer which holds the incoming arguments, and to which the outgoing
/// return values will be written.
#[no_mangle]
pub unsafe extern "C" fn wasmtime_call_trampoline(
    vmctx: *mut VMContext,
    callee: *const VMFunctionBody,
    values_vec: *mut u8,
) -> Result<(), String> {
    if WasmtimeCallTrampoline(vmctx as *mut u8, callee, values_vec) == 0 {
        Err(trap_message())
    } else {
        Ok(())
    }
}

/// Call the wasm function pointed to by `callee`, which has no arguments or
/// return values.
#[no_mangle]
pub unsafe extern "C" fn wasmtime_call(
    vmctx: *mut VMContext,
    callee: *const VMFunctionBody,
) -> Result<(), String> {
    if WasmtimeCall(vmctx as *mut u8, callee) == 0 {
        Err(trap_message())
    } else {
        Ok(())
    }
}
