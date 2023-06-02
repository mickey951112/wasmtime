//! Assembler library implementation for x64.

use crate::{
    isa::reg::Reg,
    masm::{CalleeKind, CmpKind, DivKind, OperandSize, RemKind},
};
use cranelift_codegen::{
    entity::EntityRef,
    ir::TrapCode,
    ir::{ExternalName, Opcode, UserExternalNameRef},
    isa::x64::{
        args::{
            self, AluRmiROpcode, Amode, CmpOpcode, DivSignedness, ExtMode, FromWritableReg, Gpr,
            GprMem, GprMemImm, RegMem, RegMemImm, SyntheticAmode, WritableGpr, CC,
        },
        settings as x64_settings, CallInfo, EmitInfo, EmitState, Inst,
    },
    settings, Final, MachBuffer, MachBufferFinalized, MachInstEmit, MachInstEmitState, Writable,
};

use super::{address::Address, regs};
use smallvec::smallvec;

/// A x64 instruction operand.
#[derive(Debug, Copy, Clone)]
pub(crate) enum Operand {
    /// Register.
    Reg(Reg),
    /// Memory address.
    Mem(Address),
    /// Signed 64-bit immediate.
    Imm(i64),
}

// Conversions between winch-codegen x64 types and cranelift-codegen x64 types.

impl From<Reg> for RegMemImm {
    fn from(reg: Reg) -> Self {
        RegMemImm::reg(reg.into())
    }
}

impl From<Reg> for WritableGpr {
    fn from(reg: Reg) -> Self {
        let writable = Writable::from_reg(reg.into());
        WritableGpr::from_writable_reg(writable).expect("valid writable gpr")
    }
}

impl From<Reg> for Gpr {
    fn from(reg: Reg) -> Self {
        Gpr::new(reg.into()).expect("valid gpr")
    }
}

impl From<Reg> for GprMemImm {
    fn from(reg: Reg) -> Self {
        GprMemImm::new(reg.into()).expect("valid gpr")
    }
}

impl From<OperandSize> for args::OperandSize {
    fn from(size: OperandSize) -> Self {
        match size {
            OperandSize::S32 => Self::Size32,
            OperandSize::S64 => Self::Size64,
        }
    }
}

impl From<DivKind> for DivSignedness {
    fn from(kind: DivKind) -> DivSignedness {
        match kind {
            DivKind::Signed => DivSignedness::Signed,
            DivKind::Unsigned => DivSignedness::Unsigned,
        }
    }
}

impl From<CmpKind> for CC {
    fn from(value: CmpKind) -> Self {
        match value {
            CmpKind::Eq => CC::Z,
            CmpKind::Ne => CC::NZ,
            CmpKind::LtS => CC::L,
            CmpKind::LtU => CC::B,
            CmpKind::GtS => CC::NLE,
            CmpKind::GtU => CC::NBE,
            CmpKind::LeS => CC::LE,
            CmpKind::LeU => CC::BE,
            CmpKind::GeS => CC::NL,
            CmpKind::GeU => CC::NB,
        }
    }
}

/// Low level assembler implementation for x64.
pub(crate) struct Assembler {
    /// The machine instruction buffer.
    buffer: MachBuffer<Inst>,
    /// Constant emission information.
    emit_info: EmitInfo,
    /// Emission state.
    emit_state: EmitState,
}

impl Assembler {
    /// Create a new x64 assembler.
    pub fn new(shared_flags: settings::Flags, isa_flags: x64_settings::Flags) -> Self {
        Self {
            buffer: MachBuffer::<Inst>::new(),
            emit_state: Default::default(),
            emit_info: EmitInfo::new(shared_flags, isa_flags),
        }
    }

    /// Return the emitted code.
    pub fn finalize(mut self) -> MachBufferFinalized<Final> {
        let constants = Default::default();
        let stencil = self
            .buffer
            .finish(&constants, self.emit_state.ctrl_plane_mut());
        stencil.apply_base_srcloc(Default::default())
    }

