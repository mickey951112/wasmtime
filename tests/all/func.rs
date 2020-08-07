use anyhow::Result;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use wasmtime::*;

#[test]
fn func_constructors() {
    let store = Store::default();
    Func::wrap(&store, || {});
    Func::wrap(&store, |_: i32| {});
    Func::wrap(&store, |_: i32, _: i64| {});
    Func::wrap(&store, |_: f32, _: f64| {});
    Func::wrap(&store, || -> i32 { 0 });
    Func::wrap(&store, || -> i64 { 0 });
    Func::wrap(&store, || -> f32 { 0.0 });
    Func::wrap(&store, || -> f64 { 0.0 });
    Func::wrap(&store, || -> Option<ExternRef> { None });
    Func::wrap(&store, || -> Option<Func> { None });

    Func::wrap(&store, || -> Result<(), Trap> { loop {} });
    Func::wrap(&store, || -> Result<i32, Trap> { loop {} });
    Func::wrap(&store, || -> Result<i64, Trap> { loop {} });
    Func::wrap(&store, || -> Result<f32, Trap> { loop {} });
    Func::wrap(&store, || -> Result<f64, Trap> { loop {} });
    Func::wrap(&store, || -> Result<Option<ExternRef>, Trap> { loop {} });
    Func::wrap(&store, || -> Result<Option<Func>, Trap> { loop {} });
}

#[test]
fn dtor_runs() {
    static HITS: AtomicUsize = AtomicUsize::new(0);

    struct A;

    impl Drop for A {
        fn drop(&mut self) {
            HITS.fetch_add(1, SeqCst);
        }
    }

    let store = Store::default();
    let a = A;
    assert_eq!(HITS.load(SeqCst), 0);
    Func::wrap(&store, move || {
        drop(&a);
    });
    drop(store);
    assert_eq!(HITS.load(SeqCst), 1);
}

