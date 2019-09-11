//! CLI tool to use the functions provided by the [wasmtime](../wasmtime/index.html)
//! crate.
//!
//! Reads Wasm binary files (one Wasm module per file), translates the functions' code to Cranelift
//! IL. Can also executes the `start` function of the module by laying out the memories, globals
//! and tables, then emitting the translated code with hardcoded addresses to memory.

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

use cranelift_codegen::settings;
use cranelift_codegen::settings::Configurable;
use docopt::Docopt;
use failure::{bail, Error, ResultExt};
use pretty_env_logger;
use serde::Deserialize;
use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs::File;
use std::path::Component;
use std::path::{Path, PathBuf};
use std::process::exit;
use std::rc::Rc;
use wabt;
use wasi_common::preopen_dir;
use wasmtime_api::{Config, Engine, Instance, Module, Store};
use wasmtime_environ::cache_config;
use wasmtime_interface_types::ModuleData;
use wasmtime_jit::Features;
use wasmtime_wasi::instantiate_wasi;
use wasmtime_wast::instantiate_spectest;

#[cfg(feature = "wasi-c")]
use wasmtime_wasi_c::instantiate_wasi_c;

const USAGE: &str = "
Wasm runner.

Takes a binary (wasm) or text (wat) WebAssembly module and instantiates it,
including calling the start function if one is present. Additional functions
given with --invoke are then called.

Usage:
    wasmtime [-odg] [--enable-simd] [--wasi-c] [--cache | --cache-config=<cache_config_file>] [--create-cache-config] [--preload=<wasm>...] [--env=<env>...] [--dir=<dir>...] [--mapdir=<mapping>...] <file> [<arg>...]
    wasmtime [-odg] [--enable-simd] [--wasi-c] [--cache | --cache-config=<cache_config_file>] [--create-cache-config] [--env=<env>...] [--dir=<dir>...] [--mapdir=<mapping>...] --invoke=<fn> <file> [<arg>...]
    wasmtime --help | --version

