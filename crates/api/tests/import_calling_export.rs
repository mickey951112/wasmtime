use std::cell::RefCell;
use std::rc::Rc;
use wasmtime::*;

#[test]
fn test_import_calling_export() {
    const WAT: &str = r#"
    (module
      (type $t0 (func))
      (import "" "imp" (func $.imp (type $t0)))
      (func $run call $.imp)
      (func $other)
      (export "run" (func $run))
      (export "other" (func $other))
    )
    "#;

    let store = Store::default();
    let module = Module::new(&store, WAT).expect("failed to create module");

    let other = Rc::new(RefCell::new(None::<Func>));
    let other2 = other.clone();

    let callback_func = Func::new(
        &store,
        FuncType::new(Box::new([]), Box::new([])),
        move |_, _, _| {
            other2
                .borrow()
                .as_ref()
                .expect("expected a function ref")
                .call(&[])
                .expect("expected function not to trap");
            Ok(())
        },
    );

    let imports = vec![callback_func.into()];
    let instance =
        Instance::new(&module, imports.as_slice()).expect("failed to instantiate module");

    let exports = instance.exports();
    assert!(!exports.is_empty());

    let run_func = exports[0]
        .func()
        .expect("expected a run func in the module");

    *other.borrow_mut() = Some(
        exports[1]
            .func()
            .expect("expected an other func in the module")
            .clone(),
    );

    run_func.call(&[]).expect("expected function not to trap");
}

#[test]
fn test_returns_incorrect_type() {
    const WAT: &str = r#"
    (module
        (import "env" "evil" (func $evil (result i32)))
        (func (export "run") (result i32)
            (call $evil)
        )
    )
    "#;

    let store = Store::default();
    let module = Module::new(&store, WAT).expect("failed to create module");

    let callback_func = Func::new(
        &store,
        FuncType::new(Box::new([]), Box::new([ValType::I32])),
        |_, _, results| {
            // Evil! Returns I64 here instead of promised in the signature I32.
            results[0] = Val::I64(228);
            Ok(())
        },
    );

    let imports = vec![callback_func.into()];
    let instance =
        Instance::new(&module, imports.as_slice()).expect("failed to instantiate module");

    let exports = instance.exports();
    assert!(!exports.is_empty());

    let run_func = exports[0]
        .func()
        .expect("expected a run func in the module");

    let trap = run_func.call(&[]).expect_err("the execution should fail");
    assert_eq!(
        trap.message(),
        "function attempted to return an incompatible value"
    );
}
