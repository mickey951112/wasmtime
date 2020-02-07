//! Wasmtime's embedding API
//!
//! This crate contains a high-level API used to interact with WebAssembly
//! modules. The API here is intended to mirror the proposed [WebAssembly C
//! API](https://github.com/WebAssembly/wasm-c-api), with small extensions here
//! and there to implement Rust idioms. This crate also defines the actual C API
//! itself for consumption from other languages.

#![deny(missing_docs)]

mod callable;
mod externals;
mod frame_info;
mod func;
mod instance;
mod module;
mod r#ref;
mod runtime;
mod trampoline;
mod trap;
mod types;
mod values;

pub use crate::callable::Callable;
pub use crate::externals::*;
pub use crate::frame_info::FrameInfo;
pub use crate::func::{Func, WasmArg, WasmRet};
pub use crate::instance::Instance;
pub use crate::module::Module;
pub use crate::r#ref::{AnyRef, HostInfo, HostRef};
pub use crate::runtime::{Config, Engine, OptLevel, Store, Strategy};
pub use crate::trap::Trap;
pub use crate::types::*;
pub use crate::values::*;

cfg_if::cfg_if! {
    if #[cfg(unix)] {
        pub mod unix;
    } else if #[cfg(windows)] {
        pub mod windows;
    } else {
        // ... unknown os!
    }
}
