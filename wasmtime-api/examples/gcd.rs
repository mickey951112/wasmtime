//! Example of instantiating of the WebAssembly module and
//! invoking its exported function.

use failure::{format_err, Error};
use std::cell::RefCell;
use std::fs::read;
use std::rc::Rc;
use wasmtime_api::*;

fn main() -> Result<(), Error> {
    let wasm = read("examples/gcd.wasm")?;

    // Instantiate engine and store.
    let engine = Rc::new(RefCell::new(Engine::default()));
    let store = Rc::new(RefCell::new(Store::new(engine)));

    // Load a module.
    let module = Rc::new(RefCell::new(Module::new(store.clone(), &wasm)?));

    // Find index of the `gcd` export.
    let gcd_index = module
        .borrow()
        .exports()
        .iter()
        .enumerate()
        .find(|(_, export)| export.name().to_string() == "gcd")
        .unwrap()
        .0;

    // Instantiate the module.
    let instance = Rc::new(RefCell::new(Instance::new(store.clone(), module, &[])?));

    // Invoke `gcd` export
    let gcd = instance.borrow().exports()[gcd_index]
        .borrow()
        .func()
        .clone();
    let result = gcd
        .borrow()
        .call(&[Val::from(6i32), Val::from(27i32)])
        .map_err(|e| format_err!("call error: {:?}", e))?;

    println!("{:?}", result);
    Ok(())
}
