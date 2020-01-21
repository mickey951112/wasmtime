//! Oracles.
//!
//! Oracles take a test case and determine whether we have a bug. For example,
//! one of the simplest oracles is to take a Wasm binary as our input test case,
//! validate and instantiate it, and (implicitly) check that no assertions
//! failed or segfaults happened. A more complicated oracle might compare the
//! result of executing a Wasm file with and without optimizations enabled, and
//! make sure that the two executions are observably identical.
//!
//! When an oracle finds a bug, it should report it to the fuzzing engine by
//! panicking.

pub mod dummy;

use dummy::{dummy_imports, dummy_values};
use std::collections::{HashMap, HashSet};
use wasmtime::*;

/// Instantiate the Wasm buffer, and implicitly fail if we have an unexpected
/// panic or segfault or anything else that can be detected "passively".
///
/// Performs initial validation, and returns early if the Wasm is invalid.
///
/// You can control which compiler is used via passing a `Strategy`.
pub fn instantiate(wasm: &[u8], strategy: Strategy) {
    if wasmparser::validate(wasm, None).is_err() {
        return;
    }

    let mut config = Config::new();
    config
        .strategy(strategy)
        .expect("failed to enable lightbeam");
    let engine = Engine::new(&config);
    let store = Store::new(&engine);

    let module = Module::new(&store, wasm).expect("Failed to compile a valid Wasm module!");

    let imports = match dummy_imports(&store, module.imports()) {
        Ok(imps) => imps,
        Err(_) => {
            // There are some value types that we can't synthesize a
            // dummy value for (e.g. anyrefs) and for modules that
            // import things of these types we skip instantiation.
            return;
        }
    };

    // Don't unwrap this: there can be instantiation-/link-time errors that
    // aren't caught during validation or compilation. For example, an imported
    // table might not have room for an element segment that we want to
    // initialize into it.
    let _result = Instance::new(&module, &imports);
}

/// Compile the Wasm buffer, and implicitly fail if we have an unexpected
/// panic or segfault or anything else that can be detected "passively".
///
/// Performs initial validation, and returns early if the Wasm is invalid.
///
/// You can control which compiler is used via passing a `Strategy`.
pub fn compile(wasm: &[u8], strategy: Strategy) {
    let mut config = Config::new();
    config.strategy(strategy).unwrap();
    let engine = Engine::new(&config);
    let store = Store::new(&engine);
    let _ = Module::new(&store, wasm);
}

