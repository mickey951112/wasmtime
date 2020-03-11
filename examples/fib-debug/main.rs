//! Example of enabling debuginfo for wasm code which allows interactive
//! debugging of the wasm code. When using recent versions of LLDB
//! you can debug this executable and set breakpoints in wasm code and look at
//! the rust source code as input.

// To execute this example you'll need to run two commands:
//
//      cargo build -p example-fib-wasm --target wasm32-unknown-unknown
//      cargo run --example fib-debug

use anyhow::Result;
use wasmtime::*;

fn main() -> Result<()> {
    // Load our previously compiled wasm file (built previously with Cargo) and
    // also ensure that we generate debuginfo so this executable can be
    // debugged in GDB.
    let engine = Engine::new(Config::new().debug_info(true));
    let store = Store::new(&engine);
    let module = Module::from_file(&store, "target/wasm32-unknown-unknown/debug/fib.wasm")?;
    let instance = Instance::new(&module, &[])?;

    // Invoke `fib` export
    let fib = instance
        .get_export("fib")
        .and_then(|e| e.func())
        .ok_or(anyhow::format_err!("failed to find `fib` function export"))?
        .get1::<i32, i32>()?;
    println!("fib(6) = {}", fib(6)?);
    Ok(())
}
