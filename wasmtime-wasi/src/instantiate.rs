use cranelift_codegen::ir::types;
use cranelift_codegen::{ir, isa};
use cranelift_entity::PrimaryMap;
use cranelift_wasm::DefinedFuncIndex;
use std::cell::RefCell;
use std::collections::HashMap;
use std::fs::File;
use std::rc::Rc;
use syscalls;
use target_lexicon::HOST;
use wasi_common::WasiCtxBuilder;
use wasmtime_environ::{translate_signature, Export, Module};
use wasmtime_runtime::{Imports, InstanceHandle, InstantiationError, VMFunctionBody};

/// Return an instance implementing the "wasi" interface.
pub fn instantiate_wasi(
    prefix: &str,
    global_exports: Rc<RefCell<HashMap<String, Option<wasmtime_runtime::Export>>>>,
    preopened_dirs: &[(String, File)],
    argv: &[String],
    environ: &[(String, String)],
) -> Result<InstanceHandle, InstantiationError> {
    let pointer_type = types::Type::triple_pointer_type(&HOST);
    let mut module = Module::new();
    let mut finished_functions: PrimaryMap<DefinedFuncIndex, *const VMFunctionBody> =
        PrimaryMap::new();
    let call_conv = isa::CallConv::triple_default(&HOST);

    macro_rules! signature {
        ($name:ident) => {{
            let sig = module.signatures.push(translate_signature(
                ir::Signature {
                    params: syscalls::$name::params()
                        .into_iter()
                        .map(ir::AbiParam::new)
                        .collect(),
                    returns: syscalls::$name::results()
                        .into_iter()
                        .map(ir::AbiParam::new)
                        .collect(),
                    call_conv,
                },
                pointer_type,
            ));
            let func = module.functions.push(sig);
            module.exports.insert(
                prefix.to_owned() + stringify!($name),
                Export::Function(func),
            );
            finished_functions.push(syscalls::$name::SHIM as *const VMFunctionBody);
        }};
    }

    signature!(args_get);
    signature!(args_sizes_get);
    signature!(clock_res_get);
    signature!(clock_time_get);
    signature!(environ_get);
    signature!(environ_sizes_get);
    signature!(fd_prestat_get);
    signature!(fd_prestat_dir_name);
    signature!(fd_close);
    signature!(fd_datasync);
    signature!(fd_pread);
    signature!(fd_pwrite);
    signature!(fd_read);
    signature!(fd_renumber);
    signature!(fd_seek);
    signature!(fd_tell);
    signature!(fd_fdstat_get);
    signature!(fd_fdstat_set_flags);
    signature!(fd_fdstat_set_rights);
    signature!(fd_sync);
    signature!(fd_write);
    signature!(fd_advise);
    signature!(fd_allocate);
    signature!(path_create_directory);
    signature!(path_link);
    signature!(path_open);
    signature!(fd_readdir);
    signature!(path_readlink);
    signature!(path_rename);
    signature!(fd_filestat_get);
    signature!(fd_filestat_set_times);
    signature!(fd_filestat_set_size);
    signature!(path_filestat_get);
    signature!(path_filestat_set_times);
    signature!(path_symlink);
    signature!(path_unlink_file);
    signature!(path_remove_directory);
    signature!(poll_oneoff);
    signature!(proc_exit);
    signature!(proc_raise);
    signature!(random_get);
    signature!(sched_yield);
    signature!(sock_recv);
    signature!(sock_send);
    signature!(sock_shutdown);

    let imports = Imports::none();
    let data_initializers = Vec::new();
    let signatures = PrimaryMap::new();

    let args: Vec<&str> = argv.iter().map(AsRef::as_ref).collect();
    let mut wasi_ctx_builder = WasiCtxBuilder::new().args(&args).inherit_stdio();

    for (k, v) in environ {
        wasi_ctx_builder = wasi_ctx_builder.env(k, v);
    }

    for (dir, f) in preopened_dirs {
        wasi_ctx_builder = wasi_ctx_builder.preopened_dir(
            f.try_clone().map_err(|err| {
                InstantiationError::Resource(format!(
                    "couldn't clone an instance handle to pre-opened dir: {}",
                    err
                ))
            })?,
            dir,
        );
    }

    let wasi_ctx = wasi_ctx_builder.build().map_err(|err| {
        InstantiationError::Resource(format!("couldn't assemble WASI context object: {}", err))
    })?;

    InstanceHandle::new(
        Rc::new(module),
        global_exports,
        finished_functions.into_boxed_slice(),
        imports,
        &data_initializers,
        signatures.into_boxed_slice(),
        None,
        Box::new(wasi_ctx),
    )
}
