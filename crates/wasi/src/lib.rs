pub mod old;

pub use wasi_common::{WasiCtx, WasiCtxBuilder};

// Defines a `struct Wasi` with member fields and appropriate APIs for dealing
// with all the various WASI exports.
wig::define_wasi_struct_for_wiggle!("phases/snapshot/witx/wasi_snapshot_preview1.witx");

pub fn is_wasi_module(name: &str) -> bool {
    // FIXME: this should be more conservative, but while WASI is in flux and
    // we're figuring out how to support multiple revisions, this should do the
    // trick.
    name.starts_with("wasi")
}
