use cranelift_native;

use cranelift_codegen::isa;
use cranelift_codegen::settings;
use cranelift_codegen::settings::Configurable;
use std::path::Path;
use wasmtime_jit::{CompilationStrategy, Compiler, Features};
use wasmtime_wast::WastContext;

include!(concat!(env!("OUT_DIR"), "/wast_testsuite_tests.rs"));

// Each of the tests included from `wast_testsuite_tests` will call this
// function which actually executes the `wast` test suite given the `strategy`
// to compile it.
fn run_wast(wast: &str, strategy: CompilationStrategy) -> anyhow::Result<()> {
    let wast = Path::new(wast);
    let isa = native_isa();
    let compiler = Compiler::new(isa, strategy);
    let features = Features {
        simd: wast.iter().any(|s| s == "simd"),
        multi_value: wast.iter().any(|s| s == "multi-value"),
        ..Default::default()
    };
    let mut wast_context = WastContext::new(Box::new(compiler)).with_features(features);
    wast_context.register_spectest()?;
    wast_context.run_file(wast)?;
    Ok(())
}

fn native_isa() -> Box<dyn isa::TargetIsa> {
    let mut flag_builder = settings::builder();
    flag_builder.enable("enable_verifier").unwrap();
    flag_builder.enable("avoid_div_traps").unwrap();
    flag_builder.enable("enable_simd").unwrap();

    let isa_builder = cranelift_native::builder().unwrap_or_else(|_| {
        panic!("host machine is not a supported target");
    });

    isa_builder.finish(settings::Flags::new(flag_builder))
}
