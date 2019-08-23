//! Translation from wasm to native object files.
//!
//! Reads a Wasm binary file, translates the functions' code to Cranelift
//! IL, then translates it to native code, and writes it out to a native
//! object file with relocations.

#![deny(
    missing_docs,
    trivial_numeric_casts,
    unused_extern_crates,
    unstable_features
)]
#![warn(unused_import_braces)]
#![cfg_attr(feature = "clippy", plugin(clippy(conf_file = "../clippy.toml")))]
#![cfg_attr(
    feature = "cargo-clippy",
    allow(clippy::new_without_default, clippy::new_without_default_derive)
)]
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

use cranelift_codegen::isa;
use cranelift_codegen::settings;
use cranelift_codegen::settings::Configurable;
use cranelift_entity::EntityRef;
use cranelift_native;
use cranelift_wasm::DefinedMemoryIndex;
use docopt::Docopt;
use faerie::Artifact;
use serde::Deserialize;
use std::error::Error;
use std::fmt::format;
use std::fs::File;
use std::io;
use std::io::prelude::*;
use std::path::Path;
use std::path::PathBuf;
use std::process;
use std::str;
use std::str::FromStr;
use target_lexicon::Triple;
use wasmtime_debug::{emit_debugsections, read_debuginfo};
use wasmtime_environ::cache_conf;
use wasmtime_environ::{
    Compiler, Cranelift, ModuleEnvironment, ModuleVmctxInfo, Tunables, VMOffsets,
};
use wasmtime_obj::emit_module;

const USAGE: &str = "
Wasm to native object translation utility.
Takes a binary WebAssembly module into a native object file.
The translation is dependent on the environment chosen.
The default is a dummy environment that produces placeholder values.

Usage:
    wasm2obj [--target TARGET] [-dg] [--cache] [--cache-dir=<cache_dir>] [--cache-compression-level=<compr_level>] [--enable-simd] <file> -o <output>
    wasm2obj --help | --version

Options:
    -v, --verbose       displays the module and translated functions
    -h, --help          print this help message
    --target <TARGET>   build for the target triple; default is the host machine
    -g                  generate debug information
    -c, --cache         enable caching system, use default cache directory
    --cache-dir=<cache_dir>
                        enable caching system, use specified cache directory
    --cache-compression-level=<compr_level>
                        enable caching system, use custom compression level for new cache, values 1-21
    --enable-simd       enable proposed SIMD instructions
    --version           print the Cranelift version
    -d, --debug         enable debug output on stderr/stdout
";

#[derive(Deserialize, Debug, Clone)]
struct Args {
    arg_file: String,
    arg_output: String,
    arg_target: Option<String>,
    flag_g: bool,
    flag_debug: bool,
    flag_cache: bool,
    flag_cache_dir: Option<String>,
    flag_cache_compression_level: Option<i32>,
    flag_enable_simd: bool,
}

fn read_wasm_file(path: PathBuf) -> Result<Vec<u8>, io::Error> {
    let mut buf: Vec<u8> = Vec::new();
    let mut file = File::open(path)?;
    file.read_to_end(&mut buf)?;
    Ok(buf)
}

fn main() {
    let version = env!("CARGO_PKG_VERSION");
    let args: Args = Docopt::new(USAGE)
        .and_then(|d| {
            d.help(true)
                .version(Some(String::from(version)))
                .deserialize()
        })
        .unwrap_or_else(|e| e.exit());

    if args.flag_debug {
        pretty_env_logger::init();
    } else {
        wasmtime::init_file_per_thread_logger("wasm2obj.dbg.");
    }

    cache_conf::init(
        args.flag_cache
            || args.flag_cache_dir.is_some()
            || args.flag_cache_compression_level.is_some(),
        args.flag_cache_dir.as_ref(),
        args.flag_cache_compression_level,
    );

    let path = Path::new(&args.arg_file);
    match handle_module(
        path.to_path_buf(),
        &args.arg_target,
        &args.arg_output,
        args.flag_g,
        args.flag_enable_simd,
    ) {
        Ok(()) => {}
        Err(message) => {
            println!(" error: {}", message);
            process::exit(1);
        }
    }
}

fn handle_module(
    path: PathBuf,
    target: &Option<String>,
    output: &str,
    generate_debug_info: bool,
    enable_simd: bool,
) -> Result<(), String> {
    let data = match read_wasm_file(path) {
        Ok(data) => data,
        Err(err) => {
            return Err(String::from(err.description()));
        }
    };

    let isa_builder = match *target {
        Some(ref target) => {
            let target = Triple::from_str(&target).map_err(|_| "could not parse --target")?;
            isa::lookup(target).map_err(|err| match err {
                isa::LookupError::SupportDisabled => {
                    "support for architecture disabled at compile time"
                }
                isa::LookupError::Unsupported => "unsupported architecture",
            })?
        }
        None => cranelift_native::builder().unwrap_or_else(|_| {
            panic!("host machine is not a supported target");
        }),
    };
    let mut flag_builder = settings::builder();
    if enable_simd {
        flag_builder.enable("enable_simd").unwrap();
    }
    let isa = isa_builder.finish(settings::Flags::new(flag_builder));

    let mut obj = Artifact::new(isa.triple().clone(), String::from(output));

    // TODO: Expose the tunables as command-line flags.
    let tunables = Tunables::default();

    let (module, lazy_function_body_inputs, lazy_data_initializers, target_config) = {
        let environ = ModuleEnvironment::new(isa.frontend_config(), tunables);

        let translation = environ
            .translate(&data)
            .map_err(|error| error.to_string())?;

        (
            translation.module,
            translation.function_body_inputs,
            translation.data_initializers,
            translation.target_config,
        )
    };

    let (compilation, relocations, address_transform, value_ranges, stack_slots) =
        Cranelift::compile_module(
            &module,
            lazy_function_body_inputs,
            &*isa,
            generate_debug_info,
        )
        .map_err(|e| e.to_string())?;

    let module_vmctx_info = {
        let ofs = VMOffsets::new(target_config.pointer_bytes(), &module);
        let memory_offset = ofs.vmctx_vmmemory_definition_base(DefinedMemoryIndex::new(0)) as i64;
        ModuleVmctxInfo {
            memory_offset,
            stack_slots,
        }
    };

    emit_module(
        &mut obj,
        &module,
        &compilation,
        &relocations,
        &lazy_data_initializers,
        &target_config,
    )?;

    if generate_debug_info {
        let debug_data = read_debuginfo(&data);
        emit_debugsections(
            &mut obj,
            &module_vmctx_info,
            &target_config,
            &debug_data,
            &address_transform,
            &value_ranges,
        )
        .map_err(|e| e.to_string())?;
    }

    // FIXME: Make the format a parameter.
    let file =
        ::std::fs::File::create(Path::new(output)).map_err(|x| format(format_args!("{}", x)))?;
    obj.write(file).map_err(|e| e.to_string())?;

    Ok(())
}
