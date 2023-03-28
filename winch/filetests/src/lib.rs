pub mod disasm;

#[cfg(test)]
mod test {
    use super::disasm::disasm;
    use anyhow::Context;
    use cranelift_codegen::settings;
    use serde::{Deserialize, Serialize};
    use similar::TextDiff;
    use std::str::FromStr;
    use target_lexicon::Triple;
    use wasmtime_environ::{
        wasmparser::{Parser as WasmParser, Validator},
        DefinedFuncIndex, FunctionBodyData, ModuleEnvironment, Tunables,
    };
    use winch_codegen::lookup;
    use winch_environ::FuncEnv;
    use winch_test_macros::generate_file_tests;

    #[derive(Clone, Debug, Serialize, Deserialize)]
    struct TestConfig {
        target: String,
    }

    /// A helper function to parse the test configuration from the top of the file.
    fn parse_config(wat: &str) -> TestConfig {
        let config_lines: Vec<_> = wat
            .lines()
            .take_while(|l| l.starts_with(";;!"))
            .map(|l| &l[3..])
            .collect();
        let config_text = config_lines.join("\n");

        toml::from_str(&config_text)
            .context("failed to parse the test configuration")
            .unwrap()
    }

    /// A helper function to parse the expected result from the bottom of the file.
    fn parse_expected_result(wat: &str) -> String {
        let mut expected_lines: Vec<_> = wat
            .lines()
            .rev()
            .take_while(|l| l.starts_with(";;"))
            .map(|l| {
                if l.starts_with(";; ") {
                    &l[3..]
                } else {
                    &l[2..]
                }
            })
            .collect();
        expected_lines.reverse();
        expected_lines.join("\n")
    }

    /// A helper function to rewrite the expected result in the file.
    fn rewrite_expected(wat: &str, actual: &str) -> String {
        let old_expectation_line_count = wat
            .lines()
            .rev()
            .take_while(|l| l.starts_with(";;"))
            .count();
        let old_wat_line_count = wat.lines().count();
        let new_wat_lines: Vec<_> = wat
            .lines()
            .take(old_wat_line_count - old_expectation_line_count)
            .map(|l| l.to_string())
            .chain(actual.lines().map(|l| {
                if l.is_empty() {
                    ";;".to_string()
                } else {
                    format!(";; {l}")
                }
            }))
            .collect();
        let mut new_wat = new_wat_lines.join("\n");
        new_wat.push('\n');

        new_wat
    }

    #[generate_file_tests]
    fn run_test(test_path: &str) {
        let binding = std::fs::read_to_string(test_path).unwrap();
        let wat = binding.as_str();

        let config = parse_config(wat);
        let wasm = wat::parse_str(&wat).unwrap();
        let triple = Triple::from_str(&config.target).unwrap();

        let binding = parse_expected_result(wat);
        let expected = binding.as_str();

        let shared_flags = settings::Flags::new(settings::builder());
        let isa_builder = lookup(triple).unwrap();
        let isa = isa_builder.finish(shared_flags).unwrap();

        let mut validator = Validator::new();
        let parser = WasmParser::new(0);
        let mut types = Default::default();
        let tunables = Tunables::default();
        let mut translation = ModuleEnvironment::new(&tunables, &mut validator, &mut types)
            .translate(parser, &wasm)
            .context("Failed to translate WebAssembly module")
            .unwrap();
        let _ = types.finish();

        let body_inputs = std::mem::take(&mut translation.function_body_inputs);
        let module = &translation.module;
        let types = translation.get_types();
        let env = FuncEnv::new(module, &types, &*isa);

        let binding = body_inputs
            .into_iter()
            .map(|func| compile(&env, func).join("\n"))
            .collect::<Vec<String>>()
            .join("\n\n");
        let actual = binding.as_str();

        if std::env::var("WINCH_TEST_BLESS").unwrap_or_default() == "1" {
            let new_wat = rewrite_expected(wat, actual);

            std::fs::write(test_path, new_wat)
                .with_context(|| format!("failed to write file: {}", test_path))
                .unwrap();

            return;
        }

        if expected.trim() != actual.trim() {
            eprintln!(
                "\n{}",
                TextDiff::from_lines(expected, actual)
                    .unified_diff()
                    .header("expected", "actual")
            );

            eprintln!(
                "note: You can re-run with the `WINCH_TEST_BLESS=1` environment variable set to update test expectations.\n"
            );

            panic!("Did not get the expected translation");
        }
    }

    fn compile(env: &FuncEnv, f: (DefinedFuncIndex, FunctionBodyData<'_>)) -> Vec<String> {
        let index = env.module.func_index(f.0);
        let sig = env
            .types
            .function_at(index.as_u32())
            .expect(&format!("function type at index {:?}", index.as_u32()));
        let FunctionBodyData { body, validator } = f.1;
        let validator = validator.into_validator(Default::default());

        let buffer = env
            .isa
            .compile_function(&sig, &body, env, validator)
            .expect("Couldn't compile function");

        disasm(buffer.data(), env.isa).unwrap()
    }
}
