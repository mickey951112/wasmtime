use std::path::Path;
use wasmtime::{Config, Engine, Store, Strategy};
use wasmtime_wast::WastContext;

include!(concat!(env!("OUT_DIR"), "/wast_testsuite_tests.rs"));

// Each of the tests included from `wast_testsuite_tests` will call this
// function which actually executes the `wast` test suite given the `strategy`
// to compile it.
fn run_wast(wast: &str, strategy: Strategy) -> anyhow::Result<()> {
    let wast = Path::new(wast);

    let simd = wast.iter().any(|s| s == "simd");

    // Some simd tests assume support for multiple tables, which are introduced
    // by reference types.
    let reftypes = simd || wast.iter().any(|s| s == "reference-types");

    let multi_val = wast.iter().any(|s| s == "multi-value");

    let mut cfg = Config::new();
    cfg.wasm_simd(simd)
        .wasm_reference_types(reftypes)
        .wasm_multi_value(multi_val)
        .strategy(strategy)?
        .cranelift_debug_verifier(true);
    let store = Store::new(&Engine::new(&cfg));
    let mut wast_context = WastContext::new(store);
    wast_context.register_spectest()?;
    wast_context.run_file(wast)?;
    Ok(())
}
