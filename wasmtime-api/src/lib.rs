//! Wasmtime embed API. Based on wasm-c-api.

#![cfg_attr(not(feature = "std"), no_std)]

mod callable;
mod context;
mod externals;
mod instance;
mod module;
mod r#ref;
mod runtime;
mod trampoline;
mod trap;
mod types;
mod values;

#[cfg(feature = "wasm-c-api")]
pub mod wasm;

#[macro_use]
extern crate failure_derive;
#[macro_use]
extern crate alloc;

pub use crate::callable::Callable;
pub use crate::externals::*;
pub use crate::instance::Instance;
pub use crate::module::Module;
pub use crate::r#ref::{AnyRef, HostInfo, HostRef};
pub use crate::runtime::{Config, Engine, Store};
pub use crate::trap::Trap;
pub use crate::types::*;
pub use crate::values::*;

#[cfg(not(feature = "std"))]
use hashbrown::{hash_map, HashMap, HashSet};
#[cfg(feature = "std")]
use std::collections::{hash_map, HashMap, HashSet};
