//! Run the tests in `wasi_testsuite` using Wasmtime's CLI binary and checking
//! the results with a [wasi-testsuite] spec.
//!
//! [wasi-testsuite]: https://github.com/WebAssembly/wasi-testsuite

#![cfg(not(miri))]

use crate::cli_tests::get_wasmtime_command;
use anyhow::{anyhow, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use walkdir::{DirEntry, WalkDir};

#[test]
#[cfg_attr(target_os = "windows", ignore)] // TODO: https://github.com/WebAssembly/WASI/issues/524
fn wasi_testsuite() -> Result<()> {
    // Currently, Wasmtime's implementation in wasi-common does not line up
    // exactly with the expectations in wasi-testsuite. This could be for one of
    // various reasons:
    //  - wasi-common has a bug
    //  - wasi-testsuite overspecifies (or incorrectly specifies) a test
    //  - this test runner incorrectly configures Wasmtime's CLI execution.
    //
    // This list is expected to shrink as the failures are resolved. The easiest
    // way to resolve one of these is to remove the file from the list and run
    // `cargo test wasi_testsuite -- --nocapture`. The printed output will show
    // the expected result, the actual result, and a command to replicate the
    // failure from the command line.
    const WASI_COMMON_IGNORE_LIST: &[&str] = &[
        "environ_get-multiple-variables.wasm",
        "environ_sizes_get-multiple-variables.wasm",
        "fdopendir-with-access.wasm",
        "fopen-with-access.wasm",
        "lseek.wasm",
        "pread-with-access.wasm",
        "pwrite-with-access.wasm",
        "stat-dev-ino.wasm",
        "close_preopen.wasm",
        "dangling_fd.wasm",
        "dangling_symlink.wasm",
        "directory_seek.wasm",
        "fd_advise.wasm",
        "fd_filestat_set.wasm",
        "fd_flags_set.wasm",
        "fd_readdir.wasm",
        "interesting_paths.wasm",
    ];
    run_all(
        "tests/wasi_testsuite/wasi-common",
        &[],
        WASI_COMMON_IGNORE_LIST,
    )?;
    run_all(
        "tests/wasi_testsuite/wasi-threads",
        &[
            "--wasi-modules",
            "experimental-wasi-threads",
            "--wasm-features",
            "threads",
        ],
        &[],
    )?;
    Ok(())
}

fn run_all(testsuite_dir: &str, extra_flags: &[&str], ignore: &[&str]) -> Result<()> {
    for module in list_modules(testsuite_dir) {
        if should_ignore(&module, ignore) {
            println!("Ignoring {}", module.display());
        } else {
            println!("Testing {}", module.display());
            let spec = if let Ok(contents) = fs::read_to_string(&module.with_extension("json")) {
                serde_json::from_str(&contents)?
            } else {
                Spec::default()
            };
            let mut cmd = build_command(module, extra_flags, &spec)?;
            let result = cmd.output()?;
            if spec != result {
                println!("  command: {cmd:?}");
                println!("  spec: {spec:#?}");
                println!("  result: {result:#?}");
                panic!("FAILED! The result does not match the specification");
            }
        }
    }
    Ok(())
}

fn list_modules(testsuite_dir: &str) -> impl Iterator<Item = PathBuf> {
    WalkDir::new(testsuite_dir)
        .into_iter()
        .filter_map(Result::ok)
        .filter(is_wasm)
        .map(|e| e.path().to_path_buf())
}

fn is_wasm(entry: &DirEntry) -> bool {
    let path = entry.path();
    let ext = path.extension().map(OsStr::to_str).flatten();
    path.is_file() && (ext == Some("wat") || ext == Some("wasm"))
}

fn should_ignore<P: AsRef<Path>>(path: P, ignore_list: &[&str]) -> bool {
    let file_name = path.as_ref().file_name().unwrap().to_str().unwrap();
    ignore_list.contains(&file_name)
}

fn build_command<P: AsRef<Path>>(module: P, extra_flags: &[&str], spec: &Spec) -> Result<Command> {
    let mut cmd = get_wasmtime_command()?;
    let parent_dir = module
        .as_ref()
        .parent()
        .ok_or(anyhow!("module has no parent?"))?;

    // Add arguments.
    cmd.args(["run", "--disable-cache"]);
    cmd.args(extra_flags);
    if let Some(dirs) = &spec.dirs {
        for dir in dirs {
            cmd.arg("--dir");
            cmd.arg(parent_dir.join(dir));
        }
    }
    cmd.arg(module.as_ref().to_str().unwrap());
    if let Some(spec_args) = &spec.args {
        cmd.args(spec_args);
    }

    // Create the environment. This uses the shell environment, but we could also
    // have used the Wasmtime CLI's `--env` parameter here.
    cmd.env_clear();
    if let Some(env) = &spec.env {
        cmd.envs(env);
    }

    Ok(cmd)
}

#[derive(Debug, Default, Deserialize)]
struct Spec {
    args: Option<Vec<String>>,
    dirs: Option<Vec<String>>,
    env: Option<HashMap<String, String>>,
    exit_code: Option<i32>,
    stderr: Option<String>,
    stdout: Option<String>,
}

impl PartialEq<Output> for Spec {
    fn eq(&self, other: &Output) -> bool {
        self.exit_code.unwrap_or(0) == other.status.code().unwrap()
            && matches_or_missing(&self.stdout, &other.stdout)
            && matches_or_missing(&self.stderr, &other.stderr)
    }
}

fn matches_or_missing(a: &Option<String>, b: &[u8]) -> bool {
    a.as_ref()
        .map(|s| s == &String::from_utf8_lossy(b))
        .unwrap_or(true)
}
