#![no_main]

use libfuzzer_sys::arbitrary::{Result, Unstructured};
use libfuzzer_sys::fuzz_target;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering::SeqCst;
use std::sync::Once;
use wasmtime_fuzzing::generators::{Config, DiffValue, SingleInstModule};
use wasmtime_fuzzing::oracles::diff_spec;
use wasmtime_fuzzing::oracles::diff_wasmtime::WasmtimeInstance;
use wasmtime_fuzzing::oracles::{differential, engine, log_wasm};

// Upper limit on the number of invocations for each WebAssembly function
// executed by this fuzz target.
const NUM_INVOCATIONS: usize = 5;

// Keep track of how many WebAssembly modules we actually executed (i.e. ran to
// completion) versus how many were tried.
static TOTAL_INVOCATIONS: AtomicUsize = AtomicUsize::new(0);
static TOTAL_SUCCESSES: AtomicUsize = AtomicUsize::new(0);
static TOTAL_ATTEMPTED: AtomicUsize = AtomicUsize::new(0);

// The spec interpreter requires special one-time setup.
static SETUP: Once = Once::new();

fuzz_target!(|data: &[u8]| {
    // To avoid a uncaught `SIGSEGV` due to signal handlers; see comments on
    // `setup_ocaml_runtime`.
    SETUP.call_once(|| diff_spec::setup_ocaml_runtime());

    // Errors in `run` have to do with not enough input in `data`, which we
    // ignore here since it doesn't affect how we'd like to fuzz.
    drop(run(&data));
});

fn run(data: &[u8]) -> Result<()> {
    let successes = TOTAL_SUCCESSES.load(SeqCst);
    let attempts = TOTAL_ATTEMPTED.fetch_add(1, SeqCst);
    if attempts > 1 && attempts % 1_000 == 0 {
        println!("=== Execution rate ({} successes / {} attempted modules): {}% (total invocations: {}) ===",
            successes,
            attempts,
            successes as f64 / attempts as f64 * 100f64,
            TOTAL_INVOCATIONS.load(SeqCst)
        );
    }

    let mut u = Unstructured::new(data);
    let mut config: Config = u.arbitrary()?;
    config.set_differential_config();

    // Generate the Wasm module.
    let wasm = if u.arbitrary()? {
        // TODO figure out if this always eats up the rest of the unstructured;
        // can we limit the number of instructions/functions.
        let module = config.generate(&mut u, Some(1000))?;
        module.to_bytes()
    } else {
        let module = SingleInstModule::new(&mut u, &mut config.module_config)?;
        module.to_bytes()
    };
    log_wasm(&wasm);

    // Choose a left-hand side Wasm engine.
    let lhs = engine::choose(&mut u, &config)?;
    let lhs_instance = lhs.instantiate(&wasm);

    // Choose a right-hand side Wasm engine--this will always be Wasmtime.
    let rhs_store = config.to_store();
    let rhs_module = wasmtime::Module::new(rhs_store.engine(), &wasm).unwrap();
    let rhs_instance = WasmtimeInstance::new(rhs_store, rhs_module);

    // If we fail to instantiate, check that both sides do.
    let (mut lhs_instance, mut rhs_instance) = match (lhs_instance, rhs_instance) {
        (Ok(l), Ok(r)) => (l, r),
        (Err(_), Err(_)) => return Ok(()), // TODO match the error messages.
        (l, r) => panic!(
            "failed to instantiate only one side: {:?} != {:?}",
            l.err(),
            r.err()
        ),
    };

    // Call each exported function with different sets of arguments.
    for (name, signature) in rhs_instance.exported_functions() {
        let mut invocations = 0;
        loop {
            let arguments = signature
                .params()
                .map(|t| DiffValue::arbitrary_of_type(&mut u, t.try_into().unwrap()))
                .collect::<Result<Vec<_>>>()?;
            differential(lhs_instance.as_mut(), &mut rhs_instance, &name, &arguments)
                .expect("failed to run differential evaluation");

            // We evaluate the same function with different arguments until we
            // hit a predetermined limit or we run out of unstructured data--it
            // does not make sense to re-evaluate the same arguments over and
            // over.
            invocations += 1;
            TOTAL_INVOCATIONS.fetch_add(1, SeqCst);
            if invocations > NUM_INVOCATIONS || u.is_empty() {
                break;
            }
        }
    }

    TOTAL_SUCCESSES.fetch_add(1, SeqCst);
    Ok(())
}