Options:
    --invoke=<fn>       name of function to run
    -o, --optimize      runs optimization passes on the translated functions
    -c, --cache         enable caching system, use default configuration
    --cache-config=<cache_config_file>
                        enable caching system, use specified cache configuration
    --create-cache-config
                        used with --cache or --cache-config, creates default configuration and writes it to the disk,
                        will fail if specified file already exists (or default file if used with --cache)
    -g                  generate debug information
    -d, --debug         enable debug output on stderr/stdout
    --enable-simd       enable proposed SIMD instructions
    --wasi-c            enable the wasi-c implementation of WASI
    --preload=<wasm>    load an additional wasm module before loading the main module
    --env=<env>         pass an environment variable (\"key=value\") to the program
    --dir=<dir>         grant access to the given host directory
    --mapdir=<mapping>  where <mapping> has the form <wasmdir>::<hostdir>, grant access to
                        the given host directory with the given wasm directory name
    -h, --help          print this help message
    --version           print the Cranelift version
";

#[derive(Deserialize, Debug, Clone)]
struct Args {
    arg_file: String,
    arg_arg: Vec<String>,
    flag_optimize: bool,
    flag_cache: bool, // TODO change to disable cache after implementing cache eviction
    flag_cache_config_file: Option<String>,
    flag_create_cache_config: bool,
    flag_debug: bool,
    flag_g: bool,
    flag_enable_simd: bool,
    flag_invoke: Option<String>,
    flag_preload: Vec<String>,
    flag_env: Vec<String>,
    flag_dir: Vec<String>,
    flag_mapdir: Vec<String>,
    flag_wasi_c: bool,
}

fn read_wasm(path: PathBuf) -> Result<Vec<u8>, Error> {
    let data = std::fs::read(&path)
        .with_context(|_| format!("failed to read file: {}", path.display()))?;

    // If data is a wasm binary, use that. If it's using wat format, convert it
    // to a wasm binary with wat2wasm.
    Ok(if data.starts_with(&[b'\0', b'a', b's', b'm']) {
        data
    } else {
        wabt::wat2wasm(data)?
    })
}

fn compute_preopen_dirs(flag_dir: &[String], flag_mapdir: &[String]) -> Vec<(String, File)> {
    let mut preopen_dirs = Vec::new();

    for dir in flag_dir {
        let preopen_dir = preopen_dir(dir).unwrap_or_else(|err| {
            println!("error while pre-opening directory {}: {}", dir, err);
            exit(1);
        });
        preopen_dirs.push((dir.clone(), preopen_dir));
    }

    for mapdir in flag_mapdir {
        let parts: Vec<&str> = mapdir.split("::").collect();
        if parts.len() != 2 {
            println!("--mapdir argument must contain exactly one double colon ('::'), separating a guest directory name and a host directory name");
            exit(1);
        }
        let (key, value) = (parts[0], parts[1]);
        let preopen_dir = preopen_dir(value).unwrap_or_else(|err| {
            println!("error while pre-opening directory {}: {}", value, err);
            exit(1);
        });
        preopen_dirs.push((key.to_string(), preopen_dir));
    }

    preopen_dirs
}

/// Compute the argv array values.
fn compute_argv(argv0: &str, arg_arg: &[String]) -> Vec<String> {
    let mut result = Vec::new();

    // Add argv[0], which is the program name. Only include the base name of the
    // main wasm module, to avoid leaking path information.
    result.push(
        Path::new(argv0)
            .components()
            .next_back()
            .map(Component::as_os_str)
            .and_then(OsStr::to_str)
            .unwrap_or("")
            .to_owned(),
    );

    // Add the remaining arguments.
    for arg in arg_arg {
        result.push(arg.to_owned());
    }

    result
}

/// Compute the environ array values.
fn compute_environ(flag_env: &[String]) -> Vec<(String, String)> {
    let mut result = Vec::new();

    // Add the environment variables, which must be of the form "key=value".
    for env in flag_env {
        let split = env.splitn(2, '=').collect::<Vec<_>>();
        if split.len() != 2 {
            println!(
                "environment variables must be of the form \"key=value\"; got \"{}\"",
                env
            );
        }
        result.push((split[0].to_owned(), split[1].to_owned()));
    }

    result
}

fn main() {
    let err = match rmain() {
        Ok(()) => return,
        Err(e) => e,
    };
    eprintln!("error: {}", err);
    for cause in err.iter_causes() {
        eprintln!("    caused by: {}", cause);
    }
    exit(1);
}

fn rmain() -> Result<(), Error> {
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
        wasmtime::init_file_per_thread_logger("wasmtime.dbg.");
    }

    let errors = cache_config::init(
        args.flag_cache || args.flag_cache_config_file.is_some(),
        args.flag_cache_config_file.as_ref(),
        args.flag_create_cache_config,
    );

    if !errors.is_empty() {
        eprintln!("Cache initialization failed. Errors:");
        for e in errors {
            eprintln!("-> {}", e);
        }
        exit(1);
    }

    let mut flag_builder = settings::builder();
    let mut features: Features = Default::default();

    // Enable/disable producing of debug info.
    let debug_info = args.flag_g;

    // Enable verifier passes in debug mode.
    if cfg!(debug_assertions) {
        flag_builder.enable("enable_verifier")?;
    }

    // Enable SIMD if requested
    if args.flag_enable_simd {
        flag_builder.enable("enable_simd")?;
        features.simd = true;
    }

    // Enable optimization if requested.
    if args.flag_optimize {
        flag_builder.set("opt_level", "best")?;
    }

    let config = Config::new(settings::Flags::new(flag_builder), features, debug_info);
    let engine = Rc::new(RefCell::new(Engine::new(config)));
    let store = Rc::new(RefCell::new(Store::new(engine)));

    let mut module_registry = HashMap::new();

    // Make spectest available by default.
    module_registry.insert(
        "spectest".to_owned(),
        Instance::from_handle(store.clone(), instantiate_spectest()?)?,
    );

    // Make wasi available by default.
    let global_exports = store.borrow().global_exports().clone();
    let preopen_dirs = compute_preopen_dirs(&args.flag_dir, &args.flag_mapdir);
    let argv = compute_argv(&args.arg_file, &args.arg_arg);
    let environ = compute_environ(&args.flag_env);

    let wasi = if args.flag_wasi_c {
        #[cfg(feature = "wasi-c")]
        {
            instantiate_wasi_c("", global_exports.clone(), &preopen_dirs, &argv, &environ)?
        }
        #[cfg(not(feature = "wasi-c"))]
        {
            bail!("wasi-c feature not enabled at build time")
        }
    } else {
        instantiate_wasi("", global_exports.clone(), &preopen_dirs, &argv, &environ)?
    };

    module_registry.insert(
        "wasi_unstable".to_owned(),
        Instance::from_handle(store.clone(), wasi)?,
    );

    // Load the preload wasm modules.
    for filename in &args.flag_preload {
        let path = Path::new(&filename);
        instantiate_module(store.clone(), &module_registry, path)
            .with_context(|_| format!("failed to process preload at `{}`", path.display()))?;
    }

    // Load the main wasm module.
    let path = Path::new(&args.arg_file);
    handle_module(store, &module_registry, &args, path)
        .with_context(|_| format!("failed to process main module `{}`", path.display()))?;
    Ok(())
}

