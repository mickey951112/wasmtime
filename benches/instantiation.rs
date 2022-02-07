use anyhow::Result;
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
use std::sync::Arc;
use std::thread;
use wasmtime::*;
use wasmtime_wasi::{sync::WasiCtxBuilder, WasiCtx};

fn store(engine: &Engine) -> Store<WasiCtx> {
    let wasi = WasiCtxBuilder::new().build();
    Store::new(engine, wasi)
}

fn instantiate(pre: &InstancePre<WasiCtx>, engine: &Engine) -> Result<()> {
    let mut store = store(engine);
    let _instance = pre.instantiate(&mut store)?;

    Ok(())
}

fn benchmark_name<'a>(strategy: &InstanceAllocationStrategy) -> &'static str {
    match strategy {
        InstanceAllocationStrategy::OnDemand => "default",
        #[cfg(any(not(feature = "uffd"), not(target_os = "linux")))]
        InstanceAllocationStrategy::Pooling { .. } => "pooling",
        #[cfg(all(feature = "uffd", target_os = "linux"))]
        InstanceAllocationStrategy::Pooling { .. } => "uffd",
    }
}

fn bench_sequential(c: &mut Criterion, path: &Path) {
    let mut group = c.benchmark_group("sequential");

    for strategy in strategies() {
        let mut config = Config::default();
        config.allocation_strategy(strategy.clone());

        let engine = Engine::new(&config).expect("failed to create engine");
        let module = Module::from_file(&engine, path)
            .unwrap_or_else(|e| panic!("failed to load benchmark `{}`: {:?}", path.display(), e));
        let mut linker = Linker::new(&engine);
        wasmtime_wasi::add_to_linker(&mut linker, |cx| cx).unwrap();
        let pre = linker
            .instantiate_pre(&mut store(&engine), &module)
            .expect("failed to pre-instantiate");

        group.bench_function(
            BenchmarkId::new(
                benchmark_name(&strategy),
                path.file_name().unwrap().to_str().unwrap(),
            ),
            |b| {
                b.iter(|| instantiate(&pre, &engine).expect("failed to instantiate module"));
            },
        );
    }

    group.finish();
}

fn bench_parallel(c: &mut Criterion, path: &Path) {
    let mut group = c.benchmark_group("parallel");

    for strategy in strategies() {
        let mut config = Config::default();
        config.allocation_strategy(strategy.clone());

        let engine = Engine::new(&config).expect("failed to create engine");
        let module = Module::from_file(&engine, path).expect("failed to load WASI example module");
        let mut linker = Linker::new(&engine);
        wasmtime_wasi::add_to_linker(&mut linker, |cx| cx).unwrap();
        let pre = Arc::new(
            linker
                .instantiate_pre(&mut store(&engine), &module)
                .expect("failed to pre-instantiate"),
        );

        for threads in 1..=num_cpus::get_physical() {
            group.bench_function(
                BenchmarkId::new(
                    benchmark_name(&strategy),
                    format!(
                        "{}: with {} background thread{}",
                        path.file_name().unwrap().to_str().unwrap(),
                        threads,
                        if threads == 1 { "" } else { "s" }
                    ),
                ),
                |b| {
                    // Spin up N-1 threads doing background instantiations to
                    // simulate concurrent instantiations.
                    let done = Arc::new(AtomicBool::new(false));
                    let count = Arc::new(AtomicUsize::new(0));
                    let workers = (0..threads - 1)
                        .map(|_| {
                            let pre = pre.clone();
                            let done = done.clone();
                            let engine = engine.clone();
                            let count = count.clone();
                            thread::spawn(move || {
                                count.fetch_add(1, SeqCst);
                                while !done.load(SeqCst) {
                                    instantiate(&pre, &engine).unwrap();
                                }
                            })
                        })
                        .collect::<Vec<_>>();

                    // Wait for our workers to all get started and have
                    // instantiated their first module, at which point they'll
                    // all be spinning.
                    while count.load(SeqCst) != threads - 1 {
                        thread::yield_now();
                    }

                    // Now that our background work is configured we can
                    // benchmark the amount of time it takes to instantiate this
                    // module.
                    b.iter(|| {
                        instantiate(&pre, &engine).expect("failed to instantiate module");
                    });

                    // Shut down this benchmark iteration by signalling to
                    // worker threads they should exit and then wait for them to
                    // have reached the exit point.
                    done.store(true, SeqCst);
                    for t in workers {
                        t.join().unwrap();
                    }
                },
            );
        }
    }

    group.finish();
}

fn build_wasi_example() {
    println!("Building WASI example module...");
    if !Command::new("cargo")
        .args(&[
            "build",
            "--release",
            "-p",
            "example-wasi-wasm",
            "--target",
            "wasm32-wasi",
        ])
        .spawn()
        .expect("failed to run cargo to build WASI example")
        .wait()
        .expect("failed to wait for cargo to build")
        .success()
    {
        panic!("failed to build WASI example for target `wasm32-wasi`");
    }

    std::fs::copy(
        "target/wasm32-wasi/release/wasi.wasm",
        "benches/instantiation/wasi.wasm",
    )
    .expect("failed to copy WASI example module");
}

fn bench_instantiation(c: &mut Criterion) {
    build_wasi_example();
    let modules = &[
        "empty.wat",
        "small_memory.wat",
        "data_segments.wat",
        "wasi.wasm",
    ];
    for module in modules {
        let mut path = PathBuf::new();
        path.push("benches");
        path.push("instantiation");
        path.push(module);
        bench_sequential(c, &path);
        bench_parallel(c, &path);
    }
}

fn strategies() -> impl Iterator<Item = InstanceAllocationStrategy> {
    std::array::IntoIter::new([
        // Skip the on-demand allocator when uffd is enabled
        #[cfg(any(not(feature = "uffd"), not(target_os = "linux")))]
        InstanceAllocationStrategy::OnDemand,
        InstanceAllocationStrategy::Pooling {
            strategy: Default::default(),
            module_limits: ModuleLimits {
                functions: 20_000,
                memory_pages: 1_000,
                ..ModuleLimits::default()
            },
            instance_limits: InstanceLimits::default(),
        },
    ])
}

criterion_group!(benches, bench_instantiation);
criterion_main!(benches);
