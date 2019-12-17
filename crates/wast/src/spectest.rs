#![allow(improper_ctypes)]

use anyhow::Result;
use std::collections::HashMap;
use std::rc::Rc;
use wasmtime::*;

struct MyCall<F>(F);

impl<F> Callable for MyCall<F>
where
    F: Fn(&[Val], &mut [Val]) -> Result<(), HostRef<Trap>>,
{
    fn call(&self, params: &[Val], results: &mut [Val]) -> Result<(), HostRef<Trap>> {
        (self.0)(params, results)
    }
}

fn wrap(
    store: &HostRef<Store>,
    ty: FuncType,
    callable: impl Fn(&[Val], &mut [Val]) -> Result<(), HostRef<Trap>> + 'static,
) -> Func {
    Func::new(store, ty, Rc::new(MyCall(callable)))
}

/// Return an instance implementing the "spectest" interface used in the
/// spec testsuite.
pub fn instantiate_spectest(store: &HostRef<Store>) -> HashMap<&'static str, Extern> {
    let mut ret = HashMap::new();

    let ty = FuncType::new(Box::new([]), Box::new([]));
    let func = wrap(store, ty, |_params, _results| Ok(()));
    ret.insert("print", Extern::Func(HostRef::new(func)));

    let ty = FuncType::new(Box::new([ValType::I32]), Box::new([]));
    let func = wrap(store, ty, |params, _results| {
        println!("{}: i32", params[0].unwrap_i32());
        Ok(())
    });
    ret.insert("print_i32", Extern::Func(HostRef::new(func)));

    let ty = FuncType::new(Box::new([ValType::I64]), Box::new([]));
    let func = wrap(store, ty, |params, _results| {
        println!("{}: i64", params[0].unwrap_i64());
        Ok(())
    });
    ret.insert("print_i64", Extern::Func(HostRef::new(func)));

    let ty = FuncType::new(Box::new([ValType::F32]), Box::new([]));
    let func = wrap(store, ty, |params, _results| {
        println!("{}: f32", params[0].unwrap_f32());
        Ok(())
    });
    ret.insert("print_f32", Extern::Func(HostRef::new(func)));

    let ty = FuncType::new(Box::new([ValType::F64]), Box::new([]));
    let func = wrap(store, ty, |params, _results| {
        println!("{}: f64", params[0].unwrap_f64());
        Ok(())
    });
    ret.insert("print_f64", Extern::Func(HostRef::new(func)));

    let ty = FuncType::new(Box::new([ValType::I32, ValType::F32]), Box::new([]));
    let func = wrap(store, ty, |params, _results| {
        println!("{}: i32", params[0].unwrap_i32());
        println!("{}: f32", params[1].unwrap_f32());
        Ok(())
    });
    ret.insert("print_i32_f32", Extern::Func(HostRef::new(func)));

    let ty = FuncType::new(Box::new([ValType::F64, ValType::F64]), Box::new([]));
    let func = wrap(store, ty, |params, _results| {
        println!("{}: f64", params[0].unwrap_f64());
        println!("{}: f64", params[1].unwrap_f64());
        Ok(())
    });
    ret.insert("print_f64_f64", Extern::Func(HostRef::new(func)));

    let ty = GlobalType::new(ValType::I32, Mutability::Const);
    let g = Global::new(store, ty, Val::I32(666));
    ret.insert("global_i32", Extern::Global(HostRef::new(g)));

    let ty = GlobalType::new(ValType::I64, Mutability::Const);
    let g = Global::new(store, ty, Val::I64(666));
    ret.insert("global_i64", Extern::Global(HostRef::new(g)));

    let ty = GlobalType::new(ValType::F32, Mutability::Const);
    let g = Global::new(store, ty, Val::F32(0x44268000));
    ret.insert("global_f32", Extern::Global(HostRef::new(g)));

    let ty = GlobalType::new(ValType::F64, Mutability::Const);
    let g = Global::new(store, ty, Val::F64(0x4084d00000000000));
    ret.insert("global_f64", Extern::Global(HostRef::new(g)));

    let ty = TableType::new(ValType::FuncRef, Limits::new(10, Some(20)));
    let table = Table::new(store, ty, Val::AnyRef(AnyRef::Null));
    ret.insert("table", Extern::Table(HostRef::new(table)));

    let ty = MemoryType::new(Limits::new(1, Some(2)));
    let memory = Memory::new(store, ty);
    ret.insert("memory", Extern::Memory(HostRef::new(memory)));

    return ret;
}
