//! The module that implements the `wasmtime run` command.

use crate::{init_file_per_thread_logger, CommonOptions};
use anyhow::{bail, Context as _, Result};
use std::{
    ffi::{OsStr, OsString},
    fs::File,
    path::{Component, Path, PathBuf},
};
use structopt::{clap::AppSettings, StructOpt};
use wasi_common::preopen_dir;
use wasmtime::{Engine, Instance, Module, Store};
use wasmtime_interface_types::ModuleData;
use wasmtime_wasi::{old::snapshot_0::Wasi as WasiSnapshot0, Wasi};

fn parse_module(s: &OsStr) -> Result<PathBuf, OsString> {
    // Do not accept wasmtime subcommand names as the module name
    match s.to_str() {
        Some("help") | Some("config") | Some("run") | Some("wasm2obj") | Some("wast") => {
            Err("module name cannot be the same as a subcommand".into())
        }
        _ => Ok(s.into()),
    }
}

fn parse_env_var(s: &str) -> Result<(String, String)> {
    let parts: Vec<_> = s.splitn(2, '=').collect();
    if parts.len() != 2 {
        bail!("must be of the form `key=value`");
    }
    Ok((parts[0].to_owned(), parts[1].to_owned()))
}

fn parse_map_dirs(s: &str) -> Result<(String, String)> {
    let parts: Vec<&str> = s.split("::").collect();
    if parts.len() != 2 {
        bail!("must contain exactly one double colon ('::')");
    }
    Ok((parts[0].into(), parts[1].into()))
}

/// Runs a WebAssembly module
#[derive(StructOpt)]
#[structopt(name = "run", setting = AppSettings::TrailingVarArg)]
pub struct RunCommand {
    #[structopt(flatten)]
    common: CommonOptions,

    /// Grant access to the given host directory
    #[structopt(long = "dir", number_of_values = 1, value_name = "DIRECTORY")]
    dirs: Vec<String>,

    /// Pass an environment variable to the program
    #[structopt(long = "env", number_of_values = 1, value_name = "NAME=VAL", parse(try_from_str = parse_env_var))]
    vars: Vec<(String, String)>,

    /// The name of the function to run
    #[structopt(long, value_name = "FUNCTION")]
    invoke: Option<String>,

    /// Grant access to a guest directory mapped as a host directory
    #[structopt(long = "mapdir", number_of_values = 1, value_name = "GUEST_DIR::HOST_DIR", parse(try_from_str = parse_map_dirs))]
    map_dirs: Vec<(String, String)>,

    /// The path of the WebAssembly module to run
    #[structopt(
        index = 1,
        required = true,
        value_name = "WASM_MODULE",
        parse(try_from_os_str = parse_module),
    )]
    module: PathBuf,

    /// Load the given WebAssembly module before the main module
    #[structopt(
        long = "preload",
        number_of_values = 1,
        value_name = "MODULE_PATH",
        parse(from_os_str)
    )]
    preloads: Vec<PathBuf>,

    // NOTE: this must come last for trailing varargs
    /// The arguments to pass to the module
    #[structopt(value_name = "ARGS")]
    module_args: Vec<String>,
}

impl RunCommand {
    /// Executes the command.
    pub fn execute(&self) -> Result<()> {
        if self.common.log_to_files {
            let prefix = "wasmtime.dbg.";
            init_file_per_thread_logger(prefix);
        } else {
            pretty_env_logger::init();
        }

        let config = self.common.config()?;
        let engine = Engine::new(&config);
        let store = Store::new(&engine);

        // Make wasi available by default.
        let preopen_dirs = self.compute_preopen_dirs()?;
        let argv = self.compute_argv();

        let module_registry = ModuleRegistry::new(&store, &preopen_dirs, &argv, &self.vars)?;

        // Load the preload wasm modules.
        for preload in self.preloads.iter() {
            Self::instantiate_module(&store, &module_registry, preload)
                .with_context(|| format!("failed to process preload at `{}`", preload.display()))?;
        }

        // Load the main wasm module.
        self.handle_module(&store, &module_registry)
            .with_context(|| format!("failed to run main module `{}`", self.module.display()))?;

        Ok(())
    }

    fn compute_preopen_dirs(&self) -> Result<Vec<(String, File)>> {
        let mut preopen_dirs = Vec::new();

        for dir in self.dirs.iter() {
            preopen_dirs.push((
                dir.clone(),
                preopen_dir(dir).with_context(|| format!("failed to open directory '{}'", dir))?,
            ));
        }

        for (guest, host) in self.map_dirs.iter() {
            preopen_dirs.push((
                guest.clone(),
                preopen_dir(host)
                    .with_context(|| format!("failed to open directory '{}'", host))?,
            ));
        }

        Ok(preopen_dirs)
    }

