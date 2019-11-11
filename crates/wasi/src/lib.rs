extern crate alloc;

mod instantiate;
mod syscalls;

pub use instantiate::{instantiate_wasi, instantiate_wasi_with_context};

pub fn is_wasi_module(name: &str) -> bool {
    // FIXME: this should be more conservative, but while WASI is in flux and
    // we're figuring out how to support multiple revisions, this should do the
    // trick.
    name.starts_with("wasi")
}