    fn emit(&mut self, inst: Inst) {
        inst.emit(&[], &mut self.buffer, &self.emit_info, &mut self.emit_state);
    }

    /// Push register.
    pub fn push_r(&mut self, reg: Reg) {
        self.emit(Inst::Push64 { src: reg.into() });
    }

    /// Pop to register.
    pub fn pop_r(&mut self, dst: Reg) {
        let writable = Writable::from_reg(dst.into());
        let dst = WritableGpr::from_writable_reg(writable).expect("valid writable gpr");
        self.emit(Inst::Pop64 { dst });
    }

    /// Return instruction.
    pub fn ret(&mut self) {
        self.emit(Inst::Ret {
            rets: vec![],
            stack_bytes_to_pop: 0,
        });
    }

    /// Move instruction variants.
    pub fn mov(&mut self, src: Operand, dst: Operand, size: OperandSize) {
        use self::Operand::*;

        match &(src, dst) {
            (Reg(lhs), Reg(rhs)) => self.mov_rr(*lhs, *rhs, size),
            (Reg(lhs), Mem(addr)) => match addr {
                Address::Offset { base, offset: imm } => self.mov_rm(*lhs, *base, *imm, size),
            },
            (Imm(imm), Mem(addr)) => match addr {
                Address::Offset { base, offset: disp } => {
                    self.mov_im(*imm as u64, *base, *disp, size)
                }
            },
            (Imm(imm), Reg(reg)) => self.mov_ir(*imm as u64, *reg, size),
            (Mem(addr), Reg(reg)) => match addr {
                Address::Offset { base, offset: imm } => self.mov_mr(*base, *imm, *reg, size),
            },

            _ => Self::handle_invalid_operand_combination(src, dst),
        }
    }

    /// Register-to-register move.
    pub fn mov_rr(&mut self, src: Reg, dst: Reg, size: OperandSize) {
        self.emit(Inst::MovRR {
            src: src.into(),
            dst: dst.into(),
            size: size.into(),
        });
    }

    /// Register-to-memory move.
    pub fn mov_rm(&mut self, src: Reg, base: Reg, disp: u32, size: OperandSize) {
        let dst = Amode::imm_reg(disp, base.into());

        self.emit(Inst::MovRM {
            size: size.into(),
            src: src.into(),
            dst: SyntheticAmode::real(dst),
        });
    }

    /// Immediate-to-memory move.
    pub fn mov_im(&mut self, src: u64, base: Reg, disp: u32, size: OperandSize) {
        let dst = Amode::imm_reg(disp, base.into());
        self.emit(Inst::MovImmM {
            size: size.into(),
            simm64: src,
            dst: SyntheticAmode::real(dst),
        });
    }

    /// Immediate-to-register move.
    pub fn mov_ir(&mut self, imm: u64, dst: Reg, size: OperandSize) {
        let dst = WritableGpr::from_writable_reg(Writable::from_reg(dst.into()))
            .expect("valid writable gpr");

        self.emit(Inst::Imm {
            dst_size: size.into(),
            simm64: imm,
            dst,
        });
    }

    /// Memory-to-register load.
    pub fn mov_mr(&mut self, base: Reg, disp: u32, dst: Reg, size: OperandSize) {
        use OperandSize::S64;

        let amode = Amode::imm_reg(disp, base.into());
        let src = SyntheticAmode::real(amode);

        if size == S64 {
            self.emit(Inst::Mov64MR {
                src,
                dst: dst.into(),
            });
        } else {
            let reg_mem = RegMem::mem(src);
            self.emit(Inst::MovzxRmR {
                ext_mode: ExtMode::LQ,
                src: GprMem::new(reg_mem).expect("valid memory address"),
                dst: dst.into(),
            });
        }
    }

