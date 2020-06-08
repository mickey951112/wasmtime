use crate::subtest::{run_filecheck, Context, SubTest, SubtestResult};
use cranelift_codegen::binemit::{self, Addend, CodeOffset, CodeSink, Reloc, Stackmap};
use cranelift_codegen::ir::*;
use cranelift_codegen::isa::TargetIsa;
use cranelift_codegen::print_errors::pretty_error;
use cranelift_reader::TestCommand;
use std::borrow::Cow;
use std::fmt::Write;

struct TestStackmaps;

pub fn subtest(parsed: &TestCommand) -> SubtestResult<Box<dyn SubTest>> {
    assert_eq!(parsed.command, "stackmaps");
    if !parsed.options.is_empty() {
        Err(format!("No options allowed on {}", parsed))
    } else {
        Ok(Box::new(TestStackmaps))
    }
}

impl SubTest for TestStackmaps {
    fn name(&self) -> &'static str {
        "stackmaps"
    }

    fn run(&self, func: Cow<Function>, context: &Context) -> SubtestResult<()> {
        let mut comp_ctx = cranelift_codegen::Context::for_function(func.into_owned());

        comp_ctx
            .compile(context.isa.expect("`test stackmaps` requires an isa"))
            .map_err(|e| pretty_error(&comp_ctx.func, context.isa, e))?;

        let mut sink = TestStackMapsSink::default();
        binemit::emit_function(
            &comp_ctx.func,
            |func, inst, div, sink, isa| {
                if func.dfg[inst].opcode() == Opcode::Safepoint {
                    writeln!(&mut sink.text, "{}", func.dfg.display_inst(inst, isa)).unwrap();
                }
                isa.emit_inst(func, inst, div, sink)
            },
            &mut sink,
            context.isa.expect("`test stackmaps` requires an isa"),
        );

        let mut text = comp_ctx.func.display(context.isa).to_string();
        text.push('\n');
        text.push_str("Stack maps:\n");
        text.push('\n');
        text.push_str(&sink.text);

        run_filecheck(&text, context)
    }
}

#[derive(Default)]
struct TestStackMapsSink {
    offset: u32,
    text: String,
}

impl CodeSink for TestStackMapsSink {
    fn offset(&self) -> CodeOffset {
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

    fn reloc_block(&mut self, _: Reloc, _: CodeOffset) {}
    fn reloc_external(&mut self, _: SourceLoc, _: Reloc, _: &ExternalName, _: Addend) {}
    fn reloc_constant(&mut self, _: Reloc, _: ConstantOffset) {}
    fn reloc_jt(&mut self, _: Reloc, _: JumpTable) {}
    fn trap(&mut self, _: TrapCode, _: SourceLoc) {}
    fn begin_jumptables(&mut self) {}
    fn begin_rodata(&mut self) {}
    fn end_codegen(&mut self) {}

    fn add_stackmap(&mut self, val_list: &[Value], func: &Function, isa: &dyn TargetIsa) {
        let map = Stackmap::from_values(&val_list, func, isa);

        writeln!(&mut self.text, "  - mapped words: {}", map.mapped_words()).unwrap();
        write!(&mut self.text, "  - live: [").unwrap();

        let mut needs_comma_space = false;
        for i in 0..(map.mapped_words() as usize) {
            if map.get_bit(i) {
                if needs_comma_space {
                    write!(&mut self.text, ", ").unwrap();
                }
                needs_comma_space = true;

                write!(&mut self.text, "{}", i).unwrap();
            }
        }

        writeln!(&mut self.text, "]").unwrap();
    }
}
