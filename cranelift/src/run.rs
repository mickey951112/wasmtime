//! CLI tool to compile Cranelift IR files to native code in memory and execute them.

use crate::utils::{iterate_files, read_to_string};
use cranelift_codegen::isa::{CallConv, TargetIsa};
use cranelift_filetests::SingleFunctionCompiler;
use cranelift_native::builder as host_isa_builder;
use cranelift_reader::{parse_run_command, parse_test, Details, IsaSpec, ParseOptions};
use std::path::PathBuf;
use target_lexicon::Triple;

pub fn run(files: Vec<String>, flag_print: bool) -> Result<(), String> {
    let stdin_exist = files.iter().find(|file| *file == "-").is_some();
    let filtered_files = files
        .iter()
        .filter(|file| *file != "-")
        .map(|file| file.to_string())
        .collect::<Vec<String>>();
    let mut total = 0;
    let mut errors = 0;
    let mut special_files: Vec<PathBuf> = vec![];
    if stdin_exist {
        special_files.push("-".into());
    }
    for file in iterate_files(filtered_files).chain(special_files) {
        total += 1;
        match run_single_file(&file) {
            Ok(_) => {
                if flag_print {
                    println!("{}", file.to_string_lossy());
                }
            }
            Err(e) => {
                if flag_print {
                    println!("{}: {}", file.to_string_lossy(), e);
                }
                errors += 1;
            }
        }
    }

    if flag_print {
        match total {
            0 => println!("0 files"),
            1 => println!("1 file"),
            n => println!("{} files", n),
        }
    }

    match errors {
        0 => Ok(()),
        1 => Err(String::from("1 failure")),
        n => Err(format!("{} failures", n)),
    }
}

/// Run all functions in a file that are succeeded by "run:" comments
fn run_single_file(path: &PathBuf) -> Result<(), String> {
    let file_contents = read_to_string(&path).map_err(|e| e.to_string())?;
    run_file_contents(file_contents)
}

/// Main body of `run_single_file` separated for testing
fn run_file_contents(file_contents: String) -> Result<(), String> {
    let options = ParseOptions {
        default_calling_convention: CallConv::triple_default(&Triple::host()), // use the host's default calling convention
        ..ParseOptions::default()
    };
    let test_file = parse_test(&file_contents, options).map_err(|e| e.to_string())?;
    let isa = create_target_isa(&test_file.isa_spec)?;
    let mut compiler = SingleFunctionCompiler::new(isa);
    for (func, Details { comments, .. }) in test_file.functions {
        for comment in comments {
            if let Some(command) =
                parse_run_command(comment.text, &func.signature).map_err(|e| e.to_string())?
            {
                let compiled_fn = compiler.compile(func.clone()).map_err(|e| e.to_string())?;
                command.run(|_, args| Ok(compiled_fn.call(args)))?;
            }
        }
    }
    Ok(())
}

/// Build an ISA based on the current machine running this code (the host)
fn create_target_isa(isa_spec: &IsaSpec) -> Result<Box<dyn TargetIsa>, String> {
    if let IsaSpec::None(flags) = isa_spec {
        // build an ISA for the current machine
        let builder = host_isa_builder()?;
        Ok(builder.finish(flags.clone()))
    } else {
        Err(String::from("A target ISA was specified in the file but should not have been--only the host ISA can be used for running CLIF files"))
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn nop() {
        let code = String::from(
            "
            function %test() -> b8 {
            block0:
                nop
                v1 = bconst.b8 true
                return v1
            }
            ; run
            ",
        );
        run_file_contents(code).unwrap()
    }
}