    /// Subtract instruction variants.
    pub fn sub(&mut self, src: Operand, dst: Operand, size: OperandSize) {
        match &(src, dst) {
            (Operand::Imm(imm), Operand::Reg(dst)) => {
                if let Ok(val) = i32::try_from(*imm) {
                    self.sub_ir(val, *dst, size)
                } else {
                    let scratch = regs::scratch();
                    self.load_constant(imm, scratch, size);
                    self.sub_rr(scratch, *dst, size);
                }
            }
            (Operand::Reg(src), Operand::Reg(dst)) => self.sub_rr(*src, *dst, size),
            _ => Self::handle_invalid_operand_combination(src, dst),
        }
    }

    /// Subtract register and register
    pub fn sub_rr(&mut self, src: Reg, dst: Reg, size: OperandSize) {
        self.emit(Inst::AluRmiR {
            size: size.into(),
            op: AluRmiROpcode::Sub,
            src1: dst.into(),
            src2: src.into(),
            dst: dst.into(),
        });
    }

    /// Subtact immediate register.
    pub fn sub_ir(&mut self, imm: i32, dst: Reg, size: OperandSize) {
        let imm = RegMemImm::imm(imm as u32);

        self.emit(Inst::AluRmiR {
            size: size.into(),
            op: AluRmiROpcode::Sub,
            src1: dst.into(),
            src2: GprMemImm::new(imm).expect("valid immediate"),
            dst: dst.into(),
        });
    }

    /// Signed multiplication instruction.
    pub fn mul(&mut self, src: Operand, dst: Operand, size: OperandSize) {
        match &(src, dst) {
            (Operand::Imm(imm), Operand::Reg(dst)) => {
                if let Ok(val) = i32::try_from(*imm) {
                    self.mul_ir(val, *dst, size);
                } else {
                    let scratch = regs::scratch();
                    self.load_constant(imm, scratch, size);
                    self.mul_rr(scratch, *dst, size);
                }
            }
            (Operand::Reg(src), Operand::Reg(dst)) => self.mul_rr(*src, *dst, size),
            _ => Self::handle_invalid_operand_combination(src, dst),
        }
    }

    /// Signed/unsigned division.
    ///
    /// Emits a sequence of instructions to ensure the correctness of
    /// the division invariants.  This function assumes that the
    /// caller has correctly allocated the dividend as `(rdx:rax)` and
    /// accounted for the quotient to be stored in `rax`.
    pub fn div(&mut self, divisor: Reg, dst: (Reg, Reg), kind: DivKind, size: OperandSize) {
        let trap = match kind {
            // Signed division has two trapping conditions, integer overflow and
            // divide-by-zero. Check for divide-by-zero explicitly and let the
            // hardware detect overflow.
            //
            // The dividend is sign extended to initialize `rdx`.
            DivKind::Signed => {
                self.emit(Inst::CmpRmiR {
                    size: size.into(),
                    src: GprMemImm::new(RegMemImm::imm(0)).unwrap(),
                    dst: divisor.into(),
                    opcode: CmpOpcode::Cmp,
                });
                self.emit(Inst::TrapIf {
                    cc: CC::Z,
                    trap_code: TrapCode::IntegerDivisionByZero,
                });
                self.emit(Inst::SignExtendData {
                    size: size.into(),
                    src: dst.0.into(),
                    dst: dst.1.into(),
                });
                TrapCode::IntegerOverflow
            }

            // Unsigned division only traps in one case, on divide-by-zero, so
            // defer that to the trap opcode.
            //
            // The divisor_hi reg is initialized with zero through an
            // xor-against-itself op.
            DivKind::Unsigned => {
                self.emit(Inst::AluRmiR {
                    size: size.into(),
                    op: AluRmiROpcode::Xor,
                    src1: dst.1.into(),
                    src2: dst.1.into(),
                    dst: dst.1.into(),
                });
                TrapCode::IntegerDivisionByZero
            }
        };
        self.emit(Inst::Div {
            sign: kind.into(),
            size: size.into(),
            trap,
            divisor: GprMem::new(RegMem::reg(divisor.into())).unwrap(),
            dividend_lo: dst.0.into(),
            dividend_hi: dst.1.into(),
            dst_quotient: dst.0.into(),
            dst_remainder: dst.1.into(),
        });
    }

