pub mod old;

pub use wasi_common::virtfs;
pub use wasi_common::{WasiCtx, WasiCtxBuilder};

// Defines a `struct Wasi` with member fields and appropriate APIs for dealing
// with all the various WASI exports.
wasmtime_wiggle::wasmtime_integration!({
    // The wiggle code to integrate with lives here:
    target: wasi_common::wasi,
    // This must be the same witx document as used above. This should be ensured by
    // the `WASI_ROOT` env variable, which is set in wasi-common's `build.rs`.
    witx: ["$WASI_ROOT/phases/snapshot/witx/wasi_snapshot_preview1.witx"],
    // This must be the same ctx type as used for the target:
    ctx: WasiCtx,
    // This macro will emit a struct to represent the instance,
    // with this name and docs:
    modules: { wasi_snapshot_preview1 =>
        { name: Wasi,
          docs: "An instantiated instance of the wasi exports.

This represents a wasi module which can be used to instantiate other wasm
modules. This structure exports all that various fields of the wasi instance
as fields which can be used to implement your own instantiation logic, if
necessary. Additionally [`Wasi::get_export`] can be used to do name-based
resolution.",
        }
    },
});

pub fn is_wasi_module(name: &str) -> bool {
    // FIXME: this should be more conservative, but while WASI is in flux and
    // we're figuring out how to support multiple revisions, this should do the
    // trick.
    name.starts_with("wasi")
}
