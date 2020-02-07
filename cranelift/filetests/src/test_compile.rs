//! Test command for testing the code generator pipeline
//!
//! The `compile` test command runs each function through the full code generator pipeline

use crate::subtest::{run_filecheck, Context, SubTest, SubtestResult};
use cranelift_codegen;
use cranelift_codegen::binemit::{self, CodeInfo};
use cranelift_codegen::ir;
use cranelift_codegen::isa;
use cranelift_codegen::print_errors::pretty_error;
use cranelift_reader::TestCommand;
use log::info;
use std::borrow::Cow;

struct TestCompile;

pub fn subtest(parsed: &TestCommand) -> SubtestResult<Box<dyn SubTest>> {
    assert_eq!(parsed.command, "compile");
    if !parsed.options.is_empty() {
        Err(format!("No options allowed on {}", parsed))
    } else {
        Ok(Box::new(TestCompile))
    }
}

impl SubTest for TestCompile {
    fn name(&self) -> &'static str {
        "compile"
    }

    fn is_mutating(&self) -> bool {
        true
    }

    fn needs_isa(&self) -> bool {
        true
    }

    fn run(&self, func: Cow<ir::Function>, context: &Context) -> SubtestResult<()> {
        let isa = context.isa.expect("compile needs an ISA");
        let mut comp_ctx = cranelift_codegen::Context::for_function(func.into_owned());

        let CodeInfo { total_size, .. } = comp_ctx
            .compile(isa)
            .map_err(|e| pretty_error(&comp_ctx.func, context.isa, e))?;

        info!(
            "Generated {} bytes of code:\n{}",
            total_size,
            comp_ctx.func.display(isa)
        );

        // Verify that the returned code size matches the emitted bytes.
        let mut sink = SizeSink { offset: 0 };
        binemit::emit_function(
            &comp_ctx.func,
            |func, inst, div, sink, isa| isa.emit_inst(func, inst, div, sink),
            &mut sink,
            isa,
        );

        if sink.offset != total_size {
            return Err(format!(
                "Expected code size {}, got {}",
                total_size, sink.offset
            ));
        }

        // Run final code through filecheck.
        let text = comp_ctx.func.display(Some(isa)).to_string();
        run_filecheck(&text, context)
    }
}

/// Code sink that simply counts bytes.
struct SizeSink {
    offset: binemit::CodeOffset,
}

impl binemit::CodeSink for SizeSink {
    fn offset(&self) -> binemit::CodeOffset {
        self.offset
    }

    fn put1(&mut self, _: u8) {
        self.offset += 1;
    }

    fn put2(&mut self, _: u16) {
        self.offset += 2;
    }

    fn put4(&mut self, _: u32) {
        self.offset += 4;
    }

    fn put8(&mut self, _: u64) {
        self.offset += 8;
    }

    fn reloc_block(&mut self, _reloc: binemit::Reloc, _block_offset: binemit::CodeOffset) {}
    fn reloc_external(
        &mut self,
        _reloc: binemit::Reloc,
        _name: &ir::ExternalName,
        _addend: binemit::Addend,
    ) {
    }
    fn reloc_constant(&mut self, _: binemit::Reloc, _: ir::ConstantOffset) {}
    fn reloc_jt(&mut self, _reloc: binemit::Reloc, _jt: ir::JumpTable) {}
    fn trap(&mut self, _code: ir::TrapCode, _srcloc: ir::SourceLoc) {}
    fn begin_jumptables(&mut self) {}
    fn begin_rodata(&mut self) {}
    fn end_codegen(&mut self) {}
    fn add_stackmap(
        &mut self,
        _: &[ir::entities::Value],
        _: &ir::Function,
        _: &dyn isa::TargetIsa,
    ) {
    }
}