    /// Signed/unsigned remainder.
    ///
    /// Emits a sequence of instructions to ensure the correctness of the
    /// division invariants and ultimately calculate the remainder.
    /// This function assumes that the
    /// caller has correctly allocated the dividend as `(rdx:rax)` and
    /// accounted for the remainder to be stored in `rdx`.
    pub fn rem(&mut self, divisor: Reg, dst: (Reg, Reg), kind: RemKind, size: OperandSize) {
        match kind {
            // Signed remainder goes through a pseudo-instruction which has
            // some internal branching. The `dividend_hi`, or `rdx`, is
            // initialized here with a `SignExtendData` instruction.
            RemKind::Signed => {
                self.emit(Inst::SignExtendData {
                    size: size.into(),
                    src: dst.0.into(),
                    dst: dst.1.into(),
                });
                self.emit(Inst::CheckedSRemSeq {
                    size: size.into(),
                    divisor: divisor.into(),
                    dividend_lo: dst.0.into(),
                    dividend_hi: dst.1.into(),
                    dst_quotient: dst.0.into(),
                    dst_remainder: dst.1.into(),
                });
            }

            // Unsigned remainder initializes `dividend_hi` with zero and
            // then executes a normal `div` instruction.
            RemKind::Unsigned => {
                self.emit(Inst::AluRmiR {
                    size: size.into(),
                    op: AluRmiROpcode::Xor,
                    src1: dst.1.into(),
                    src2: dst.1.into(),
                    dst: dst.1.into(),
                });
                self.emit(Inst::Div {
                    sign: DivSignedness::Unsigned,
                    trap: TrapCode::IntegerDivisionByZero,
                    size: size.into(),
                    divisor: GprMem::new(RegMem::reg(divisor.into())).unwrap(),
                    dividend_lo: dst.0.into(),
                    dividend_hi: dst.1.into(),
                    dst_quotient: dst.0.into(),
                    dst_remainder: dst.1.into(),
                });
            }
        }
    }

    /// Multiply immediate and register.
    pub fn mul_ir(&mut self, imm: i32, dst: Reg, size: OperandSize) {
        let imm = RegMemImm::imm(imm as u32);

        self.emit(Inst::AluRmiR {
            size: size.into(),
            op: AluRmiROpcode::Mul,
            src1: dst.into(),
            src2: GprMemImm::new(imm).expect("valid immediate"),
            dst: dst.into(),
        });
    }

    /// Multiply register and register.
    pub fn mul_rr(&mut self, src: Reg, dst: Reg, size: OperandSize) {
        self.emit(Inst::AluRmiR {
            size: size.into(),
            op: AluRmiROpcode::Mul,
            src1: dst.into(),
            src2: src.into(),
            dst: dst.into(),
        });
    }

    /// Add instruction variants.
    pub fn add(&mut self, src: Operand, dst: Operand, size: OperandSize) {
        match &(src, dst) {
            (Operand::Imm(imm), Operand::Reg(dst)) => {
                if let Ok(val) = i32::try_from(*imm) {
                    self.add_ir(val, *dst, size)
                } else {
                    let scratch = regs::scratch();
                    self.load_constant(imm, scratch, size);
                    self.add_rr(scratch, *dst, size);
                }
            }
            (Operand::Reg(src), Operand::Reg(dst)) => self.add_rr(*src, *dst, size),
            _ => Self::handle_invalid_operand_combination(src, dst),
        }
    }

    /// Add immediate and register.
    pub fn add_ir(&mut self, imm: i32, dst: Reg, size: OperandSize) {
        let imm = RegMemImm::imm(imm as u32);

        self.emit(Inst::AluRmiR {
            size: size.into(),
            op: AluRmiROpcode::Add,
            src1: dst.into(),
            src2: GprMemImm::new(imm).expect("valid immediate"),
            dst: dst.into(),
        });
    }

