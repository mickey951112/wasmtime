use lazy_static::lazy_static;
use std::collections::HashMap;
use std::fmt;
use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};
use wasmtime_environ::ir;

lazy_static! {
    static ref REGISTRY: RwLock<TrapRegistry> = RwLock::new(TrapRegistry::default());
}

/// The registry maintains descriptions of traps in currently allocated functions.
#[derive(Default)]
pub struct TrapRegistry {
    traps: HashMap<usize, TrapDescription>,
}

/// Description of a trap.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct TrapDescription {
    /// Location of the trap in source binary module.
    pub source_loc: ir::SourceLoc,
    /// Code of the trap.
    pub trap_code: ir::TrapCode,
}

impl fmt::Display for TrapDescription {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "wasm trap: {}, source location: {}",
            trap_code_to_expected_string(self.trap_code),
            self.source_loc
        )
    }
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

/// RAII guard for deregistering traps
pub struct TrapRegistrationGuard(usize);

impl TrapRegistry {
    /// Registers a new trap.
    /// Returns a RAII guard that deregisters the trap when dropped.
    pub fn register_trap(
        &mut self,
        address: usize,
        source_loc: ir::SourceLoc,
        trap_code: ir::TrapCode,
    ) -> TrapRegistrationGuard {
        let entry = TrapDescription {
            source_loc,
            trap_code,
        };
        let previous_trap = self.traps.insert(address, entry);
        assert!(previous_trap.is_none());
        TrapRegistrationGuard(address)
    }

    fn deregister_trap(&mut self, address: usize) {
        assert!(self.traps.remove(&address).is_some());
    }

    /// Gets a trap description at given address.
    pub fn get_trap(&self, address: usize) -> Option<TrapDescription> {
        self.traps.get(&address).copied()
    }
}

impl Drop for TrapRegistrationGuard {
    fn drop(&mut self) {
        let mut registry = get_mut_trap_registry();
        registry.deregister_trap(self.0);
    }
}

/// Gets guarded writable reference to traps registry
pub fn get_mut_trap_registry() -> RwLockWriteGuard<'static, TrapRegistry> {
    REGISTRY.write().expect("trap registry lock got poisoned")
}

/// Gets guarded readable reference to traps registry
pub fn get_trap_registry() -> RwLockReadGuard<'static, TrapRegistry> {
    REGISTRY.read().expect("trap registry lock got poisoned")
}