    fn compute_argv(&self) -> Vec<String> {
        let mut result = Vec::new();

        // Add argv[0], which is the program name. Only include the base name of the
        // main wasm module, to avoid leaking path information.
        result.push(
            self.module
                .components()
                .next_back()
                .map(Component::as_os_str)
                .and_then(OsStr::to_str)
                .unwrap_or("")
                .to_owned(),
        );

        // Add the remaining arguments.
        for arg in self.module_args.iter() {
            result.push(arg.clone());
        }

        result
    }

    fn instantiate_module(
        store: &Store,
        module_registry: &ModuleRegistry,
        path: &Path,
    ) -> Result<(Instance, Module, Vec<u8>)> {
        // Read the wasm module binary either as `*.wat` or a raw binary
        let data = wat::parse_file(path)?;

        let module = Module::new(store, &data)?;

        // Resolve import using module_registry.
        let imports = module
            .imports()
            .iter()
            .map(|i| {
                let export = match i.module() {
                    "wasi_snapshot_preview1" => {
                        module_registry.wasi_snapshot_preview1.get_export(i.name())
                    }
                    "wasi_unstable" => module_registry.wasi_unstable.get_export(i.name()),
                    other => bail!("import module `{}` was not found", other),
                };
                match export {
                    Some(export) => Ok(export.clone().into()),
                    None => bail!(
                        "import `{}` was not found in module `{}`",
                        i.name(),
                        i.module()
                    ),
                }
            })
            .collect::<Result<Vec<_>, _>>()?;

        let instance = Instance::new(&module, &imports)
            .context(format!("failed to instantiate {:?}", path))?;

        Ok((instance, module, data))
    }

    fn handle_module(&self, store: &Store, module_registry: &ModuleRegistry) -> Result<()> {
        let (instance, module, data) =
            Self::instantiate_module(store, module_registry, &self.module)?;

        // If a function to invoke was given, invoke it.
        if let Some(name) = self.invoke.as_ref() {
            let data = ModuleData::new(&data)?;
            self.invoke_export(instance, &data, name)?;
        } else if module
            .exports()
            .iter()
            .any(|export| export.name().is_empty())
        {
            // Launch the default command export.
            let data = ModuleData::new(&data)?;
            self.invoke_export(instance, &data, "")?;
        } else {
            // If the module doesn't have a default command export, launch the
            // _start function if one is present, as a compatibility measure.
            let data = ModuleData::new(&data)?;
            self.invoke_export(instance, &data, "_start")?;
        }

        Ok(())
    }

    fn invoke_export(&self, instance: Instance, data: &ModuleData, name: &str) -> Result<()> {
        use wasm_webidl_bindings::ast;
        use wasmtime_interface_types::Value;

        let mut handle = instance.handle().clone();

        // Use the binding information in `ModuleData` to figure out what arguments
        // need to be passed to the function that we're invoking. Currently we take
        // the CLI parameters and attempt to parse them into function arguments for
        // the function we'll invoke.
        let binding = data.binding_for_export(&mut handle, name)?;
        if !binding.param_types()?.is_empty() {
            eprintln!(
                "warning: using `--invoke` with a function that takes arguments \
                 is experimental and may break in the future"
            );
        }
        let mut args = self.module_args.iter();
        let mut values = Vec::new();
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
        let results = data
            .invoke_export(&instance, name, &values)
            .with_context(|| format!("failed to invoke `{}`", name))?;
        if !results.is_empty() {
            eprintln!(
                "warning: using `--invoke` with a function that returns values \
                 is experimental and may break in the future"
            );
        }
        for result in results {
            println!("{}", result);
        }

        Ok(())
    }
}

struct ModuleRegistry {
    wasi_snapshot_preview1: Wasi,
    wasi_unstable: WasiSnapshot0,
}

impl ModuleRegistry {
    fn new(
        store: &Store,
        preopen_dirs: &[(String, File)],
        argv: &[String],
        vars: &[(String, String)],
    ) -> Result<ModuleRegistry> {
        let mut cx1 = wasi_common::WasiCtxBuilder::new();

        cx1.inherit_stdio().args(argv).envs(vars);

        for (name, file) in preopen_dirs {
            cx1.preopened_dir(file.try_clone()?, name);
        }

        let cx1 = cx1.build()?;

        let mut cx2 = wasi_common::old::snapshot_0::WasiCtxBuilder::new()
            .inherit_stdio()
            .args(argv)
            .envs(vars);

        for (name, file) in preopen_dirs {
            cx2 = cx2.preopened_dir(file.try_clone()?, name);
        }

        let cx2 = cx2.build()?;

        Ok(ModuleRegistry {
            wasi_snapshot_preview1: Wasi::new(store, cx1),
            wasi_unstable: WasiSnapshot0::new(store, cx2),
        })
    }
}