    /// Add register and register.
    pub fn add_rr(&mut self, src: Reg, dst: Reg, size: OperandSize) {
        self.emit(Inst::AluRmiR {
            size: size.into(),
            op: AluRmiROpcode::Add,
            src1: dst.into(),
            src2: src.into(),
            dst: dst.into(),
        });
    }

    /// Logical exclusive or with registers.
    pub fn xor_rr(&mut self, src: Reg, dst: Reg, size: OperandSize) {
        self.emit(Inst::AluRmiR {
            size: size.into(),
            op: AluRmiROpcode::Xor,
            src1: dst.into(),
            src2: src.into(),
            dst: dst.into(),
        });
    }

    /// Compare two operands and set status register flags.
    pub fn cmp(&mut self, src: Operand, dst: Operand, size: OperandSize) {
        match &(src, dst) {
            (Operand::Imm(imm), Operand::Reg(dst)) => {
                if let Ok(val) = i32::try_from(*imm) {
                    self.cmp_ir(val, *dst, size)
                } else {
                    let scratch = regs::scratch();
                    self.load_constant(imm, scratch, size);
                    self.cmp_rr(scratch, *dst, size);
                }
            }
            (Operand::Reg(src), Operand::Reg(dst)) => self.cmp_rr(*src, *dst, size),
            _ => Self::handle_invalid_operand_combination(src, dst),
        }
    }

    fn cmp_ir(&mut self, imm: i32, dst: Reg, size: OperandSize) {
        let imm = RegMemImm::imm(imm as u32);

        self.emit(Inst::CmpRmiR {
            size: size.into(),
            opcode: CmpOpcode::Cmp,
            src: GprMemImm::new(imm).expect("valid immediate"),
            dst: dst.into(),
        });
    }

    fn cmp_rr(&mut self, src: Reg, dst: Reg, size: OperandSize) {
        self.emit(Inst::CmpRmiR {
            size: size.into(),
            opcode: CmpOpcode::Cmp,
            src: src.into(),
            dst: dst.into(),
        });
    }

    /// Set value in dst to `0` or `1` based on flags in status register and
    /// [`CmpKind`].
    pub fn setcc(&mut self, kind: CmpKind, dst: Operand) {
        let dst = match dst {
            Operand::Reg(r) => r,
            _ => panic!("Invalid operand for dst"),
        };
        // Clear the dst register or bits 1 to 31 may be incorrectly set.
        // Don't use xor since it updates the status register.
        self.emit(Inst::Imm {
            dst_size: args::OperandSize::Size32, // Always going to be an i32 result.
            simm64: 0,
            dst: dst.into(),
        });
        // Copy correct bit from status register into dst register.
        self.emit(Inst::Setcc {
            cc: kind.into(),
            dst: dst.into(),
        });
    }

    pub fn call(&mut self, callee: CalleeKind) {
        match callee {
            CalleeKind::Indirect(reg) => {
                self.emit(Inst::CallUnknown {
                    dest: RegMem::reg(reg.into()),
                    info: Box::new(CallInfo {
                        uses: smallvec![],
                        defs: smallvec![],
                        clobbers: Default::default(),
                        opcode: Opcode::Call,
                    }),
                });
            }
            CalleeKind::Direct(index) => {
                let dest = ExternalName::user(UserExternalNameRef::new(index as usize));
                self.emit(Inst::CallKnown {
                    dest,
                    info: Box::new(CallInfo {
                        uses: smallvec![],
                        defs: smallvec![],
                        clobbers: Default::default(),
                        opcode: Opcode::Call,
                    }),
                });
            }
        }
    }

    fn load_constant(&mut self, imm: &i64, dst: Reg, size: OperandSize) {
        self.mov_ir(*imm as u64, dst, size);
    }

    fn handle_invalid_operand_combination(src: Operand, dst: Operand) {
        panic!("Invalid operand combination; src={:?}, dst={:?}", src, dst);
    }
}