/// Instantiate the given Wasm module with each `Config` and call all of its
/// exports. Modulo OOM, non-canonical NaNs, and usage of Wasm features that are
/// or aren't enabled for different configs, we should get the same results when
/// we call the exported functions for all of our different configs.
pub fn differential_execution(
    ttf: &crate::generators::WasmOptTtf,
    configs: &[crate::generators::DifferentialConfig],
) {
    // We need at least two configs.
    if configs.len() < 2
        // And all the configs should be unique.
        || configs.iter().collect::<HashSet<_>>().len() != configs.len()
    {
        return;
    }

    let configs: Vec<_> = match configs.iter().map(|c| c.to_wasmtime_config()).collect() {
        Ok(cs) => cs,
        // If the config is trying to use something that was turned off at
        // compile time, eg lightbeam, just continue to the next fuzz input.
        Err(_) => return,
    };

    let mut export_func_results: HashMap<String, Result<Box<[Val]>, Trap>> = Default::default();

    for config in &configs {
        let engine = Engine::new(config);
        let store = Store::new(&engine);

        let module = match Module::new(&store, &ttf.wasm) {
            Ok(module) => module,
            // The module might rely on some feature that our config didn't
            // enable or something like that.
            Err(e) => {
                eprintln!("Warning: failed to compile `wasm-opt -ttf` module: {}", e);
                continue;
            }
        };

        // TODO: we should implement tracing versions of these dummy imports
        // that record a trace of the order that imported functions were called
        // in and with what values. Like the results of exported functions,
        // calls to imports should also yield the same values for each
        // configuration, and we should assert that.
        let imports = match dummy_imports(&store, module.imports()) {
            Ok(imps) => imps,
            Err(e) => {
                // There are some value types that we can't synthesize a
                // dummy value for (e.g. anyrefs) and for modules that
                // import things of these types we skip instantiation.
                eprintln!("Warning: failed to synthesize dummy imports: {}", e);
                continue;
            }
        };

        // Don't unwrap this: there can be instantiation-/link-time errors that
        // aren't caught during validation or compilation. For example, an imported
        // table might not have room for an element segment that we want to
        // initialize into it.
        let instance = match Instance::new(&module, &imports) {
            Ok(instance) => instance,
            Err(e) => {
                eprintln!(
                    "Warning: failed to instantiate `wasm-opt -ttf` module: {}",
                    e
                );
                continue;
            }
        };

        let funcs = module
            .exports()
            .iter()
            .filter_map(|e| {
                if let ExternType::Func(_) = e.ty() {
                    Some(e.name())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();

        for name in funcs {
            // Always call the hang limit initializer first, so that we don't
            // infinite loop when calling another export.
            init_hang_limit(&instance);

            let f = match instance
                .get_export(&name)
                .expect("instance should have export from module")
            {
                Extern::Func(f) => f.clone(),
                _ => panic!("export should be a function"),
            };

            let ty = f.ty();
            let params = match dummy_values(ty.params()) {
                Ok(p) => p,
                Err(_) => continue,
            };
            let this_result = f.call(&params);

            let existing_result = export_func_results
                .entry(name.to_string())
                .or_insert_with(|| this_result.clone());
            assert_same_export_func_result(&existing_result, &this_result, name);
        }
    }
}

fn init_hang_limit(instance: &Instance) {
    match instance.get_export("hangLimitInitializer") {
        None => return,
        Some(Extern::Func(f)) => {
            f.call(&[])
                .expect("initializing the hang limit should not fail");
        }
        Some(_) => panic!("unexpected hangLimitInitializer export"),
    }
}

fn assert_same_export_func_result(
    lhs: &Result<Box<[Val]>, Trap>,
    rhs: &Result<Box<[Val]>, Trap>,
    func_name: &str,
) {
    let fail = || {
        panic!(
            "differential fuzzing failed: exported func {} returned two \
             different results: {:?} != {:?}",
            func_name, lhs, rhs
        )
    };

    match (lhs, rhs) {
        (Err(_), Err(_)) => {}
        (Ok(lhs), Ok(rhs)) => {
            if lhs.len() != rhs.len() {
                fail();
            }
            for (lhs, rhs) in lhs.iter().zip(rhs.iter()) {
                match (lhs, rhs) {
                    (Val::I32(lhs), Val::I32(rhs)) if lhs == rhs => continue,
                    (Val::I64(lhs), Val::I64(rhs)) if lhs == rhs => continue,
                    (Val::V128(lhs), Val::V128(rhs)) if lhs == rhs => continue,
                    (Val::F32(lhs), Val::F32(rhs)) => {
                        let lhs = f32::from_bits(*lhs);
                        let rhs = f32::from_bits(*rhs);
                        if lhs == rhs || (lhs.is_nan() && rhs.is_nan()) {
                            continue;
                        } else {
                            fail()
                        }
                    }
                    (Val::F64(lhs), Val::F64(rhs)) => {
                        let lhs = f64::from_bits(*lhs);
                        let rhs = f64::from_bits(*rhs);
                        if lhs == rhs || (lhs.is_nan() && rhs.is_nan()) {
                            continue;
                        } else {
                            fail()
                        }
                    }
                    (Val::AnyRef(_), Val::AnyRef(_)) | (Val::FuncRef(_), Val::FuncRef(_)) => {
                        continue
                    }
                    _ => fail(),
                }
            }
        }
        _ => fail(),
    }
}

/// Invoke the given API calls.
pub fn make_api_calls(api: crate::generators::api::ApiCalls) {
    use crate::generators::api::ApiCall;

    let mut config: Option<Config> = None;
    let mut engine: Option<Engine> = None;
    let mut store: Option<Store> = None;
    let mut modules: HashMap<usize, Module> = Default::default();
    let mut instances: HashMap<usize, Instance> = Default::default();

    for call in api.calls {
        match call {
            ApiCall::ConfigNew => {
                assert!(config.is_none());
                config = Some(Config::new());
            }

            ApiCall::ConfigDebugInfo(b) => {
                config.as_mut().unwrap().debug_info(b);
            }

            ApiCall::EngineNew => {
                assert!(engine.is_none());
                engine = Some(Engine::new(config.as_ref().unwrap()));
            }

            ApiCall::StoreNew => {
                assert!(store.is_none());
                store = Some(Store::new(engine.as_ref().unwrap()));
            }

            ApiCall::ModuleNew { id, wasm } => {
                let module = match Module::new(store.as_ref().unwrap(), &wasm.wasm) {
                    Ok(m) => m,
                    Err(_) => continue,
                };
                let old = modules.insert(id, module);
                assert!(old.is_none());
            }

            ApiCall::ModuleDrop { id } => {
                drop(modules.remove(&id));
            }

            ApiCall::InstanceNew { id, module } => {
                let module = match modules.get(&module) {
                    Some(m) => m,
                    None => continue,
                };

                let imports = match dummy_imports(store.as_ref().unwrap(), module.imports()) {
                    Ok(imps) => imps,
                    Err(_) => {
                        // There are some value types that we can't synthesize a
                        // dummy value for (e.g. anyrefs) and for modules that
                        // import things of these types we skip instantiation.
                        continue;
                    }
                };

                // Don't unwrap this: there can be instantiation-/link-time errors that
                // aren't caught during validation or compilation. For example, an imported
                // table might not have room for an element segment that we want to
                // initialize into it.
                if let Ok(instance) = Instance::new(&module, &imports) {
                    instances.insert(id, instance);
                }
            }

            ApiCall::InstanceDrop { id } => {
                drop(instances.remove(&id));
            }

            ApiCall::CallExportedFunc { instance, nth } => {
                let instance = match instances.get(&instance) {
                    Some(i) => i,
                    None => {
                        // Note that we aren't guaranteed to instantiate valid
                        // modules, see comments in `InstanceNew` for details on
                        // that. But the API call generator can't know if
                        // instantiation failed, so we might not actually have
                        // this instance. When that's the case, just skip the
                        // API call and keep going.
                        continue;
                    }
                };

                let funcs = instance
                    .exports()
                    .iter()
                    .filter_map(|e| match e {
                        Extern::Func(f) => Some(f.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>();

                if funcs.is_empty() {
                    continue;
                }

                let nth = nth % funcs.len();
                let f = &funcs[nth];
                let ty = f.ty();
                let params = match dummy_values(ty.params()) {
                    Ok(p) => p,
                    Err(_) => continue,
                };
                let _ = f.call(&params);
            }
        }
    }
}
