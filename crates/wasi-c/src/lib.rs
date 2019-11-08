#[macro_use]
extern crate log;
extern crate alloc;
extern crate core;

mod host;
mod instantiate;
mod syscalls;
mod translate;
mod wasm32;

pub use instantiate::instantiate_wasi_c;
