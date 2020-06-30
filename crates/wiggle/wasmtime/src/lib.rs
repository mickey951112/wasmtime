pub use wasmtime_wiggle_macro::*;
pub use wiggle::*;

mod borrow;

use borrow::BorrowChecker;

/// Lightweight `wasmtime::Memory` wrapper so we can implement the
/// `wiggle::GuestMemory` trait on it.
pub struct WasmtimeGuestMemory {
    mem: wasmtime::Memory,
    bc: BorrowChecker,
}

impl WasmtimeGuestMemory {
    pub fn new(mem: wasmtime::Memory) -> Self {
        Self {
            mem,
            // Wiggle does not expose any methods for functions to re-enter
            // the WebAssembly instance, or expose the memory via non-wiggle
            // mechanisms. However, the user-defined code may end up
            // re-entering the instance, in which case this is an incorrect
            // implementation - we require exactly one BorrowChecker exist per
            // instance.
            // This BorrowChecker construction is a holdover until it is
            // integrated fully with wasmtime:
            // https://github.com/bytecodealliance/wasmtime/issues/1917
            bc: BorrowChecker::new(),
        }
    }
}

unsafe impl GuestMemory for WasmtimeGuestMemory {
    fn base(&self) -> (*mut u8, u32) {
        (self.mem.data_ptr(), self.mem.data_size() as _)
    }
    fn has_outstanding_borrows(&self) -> bool {
        self.bc.has_outstanding_borrows()
    }
    fn is_borrowed(&self, r: Region) -> bool {
        self.bc.is_borrowed(r)
    }
    fn borrow(&self, r: Region) -> Result<BorrowHandle, GuestError> {
        self.bc.borrow(r)
    }
    fn unborrow(&self, h: BorrowHandle) {
        self.bc.unborrow(h)
    }
}
