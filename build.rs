//! Build program to generate a program which runs all the testsuites.
//!
//! By generating a separate `#[test]` test for each file, we allow cargo test
//! to automatically run the files in parallel.

use anyhow::Context;
use std::env;
use std::fmt::Write;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() -> anyhow::Result<()> {
    let out_dir = PathBuf::from(
        env::var_os("OUT_DIR").expect("The OUT_DIR environment variable must be set"),
    );
    let mut out = String::new();

    for strategy in &[
        "Cranelift",
        #[cfg(feature = "lightbeam")]
        "Lightbeam",
    ] {
        writeln!(out, "#[cfg(test)]")?;
        writeln!(out, "#[allow(non_snake_case)]")?;
        writeln!(out, "mod {} {{", strategy)?;

        test_directory(&mut out, "tests/misc_testsuite", strategy)?;
        let spec_tests = test_directory(&mut out, "tests/spec_testsuite", strategy)?;
        // Skip running spec_testsuite tests if the submodule isn't checked
        // out.
        if spec_tests > 0 {
            start_test_module(&mut out, "simd")?;
            write_testsuite_tests(
                &mut out,
                "tests/spec_testsuite/proposals/simd/simd_address.wast",
                "simd",
                strategy,
            )?;
            write_testsuite_tests(
                &mut out,
                "tests/spec_testsuite/proposals/simd/simd_align.wast",
                "simd",
                strategy,
            )?;
            // FIXME this uses some features from the reference types proposal
            // (multi-table) which aren't fully implemented yet
            // write_testsuite_tests(
            //     &mut out,
            //     "tests/spec_testsuite/proposals/simd/simd_const.wast",
            //     "simd",
            //     strategy,
            // )?;
            finish_test_module(&mut out)?;

            test_directory(
                &mut out,
                "tests/spec_testsuite/proposals/multi-value",
                strategy,
            )
            .expect("generating tests");
        } else {
            println!(
                "cargo:warning=The spec testsuite is disabled. To enable, run `git submodule \
                 update --remote`."
            );
        }

        writeln!(out, "}}")?;
    }

    // Write out our auto-generated tests and opportunistically format them with
    // `rustfmt` if it's installed.
    let output = out_dir.join("wast_testsuite_tests.rs");
    fs::write(&output, out)?;
    drop(Command::new("rustfmt").arg(&output).status());
    Ok(())
}

fn test_directory(
    out: &mut String,
    path: impl AsRef<Path>,
    strategy: &str,
) -> anyhow::Result<usize> {
    let path = path.as_ref();
    let mut dir_entries: Vec<_> = path
        .read_dir()
        .context(format!("failed to read {:?}", path))?
        .map(|r| r.expect("reading testsuite directory entry"))
        .filter_map(|dir_entry| {
            let p = dir_entry.path();
            let ext = p.extension()?;
            // Only look at wast files.
            if ext != "wast" {
                return None;
            }
            // Ignore files starting with `.`, which could be editor temporary files
            if p.file_stem()?.to_str()?.starts_with(".") {
                return None;
            }
            Some(p)
        })
        .collect();

    dir_entries.sort();

    let testsuite = &extract_name(path);
    start_test_module(out, testsuite)?;
    for entry in dir_entries.iter() {
        write_testsuite_tests(out, entry, testsuite, strategy)?;
    }
    finish_test_module(out)?;
    Ok(dir_entries.len())
}

/// Extract a valid Rust identifier from the stem of a path.
fn extract_name(path: impl AsRef<Path>) -> String {
    path.as_ref()
        .file_stem()
        .expect("filename should have a stem")
        .to_str()
        .expect("filename should be representable as a string")
        .replace("-", "_")
        .replace("/", "_")
}

fn start_test_module(out: &mut String, testsuite: &str) -> anyhow::Result<()> {
    writeln!(out, "mod {} {{", testsuite)?;
    Ok(())
}

fn finish_test_module(out: &mut String) -> anyhow::Result<()> {
    out.push_str("}\n");
    Ok(())
}

fn write_testsuite_tests(
    out: &mut String,
    path: impl AsRef<Path>,
    testsuite: &str,
    strategy: &str,
) -> anyhow::Result<()> {
    let path = path.as_ref();
    let testname = extract_name(path);

    writeln!(out, "#[test]")?;
    if ignore(testsuite, &testname, strategy) {
        writeln!(out, "#[ignore]")?;
    }
    writeln!(out, "fn r#{}() -> anyhow::Result<()> {{", &testname)?;
    writeln!(
        out,
        "crate::run_wast(r#\"{}\"#, crate::Strategy::{})",
        path.display(),
        strategy
    )?;
    writeln!(out, "}}")?;
    writeln!(out)?;
    Ok(())
}

/// Ignore tests that aren't supported yet.
fn ignore(testsuite: &str, testname: &str, strategy: &str) -> bool {
    match strategy {
        #[cfg(feature = "lightbeam")]
        "Lightbeam" => match (testsuite, testname) {
            (_, _) if testname.starts_with("simd") => return true,
            (_, _) if testsuite.ends_with("multi_value") => return true,
            // Lightbeam doesn't support float arguments on the stack.
            ("spec_testsuite", "call.wast") => return true,
            _ => (),
        },
        "Cranelift" => match (testsuite, testname) {
            _ => {}
        },
        _ => panic!("unrecognized strategy"),
    }

    false
}
