//! Translation of the memory example

use anyhow::{bail, ensure, Context as _, Error};
use core::cell::Ref;
use std::fs::read;
use wasmtime_api::*;

fn get_export_memory(exports: &[Extern], i: usize) -> Result<HostRef<Memory>, Error> {
    if exports.len() <= i {
        bail!("> Error accessing memory export {}!", i);
    }
    Ok(exports[i]
        .memory()
        .with_context(|| format!("> Error accessing memory export {}!", i))?
        .clone())
}

fn get_export_func(exports: &[Extern], i: usize) -> Result<HostRef<Func>, Error> {
    if exports.len() <= i {
        bail!("> Error accessing function export {}!", i);
    }
    Ok(exports[i]
        .func()
        .with_context(|| format!("> Error accessing function export {}!", i))?
        .clone())
}

macro_rules! check {
    ($actual:expr, $expected:expr) => {
        if $actual != $expected {
            bail!("> Error on result, expected {}, got {}", $expected, $actual);
        }
    };
}

macro_rules! check_ok {
  ($func:expr, $($p:expr),*) => {
    if let Err(_) = $func.borrow().call(&[$($p.into()),*]) {
      bail!("> Error on result, expected return");
    }
  }
}

macro_rules! check_trap {
  ($func:expr, $($p:expr),*) => {
    if let Ok(_) = $func.borrow().call(&[$($p.into()),*]) {
      bail!("> Error on result, expected trap");
    }
  }
}

macro_rules! call {
  ($func:expr, $($p:expr),*) => {
    match $func.borrow().call(&[$($p.into()),*]) {
      Ok(result) => {
        let result: i32 = result[0].clone().into();
        result
      }
      Err(_) => { bail!("> Error on result, expected return"); }
    }
  }
}

fn main() -> Result<(), Error> {
    // Initialize.
    println!("Initializing...");
    let engine = HostRef::new(Engine::default());
    let store = HostRef::new(Store::new(&engine));

    // Load binary.
    println!("Loading binary...");
    let binary = read("examples/memory.wasm")?;

    // Compile.
    println!("Compiling module...");
    let module = HostRef::new(Module::new(&store, &binary).context("> Error compiling module!")?);

    // Instantiate.
    println!("Instantiating module...");
    let instance =
        HostRef::new(Instance::new(&store, &module, &[]).context("> Error instantiating module!")?);

    // Extract export.
    println!("Extracting export...");
    let exports = Ref::map(instance.borrow(), |instance| instance.exports());
    ensure!(!exports.is_empty(), "> Error accessing exports!");
    let memory = get_export_memory(&exports, 0)?;
    let size_func = get_export_func(&exports, 1)?;
    let load_func = get_export_func(&exports, 2)?;
    let store_func = get_export_func(&exports, 3)?;

    // Try cloning.
    check!(memory.clone().ptr_eq(&memory), true);

    // Check initial memory.
    println!("Checking memory...");
    check!(memory.borrow().size(), 2u32);
    check!(memory.borrow().data_size(), 0x20000usize);
    check!(unsafe { memory.borrow().data()[0] }, 0);
    check!(unsafe { memory.borrow().data()[0x1000] }, 1);
    check!(unsafe { memory.borrow().data()[0x1003] }, 4);

    check!(call!(size_func,), 2);
    check!(call!(load_func, 0), 0);
    check!(call!(load_func, 0x1000), 1);
    check!(call!(load_func, 0x1003), 4);
    check!(call!(load_func, 0x1ffff), 0);
    check_trap!(load_func, 0x20000);

    // Mutate memory.
    println!("Mutating memory...");
    unsafe {
        memory.borrow_mut().data()[0x1003] = 5;
    }

    check_ok!(store_func, 0x1002, 6);
    check_trap!(store_func, 0x20000, 0);

    check!(unsafe { memory.borrow().data()[0x1002] }, 6);
    check!(unsafe { memory.borrow().data()[0x1003] }, 5);
    check!(call!(load_func, 0x1002), 6);
    check!(call!(load_func, 0x1003), 5);

    // Grow memory.
    println!("Growing memory...");
    check!(memory.borrow_mut().grow(1), true);
    check!(memory.borrow().size(), 3u32);
    check!(memory.borrow().data_size(), 0x30000usize);

    check!(call!(load_func, 0x20000), 0);
    check_ok!(store_func, 0x20000, 0);
    check_trap!(load_func, 0x30000);
    check_trap!(store_func, 0x30000, 0);

    check!(memory.borrow_mut().grow(1), false);
    check!(memory.borrow_mut().grow(0), true);

    // Create stand-alone memory.
    // TODO(wasm+): Once Wasm allows multiple memories, turn this into import.
    println!("Creating stand-alone memory...");
    let memorytype = MemoryType::new(Limits::new(5, 5));
    let mut memory2 = Memory::new(&store, memorytype);
    check!(memory2.size(), 5u32);
    check!(memory2.grow(1), false);
    check!(memory2.grow(0), true);

    // Shut down.
    println!("Shutting down...");
    drop(store);

    println!("Done.");
    Ok(())
}