#[test]
fn dtor_delayed() -> Result<()> {
    static HITS: AtomicUsize = AtomicUsize::new(0);

    struct A;

    impl Drop for A {
        fn drop(&mut self) {
            HITS.fetch_add(1, SeqCst);
        }
    }

    let store = Store::default();
    let a = A;
    let func = Func::wrap(&store, move || drop(&a));

    assert_eq!(HITS.load(SeqCst), 0);
    let wasm = wat::parse_str(r#"(import "" "" (func))"#)?;
    let module = Module::new(store.engine(), &wasm)?;
    let instance = Instance::new(&store, &module, &[func.into()])?;
    assert_eq!(HITS.load(SeqCst), 0);
    drop((instance, module, store));
    assert_eq!(HITS.load(SeqCst), 1);
    Ok(())
}

#[test]
fn signatures_match() {
    let store = Store::default();

    let f = Func::wrap(&store, || {});
    assert_eq!(f.ty().params(), &[]);
    assert_eq!(f.param_arity(), 0);
    assert_eq!(f.ty().results(), &[]);
    assert_eq!(f.result_arity(), 0);

    let f = Func::wrap(&store, || -> i32 { loop {} });
    assert_eq!(f.ty().params(), &[]);
    assert_eq!(f.ty().results(), &[ValType::I32]);

    let f = Func::wrap(&store, || -> i64 { loop {} });
    assert_eq!(f.ty().params(), &[]);
    assert_eq!(f.ty().results(), &[ValType::I64]);

    let f = Func::wrap(&store, || -> f32 { loop {} });
    assert_eq!(f.ty().params(), &[]);
    assert_eq!(f.ty().results(), &[ValType::F32]);

    let f = Func::wrap(&store, || -> f64 { loop {} });
    assert_eq!(f.ty().params(), &[]);
    assert_eq!(f.ty().results(), &[ValType::F64]);

    let f = Func::wrap(
        &store,
        |_: f32, _: f64, _: i32, _: i64, _: i32, _: Option<ExternRef>, _: Option<Func>| -> f64 {
            loop {}
        },
    );
    assert_eq!(
        f.ty().params(),
        &[
            ValType::F32,
            ValType::F64,
            ValType::I32,
            ValType::I64,
            ValType::I32,
            ValType::ExternRef,
            ValType::FuncRef,
        ]
    );
    assert_eq!(f.ty().results(), &[ValType::F64]);
}

#[test]
// Note: Cranelift only supports refrerence types (used in the wasm in this
// test) on x64.
#[cfg(target_arch = "x86_64")]
fn import_works() -> Result<()> {
    static HITS: AtomicUsize = AtomicUsize::new(0);

    let wasm = wat::parse_str(
        r#"
            (import "" "" (func))
            (import "" "" (func (param i32) (result i32)))
            (import "" "" (func (param i32) (param i64)))
            (import "" "" (func (param i32 i64 i32 f32 f64 externref funcref)))

            (func (export "run") (param externref funcref)
                call 0
                i32.const 0
                call 1
                i32.const 1
                i32.add
                i64.const 3
                call 2

                i32.const 100
                i64.const 200
                i32.const 300
                f32.const 400
                f64.const 500
                local.get 0
                local.get 1
                call 3
            )
        "#,
    )?;
    let mut config = Config::new();
    config.wasm_reference_types(true);
    let engine = Engine::new(&config);
    let store = Store::new(&engine);
    let module = Module::new(&engine, &wasm)?;
    let instance = Instance::new(
        &store,
        &module,
        &[
            Func::wrap(&store, || {
                assert_eq!(HITS.fetch_add(1, SeqCst), 0);
            })
            .into(),
            Func::wrap(&store, |x: i32| -> i32 {
                assert_eq!(x, 0);
                assert_eq!(HITS.fetch_add(1, SeqCst), 1);
                1
            })
            .into(),
            Func::wrap(&store, |x: i32, y: i64| {
                assert_eq!(x, 2);
                assert_eq!(y, 3);
                assert_eq!(HITS.fetch_add(1, SeqCst), 2);
            })
            .into(),
            Func::wrap(
                &store,
                |a: i32, b: i64, c: i32, d: f32, e: f64, f: Option<ExternRef>, g: Option<Func>| {
                    assert_eq!(a, 100);
                    assert_eq!(b, 200);
                    assert_eq!(c, 300);
                    assert_eq!(d, 400.0);
                    assert_eq!(e, 500.0);
                    assert_eq!(
                        f.as_ref().unwrap().data().downcast_ref::<String>().unwrap(),
                        "hello"
                    );
                    assert_eq!(g.as_ref().unwrap().call(&[]).unwrap()[0].unwrap_i32(), 42);
                    assert_eq!(HITS.fetch_add(1, SeqCst), 3);
                },
            )
            .into(),
        ],
    )?;
    let run = instance.get_func("run").unwrap();
    run.call(&[
        Val::ExternRef(Some(ExternRef::new("hello".to_string()))),
        Val::FuncRef(Some(Func::wrap(&store, || -> i32 { 42 }))),
    ])?;
    assert_eq!(HITS.load(SeqCst), 4);
    Ok(())
}

#[test]
fn trap_smoke() -> Result<()> {
    let store = Store::default();
    let f = Func::wrap(&store, || -> Result<(), Trap> { Err(Trap::new("test")) });
    let err = f.call(&[]).unwrap_err().downcast::<Trap>()?;
    assert!(err.to_string().contains("test"));
    assert!(err.i32_exit_status().is_none());
    Ok(())
}

#[test]
fn trap_import() -> Result<()> {
    let wasm = wat::parse_str(
        r#"
            (import "" "" (func))
            (start 0)
        "#,
    )?;
    let store = Store::default();
    let module = Module::new(store.engine(), &wasm)?;
    let trap = Instance::new(
        &store,
        &module,
        &[Func::wrap(&store, || -> Result<(), Trap> { Err(Trap::new("foo")) }).into()],
    )
    .err()
    .unwrap()
    .downcast::<Trap>()?;
    assert!(trap.to_string().contains("foo"));
    Ok(())
}

#[test]
fn get_from_wrapper() {
    let store = Store::default();
    let f = Func::wrap(&store, || {});
    assert!(f.get0::<()>().is_ok());
    assert!(f.get0::<i32>().is_err());
    assert!(f.get1::<(), ()>().is_ok());
    assert!(f.get1::<i32, ()>().is_err());
    assert!(f.get1::<i32, i32>().is_err());
    assert!(f.get2::<(), (), ()>().is_ok());
    assert!(f.get2::<i32, i32, ()>().is_err());
    assert!(f.get2::<i32, i32, i32>().is_err());

    let f = Func::wrap(&store, || -> i32 { loop {} });
    assert!(f.get0::<i32>().is_ok());
    let f = Func::wrap(&store, || -> f32 { loop {} });
    assert!(f.get0::<f32>().is_ok());
    let f = Func::wrap(&store, || -> f64 { loop {} });
    assert!(f.get0::<f64>().is_ok());
    let f = Func::wrap(&store, || -> Option<ExternRef> { loop {} });
    assert!(f.get0::<Option<ExternRef>>().is_ok());
    let f = Func::wrap(&store, || -> Option<Func> { loop {} });
    assert!(f.get0::<Option<Func>>().is_ok());

    let f = Func::wrap(&store, |_: i32| {});
    assert!(f.get1::<i32, ()>().is_ok());
    assert!(f.get1::<i64, ()>().is_err());
    assert!(f.get1::<f32, ()>().is_err());
    assert!(f.get1::<f64, ()>().is_err());
    let f = Func::wrap(&store, |_: i64| {});
    assert!(f.get1::<i64, ()>().is_ok());
    let f = Func::wrap(&store, |_: f32| {});
    assert!(f.get1::<f32, ()>().is_ok());
    let f = Func::wrap(&store, |_: f64| {});
    assert!(f.get1::<f64, ()>().is_ok());
    let f = Func::wrap(&store, |_: Option<ExternRef>| {});
    assert!(f.get1::<Option<ExternRef>, ()>().is_ok());
    let f = Func::wrap(&store, |_: Option<Func>| {});
    assert!(f.get1::<Option<Func>, ()>().is_ok());
}

#[test]
fn get_from_signature() {
    let store = Store::default();
    let ty = FuncType::new(Box::new([]), Box::new([]));
    let f = Func::new(&store, ty, |_, _, _| panic!());
    assert!(f.get0::<()>().is_ok());
    assert!(f.get0::<i32>().is_err());
    assert!(f.get1::<i32, ()>().is_err());

    let ty = FuncType::new(Box::new([ValType::I32]), Box::new([ValType::F64]));
    let f = Func::new(&store, ty, |_, _, _| panic!());
    assert!(f.get0::<()>().is_err());
    assert!(f.get0::<i32>().is_err());
    assert!(f.get1::<i32, ()>().is_err());
    assert!(f.get1::<i32, f64>().is_ok());
}

#[test]
fn get_from_module() -> anyhow::Result<()> {
    let store = Store::default();
    let module = Module::new(
        store.engine(),
        r#"
            (module
                (func (export "f0"))
                (func (export "f1") (param i32))
                (func (export "f2") (result i32)
                    i32.const 0)
            )

        "#,
    )?;
    let instance = Instance::new(&store, &module, &[])?;
    let f0 = instance.get_func("f0").unwrap();
    assert!(f0.get0::<()>().is_ok());
    assert!(f0.get0::<i32>().is_err());
    let f1 = instance.get_func("f1").unwrap();
    assert!(f1.get0::<()>().is_err());
    assert!(f1.get1::<i32, ()>().is_ok());
    assert!(f1.get1::<i32, f32>().is_err());
    let f2 = instance.get_func("f2").unwrap();
    assert!(f2.get0::<()>().is_err());
    assert!(f2.get0::<i32>().is_ok());
    assert!(f2.get1::<i32, ()>().is_err());
    assert!(f2.get1::<i32, f32>().is_err());
    Ok(())
}

#[test]
fn call_wrapped_func() -> Result<()> {
    let store = Store::default();
    let f = Func::wrap(&store, |a: i32, b: i64, c: f32, d: f64| {
        assert_eq!(a, 1);
        assert_eq!(b, 2);
        assert_eq!(c, 3.0);
        assert_eq!(d, 4.0);
    });
    f.call(&[Val::I32(1), Val::I64(2), 3.0f32.into(), 4.0f64.into()])?;
    f.get4::<i32, i64, f32, f64, ()>()?(1, 2, 3.0, 4.0)?;

    let f = Func::wrap(&store, || 1i32);
    let results = f.call(&[])?;
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].unwrap_i32(), 1);
    assert_eq!(f.get0::<i32>()?()?, 1);

    let f = Func::wrap(&store, || 2i64);
    let results = f.call(&[])?;
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].unwrap_i64(), 2);
    assert_eq!(f.get0::<i64>()?()?, 2);

    let f = Func::wrap(&store, || 3.0f32);
    let results = f.call(&[])?;
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].unwrap_f32(), 3.0);
    assert_eq!(f.get0::<f32>()?()?, 3.0);

    let f = Func::wrap(&store, || 4.0f64);
    let results = f.call(&[])?;
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].unwrap_f64(), 4.0);
    assert_eq!(f.get0::<f64>()?()?, 4.0);
    Ok(())
}

