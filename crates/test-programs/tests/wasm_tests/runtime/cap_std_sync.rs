use anyhow::Context;
use std::convert::TryInto;
use std::path::Path;
use wasi_cap_std_sync::WasiCtxBuilder;
use wasi_common::pipe::{ReadPipe, WritePipe};
use wasmtime::{Linker, Module, Store};

pub fn instantiate(data: &[u8], bin_name: &str, workspace: Option<&Path>) -> anyhow::Result<()> {
    let stdout = WritePipe::new_in_memory();
    let stderr = WritePipe::new_in_memory();

    let r = {
        let store = Store::default();

        // Create our wasi context.
        // Additionally register any preopened directories if we have them.
        let mut builder = WasiCtxBuilder::new();

        builder = builder
            .arg(bin_name)?
            .arg(".")?
            .stdin(Box::new(ReadPipe::from(Vec::new())))
            .stdout(Box::new(stdout.clone()))
            .stderr(Box::new(stderr.clone()));

        if let Some(workspace) = workspace {
            println!("preopen: {:?}", workspace);
            let preopen_dir = unsafe { cap_std::fs::Dir::open_ambient_dir(workspace) }?;
            builder = builder.preopened_dir(preopen_dir, ".")?;
        }

        #[cfg(windows)]
        {
            builder = builder
                .env("ERRNO_MODE_WINDOWS", "1")?
                .env("NO_DANGLING_SYMLINKS", "1")?
                .env("NO_FD_ALLOCATE", "1")?
                .env("NO_RENAME_DIR_TO_EMPTY_DIR", "1")?
                .env("NO_DANGLING_DIRECTORY", "1")?;
        }
        #[cfg(unix)]
        {
            builder = builder.env("ERRNO_MODE_UNIX", "1")?;
        }

        // cap-std-sync does not yet support the sync family of fdflags
        builder = builder.env("NO_FDFLAGS_SYNC_SUPPORT", "1")?;

        let wasi = wasmtime_wasi::Wasi::new(&store, builder.build()?);

        let mut linker = Linker::new(&store);

        wasi.add_to_linker(&mut linker)?;

        let module = Module::new(store.engine(), &data).context("failed to create wasm module")?;
        let instance = linker.instantiate(&module)?;
        let start = instance.get_func("_start").unwrap();
        let with_type = start.get0::<()>()?;
        with_type().map_err(anyhow::Error::from)
    };

    match r {
        Ok(()) => Ok(()),
        Err(trap) => {
            let stdout = stdout
                .try_into_inner()
                .expect("sole ref to stdout")
                .into_inner();
            if !stdout.is_empty() {
                println!("guest stdout:\n{}\n===", String::from_utf8_lossy(&stdout));
            }
            let stderr = stderr
                .try_into_inner()
                .expect("sole ref to stderr")
                .into_inner();
            if !stderr.is_empty() {
                println!("guest stderr:\n{}\n===", String::from_utf8_lossy(&stderr));
            }
            Err(trap.context(format!("error while testing Wasm module '{}'", bin_name,)))
        }
    }
}

pub fn instantiate_inherit_stdio(
    data: &[u8],
    bin_name: &str,
    workspace: Option<&Path>,
) -> anyhow::Result<()> {
    let r = {
        let store = Store::default();

        // Tests assume that stdin does not have any bytes available to read. Make sure this is the
        // case, regardless of the test environment:
        use std::io::Read;
        use system_interface::io::ReadReady;
        let nbytes = std::io::stdin()
            .num_ready_bytes()
            .expect("get stdin's ready bytes");
        if nbytes > 0 {
            let mut stdin_contents = Vec::new();
            stdin_contents.resize(nbytes.try_into().expect("ready bytes fits in memory"), 0);
            std::io::stdin()
                .read(stdin_contents.as_mut_slice())
                .expect("read stdin to end");
        }

        // Create our wasi context.
        // Additionally register any preopened directories if we have them.
        let mut builder = WasiCtxBuilder::new();

        builder = builder.arg(bin_name)?.arg(".")?.inherit_stdio();

        if let Some(workspace) = workspace {
            println!("preopen: {:?}", workspace);
            let preopen_dir = unsafe { cap_std::fs::Dir::open_ambient_dir(workspace) }?;
            builder = builder.preopened_dir(preopen_dir, ".")?;
        }

        let snapshot1 = wasmtime_wasi::Wasi::new(&store, builder.build()?);

        let mut linker = Linker::new(&store);

        snapshot1.add_to_linker(&mut linker)?;

        let module = Module::new(store.engine(), &data).context("failed to create wasm module")?;
        let instance = linker.instantiate(&module)?;
        let start = instance.get_func("_start").unwrap();
        let with_type = start.get0::<()>()?;
        with_type().map_err(anyhow::Error::from)
    };

    match r {
        Ok(()) => Ok(()),
        Err(trap) => Err(trap.context(format!("error while testing Wasm module '{}'", bin_name,))),
    }
}
