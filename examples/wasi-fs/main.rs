//! Example of running a wasi binary in a memory filesystem

// The corresponding wasm binary can be built with:
// `cargo build -p example-wasi-fs-wasm --target wasm32-wasi`
//
// then you can execute this example with `cargo run --example wasi-fs`

use anyhow::Result;
use std::collections::HashMap;
use wasmtime::*;
use wasmtime_wasi::virtfs::{VecFileContents, VirtualDirEntry};
use wasmtime_wasi::{Wasi, WasiCtxBuilder};

fn main() -> Result<()> {
    tracing_subscriber::FmtSubscriber::builder()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_ansi(true)
        .init();

    let store = Store::default();
    let mut linker = Linker::new(&store);

    // Create an instance of `Wasi` which contains a `WasiCtx`. Note that
    // `WasiCtx` provides a number of ways to configure what the target program
    // will have access to.
    let entry = VirtualDirEntry::File(Box::new(VecFileContents::with_content(
        "world".as_bytes().to_owned(),
    )));
    let mut map = HashMap::new();
    map.insert("test.txt".to_string(), entry);
    let dir = VirtualDirEntry::Directory(map);
    let ctx = WasiCtxBuilder::new()
        .inherit_stdout()
        .inherit_stderr()
        .preopened_virt(dir, ".")
        .build()?;
    let wasi = Wasi::new(&store, ctx);
    wasi.add_to_linker(&mut linker)?;

    // Instantiate our module with the imports we've created, and run it.
    let module = Module::from_file(store.engine(), "target/wasm32-wasi/debug/wasi-fs.wasm")?;
    linker.module("", &module)?;
    linker.get_default("")?.get0::<()>()?()?;

    Ok(())
}