#[test]
fn caller_memory() -> anyhow::Result<()> {
    let store = Store::default();
    let f = Func::wrap(&store, |c: Caller<'_>| {
        assert!(c.get_export("x").is_none());
        assert!(c.get_export("y").is_none());
        assert!(c.get_export("z").is_none());
    });
    f.call(&[])?;

    let f = Func::wrap(&store, |c: Caller<'_>| {
        assert!(c.get_export("x").is_none());
    });
    let module = Module::new(
        store.engine(),
        r#"
            (module
                (import "" "" (func $f))
                (start $f)
            )

        "#,
    )?;
    Instance::new(&store, &module, &[f.into()])?;

    let f = Func::wrap(&store, |c: Caller<'_>| {
        assert!(c.get_export("memory").is_some());
    });
    let module = Module::new(
        store.engine(),
        r#"
            (module
                (import "" "" (func $f))
                (memory (export "memory") 1)
                (start $f)
            )

        "#,
    )?;
    Instance::new(&store, &module, &[f.into()])?;

    let f = Func::wrap(&store, |c: Caller<'_>| {
        assert!(c.get_export("m").is_some());
        assert!(c.get_export("f").is_some());
        assert!(c.get_export("g").is_none());
        assert!(c.get_export("t").is_none());
    });
    let module = Module::new(
        store.engine(),
        r#"
            (module
                (import "" "" (func $f))
                (memory (export "m") 1)
                (func (export "f"))
                (global (export "g") i32 (i32.const 0))
                (table (export "t") 1 funcref)
                (start $f)
            )

        "#,
    )?;
    Instance::new(&store, &module, &[f.into()])?;
    Ok(())
}

