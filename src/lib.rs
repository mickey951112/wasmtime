//! The Wasmtime command line interface (CLI) crate.
//!
//! This crate implements the Wasmtime command line tools.

#![deny(
    missing_docs,
    trivial_numeric_casts,
    unused_extern_crates,
    unstable_features
)]
#![warn(unused_import_braces)]
#![cfg_attr(feature = "clippy", plugin(clippy(conf_file = "../clippy.toml")))]
#![cfg_attr(feature = "cargo-clippy", allow(clippy::new_without_default))]
#![cfg_attr(
    feature = "cargo-clippy",
    warn(
        clippy::float_arithmetic,
        clippy::mut_mut,
        clippy::nonminimal_bool,
        clippy::option_map_unwrap_or,
        clippy::option_map_unwrap_or_else,
        clippy::unicode_not_nfc,
        clippy::use_self
    )
)]

pub mod commands;
mod obj;

use anyhow::{bail, Result};
use std::path::PathBuf;
use structopt::StructOpt;
use wasmtime::{Config, ProfilingStrategy, Strategy};

pub use obj::compile_to_obj;

fn pick_compilation_strategy(cranelift: bool, lightbeam: bool) -> Result<Strategy> {
    Ok(match (lightbeam, cranelift) {
        (true, false) => Strategy::Lightbeam,
        (false, true) => Strategy::Cranelift,
        (false, false) => Strategy::Auto,
        (true, true) => bail!("Can't enable --cranelift and --lightbeam at the same time"),
    })
}

fn pick_profiling_strategy(jitdump: bool) -> Result<ProfilingStrategy> {
    Ok(match jitdump {
        true => ProfilingStrategy::JitDump,
        false => ProfilingStrategy::None,
    })
}

fn init_file_per_thread_logger(prefix: &'static str) {
    file_per_thread_logger::initialize(prefix);

    // Extending behavior of default spawner:
    // https://docs.rs/rayon/1.1.0/rayon/struct.ThreadPoolBuilder.html#method.spawn_handler
    // Source code says DefaultSpawner is implementation detail and
    // shouldn't be used directly.
    rayon::ThreadPoolBuilder::new()
        .spawn_handler(move |thread| {
            let mut b = std::thread::Builder::new();
            if let Some(name) = thread.name() {
                b = b.name(name.to_owned());
            }
            if let Some(stack_size) = thread.stack_size() {
                b = b.stack_size(stack_size);
            }
            b.spawn(move || {
                file_per_thread_logger::initialize(prefix);
                thread.run()
            })?;
            Ok(())
        })
        .build_global()
        .unwrap();
}

/// Common options for commands that translate WebAssembly modules
#[derive(StructOpt)]
struct CommonOptions {
    /// Use specified configuration file
    #[structopt(long, parse(from_os_str), value_name = "CONFIG_PATH")]
    config: Option<PathBuf>,

    /// Use Cranelift for all compilation
    #[structopt(long, conflicts_with = "lightbeam")]
    cranelift: bool,

    /// Log to per-thread log files instead of stderr.
    #[structopt(long)]
    log_to_files: bool,

    /// Generate debug information
    #[structopt(short = "g")]
    debug_info: bool,

    /// Disable cache system
    #[structopt(long)]
    disable_cache: bool,

    /// Enable support for proposed SIMD instructions
    #[structopt(long)]
    enable_simd: bool,

    /// Enable support for reference types
    #[structopt(long)]
    enable_reference_types: bool,

    /// Enable support for multi-value functions
    #[structopt(long)]
    enable_multi_value: bool,

    /// Enable support for Wasm threads
    #[structopt(long)]
    enable_threads: bool,

    /// Enable support for bulk memory instructions
    #[structopt(long)]
    enable_bulk_memory: bool,

    /// Enable all experimental Wasm features
    #[structopt(long)]
    enable_all: bool,

    /// Use Lightbeam for all compilation
    #[structopt(long, conflicts_with = "cranelift")]
    lightbeam: bool,

    /// Generate jitdump file (supported on --features=profiling build)
    #[structopt(long)]
    jitdump: bool,

    /// Run optimization passes on translated functions, on by default
    #[structopt(short = "O", long)]
    optimize: bool,

    /// Optimization level for generated functions (0 (none), 1, 2 (most), or s
    /// (size))
    #[structopt(
        long,
        parse(try_from_str = parse_opt_level),
        default_value = "2",
    )]
    opt_level: wasmtime::OptLevel,
}

impl CommonOptions {
    fn config(&self) -> Result<Config> {
        let mut config = Config::new();
        config
            .cranelift_debug_verifier(cfg!(debug_assertions))
            .debug_info(self.debug_info)
            .wasm_bulk_memory(self.enable_bulk_memory || self.enable_all)
            .wasm_simd(self.enable_simd || self.enable_all)
            .wasm_reference_types(self.enable_reference_types || self.enable_all)
            .wasm_multi_value(self.enable_multi_value || self.enable_all)
            .wasm_threads(self.enable_threads || self.enable_all)
            .cranelift_opt_level(self.opt_level())
            .strategy(pick_compilation_strategy(self.cranelift, self.lightbeam)?)?
            .profiler(pick_profiling_strategy(self.jitdump)?)?;
        if !self.disable_cache {
            match &self.config {
                Some(path) => {
                    config.cache_config_load(path)?;
                }
                None => {
                    config.cache_config_load_default()?;
                }
            }
        }
        Ok(config)
    }

    fn opt_level(&self) -> wasmtime::OptLevel {
        match (self.optimize, self.opt_level.clone()) {
            (true, _) => wasmtime::OptLevel::Speed,
            (false, other) => other,
        }
    }
}

fn parse_opt_level(opt_level: &str) -> Result<wasmtime::OptLevel> {
    match opt_level {
        "s" => Ok(wasmtime::OptLevel::SpeedAndSize),
        "0" => Ok(wasmtime::OptLevel::None),
        "1" => Ok(wasmtime::OptLevel::Speed),
        "2" => Ok(wasmtime::OptLevel::Speed),
        other => bail!(
            "unknown optimization level `{}`, only 0,1,2,s accepted",
            other
        ),
    }
}
