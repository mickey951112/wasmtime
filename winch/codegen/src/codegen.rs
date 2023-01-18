use crate::{
    abi::{ABISig, ABI},
    frame::Frame,
    masm::{MacroAssembler, OperandSize},
    regalloc::RegAlloc,
    stack::Stack,
};
use anyhow::Result;
use wasmparser::{BinaryReader, FuncValidator, ValType, ValidatorResources, VisitOperator};

/// The code generation context.
pub(crate) struct CodeGenContext<'a, M>
where
    M: MacroAssembler,
{
    pub masm: &'a mut M,
    pub stack: Stack,
    pub frame: &'a Frame,
}

impl<'a, M> CodeGenContext<'a, M>
where
    M: MacroAssembler,
{
    pub fn new(masm: &'a mut M, stack: Stack, frame: &'a Frame) -> Self {
        Self { masm, stack, frame }
    }
}

/// The code generation abstraction.
pub(crate) struct CodeGen<'a, M>
where
    M: MacroAssembler,
{
    /// The word size in bytes, extracted from the current ABI.
    word_size: u32,

    /// The ABI-specific representation of the function signature, excluding results.
    sig: ABISig,

    /// The code generation context.
    pub context: CodeGenContext<'a, M>,

    /// The register allocator.
    pub regalloc: RegAlloc,
}

impl<'a, M> CodeGen<'a, M>
where
    M: MacroAssembler,
{
    pub fn new<A: ABI>(context: CodeGenContext<'a, M>, sig: ABISig, regalloc: RegAlloc) -> Self {
        Self {
            word_size: <A as ABI>::word_bytes(),
            sig,
            context,
            regalloc,
        }
    }

    /// Emit the function body to machine code.
    pub fn emit(
        &mut self,
        body: &mut BinaryReader<'a>,
        validator: FuncValidator<ValidatorResources>,
    ) -> Result<()> {
        self.emit_start()
            .and_then(|_| self.emit_body(body, validator))
            .and_then(|_| self.emit_end())?;

        Ok(())
    }

    // TODO stack checks
    fn emit_start(&mut self) -> Result<()> {
        self.context.masm.prologue();
        self.context
            .masm
            .reserve_stack(self.context.frame.locals_size);
        Ok(())
    }

    fn emit_body(
        &mut self,
        body: &mut BinaryReader<'a>,
        mut validator: FuncValidator<ValidatorResources>,
    ) -> Result<()> {
        self.spill_register_arguments();
        let defined_locals_range = &self.context.frame.defined_locals_range;
        self.context.masm.zero_mem_range(
            defined_locals_range.as_range(),
            self.word_size,
            &mut self.regalloc,
        );

        while !body.eof() {
            let offset = body.original_position();
            body.visit_operator(&mut ValidateThenVisit(validator.visitor(offset), self))??;
        }
        validator.finish(body.original_position())?;
        return Ok(());

        struct ValidateThenVisit<'a, T, U>(T, &'a mut U);

        macro_rules! validate_then_visit {
            ($( @$proposal:ident $op:ident $({ $($arg:ident: $argty:ty),* })? => $visit:ident)*) => {
                $(
                    fn $visit(&mut self $($(,$arg: $argty)*)?) -> Self::Output {
                        self.0.$visit($($($arg.clone()),*)?)?;
                        Ok(self.1.$visit($($($arg),*)?))
                    }
                )*
            };
        }

        impl<'a, T, U> VisitOperator<'a> for ValidateThenVisit<'_, T, U>
        where
            T: VisitOperator<'a, Output = wasmparser::Result<()>>,
            U: VisitOperator<'a>,
        {
            type Output = Result<U::Output>;

            wasmparser::for_each_operator!(validate_then_visit);
        }
    }

    // Emit the usual function end instruction sequence.
    pub fn emit_end(&mut self) -> Result<()> {
        self.handle_abi_result();
        self.context.masm.epilogue(self.context.frame.locals_size);
        Ok(())
    }

    fn spill_register_arguments(&mut self) {
        // TODO
        // Revisit this once the implicit VMContext argument is introduced;
        // when that happens the mapping between local slots and abi args
        // is not going to be symmetric.
        self.sig
            .params
            .iter()
            .enumerate()
            .filter(|(_, a)| a.is_reg())
            .for_each(|(index, arg)| {
                let ty = arg.ty();
                let local = self
                    .context
                    .frame
                    .get_local(index as u32)
                    .expect("valid local slot at location");
                let addr = self.context.masm.local_address(local);
                let src = arg
                    .get_reg()
                    .expect("arg should be associated to a register");

                match &ty {
                    ValType::I32 => self.context.masm.store(src.into(), addr, OperandSize::S32),
                    ValType::I64 => self.context.masm.store(src.into(), addr, OperandSize::S64),
                    _ => panic!("Unsupported type {:?}", ty),
                }
            });
    }

    pub fn handle_abi_result(&mut self) {
        if self.sig.result.is_void() {
            return;
        }
        let named_reg = self.sig.result.result_reg();
        let reg = self
            .regalloc
            .pop_to_named_reg(&mut self.context, named_reg, OperandSize::S64);
        self.regalloc.free_gpr(reg);
    }
}