#[test]
fn func_write_nothing() -> anyhow::Result<()> {
    let store = Store::default();
    let ty = FuncType::new(Box::new([]), Box::new([ValType::I32]));
    let f = Func::new(&store, ty, |_, _, _| Ok(()));
    let err = f.call(&[]).unwrap_err().downcast::<Trap>()?;
    assert!(err
        .to_string()
        .contains("function attempted to return an incompatible value"));
    Ok(())
}

#[test]
// Note: Cranelift only supports refrerence types (used in the wasm in this
// test) on x64.
#[cfg(target_arch = "x86_64")]
fn return_cross_store_value() -> anyhow::Result<()> {
    let wasm = wat::parse_str(
        r#"
            (import "" "" (func (result funcref)))

            (func (export "run") (result funcref)
                call 0
            )
        "#,
    )?;
    let mut config = Config::new();
    config.wasm_reference_types(true);
    let engine = Engine::new(&config);
    let module = Module::new(&engine, &wasm)?;

    let store1 = Store::new(&engine);
    let store2 = Store::new(&engine);

    let store2_func = Func::wrap(&store2, || {});
    let return_cross_store_func = Func::wrap(&store1, move || Some(store2_func.clone()));

    let instance = Instance::new(&store1, &module, &[return_cross_store_func.into()])?;

    let run = instance.get_func("run").unwrap();
    let result = run.call(&[]);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("cross-`Store`"));

    Ok(())
}

#[test]
// Note: Cranelift only supports refrerence types (used in the wasm in this
// test) on x64.
#[cfg(target_arch = "x86_64")]
fn pass_cross_store_arg() -> anyhow::Result<()> {
    let mut config = Config::new();
    config.wasm_reference_types(true);
    let engine = Engine::new(&config);

    let store1 = Store::new(&engine);
    let store2 = Store::new(&engine);

    let store1_func = Func::wrap(&store1, |_: Option<Func>| {});
    let store2_func = Func::wrap(&store2, || {});

    // Using regular `.call` fails with cross-Store arguments.
    assert!(store1_func
        .call(&[Val::FuncRef(Some(store2_func.clone()))])
        .is_err());

    // And using `.get` followed by a function call also fails with cross-Store
    // arguments.
    let f = store1_func.get1::<Option<Func>, ()>()?;
    let result = f(Some(store2_func));
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("cross-`Store`"));

    Ok(())
}

#[test]
fn externref_signature_no_reference_types() -> anyhow::Result<()> {
    let store = Store::default();
    Func::wrap(&store, |_: Option<Func>| {});
    Func::new(
        &store,
        FuncType::new(
            Box::new([ValType::FuncRef, ValType::ExternRef]),
            Box::new([ValType::FuncRef, ValType::ExternRef]),
        ),
        |_, _, _| Ok(()),
    );
    Ok(())
}
