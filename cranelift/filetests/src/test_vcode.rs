use crate::subtest::{run_filecheck, Context, SubTest, SubtestResult};
use cranelift_codegen::ir::Function;
use cranelift_codegen::isa::lookup;
use cranelift_codegen::settings;
use cranelift_codegen::Context as CodegenContext;
use cranelift_reader::{TestCommand, TestOption};

use log::info;
use std::borrow::Cow;
use std::string::String;

struct TestVCode {
    arch: String,
}

pub fn subtest(parsed: &TestCommand) -> SubtestResult<Box<dyn SubTest>> {
    assert_eq!(parsed.command, "vcode");

    let mut arch = "arm64".to_string();
    for option in &parsed.options {
        match option {
            TestOption::Value(k, v) if k == &"arch" => {
                arch = v.to_string();
            }
            _ => {}
        }
    }

    Ok(Box::new(TestVCode { arch }))
}

impl SubTest for TestVCode {
    fn name(&self) -> &'static str {
        "vcode"
    }

    fn is_mutating(&self) -> bool {
        true
    }

    fn needs_isa(&self) -> bool {
        true
    }

    fn run(&self, func: Cow<Function>, context: &Context) -> SubtestResult<()> {
        let triple = context.isa.unwrap().triple().clone();
        let func = func.into_owned();

        let mut isa = lookup(triple)
            .map_err(|_| format!("Could not look up backend for arch '{}'", self.arch))?
            .finish(settings::Flags::new(settings::builder()));

        let mut codectx = CodegenContext::for_function(func);
        codectx.set_disasm(true);

        codectx
            .compile(&mut *isa)
            .map_err(|e| format!("Could not compile with arch '{}': {:?}", self.arch, e))?;

        let result = codectx.mach_compile_result.take().unwrap();
        let text = result.disasm.unwrap();

        info!("text input to filecheck is:\n{}\n", text);

        run_filecheck(&text, context)
    }
}
