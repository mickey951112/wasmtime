//! Example of instantiating of instantiating a wasm module which uses WASI
//! imports.

// You can execute this example with `cargo run --example wasi`

use anyhow::Result;
use wasmtime::*;
use wasmtime_wasi::{Wasi, WasiCtx};

fn main() -> Result<()> {
    let store = Store::default();
    let mut linker = Linker::new(&store);

    // Create an instance of `Wasi` which contains a `WasiCtx`. Note that
    // `WasiCtx` provides a number of ways to configure what the target program
    // will have access to.
    let wasi = Wasi::new(&store, WasiCtx::new(std::env::args())?);
    wasi.add_to_linker(&mut linker)?;

    // Instantiate our module with the imports we've created, and run it.
    let module = Module::from_file(&store, "target/wasm32-wasi/debug/wasi.wasm")?;
    linker.module("", &module)?;
    linker.get_default("")?.get0::<()>()?()?;

    Ok(())
}