fn instantiate_module(
    store: Rc<RefCell<Store>>,
    module_registry: &HashMap<String, (Instance, HashMap<String, usize>)>,
    path: &Path,
) -> Result<(Rc<RefCell<Instance>>, Rc<RefCell<Module>>, Vec<u8>), Error> {
    // Read the wasm module binary.
    let data = read_wasm(path.to_path_buf())?;

    let module = Rc::new(RefCell::new(Module::new(store.clone(), &data)?));

    // Resolve import using module_registry.
    let imports = module
        .borrow()
        .imports()
        .iter()
        .map(|i| {
            let module_name = i.module().to_string();
            if let Some((instance, map)) = module_registry.get(&module_name) {
                let field_name = i.name().to_string();
                if let Some(export_index) = map.get(&field_name) {
                    Ok(instance.exports()[*export_index].clone())
                } else {
                    bail!(
                        "Import {} was not found in module {}",
                        field_name,
                        module_name
                    )
                }
            } else {
                bail!("Import module {} was not found", module_name)
            }
        })
        .collect::<Result<Vec<_>, _>>()?;

    let instance = Rc::new(RefCell::new(Instance::new(
        store.clone(),
        module.clone(),
        &imports,
    )?));

    Ok((instance, module, data))
}

fn handle_module(
    store: Rc<RefCell<Store>>,
    module_registry: &HashMap<String, (Instance, HashMap<String, usize>)>,
    args: &Args,
    path: &Path,
) -> Result<(), Error> {
    let (instance, _module, data) = instantiate_module(store.clone(), module_registry, path)?;

    // If a function to invoke was given, invoke it.
    if let Some(f) = &args.flag_invoke {
        let data = ModuleData::new(&data)?;
        invoke_export(store, instance, &data, f, args)?;
    }

    Ok(())
}

fn invoke_export(
    store: Rc<RefCell<Store>>,
    instance: Rc<RefCell<Instance>>,
    data: &ModuleData,
    name: &str,
    args: &Args,
) -> Result<(), Error> {
    use wasm_webidl_bindings::ast;
    use wasmtime_interface_types::Value;

    let mut handle = instance.borrow().handle().clone();

    // Use the binding information in `ModuleData` to figure out what arguments
    // need to be passed to the function that we're invoking. Currently we take
    // the CLI parameters and attempt to parse them into function arguments for
    // the function we'll invoke.
    let binding = data.binding_for_export(&mut handle, name)?;
    if binding.param_types()?.len() > 0 {
        eprintln!(
            "warning: using `--render` with a function that takes arguments \
             is experimental and may break in the future"
        );
    }
    let mut values = Vec::new();
    let mut args = args.arg_arg.iter();
    for ty in binding.param_types()? {
        let val = match args.next() {
            Some(s) => s,
            None => bail!("not enough arguments for `{}`", name),
        };
        values.push(match ty {
            // TODO: integer parsing here should handle hexadecimal notation
            // like `0x0...`, but the Rust standard library currently only
            // parses base-10 representations.
            ast::WebidlScalarType::Long => Value::I32(val.parse()?),
            ast::WebidlScalarType::LongLong => Value::I64(val.parse()?),
            ast::WebidlScalarType::UnsignedLong => Value::U32(val.parse()?),
            ast::WebidlScalarType::UnsignedLongLong => Value::U64(val.parse()?),

            ast::WebidlScalarType::Float | ast::WebidlScalarType::UnrestrictedFloat => {
                Value::F32(val.parse()?)
            }
            ast::WebidlScalarType::Double | ast::WebidlScalarType::UnrestrictedDouble => {
                Value::F64(val.parse()?)
            }
            ast::WebidlScalarType::DomString => Value::String(val.to_string()),
            t => bail!("unsupported argument type {:?}", t),
        });
    }

    // Invoke the function and then afterwards print all the results that came
    // out, if there are any.
    let mut context = store.borrow().engine().borrow().create_wasmtime_context();
    let results = data
        .invoke(&mut context, &mut handle, name, &values)
        .with_context(|_| format!("failed to invoke `{}`", name))?;
    if results.len() > 0 {
        eprintln!(
            "warning: using `--render` with a function that returns values \
             is experimental and may break in the future"
        );
    }
    for result in results {
        println!("{}", result);
    }

    Ok(())
}
