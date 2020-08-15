//! This module defines aarch64-specific machine instruction types.

// Some variants are not constructed, but we still want them as options in the future.
#![allow(dead_code)]

use crate::binemit::CodeOffset;
use crate::ir::types::{
    B1, B16, B16X8, B32, B32X4, B64, B64X2, B8, B8X16, F32, F32X4, F64, F64X2, FFLAGS, I16, I16X8,
    I32, I32X4, I64, I64X2, I8, I8X16, IFLAGS, R32, R64,
};
use crate::ir::{ExternalName, Opcode, SourceLoc, TrapCode, Type};
use crate::machinst::*;
use crate::{settings, CodegenError, CodegenResult};

use regalloc::{RealRegUniverse, Reg, RegClass, SpillSlot, VirtualReg, Writable};
use regalloc::{RegUsageCollector, RegUsageMapper};

use alloc::boxed::Box;
use alloc::vec::Vec;
use smallvec::{smallvec, SmallVec};
use std::string::{String, ToString};

pub mod regs;
pub use self::regs::*;
pub mod imms;
pub use self::imms::*;
pub mod args;
pub use self::args::*;
pub mod emit;
pub use self::emit::*;

#[cfg(test)]
mod emit_tests;

//=============================================================================
// Instructions (top level): definition

/// An ALU operation. This can be paired with several instruction formats
/// below (see `Inst`) in any combination.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum ALUOp {
    Add32,
    Add64,
    Sub32,
    Sub64,
    Orr32,
    Orr64,
    /// NOR
    OrrNot32,
    /// NOR
    OrrNot64,
    And32,
    And64,
    /// NAND
    AndNot32,
    /// NAND
    AndNot64,
    /// XOR (AArch64 calls this "EOR")
    Eor32,
    /// XOR (AArch64 calls this "EOR")
    Eor64,
    /// XNOR (AArch64 calls this "EOR-NOT")
    EorNot32,
    /// XNOR (AArch64 calls this "EOR-NOT")
    EorNot64,
    /// Add, setting flags
    AddS32,
    /// Add, setting flags
    AddS64,
    /// Sub, setting flags
    SubS32,
    /// Sub, setting flags
    SubS64,
    /// Sub, setting flags, using extended registers
    SubS64XR,
    /// Multiply-add
    MAdd32,
    /// Multiply-add
    MAdd64,
    /// Multiply-sub
    MSub32,
    /// Multiply-sub
    MSub64,
    /// Signed multiply, high-word result
    SMulH,
    /// Unsigned multiply, high-word result
    UMulH,
    SDiv64,
    UDiv64,
    RotR32,
    RotR64,
    Lsr32,
    Lsr64,
    Asr32,
    Asr64,
    Lsl32,
    Lsl64,
}

/// A floating-point unit (FPU) operation with one arg.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum FPUOp1 {
    Abs32,
    Abs64,
    Neg32,
    Neg64,
    Sqrt32,
    Sqrt64,
    Cvt32To64,
    Cvt64To32,
}

/// A floating-point unit (FPU) operation with two args.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum FPUOp2 {
    Add32,
    Add64,
    Sub32,
    Sub64,
    Mul32,
    Mul64,
    Div32,
    Div64,
    Max32,
    Max64,
    Min32,
    Min64,
    /// Signed saturating add
    Sqadd64,
    /// Unsigned saturating add
    Uqadd64,
    /// Signed saturating subtract
    Sqsub64,
    /// Unsigned saturating subtract
    Uqsub64,
}

/// A floating-point unit (FPU) operation with two args, a register and an immediate.
#[derive(Copy, Clone, Debug)]
pub enum FPUOpRI {
    /// Unsigned right shift. Rd = Rn << #imm
    UShr32(FPURightShiftImm),
    /// Unsigned right shift. Rd = Rn << #imm
    UShr64(FPURightShiftImm),
    /// Shift left and insert. Rd |= Rn << #imm
    Sli32(FPULeftShiftImm),
    /// Shift left and insert. Rd |= Rn << #imm
    Sli64(FPULeftShiftImm),
}

/// A floating-point unit (FPU) operation with three args.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum FPUOp3 {
    MAdd32,
    MAdd64,
}

/// A conversion from an FP to an integer value.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum FpuToIntOp {
    F32ToU32,
    F32ToI32,
    F32ToU64,
    F32ToI64,
    F64ToU32,
    F64ToI32,
    F64ToU64,
    F64ToI64,
}

/// A conversion from an integer to an FP value.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum IntToFpuOp {
    U32ToF32,
    I32ToF32,
    U32ToF64,
    I32ToF64,
    U64ToF32,
    I64ToF32,
    U64ToF64,
    I64ToF64,
}

/// Modes for FP rounding ops: round down (floor) or up (ceil), or toward zero (trunc), or to
/// nearest, and for 32- or 64-bit FP values.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum FpuRoundMode {
    Minus32,
    Minus64,
    Plus32,
    Plus64,
    Zero32,
    Zero64,
    Nearest32,
    Nearest64,
}

/// Type of vector element extensions.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum VecExtendOp {
    /// Signed extension of 8-bit elements
    Sxtl8,
    /// Signed extension of 16-bit elements
    Sxtl16,
    /// Signed extension of 32-bit elements
    Sxtl32,
    /// Unsigned extension of 8-bit elements
    Uxtl8,
    /// Unsigned extension of 16-bit elements
    Uxtl16,
    /// Unsigned extension of 32-bit elements
    Uxtl32,
}

/// A vector ALU operation.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum VecALUOp {
    /// Signed saturating add
    Sqadd,
    /// Unsigned saturating add
    Uqadd,
    /// Signed saturating subtract
    Sqsub,
    /// Unsigned saturating subtract
    Uqsub,
    /// Compare bitwise equal
    Cmeq,
    /// Compare signed greater than or equal
    Cmge,
    /// Compare signed greater than
    Cmgt,
    /// Compare unsigned higher
    Cmhs,
    /// Compare unsigned higher or same
    Cmhi,
    /// Floating-point compare equal
    Fcmeq,
    /// Floating-point compare greater than
    Fcmgt,
    /// Floating-point compare greater than or equal
    Fcmge,
    /// Bitwise and
    And,
    /// Bitwise bit clear
    Bic,
    /// Bitwise inclusive or
    Orr,
    /// Bitwise exclusive or
    Eor,
    /// Bitwise select
    Bsl,
    /// Unsigned maximum pairwise
    Umaxp,
    /// Add
    Add,
    /// Subtract
    Sub,
    /// Multiply
    Mul,
    /// Signed shift left
    Sshl,
    /// Unsigned shift left
    Ushl,
    /// Unsigned minimum
    Umin,
    /// Signed minimum
    Smin,
    /// Unsigned maximum
    Umax,
    /// Signed maximum
    Smax,
    /// Unsigned rounding halving add
    Urhadd,
    /// Floating-point add
    Fadd,
    /// Floating-point subtract
    Fsub,
    /// Floating-point divide
    Fdiv,
    /// Floating-point maximum
    Fmax,
    /// Floating-point minimum
    Fmin,
    /// Floating-point multiply
    Fmul,
}

/// A Vector miscellaneous operation with two registers.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum VecMisc2 {
    /// Bitwise NOT
    Not,
    /// Negate
    Neg,
    /// Absolute value
    Abs,
    /// Floating-point absolute value
    Fabs,
    /// Floating-point negate
    Fneg,
    /// Floating-point square root
    Fsqrt,
}

/// An operation across the lanes of vectors.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum VecLanesOp {
    /// Unsigned minimum across a vector
    Uminv,
}

/// An operation on the bits of a register. This can be paired with several instruction formats
/// below (see `Inst`) in any combination.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum BitOp {
    /// Bit reverse
    RBit32,
    /// Bit reverse
    RBit64,
    Clz32,
    Clz64,
    Cls32,
    Cls64,
}

impl BitOp {
    /// What is the opcode's native width?
    pub fn operand_size(&self) -> OperandSize {
        match self {
            BitOp::RBit32 | BitOp::Clz32 | BitOp::Cls32 => OperandSize::Size32,
            _ => OperandSize::Size64,
        }
    }

    /// Get the assembly mnemonic for this opcode.
    pub fn op_str(&self) -> &'static str {
        match self {
            BitOp::RBit32 | BitOp::RBit64 => "rbit",
            BitOp::Clz32 | BitOp::Clz64 => "clz",
            BitOp::Cls32 | BitOp::Cls64 => "cls",
        }
    }
}

impl From<(Opcode, Type)> for BitOp {
    /// Get the BitOp from the IR opcode.
    fn from(op_ty: (Opcode, Type)) -> BitOp {
        match op_ty {
            (Opcode::Bitrev, I32) => BitOp::RBit32,
            (Opcode::Bitrev, I64) => BitOp::RBit64,
            (Opcode::Clz, I32) => BitOp::Clz32,
            (Opcode::Clz, I64) => BitOp::Clz64,
            (Opcode::Cls, I32) => BitOp::Cls32,
            (Opcode::Cls, I64) => BitOp::Cls64,
            _ => unreachable!("Called with non-bit op!: {:?}", op_ty),
        }
    }
}

/// Additional information for (direct) Call instructions, left out of line to lower the size of
/// the Inst enum.
#[derive(Clone, Debug)]
pub struct CallInfo {
    pub dest: ExternalName,
    pub uses: Vec<Reg>,
    pub defs: Vec<Writable<Reg>>,
    pub loc: SourceLoc,
    pub opcode: Opcode,
}

/// Additional information for CallInd instructions, left out of line to lower the size of the Inst
/// enum.
#[derive(Clone, Debug)]
pub struct CallIndInfo {
    pub rn: Reg,
    pub uses: Vec<Reg>,
    pub defs: Vec<Writable<Reg>>,
    pub loc: SourceLoc,
    pub opcode: Opcode,
}

/// Additional information for JTSequence instructions, left out of line to lower the size of the Inst
/// enum.
#[derive(Clone, Debug)]
pub struct JTSequenceInfo {
    pub targets: Vec<BranchTarget>,
    pub default_target: BranchTarget,
    pub targets_for_term: Vec<MachLabel>, // needed for MachTerminator.
}

/// Instruction formats.
#[derive(Clone, Debug)]
pub enum Inst {
    /// A no-op of zero size.
    Nop0,

    /// A no-op that is one instruction large.
    Nop4,

    /// An ALU operation with two register sources and a register destination.
    AluRRR {
        alu_op: ALUOp,
        rd: Writable<Reg>,
        rn: Reg,
        rm: Reg,
    },
    /// An ALU operation with three register sources and a register destination.
    AluRRRR {
        alu_op: ALUOp,
        rd: Writable<Reg>,
        rn: Reg,
        rm: Reg,
        ra: Reg,
    },
    /// An ALU operation with a register source and an immediate-12 source, and a register
    /// destination.
    AluRRImm12 {
        alu_op: ALUOp,
        rd: Writable<Reg>,
        rn: Reg,
        imm12: Imm12,
    },
    /// An ALU operation with a register source and an immediate-logic source, and a register destination.
    AluRRImmLogic {
        alu_op: ALUOp,
        rd: Writable<Reg>,
        rn: Reg,
        imml: ImmLogic,
    },
    /// An ALU operation with a register source and an immediate-shiftamt source, and a register destination.
    AluRRImmShift {
        alu_op: ALUOp,
        rd: Writable<Reg>,
        rn: Reg,
        immshift: ImmShift,
    },
    /// An ALU operation with two register sources, one of which can be shifted, and a register
    /// destination.
    AluRRRShift {
        alu_op: ALUOp,
        rd: Writable<Reg>,
        rn: Reg,
        rm: Reg,
        shiftop: ShiftOpAndAmt,
    },
    /// An ALU operation with two register sources, one of which can be {zero,sign}-extended and
    /// shifted, and a register destination.
    AluRRRExtend {
        alu_op: ALUOp,
        rd: Writable<Reg>,
        rn: Reg,
        rm: Reg,
        extendop: ExtendOp,
    },

    /// A bit op instruction with a single register source.
    BitRR {
        op: BitOp,
        rd: Writable<Reg>,
        rn: Reg,
    },

    /// An unsigned (zero-extending) 8-bit load.
    ULoad8 {
        rd: Writable<Reg>,
        mem: AMode,
        srcloc: Option<SourceLoc>,
    },
    /// A signed (sign-extending) 8-bit load.
    SLoad8 {
        rd: Writable<Reg>,
        mem: AMode,
        srcloc: Option<SourceLoc>,
    },
    /// An unsigned (zero-extending) 16-bit load.
    ULoad16 {
        rd: Writable<Reg>,
        mem: AMode,
        srcloc: Option<SourceLoc>,
    },
    /// A signed (sign-extending) 16-bit load.
    SLoad16 {
        rd: Writable<Reg>,
        mem: AMode,
        srcloc: Option<SourceLoc>,
    },
    /// An unsigned (zero-extending) 32-bit load.
    ULoad32 {
        rd: Writable<Reg>,
        mem: AMode,
        srcloc: Option<SourceLoc>,
    },
    /// A signed (sign-extending) 32-bit load.
    SLoad32 {
        rd: Writable<Reg>,
        mem: AMode,
        srcloc: Option<SourceLoc>,
    },
    /// A 64-bit load.
    ULoad64 {
        rd: Writable<Reg>,
        mem: AMode,
        srcloc: Option<SourceLoc>,
    },

    /// An 8-bit store.
    Store8 {
        rd: Reg,
        mem: AMode,
        srcloc: Option<SourceLoc>,
    },
    /// A 16-bit store.
    Store16 {
        rd: Reg,
        mem: AMode,
        srcloc: Option<SourceLoc>,
    },
    /// A 32-bit store.
    Store32 {
        rd: Reg,
        mem: AMode,
        srcloc: Option<SourceLoc>,
    },
    /// A 64-bit store.
    Store64 {
        rd: Reg,
        mem: AMode,
        srcloc: Option<SourceLoc>,
    },

    /// A store of a pair of registers.
    StoreP64 {
        rt: Reg,
        rt2: Reg,
        mem: PairAMode,
    },
    /// A load of a pair of registers.
    LoadP64 {
        rt: Writable<Reg>,
        rt2: Writable<Reg>,
        mem: PairAMode,
    },

    /// A MOV instruction. These are encoded as ORR's (AluRRR form) but we
    /// keep them separate at the `Inst` level for better pretty-printing
    /// and faster `is_move()` logic.
    Mov {
        rd: Writable<Reg>,
        rm: Reg,
    },

    /// A 32-bit MOV. Zeroes the top 32 bits of the destination. This is
    /// effectively an alias for an unsigned 32-to-64-bit extension.
    Mov32 {
        rd: Writable<Reg>,
        rm: Reg,
    },

    /// A MOVZ with a 16-bit immediate.
    MovZ {
        rd: Writable<Reg>,
        imm: MoveWideConst,
    },

    /// A MOVN with a 16-bit immediate.
    MovN {
        rd: Writable<Reg>,
        imm: MoveWideConst,
    },

    /// A MOVK with a 16-bit immediate.
    MovK {
        rd: Writable<Reg>,
        imm: MoveWideConst,
    },

    /// A sign- or zero-extend operation.
    Extend {
        rd: Writable<Reg>,
        rn: Reg,
        signed: bool,
        from_bits: u8,
        to_bits: u8,
    },

    /// A conditional-select operation.
    CSel {
        rd: Writable<Reg>,
        cond: Cond,
        rn: Reg,
        rm: Reg,
    },

    /// A conditional-set operation.
    CSet {
        rd: Writable<Reg>,
        cond: Cond,
    },

    /// A conditional comparison with an immediate.
    CCmpImm {
        size: OperandSize,
        rn: Reg,
        imm: UImm5,
        nzcv: NZCV,
        cond: Cond,
    },

    /// A synthetic insn, which is a load-linked store-conditional loop, that has the overall
    /// effect of atomically modifying a memory location in a particular way.  Because we have
    /// no way to explain to the regalloc about earlyclobber registers, this instruction has
    /// completely fixed operand registers, and we rely on the RA's coalescing to remove copies
    /// in the surrounding code to the extent it can.  The sequence is both preceded and
    /// followed by a fence which is at least as comprehensive as that of the `Fence`
    /// instruction below.  This instruction is sequentially consistent.  The operand
    /// conventions are:
    ///
    /// x25   (rd) address
    /// x26   (rd) second operand for `op`
    /// x27   (wr) old value
    /// x24   (wr) scratch reg; value afterwards has no meaning
    /// x28   (wr) scratch reg; value afterwards has no meaning
    AtomicRMW {
        ty: Type, // I8, I16, I32 or I64
        op: AtomicRMWOp,
        srcloc: Option<SourceLoc>,
    },

    /// Similar to AtomicRMW, a compare-and-swap operation implemented using a load-linked
    /// store-conditional loop.  (Although we could possibly implement it more directly using
    /// CAS insns that are available in some revisions of AArch64 above 8.0).  The sequence is
    /// both preceded and followed by a fence which is at least as comprehensive as that of the
    /// `Fence` instruction below.  This instruction is sequentially consistent.  Note that the
    /// operand conventions, although very similar to AtomicRMW, are different:
    ///
    /// x25   (rd) address
    /// x26   (rd) expected value
    /// x28   (rd) replacement value
    /// x27   (wr) old value
    /// x24   (wr) scratch reg; value afterwards has no meaning
    AtomicCAS {
        ty: Type, // I8, I16, I32 or I64
        srcloc: Option<SourceLoc>,
    },

    /// Read `ty` bits from address `r_addr`, zero extend the loaded value to 64 bits and put it
    /// in `r_data`.  The load instruction is preceded by a fence at least as comprehensive as
    /// that of the `Fence` instruction below.  This instruction is sequentially consistent.
    AtomicLoad {
        ty: Type, // I8, I16, I32 or I64
        r_data: Writable<Reg>,
        r_addr: Reg,
        srcloc: Option<SourceLoc>,
    },

    /// Write the lowest `ty` bits of `r_data` to address `r_addr`, with a memory fence
    /// instruction following the store.  The fence is at least as comprehensive as that of the
    /// `Fence` instruction below.  This instruction is sequentially consistent.
    AtomicStore {
        ty: Type, // I8, I16, I32 or I64
        r_data: Reg,
        r_addr: Reg,
        srcloc: Option<SourceLoc>,
    },

    /// A memory fence.  This must provide ordering to ensure that, at a minimum, neither loads
    /// nor stores may move forwards or backwards across the fence.  Currently emitted as "dmb
    /// ish".  This instruction is sequentially consistent.
    Fence,

    /// FPU move. Note that this is distinct from a vector-register
    /// move; moving just 64 bits seems to be significantly faster.
    FpuMove64 {
        rd: Writable<Reg>,
        rn: Reg,
    },

    /// Vector register move.
    FpuMove128 {
        rd: Writable<Reg>,
        rn: Reg,
    },

    /// Move to scalar from a vector element.
    FpuMoveFromVec {
        rd: Writable<Reg>,
        rn: Reg,
        idx: u8,
        size: VectorSize,
    },

    /// 1-op FPU instruction.
    FpuRR {
        fpu_op: FPUOp1,
        rd: Writable<Reg>,
        rn: Reg,
    },

    /// 2-op FPU instruction.
    FpuRRR {
        fpu_op: FPUOp2,
        rd: Writable<Reg>,
        rn: Reg,
        rm: Reg,
    },

    FpuRRI {
        fpu_op: FPUOpRI,
        rd: Writable<Reg>,
        rn: Reg,
    },

    /// 3-op FPU instruction.
    FpuRRRR {
        fpu_op: FPUOp3,
        rd: Writable<Reg>,
        rn: Reg,
        rm: Reg,
        ra: Reg,
    },

    /// FPU comparison, single-precision (32 bit).
    FpuCmp32 {
        rn: Reg,
        rm: Reg,
    },

    /// FPU comparison, double-precision (64 bit).
    FpuCmp64 {
        rn: Reg,
        rm: Reg,
    },

    /// Floating-point load, single-precision (32 bit).
    FpuLoad32 {
        rd: Writable<Reg>,
        mem: AMode,
        srcloc: Option<SourceLoc>,
    },
    /// Floating-point store, single-precision (32 bit).
    FpuStore32 {
        rd: Reg,
        mem: AMode,
        srcloc: Option<SourceLoc>,
    },
    /// Floating-point load, double-precision (64 bit).
    FpuLoad64 {
        rd: Writable<Reg>,
        mem: AMode,
        srcloc: Option<SourceLoc>,
    },
    /// Floating-point store, double-precision (64 bit).
    FpuStore64 {
        rd: Reg,
        mem: AMode,
        srcloc: Option<SourceLoc>,
    },
    /// Floating-point/vector load, 128 bit.
    FpuLoad128 {
        rd: Writable<Reg>,
        mem: AMode,
        srcloc: Option<SourceLoc>,
    },
    /// Floating-point/vector store, 128 bit.
    FpuStore128 {
        rd: Reg,
        mem: AMode,
        srcloc: Option<SourceLoc>,
    },

    LoadFpuConst32 {
        rd: Writable<Reg>,
        const_data: f32,
    },

    LoadFpuConst64 {
        rd: Writable<Reg>,
        const_data: f64,
    },

    LoadFpuConst128 {
        rd: Writable<Reg>,
        const_data: u128,
    },

    /// Conversion: FP -> integer.
    FpuToInt {
        op: FpuToIntOp,
        rd: Writable<Reg>,
        rn: Reg,
    },

    /// Conversion: integer -> FP.
    IntToFpu {
        op: IntToFpuOp,
        rd: Writable<Reg>,
        rn: Reg,
    },

    /// FP conditional select, 32 bit.
    FpuCSel32 {
        rd: Writable<Reg>,
        rn: Reg,
        rm: Reg,
        cond: Cond,
    },
    /// FP conditional select, 64 bit.
    FpuCSel64 {
        rd: Writable<Reg>,
        rn: Reg,
        rm: Reg,
        cond: Cond,
    },

    /// Round to integer.
    FpuRound {
        op: FpuRoundMode,
        rd: Writable<Reg>,
        rn: Reg,
    },

    /// Move from a GPR to a scalar FP register.
    MovToFpu {
        rd: Writable<Reg>,
        rn: Reg,
    },

    /// Move to a vector element from a GPR.
    MovToVec {
        rd: Writable<Reg>,
        rn: Reg,
        idx: u8,
        size: VectorSize,
    },

    /// Unsigned move from a vector element to a GPR.
    MovFromVec {
        rd: Writable<Reg>,
        rn: Reg,
        idx: u8,
        size: VectorSize,
    },

    /// Signed move from a vector element to a GPR.
    MovFromVecSigned {
        rd: Writable<Reg>,
        rn: Reg,
        idx: u8,
        size: VectorSize,
        scalar_size: OperandSize,
    },

    /// Duplicate general-purpose register to vector.
    VecDup {
        rd: Writable<Reg>,
        rn: Reg,
        size: VectorSize,
    },

    /// Duplicate scalar to vector.
    VecDupFromFpu {
        rd: Writable<Reg>,
        rn: Reg,
        size: VectorSize,
    },

    /// Vector extend.
    VecExtend {
        t: VecExtendOp,
        rd: Writable<Reg>,
        rn: Reg,
    },

    /// Move vector element to another vector element.
    VecMovElement {
        rd: Writable<Reg>,
        rn: Reg,
        idx1: u8,
        idx2: u8,
        size: VectorSize,
    },

    /// A vector ALU op.
    VecRRR {
        alu_op: VecALUOp,
        rd: Writable<Reg>,
        rn: Reg,
        rm: Reg,
        size: VectorSize,
    },

    /// Vector two register miscellaneous instruction.
    VecMisc {
        op: VecMisc2,
        rd: Writable<Reg>,
        rn: Reg,
        size: VectorSize,
    },

    /// Vector instruction across lanes.
    VecLanes {
        op: VecLanesOp,
        rd: Writable<Reg>,
        rn: Reg,
        size: VectorSize,
    },

    /// Table vector lookup - single register table. The table consists of 8-bit elements and is
    /// stored in `rn`, while `rm` contains 8-bit element indices. `is_extension` specifies whether
    /// to emit a TBX or a TBL instruction, i.e. whether to leave the elements in the destination
    /// vector that correspond to out-of-range indices (greater than 15) unmodified or to set them
    /// to 0.
    VecTbl {
        rd: Writable<Reg>,
        rn: Reg,
        rm: Reg,
        is_extension: bool,
    },

    /// Table vector lookup - two register table. The table consists of 8-bit elements and is
    /// stored in `rn` and `rn2`, while `rm` contains 8-bit element indices. `is_extension`
    /// specifies whether to emit a TBX or a TBL instruction, i.e. whether to leave the elements in
    /// the destination vector that correspond to out-of-range indices (greater than 31) unmodified
    /// or to set them to 0. The table registers `rn` and `rn2` must have consecutive numbers
    /// modulo 32, that is v31 and v0 (in that order) are consecutive registers.
    VecTbl2 {
        rd: Writable<Reg>,
        rn: Reg,
        rn2: Reg,
        rm: Reg,
        is_extension: bool,
    },

    /// Move to the NZCV flags (actually a `MSR NZCV, Xn` insn).
    MovToNZCV {
        rn: Reg,
    },

    /// Move from the NZCV flags (actually a `MRS Xn, NZCV` insn).
    MovFromNZCV {
        rd: Writable<Reg>,
    },

    /// A machine call instruction. N.B.: this allows only a +/- 128MB offset (it uses a relocation
    /// of type `Reloc::Arm64Call`); if the destination distance is not `RelocDistance::Near`, the
    /// code should use a `LoadExtName` / `CallInd` sequence instead, allowing an arbitrary 64-bit
    /// target.
    Call {
        info: Box<CallInfo>,
    },
    /// A machine indirect-call instruction.
    CallInd {
        info: Box<CallIndInfo>,
    },

    // ---- branches (exactly one must appear at end of BB) ----
    /// A machine return instruction.
    Ret,

    /// A placeholder instruction, generating no code, meaning that a function epilogue must be
    /// inserted there.
    EpiloguePlaceholder,

    /// An unconditional branch.
    Jump {
        dest: BranchTarget,
    },

    /// A conditional branch. Contains two targets; at emission time, both are emitted, but
    /// the MachBuffer knows to truncate the trailing branch if fallthrough. We optimize the
    /// choice of taken/not_taken (inverting the branch polarity as needed) based on the
    /// fallthrough at the time of lowering.
    CondBr {
        taken: BranchTarget,
        not_taken: BranchTarget,
        kind: CondBrKind,
    },

    /// A conditional trap: execute a `udf` if the condition is true. This is
    /// one VCode instruction because it uses embedded control flow; it is
    /// logically a single-in, single-out region, but needs to appear as one
    /// unit to the register allocator.
    ///
    /// The `CondBrKind` gives the conditional-branch condition that will
    /// *execute* the embedded `Inst`. (In the emitted code, we use the inverse
    /// of this condition in a branch that skips the trap instruction.)
    TrapIf {
        kind: CondBrKind,
        trap_info: (SourceLoc, TrapCode),
    },

    /// An indirect branch through a register, augmented with set of all
    /// possible successors.
    IndirectBr {
        rn: Reg,
        targets: Vec<MachLabel>,
    },

    /// A "break" instruction, used for e.g. traps and debug breakpoints.
    Brk,

    /// An instruction guaranteed to always be undefined and to trigger an illegal instruction at
    /// runtime.
    Udf {
        trap_info: (SourceLoc, TrapCode),
    },

    /// Compute the address (using a PC-relative offset) of a memory location, using the `ADR`
    /// instruction. Note that we take a simple offset, not a `MemLabel`, here, because `Adr` is
    /// only used for now in fixed lowering sequences with hardcoded offsets. In the future we may
    /// need full `MemLabel` support.
    Adr {
        rd: Writable<Reg>,
        /// Offset in range -2^20 .. 2^20.
        off: i32,
    },

    /// Raw 32-bit word, used for inline constants and jump-table entries.
    Word4 {
        data: u32,
    },

    /// Raw 64-bit word, used for inline constants.
    Word8 {
        data: u64,
    },

    /// Jump-table sequence, as one compound instruction (see note in lower_inst.rs for rationale).
    JTSequence {
        info: Box<JTSequenceInfo>,
        ridx: Reg,
        rtmp1: Writable<Reg>,
        rtmp2: Writable<Reg>,
    },

    /// Load an inline constant.
    LoadConst64 {
        rd: Writable<Reg>,
        const_data: u64,
    },

    /// Load an inline symbol reference.
    LoadExtName {
        rd: Writable<Reg>,
        name: Box<ExternalName>,
        srcloc: SourceLoc,
        offset: i64,
    },

    /// Load address referenced by `mem` into `rd`.
    LoadAddr {
        rd: Writable<Reg>,
        mem: AMode,
    },

    /// Marker, no-op in generated code: SP "virtual offset" is adjusted. This
    /// controls how AMode::NominalSPOffset args are lowered.
    VirtualSPOffsetAdj {
        offset: i64,
    },

    /// Meta-insn, no-op in generated code: emit constant/branch veneer island
    /// at this point (with a guard jump around it) if less than the needed
    /// space is available before the next branch deadline. See the `MachBuffer`
    /// implementation in `machinst/buffer.rs` for the overall algorithm. In
    /// brief, we retain a set of "pending/unresolved label references" from
    /// branches as we scan forward through instructions to emit machine code;
    /// if we notice we're about to go out of range on an unresolved reference,
    /// we stop, emit a bunch of "veneers" (branches in a form that has a longer
    /// range, e.g. a 26-bit-offset unconditional jump), and point the original
    /// label references to those. This is an "island" because it comes in the
    /// middle of the code.
    ///
    /// This meta-instruction is a necessary part of the logic that determines
    /// where to place islands. Ordinarily, we want to place them between basic
    /// blocks, so we compute the worst-case size of each block, and emit the
    /// island before starting a block if we would exceed a deadline before the
    /// end of the block. However, some sequences (such as an inline jumptable)
    /// are variable-length and not accounted for by this logic; so these
    /// lowered sequences include an `EmitIsland` to trigger island generation
    /// where necessary.
    EmitIsland {
        /// The needed space before the next deadline.
        needed_space: CodeOffset,
    },
}

fn count_zero_half_words(mut value: u64) -> usize {
    let mut count = 0;
    for _ in 0..4 {
        if value & 0xffff == 0 {
            count += 1;
        }
        value >>= 16;
    }

    count
}

#[test]
fn inst_size_test() {
    // This test will help with unintentionally growing the size
    // of the Inst enum.
    assert_eq!(32, std::mem::size_of::<Inst>());
}

impl Inst {
    /// Create a move instruction.
    pub fn mov(to_reg: Writable<Reg>, from_reg: Reg) -> Inst {
        assert!(to_reg.to_reg().get_class() == from_reg.get_class());
        if from_reg.get_class() == RegClass::I64 {
            Inst::Mov {
                rd: to_reg,
                rm: from_reg,
            }
        } else if from_reg.get_class() == RegClass::V128 {
            Inst::FpuMove128 {
                rd: to_reg,
                rn: from_reg,
            }
        } else {
            Inst::FpuMove64 {
                rd: to_reg,
                rn: from_reg,
            }
        }
    }

    /// Create a 32-bit move instruction.
    pub fn mov32(to_reg: Writable<Reg>, from_reg: Reg) -> Inst {
        Inst::Mov32 {
            rd: to_reg,
            rm: from_reg,
        }
    }

    /// Create an instruction that loads a constant, using one of serveral options (MOVZ, MOVN,
    /// logical immediate, or constant pool).
    pub fn load_constant(rd: Writable<Reg>, value: u64) -> SmallVec<[Inst; 4]> {
        if let Some(imm) = MoveWideConst::maybe_from_u64(value) {
            // 16-bit immediate (shifted by 0, 16, 32 or 48 bits) in MOVZ
            smallvec![Inst::MovZ { rd, imm }]
        } else if let Some(imm) = MoveWideConst::maybe_from_u64(!value) {
            // 16-bit immediate (shifted by 0, 16, 32 or 48 bits) in MOVN
            smallvec![Inst::MovN { rd, imm }]
        } else if let Some(imml) = ImmLogic::maybe_from_u64(value, I64) {
            // Weird logical-instruction immediate in ORI using zero register
            smallvec![Inst::AluRRImmLogic {
                alu_op: ALUOp::Orr64,
                rd,
                rn: zero_reg(),
                imml,
            }]
        } else {
            let mut insts = smallvec![];

            // If the number of 0xffff half words is greater than the number of 0x0000 half words
            // it is more efficient to use `movn` for the first instruction.
            let first_is_inverted = count_zero_half_words(!value) > count_zero_half_words(value);
            // Either 0xffff or 0x0000 half words can be skipped, depending on the first
            // instruction used.
            let ignored_halfword = if first_is_inverted { 0xffff } else { 0 };
            let mut first_mov_emitted = false;

            for i in 0..4 {
                let imm16 = (value >> (16 * i)) & 0xffff;
                if imm16 != ignored_halfword {
                    if !first_mov_emitted {
                        first_mov_emitted = true;
                        if first_is_inverted {
                            let imm =
                                MoveWideConst::maybe_with_shift(((!imm16) & 0xffff) as u16, i * 16)
                                    .unwrap();
                            insts.push(Inst::MovN { rd, imm });
                        } else {
                            let imm =
                                MoveWideConst::maybe_with_shift(imm16 as u16, i * 16).unwrap();
                            insts.push(Inst::MovZ { rd, imm });
                        }
                    } else {
                        let imm = MoveWideConst::maybe_with_shift(imm16 as u16, i * 16).unwrap();
                        insts.push(Inst::MovK { rd, imm });
                    }
                }
            }

            assert!(first_mov_emitted);

            insts
        }
    }

    /// Create an instruction that loads a 32-bit floating-point constant.
    pub fn load_fp_constant32(rd: Writable<Reg>, value: f32) -> Inst {
        // TODO: use FMOV immediate form when `value` has sufficiently few mantissa/exponent bits.
        Inst::LoadFpuConst32 {
            rd,
            const_data: value,
        }
    }

    /// Create an instruction that loads a 64-bit floating-point constant.
    pub fn load_fp_constant64(rd: Writable<Reg>, value: f64) -> Inst {
        // TODO: use FMOV immediate form when `value` has sufficiently few mantissa/exponent bits.
        Inst::LoadFpuConst64 {
            rd,
            const_data: value,
        }
    }

    /// Create an instruction that loads a 128-bit vector constant.
    pub fn load_fp_constant128(rd: Writable<Reg>, value: u128) -> Inst {
        Inst::LoadFpuConst128 {
            rd,
            const_data: value,
        }
    }

    /// Generic constructor for a load (zero-extending where appropriate).
    pub fn gen_load(into_reg: Writable<Reg>, mem: AMode, ty: Type) -> Inst {
        match ty {
            B1 | B8 | I8 => Inst::ULoad8 {
                rd: into_reg,
                mem,
                srcloc: None,
            },
            B16 | I16 => Inst::ULoad16 {
                rd: into_reg,
                mem,
                srcloc: None,
            },
            B32 | I32 | R32 => Inst::ULoad32 {
                rd: into_reg,
                mem,
                srcloc: None,
            },
            B64 | I64 | R64 => Inst::ULoad64 {
                rd: into_reg,
                mem,
                srcloc: None,
            },
            F32 => Inst::FpuLoad32 {
                rd: into_reg,
                mem,
                srcloc: None,
            },
            F64 => Inst::FpuLoad64 {
                rd: into_reg,
                mem,
                srcloc: None,
            },
            _ => unimplemented!("gen_load({})", ty),
        }
    }

    /// Generic constructor for a store.
    pub fn gen_store(mem: AMode, from_reg: Reg, ty: Type) -> Inst {
        match ty {
            B1 | B8 | I8 => Inst::Store8 {
                rd: from_reg,
                mem,
                srcloc: None,
            },
            B16 | I16 => Inst::Store16 {
                rd: from_reg,
                mem,
                srcloc: None,
            },
            B32 | I32 | R32 => Inst::Store32 {
                rd: from_reg,
                mem,
                srcloc: None,
            },
            B64 | I64 | R64 => Inst::Store64 {
                rd: from_reg,
                mem,
                srcloc: None,
            },
            F32 => Inst::FpuStore32 {
                rd: from_reg,
                mem,
                srcloc: None,
            },
            F64 => Inst::FpuStore64 {
                rd: from_reg,
                mem,
                srcloc: None,
            },
            _ => unimplemented!("gen_store({})", ty),
        }
    }
}

//=============================================================================
// Instructions: get_regs

fn memarg_regs(memarg: &AMode, collector: &mut RegUsageCollector) {
    match memarg {
        &AMode::Unscaled(reg, ..) | &AMode::UnsignedOffset(reg, ..) => {
            collector.add_use(reg);
        }
        &AMode::RegReg(r1, r2, ..)
        | &AMode::RegScaled(r1, r2, ..)
        | &AMode::RegScaledExtended(r1, r2, ..)
        | &AMode::RegExtended(r1, r2, ..) => {
            collector.add_use(r1);
            collector.add_use(r2);
        }
        &AMode::Label(..) => {}
        &AMode::PreIndexed(reg, ..) | &AMode::PostIndexed(reg, ..) => {
            collector.add_mod(reg);
        }
        &AMode::FPOffset(..) => {
            collector.add_use(fp_reg());
        }
        &AMode::SPOffset(..) | &AMode::NominalSPOffset(..) => {
            collector.add_use(stack_reg());
        }
        &AMode::RegOffset(r, ..) => {
            collector.add_use(r);
        }
    }
}

fn pairmemarg_regs(pairmemarg: &PairAMode, collector: &mut RegUsageCollector) {
    match pairmemarg {
        &PairAMode::SignedOffset(reg, ..) => {
            collector.add_use(reg);
        }
        &PairAMode::PreIndexed(reg, ..) | &PairAMode::PostIndexed(reg, ..) => {
            collector.add_mod(reg);
        }
    }
}

fn aarch64_get_regs(inst: &Inst, collector: &mut RegUsageCollector) {
    match inst {
        &Inst::AluRRR { rd, rn, rm, .. } => {
            collector.add_def(rd);
            collector.add_use(rn);
            collector.add_use(rm);
        }
        &Inst::AluRRRR { rd, rn, rm, ra, .. } => {
            collector.add_def(rd);
            collector.add_use(rn);
            collector.add_use(rm);
            collector.add_use(ra);
        }
        &Inst::AluRRImm12 { rd, rn, .. } => {
            collector.add_def(rd);
            collector.add_use(rn);
        }
        &Inst::AluRRImmLogic { rd, rn, .. } => {
            collector.add_def(rd);
            collector.add_use(rn);
        }
        &Inst::AluRRImmShift { rd, rn, .. } => {
            collector.add_def(rd);
            collector.add_use(rn);
        }
        &Inst::AluRRRShift { rd, rn, rm, .. } => {
            collector.add_def(rd);
            collector.add_use(rn);
            collector.add_use(rm);
        }
        &Inst::AluRRRExtend { rd, rn, rm, .. } => {
            collector.add_def(rd);
            collector.add_use(rn);
            collector.add_use(rm);
        }
        &Inst::BitRR { rd, rn, .. } => {
            collector.add_def(rd);
            collector.add_use(rn);
        }
        &Inst::ULoad8 { rd, ref mem, .. }
        | &Inst::SLoad8 { rd, ref mem, .. }
        | &Inst::ULoad16 { rd, ref mem, .. }
        | &Inst::SLoad16 { rd, ref mem, .. }
        | &Inst::ULoad32 { rd, ref mem, .. }
        | &Inst::SLoad32 { rd, ref mem, .. }
        | &Inst::ULoad64 { rd, ref mem, .. } => {
            collector.add_def(rd);
            memarg_regs(mem, collector);
        }
        &Inst::Store8 { rd, ref mem, .. }
        | &Inst::Store16 { rd, ref mem, .. }
        | &Inst::Store32 { rd, ref mem, .. }
        | &Inst::Store64 { rd, ref mem, .. } => {
            collector.add_use(rd);
            memarg_regs(mem, collector);
        }
        &Inst::StoreP64 {
            rt, rt2, ref mem, ..
        } => {
            collector.add_use(rt);
            collector.add_use(rt2);
            pairmemarg_regs(mem, collector);
        }
        &Inst::LoadP64 {
            rt, rt2, ref mem, ..
        } => {
            collector.add_def(rt);
            collector.add_def(rt2);
            pairmemarg_regs(mem, collector);
        }
        &Inst::Mov { rd, rm } => {
            collector.add_def(rd);
            collector.add_use(rm);
        }
        &Inst::Mov32 { rd, rm } => {
            collector.add_def(rd);
            collector.add_use(rm);
        }
        &Inst::MovZ { rd, .. } | &Inst::MovN { rd, .. } => {
            collector.add_def(rd);
        }
        &Inst::MovK { rd, .. } => {
            collector.add_mod(rd);
        }
        &Inst::CSel { rd, rn, rm, .. } => {
            collector.add_def(rd);
            collector.add_use(rn);
            collector.add_use(rm);
        }
        &Inst::CSet { rd, .. } => {
            collector.add_def(rd);
        }
        &Inst::CCmpImm { rn, .. } => {
            collector.add_use(rn);
        }
        &Inst::AtomicRMW { .. } => {
            collector.add_use(xreg(25));
            collector.add_use(xreg(26));
            collector.add_def(writable_xreg(24));
            collector.add_def(writable_xreg(27));
            collector.add_def(writable_xreg(28));
        }
        &Inst::AtomicCAS { .. } => {
            collector.add_use(xreg(25));
            collector.add_use(xreg(26));
            collector.add_use(xreg(28));
            collector.add_def(writable_xreg(24));
            collector.add_def(writable_xreg(27));
        }
        &Inst::AtomicLoad { r_data, r_addr, .. } => {
            collector.add_use(r_addr);
            collector.add_def(r_data);
        }
        &Inst::AtomicStore { r_data, r_addr, .. } => {
            collector.add_use(r_addr);
            collector.add_use(r_data);
        }
        &Inst::Fence {} => {}
        &Inst::FpuMove64 { rd, rn } => {
            collector.add_def(rd);
            collector.add_use(rn);
        }
        &Inst::FpuMove128 { rd, rn } => {
            collector.add_def(rd);
            collector.add_use(rn);
        }
        &Inst::FpuMoveFromVec { rd, rn, .. } => {
            collector.add_def(rd);
            collector.add_use(rn);
        }
        &Inst::FpuRR { rd, rn, .. } => {
            collector.add_def(rd);
            collector.add_use(rn);
        }
        &Inst::FpuRRR { rd, rn, rm, .. } => {
            collector.add_def(rd);
            collector.add_use(rn);
            collector.add_use(rm);
        }
        &Inst::FpuRRI { fpu_op, rd, rn, .. } => {
            match fpu_op {
                FPUOpRI::UShr32(..) | FPUOpRI::UShr64(..) => collector.add_def(rd),
                FPUOpRI::Sli32(..) | FPUOpRI::Sli64(..) => collector.add_mod(rd),
            }
            collector.add_use(rn);
        }
        &Inst::FpuRRRR { rd, rn, rm, ra, .. } => {
            collector.add_def(rd);
            collector.add_use(rn);
            collector.add_use(rm);
            collector.add_use(ra);
        }
        &Inst::VecMisc { rd, rn, .. } => {
            collector.add_def(rd);
            collector.add_use(rn);
        }

        &Inst::VecLanes { rd, rn, .. } => {
            collector.add_def(rd);
            collector.add_use(rn);
        }
        &Inst::VecTbl {
            rd,
            rn,
            rm,
            is_extension,
        } => {
            collector.add_use(rn);
            collector.add_use(rm);

            if is_extension {
                collector.add_mod(rd);
            } else {
                collector.add_def(rd);
            }
        }
        &Inst::VecTbl2 {
            rd,
            rn,
            rn2,
            rm,
            is_extension,
        } => {
            collector.add_use(rn);
            collector.add_use(rn2);
            collector.add_use(rm);

            if is_extension {
                collector.add_mod(rd);
            } else {
                collector.add_def(rd);
            }
        }

        &Inst::FpuCmp32 { rn, rm } | &Inst::FpuCmp64 { rn, rm } => {
            collector.add_use(rn);
            collector.add_use(rm);
        }
        &Inst::FpuLoad32 { rd, ref mem, .. } => {
            collector.add_def(rd);
            memarg_regs(mem, collector);
        }
        &Inst::FpuLoad64 { rd, ref mem, .. } => {
            collector.add_def(rd);
            memarg_regs(mem, collector);
        }
        &Inst::FpuLoad128 { rd, ref mem, .. } => {
            collector.add_def(rd);
            memarg_regs(mem, collector);
        }
        &Inst::FpuStore32 { rd, ref mem, .. } => {
            collector.add_use(rd);
            memarg_regs(mem, collector);
        }
        &Inst::FpuStore64 { rd, ref mem, .. } => {
            collector.add_use(rd);
            memarg_regs(mem, collector);
        }
        &Inst::FpuStore128 { rd, ref mem, .. } => {
            collector.add_use(rd);
            memarg_regs(mem, collector);
        }
        &Inst::LoadFpuConst32 { rd, .. }
        | &Inst::LoadFpuConst64 { rd, .. }
        | &Inst::LoadFpuConst128 { rd, .. } => {
            collector.add_def(rd);
        }
        &Inst::FpuToInt { rd, rn, .. } => {
            collector.add_def(rd);
            collector.add_use(rn);
        }
        &Inst::IntToFpu { rd, rn, .. } => {
            collector.add_def(rd);
            collector.add_use(rn);
        }
        &Inst::FpuCSel32 { rd, rn, rm, .. } | &Inst::FpuCSel64 { rd, rn, rm, .. } => {
            collector.add_def(rd);
            collector.add_use(rn);
            collector.add_use(rm);
        }
        &Inst::FpuRound { rd, rn, .. } => {
            collector.add_def(rd);
            collector.add_use(rn);
        }
        &Inst::MovToFpu { rd, rn } => {
            collector.add_def(rd);
            collector.add_use(rn);
        }
        &Inst::MovToVec { rd, rn, .. } => {
            collector.add_mod(rd);
            collector.add_use(rn);
        }
        &Inst::MovFromVec { rd, rn, .. } | &Inst::MovFromVecSigned { rd, rn, .. } => {
            collector.add_def(rd);
            collector.add_use(rn);
        }
        &Inst::VecDup { rd, rn, .. } => {
            collector.add_def(rd);
            collector.add_use(rn);
        }
        &Inst::VecDupFromFpu { rd, rn, .. } => {
            collector.add_def(rd);
            collector.add_use(rn);
        }
        &Inst::VecExtend { rd, rn, .. } => {
            collector.add_def(rd);
            collector.add_use(rn);
        }
        &Inst::VecMovElement { rd, rn, .. } => {
            collector.add_mod(rd);
            collector.add_use(rn);
        }
        &Inst::VecRRR {
            alu_op, rd, rn, rm, ..
        } => {
            if alu_op == VecALUOp::Bsl {
                collector.add_mod(rd);
            } else {
                collector.add_def(rd);
            }
            collector.add_use(rn);
            collector.add_use(rm);
        }
        &Inst::MovToNZCV { rn } => {
            collector.add_use(rn);
        }
        &Inst::MovFromNZCV { rd } => {
            collector.add_def(rd);
        }
        &Inst::Extend { rd, rn, .. } => {
            collector.add_def(rd);
            collector.add_use(rn);
        }
        &Inst::Jump { .. } | &Inst::Ret | &Inst::EpiloguePlaceholder => {}
        &Inst::Call { ref info, .. } => {
            collector.add_uses(&*info.uses);
            collector.add_defs(&*info.defs);
        }
        &Inst::CallInd { ref info, .. } => {
            collector.add_uses(&*info.uses);
            collector.add_defs(&*info.defs);
            collector.add_use(info.rn);
        }
        &Inst::CondBr { ref kind, .. } => match kind {
            CondBrKind::Zero(rt) | CondBrKind::NotZero(rt) => {
                collector.add_use(*rt);
            }
            CondBrKind::Cond(_) => {}
        },
        &Inst::IndirectBr { rn, .. } => {
            collector.add_use(rn);
        }
        &Inst::Nop0 | Inst::Nop4 => {}
        &Inst::Brk => {}
        &Inst::Udf { .. } => {}
        &Inst::TrapIf { ref kind, .. } => match kind {
            CondBrKind::Zero(rt) | CondBrKind::NotZero(rt) => {
                collector.add_use(*rt);
            }
            CondBrKind::Cond(_) => {}
        },
        &Inst::Adr { rd, .. } => {
            collector.add_def(rd);
        }
        &Inst::Word4 { .. } | &Inst::Word8 { .. } => {}
        &Inst::JTSequence {
            ridx, rtmp1, rtmp2, ..
        } => {
            collector.add_use(ridx);
            collector.add_def(rtmp1);
            collector.add_def(rtmp2);
        }
        &Inst::LoadConst64 { rd, .. } | &Inst::LoadExtName { rd, .. } => {
            collector.add_def(rd);
        }
        &Inst::LoadAddr { rd, mem: _ } => {
            collector.add_def(rd);
        }
        &Inst::VirtualSPOffsetAdj { .. } => {}
        &Inst::EmitIsland { .. } => {}
    }
}

//=============================================================================
// Instructions: map_regs

fn aarch64_map_regs<RUM: RegUsageMapper>(inst: &mut Inst, mapper: &RUM) {
    fn map_use<RUM: RegUsageMapper>(m: &RUM, r: &mut Reg) {
        if r.is_virtual() {
            let new = m.get_use(r.to_virtual_reg()).unwrap().to_reg();
            *r = new;
        }
    }

    fn map_def<RUM: RegUsageMapper>(m: &RUM, r: &mut Writable<Reg>) {
        if r.to_reg().is_virtual() {
            let new = m.get_def(r.to_reg().to_virtual_reg()).unwrap().to_reg();
            *r = Writable::from_reg(new);
        }
    }

    fn map_mod<RUM: RegUsageMapper>(m: &RUM, r: &mut Writable<Reg>) {
        if r.to_reg().is_virtual() {
            let new = m.get_mod(r.to_reg().to_virtual_reg()).unwrap().to_reg();
            *r = Writable::from_reg(new);
        }
    }

    fn map_mem<RUM: RegUsageMapper>(m: &RUM, mem: &mut AMode) {
        // N.B.: we take only the pre-map here, but this is OK because the
        // only addressing modes that update registers (pre/post-increment on
        // AArch64) both read and write registers, so they are "mods" rather
        // than "defs", so must be the same in both the pre- and post-map.
        match mem {
            &mut AMode::Unscaled(ref mut reg, ..) => map_use(m, reg),
            &mut AMode::UnsignedOffset(ref mut reg, ..) => map_use(m, reg),
            &mut AMode::RegReg(ref mut r1, ref mut r2)
            | &mut AMode::RegScaled(ref mut r1, ref mut r2, ..)
            | &mut AMode::RegScaledExtended(ref mut r1, ref mut r2, ..)
            | &mut AMode::RegExtended(ref mut r1, ref mut r2, ..) => {
                map_use(m, r1);
                map_use(m, r2);
            }
            &mut AMode::Label(..) => {}
            &mut AMode::PreIndexed(ref mut r, ..) => map_mod(m, r),
            &mut AMode::PostIndexed(ref mut r, ..) => map_mod(m, r),
            &mut AMode::FPOffset(..)
            | &mut AMode::SPOffset(..)
            | &mut AMode::NominalSPOffset(..) => {}
            &mut AMode::RegOffset(ref mut r, ..) => map_use(m, r),
        };
    }

    fn map_pairmem<RUM: RegUsageMapper>(m: &RUM, mem: &mut PairAMode) {
        match mem {
            &mut PairAMode::SignedOffset(ref mut reg, ..) => map_use(m, reg),
            &mut PairAMode::PreIndexed(ref mut reg, ..) => map_def(m, reg),
            &mut PairAMode::PostIndexed(ref mut reg, ..) => map_def(m, reg),
        }
    }

    fn map_br<RUM: RegUsageMapper>(m: &RUM, br: &mut CondBrKind) {
        match br {
            &mut CondBrKind::Zero(ref mut reg) => map_use(m, reg),
            &mut CondBrKind::NotZero(ref mut reg) => map_use(m, reg),
            &mut CondBrKind::Cond(..) => {}
        };
    }

    match inst {
        &mut Inst::AluRRR {
            ref mut rd,
            ref mut rn,
            ref mut rm,
            ..
        } => {
            map_def(mapper, rd);
            map_use(mapper, rn);
            map_use(mapper, rm);
        }
        &mut Inst::AluRRRR {
            ref mut rd,
            ref mut rn,
            ref mut rm,
            ref mut ra,
            ..
        } => {
            map_def(mapper, rd);
            map_use(mapper, rn);
            map_use(mapper, rm);
            map_use(mapper, ra);
        }
        &mut Inst::AluRRImm12 {
            ref mut rd,
            ref mut rn,
            ..
        } => {
            map_def(mapper, rd);
            map_use(mapper, rn);
        }
        &mut Inst::AluRRImmLogic {
            ref mut rd,
            ref mut rn,
            ..
        } => {
            map_def(mapper, rd);
            map_use(mapper, rn);
        }
        &mut Inst::AluRRImmShift {
            ref mut rd,
            ref mut rn,
            ..
        } => {
            map_def(mapper, rd);
            map_use(mapper, rn);
        }
        &mut Inst::AluRRRShift {
            ref mut rd,
            ref mut rn,
            ref mut rm,
            ..
        } => {
            map_def(mapper, rd);
            map_use(mapper, rn);
            map_use(mapper, rm);
        }
        &mut Inst::AluRRRExtend {
            ref mut rd,
            ref mut rn,
            ref mut rm,
            ..
        } => {
            map_def(mapper, rd);
            map_use(mapper, rn);
            map_use(mapper, rm);
        }
        &mut Inst::BitRR {
            ref mut rd,
            ref mut rn,
            ..
        } => {
            map_def(mapper, rd);
            map_use(mapper, rn);
        }
        &mut Inst::ULoad8 {
            ref mut rd,
            ref mut mem,
            ..
        } => {
            map_def(mapper, rd);
            map_mem(mapper, mem);
        }
        &mut Inst::SLoad8 {
            ref mut rd,
            ref mut mem,
            ..
        } => {
            map_def(mapper, rd);
            map_mem(mapper, mem);
        }
        &mut Inst::ULoad16 {
            ref mut rd,
            ref mut mem,
            ..
        } => {
            map_def(mapper, rd);
            map_mem(mapper, mem);
        }
        &mut Inst::SLoad16 {
            ref mut rd,
            ref mut mem,
            ..
        } => {
            map_def(mapper, rd);
            map_mem(mapper, mem);
        }
        &mut Inst::ULoad32 {
            ref mut rd,
            ref mut mem,
            ..
        } => {
            map_def(mapper, rd);
            map_mem(mapper, mem);
        }
        &mut Inst::SLoad32 {
            ref mut rd,
            ref mut mem,
            ..
        } => {
            map_def(mapper, rd);
            map_mem(mapper, mem);
        }

        &mut Inst::ULoad64 {
            ref mut rd,
            ref mut mem,
            ..
        } => {
            map_def(mapper, rd);
            map_mem(mapper, mem);
        }
        &mut Inst::Store8 {
            ref mut rd,
            ref mut mem,
            ..
        } => {
            map_use(mapper, rd);
            map_mem(mapper, mem);
        }
        &mut Inst::Store16 {
            ref mut rd,
            ref mut mem,
            ..
        } => {
            map_use(mapper, rd);
            map_mem(mapper, mem);
        }
        &mut Inst::Store32 {
            ref mut rd,
            ref mut mem,
            ..
        } => {
            map_use(mapper, rd);
            map_mem(mapper, mem);
        }
        &mut Inst::Store64 {
            ref mut rd,
            ref mut mem,
            ..
        } => {
            map_use(mapper, rd);
            map_mem(mapper, mem);
        }

        &mut Inst::StoreP64 {
            ref mut rt,
            ref mut rt2,
            ref mut mem,
        } => {
            map_use(mapper, rt);
            map_use(mapper, rt2);
            map_pairmem(mapper, mem);
        }
        &mut Inst::LoadP64 {
            ref mut rt,
            ref mut rt2,
            ref mut mem,
        } => {
            map_def(mapper, rt);
            map_def(mapper, rt2);
            map_pairmem(mapper, mem);
        }
        &mut Inst::Mov {
            ref mut rd,
            ref mut rm,
        } => {
            map_def(mapper, rd);
            map_use(mapper, rm);
        }
        &mut Inst::Mov32 {
            ref mut rd,
            ref mut rm,
        } => {
            map_def(mapper, rd);
            map_use(mapper, rm);
        }
        &mut Inst::MovZ { ref mut rd, .. } => {
            map_def(mapper, rd);
        }
        &mut Inst::MovN { ref mut rd, .. } => {
            map_def(mapper, rd);
        }
        &mut Inst::MovK { ref mut rd, .. } => {
            map_def(mapper, rd);
        }
        &mut Inst::CSel {
            ref mut rd,
            ref mut rn,
            ref mut rm,
            ..
        } => {
            map_def(mapper, rd);
            map_use(mapper, rn);
            map_use(mapper, rm);
        }
        &mut Inst::CSet { ref mut rd, .. } => {
            map_def(mapper, rd);
        }
        &mut Inst::CCmpImm { ref mut rn, .. } => {
            map_use(mapper, rn);
        }
        &mut Inst::AtomicRMW { .. } => {
            // There are no vregs to map in this insn.
        }
        &mut Inst::AtomicCAS { .. } => {
            // There are no vregs to map in this insn.
        }
        &mut Inst::AtomicLoad {
            ref mut r_data,
            ref mut r_addr,
            ..
        } => {
            map_def(mapper, r_data);
            map_use(mapper, r_addr);
        }
        &mut Inst::AtomicStore {
            ref mut r_data,
            ref mut r_addr,
            ..
        } => {
            map_use(mapper, r_data);
            map_use(mapper, r_addr);
        }
        &mut Inst::Fence {} => {}
        &mut Inst::FpuMove64 {
            ref mut rd,
            ref mut rn,
        } => {
            map_def(mapper, rd);
            map_use(mapper, rn);
        }
        &mut Inst::FpuMove128 {
            ref mut rd,
            ref mut rn,
        } => {
            map_def(mapper, rd);
            map_use(mapper, rn);
        }
        &mut Inst::FpuMoveFromVec {
            ref mut rd,
            ref mut rn,
            ..
        } => {
            map_def(mapper, rd);
            map_use(mapper, rn);
        }
        &mut Inst::FpuRR {
            ref mut rd,
            ref mut rn,
            ..
        } => {
            map_def(mapper, rd);
            map_use(mapper, rn);
        }
        &mut Inst::FpuRRR {
            ref mut rd,
            ref mut rn,
            ref mut rm,
            ..
        } => {
            map_def(mapper, rd);
            map_use(mapper, rn);
            map_use(mapper, rm);
        }
        &mut Inst::FpuRRI {
            ref mut rd,
            ref mut rn,
            ..
        } => {
            map_def(mapper, rd);
            map_use(mapper, rn);
        }
        &mut Inst::FpuRRRR {
            ref mut rd,
            ref mut rn,
            ref mut rm,
            ref mut ra,
            ..
        } => {
            map_def(mapper, rd);
            map_use(mapper, rn);
            map_use(mapper, rm);
            map_use(mapper, ra);
        }
        &mut Inst::VecMisc {
            ref mut rd,
            ref mut rn,
            ..
        } => {
            map_def(mapper, rd);
            map_use(mapper, rn);
        }
        &mut Inst::VecLanes {
            ref mut rd,
            ref mut rn,
            ..
        } => {
            map_def(mapper, rd);
            map_use(mapper, rn);
        }
        &mut Inst::VecTbl {
            ref mut rd,
            ref mut rn,
            ref mut rm,
            is_extension,
        } => {
            map_use(mapper, rn);
            map_use(mapper, rm);

            if is_extension {
                map_mod(mapper, rd);
            } else {
                map_def(mapper, rd);
            }
        }
        &mut Inst::VecTbl2 {
            ref mut rd,
            ref mut rn,
            ref mut rn2,
            ref mut rm,
            is_extension,
        } => {
            map_use(mapper, rn);
            map_use(mapper, rn2);
            map_use(mapper, rm);

            if is_extension {
                map_mod(mapper, rd);
            } else {
                map_def(mapper, rd);
            }
        }
        &mut Inst::FpuCmp32 {
            ref mut rn,
            ref mut rm,
        } => {
            map_use(mapper, rn);
            map_use(mapper, rm);
        }
        &mut Inst::FpuCmp64 {
            ref mut rn,
            ref mut rm,
        } => {
            map_use(mapper, rn);
            map_use(mapper, rm);
        }
        &mut Inst::FpuLoad32 {
            ref mut rd,
            ref mut mem,
            ..
        } => {
            map_def(mapper, rd);
            map_mem(mapper, mem);
        }
        &mut Inst::FpuLoad64 {
            ref mut rd,
            ref mut mem,
            ..
        } => {
            map_def(mapper, rd);
            map_mem(mapper, mem);
        }
        &mut Inst::FpuLoad128 {
            ref mut rd,
            ref mut mem,
            ..
        } => {
            map_def(mapper, rd);
            map_mem(mapper, mem);
        }
        &mut Inst::FpuStore32 {
            ref mut rd,
            ref mut mem,
            ..
        } => {
            map_use(mapper, rd);
            map_mem(mapper, mem);
        }
        &mut Inst::FpuStore64 {
            ref mut rd,
            ref mut mem,
            ..
        } => {
            map_use(mapper, rd);
            map_mem(mapper, mem);
        }
        &mut Inst::FpuStore128 {
            ref mut rd,
            ref mut mem,
            ..
        } => {
            map_use(mapper, rd);
            map_mem(mapper, mem);
        }
        &mut Inst::LoadFpuConst32 { ref mut rd, .. } => {
            map_def(mapper, rd);
        }
        &mut Inst::LoadFpuConst64 { ref mut rd, .. } => {
            map_def(mapper, rd);
        }
        &mut Inst::LoadFpuConst128 { ref mut rd, .. } => {
            map_def(mapper, rd);
        }
        &mut Inst::FpuToInt {
            ref mut rd,
            ref mut rn,
            ..
        } => {
            map_def(mapper, rd);
            map_use(mapper, rn);
        }
        &mut Inst::IntToFpu {
            ref mut rd,
            ref mut rn,
            ..
        } => {
            map_def(mapper, rd);
            map_use(mapper, rn);
        }
        &mut Inst::FpuCSel32 {
            ref mut rd,
            ref mut rn,
            ref mut rm,
            ..
        } => {
            map_def(mapper, rd);
            map_use(mapper, rn);
            map_use(mapper, rm);
        }
        &mut Inst::FpuCSel64 {
            ref mut rd,
            ref mut rn,
            ref mut rm,
            ..
        } => {
            map_def(mapper, rd);
            map_use(mapper, rn);
            map_use(mapper, rm);
        }
        &mut Inst::FpuRound {
            ref mut rd,
            ref mut rn,
            ..
        } => {
            map_def(mapper, rd);
            map_use(mapper, rn);
        }
        &mut Inst::MovToFpu {
            ref mut rd,
            ref mut rn,
        } => {
            map_def(mapper, rd);
            map_use(mapper, rn);
        }
        &mut Inst::MovToVec {
            ref mut rd,
            ref mut rn,
            ..
        } => {
            map_mod(mapper, rd);
            map_use(mapper, rn);
        }
        &mut Inst::MovFromVec {
            ref mut rd,
            ref mut rn,
            ..
        }
        | &mut Inst::MovFromVecSigned {
            ref mut rd,
            ref mut rn,
            ..
        } => {
            map_def(mapper, rd);
            map_use(mapper, rn);
        }
        &mut Inst::VecDup {
            ref mut rd,
            ref mut rn,
            ..
        } => {
            map_def(mapper, rd);
            map_use(mapper, rn);
        }
        &mut Inst::VecDupFromFpu {
            ref mut rd,
            ref mut rn,
            ..
        } => {
            map_def(mapper, rd);
            map_use(mapper, rn);
        }
        &mut Inst::VecExtend {
            ref mut rd,
            ref mut rn,
            ..
        } => {
            map_def(mapper, rd);
            map_use(mapper, rn);
        }
        &mut Inst::VecMovElement {
            ref mut rd,
            ref mut rn,
            ..
        } => {
            map_mod(mapper, rd);
            map_use(mapper, rn);
        }
        &mut Inst::VecRRR {
            alu_op,
            ref mut rd,
            ref mut rn,
            ref mut rm,
            ..
        } => {
            if alu_op == VecALUOp::Bsl {
                map_mod(mapper, rd);
            } else {
                map_def(mapper, rd);
            }
            map_use(mapper, rn);
            map_use(mapper, rm);
        }
        &mut Inst::MovToNZCV { ref mut rn } => {
            map_use(mapper, rn);
        }
        &mut Inst::MovFromNZCV { ref mut rd } => {
            map_def(mapper, rd);
        }
        &mut Inst::Extend {
            ref mut rd,
            ref mut rn,
            ..
        } => {
            map_def(mapper, rd);
            map_use(mapper, rn);
        }
        &mut Inst::Jump { .. } => {}
        &mut Inst::Call { ref mut info } => {
            for r in info.uses.iter_mut() {
                map_use(mapper, r);
            }
            for r in info.defs.iter_mut() {
                map_def(mapper, r);
            }
        }
        &mut Inst::Ret | &mut Inst::EpiloguePlaceholder => {}
        &mut Inst::CallInd { ref mut info, .. } => {
            for r in info.uses.iter_mut() {
                map_use(mapper, r);
            }
            for r in info.defs.iter_mut() {
                map_def(mapper, r);
            }
            map_use(mapper, &mut info.rn);
        }
        &mut Inst::CondBr { ref mut kind, .. } => {
            map_br(mapper, kind);
        }
        &mut Inst::IndirectBr { ref mut rn, .. } => {
            map_use(mapper, rn);
        }
        &mut Inst::Nop0 | &mut Inst::Nop4 | &mut Inst::Brk | &mut Inst::Udf { .. } => {}
        &mut Inst::TrapIf { ref mut kind, .. } => {
            map_br(mapper, kind);
        }
        &mut Inst::Adr { ref mut rd, .. } => {
            map_def(mapper, rd);
        }
        &mut Inst::Word4 { .. } | &mut Inst::Word8 { .. } => {}
        &mut Inst::JTSequence {
            ref mut ridx,
            ref mut rtmp1,
            ref mut rtmp2,
            ..
        } => {
            map_use(mapper, ridx);
            map_def(mapper, rtmp1);
            map_def(mapper, rtmp2);
        }
        &mut Inst::LoadConst64 { ref mut rd, .. } => {
            map_def(mapper, rd);
        }
        &mut Inst::LoadExtName { ref mut rd, .. } => {
            map_def(mapper, rd);
        }
        &mut Inst::LoadAddr {
            ref mut rd,
            ref mut mem,
        } => {
            map_def(mapper, rd);
            map_mem(mapper, mem);
        }
        &mut Inst::VirtualSPOffsetAdj { .. } => {}
        &mut Inst::EmitIsland { .. } => {}
    }
}

//=============================================================================
// Instructions: misc functions and external interface

impl MachInst for Inst {
    type LabelUse = LabelUse;

    fn get_regs(&self, collector: &mut RegUsageCollector) {
        aarch64_get_regs(self, collector)
    }

    fn map_regs<RUM: RegUsageMapper>(&mut self, mapper: &RUM) {
        aarch64_map_regs(self, mapper);
    }

    fn is_move(&self) -> Option<(Writable<Reg>, Reg)> {
        match self {
            &Inst::Mov { rd, rm } => Some((rd, rm)),
            &Inst::FpuMove64 { rd, rn } => Some((rd, rn)),
            &Inst::FpuMove128 { rd, rn } => Some((rd, rn)),
            _ => None,
        }
    }

    fn is_epilogue_placeholder(&self) -> bool {
        if let Inst::EpiloguePlaceholder = self {
            true
        } else {
            false
        }
    }

    fn is_term<'a>(&'a self) -> MachTerminator<'a> {
        match self {
            &Inst::Ret | &Inst::EpiloguePlaceholder => MachTerminator::Ret,
            &Inst::Jump { dest } => MachTerminator::Uncond(dest.as_label().unwrap()),
            &Inst::CondBr {
                taken, not_taken, ..
            } => MachTerminator::Cond(taken.as_label().unwrap(), not_taken.as_label().unwrap()),
            &Inst::IndirectBr { ref targets, .. } => MachTerminator::Indirect(&targets[..]),
            &Inst::JTSequence { ref info, .. } => {
                MachTerminator::Indirect(&info.targets_for_term[..])
            }
            _ => MachTerminator::None,
        }
    }

    fn gen_move(to_reg: Writable<Reg>, from_reg: Reg, ty: Type) -> Inst {
        assert!(ty.bits() <= 128);
        Inst::mov(to_reg, from_reg)
    }

    fn gen_constant<F: FnMut(RegClass, Type) -> Writable<Reg>>(
        to_reg: Writable<Reg>,
        value: u64,
        ty: Type,
        _alloc_tmp: F,
    ) -> SmallVec<[Inst; 4]> {
        if ty == F64 {
            let mut ret = SmallVec::new();
            ret.push(Inst::load_fp_constant64(to_reg, f64::from_bits(value)));
            ret
        } else if ty == F32 {
            let mut ret = SmallVec::new();
            ret.push(Inst::load_fp_constant32(
                to_reg,
                f32::from_bits(value as u32),
            ));
            ret
        } else {
            // Must be an integer type.
            debug_assert!(
                ty == B1
                    || ty == I8
                    || ty == B8
                    || ty == I16
                    || ty == B16
                    || ty == I32
                    || ty == B32
                    || ty == I64
                    || ty == B64
                    || ty == R32
                    || ty == R64
            );
            Inst::load_constant(to_reg, value)
        }
    }

    fn gen_zero_len_nop() -> Inst {
        Inst::Nop0
    }

    fn gen_nop(preferred_size: usize) -> Inst {
        // We can't give a NOP (or any insn) < 4 bytes.
        assert!(preferred_size >= 4);
        Inst::Nop4
    }

    fn maybe_direct_reload(&self, _reg: VirtualReg, _slot: SpillSlot) -> Option<Inst> {
        None
    }

    fn rc_for_type(ty: Type) -> CodegenResult<RegClass> {
        match ty {
            I8 | I16 | I32 | I64 | B1 | B8 | B16 | B32 | B64 | R32 | R64 => Ok(RegClass::I64),
            F32 | F64 => Ok(RegClass::V128),
            IFLAGS | FFLAGS => Ok(RegClass::I64),
            B8X16 | I8X16 | B16X8 | I16X8 | B32X4 | I32X4 | B64X2 | I64X2 | F32X4 | F64X2 => {
                Ok(RegClass::V128)
            }
            _ => Err(CodegenError::Unsupported(format!(
                "Unexpected SSA-value type: {}",
                ty
            ))),
        }
    }

    fn gen_jump(target: MachLabel) -> Inst {
        Inst::Jump {
            dest: BranchTarget::Label(target),
        }
    }

    fn reg_universe(flags: &settings::Flags) -> RealRegUniverse {
        create_reg_universe(flags)
    }

    fn worst_case_size() -> CodeOffset {
        // The maximum size, in bytes, of any `Inst`'s emitted code. We have at least one case of
        // an 8-instruction sequence (saturating int-to-float conversions) with three embedded
        // 64-bit f64 constants.
        //
        // Note that inline jump-tables handle island/pool insertion separately, so we do not need
        // to account for them here (otherwise the worst case would be 2^31 * 4, clearly not
        // feasible for other reasons).
        44
    }

    fn ref_type_regclass(_: &settings::Flags) -> RegClass {
        RegClass::I64
    }
}

//=============================================================================
// Pretty-printing of instructions.

fn mem_finalize_for_show(
    mem: &AMode,
    mb_rru: Option<&RealRegUniverse>,
    state: &EmitState,
) -> (String, AMode) {
    let (mem_insts, mem) = mem_finalize(0, mem, state);
    let mut mem_str = mem_insts
        .into_iter()
        .map(|inst| inst.show_rru(mb_rru))
        .collect::<Vec<_>>()
        .join(" ; ");
    if !mem_str.is_empty() {
        mem_str += " ; ";
    }

    (mem_str, mem)
}

impl ShowWithRRU for Inst {
    fn show_rru(&self, mb_rru: Option<&RealRegUniverse>) -> String {
        self.pretty_print(mb_rru, &mut EmitState::default())
    }
}

impl Inst {
    fn print_with_state(&self, mb_rru: Option<&RealRegUniverse>, state: &mut EmitState) -> String {
        fn op_name_size(alu_op: ALUOp) -> (&'static str, OperandSize) {
            match alu_op {
                ALUOp::Add32 => ("add", OperandSize::Size32),
                ALUOp::Add64 => ("add", OperandSize::Size64),
                ALUOp::Sub32 => ("sub", OperandSize::Size32),
                ALUOp::Sub64 => ("sub", OperandSize::Size64),
                ALUOp::Orr32 => ("orr", OperandSize::Size32),
                ALUOp::Orr64 => ("orr", OperandSize::Size64),
                ALUOp::And32 => ("and", OperandSize::Size32),
                ALUOp::And64 => ("and", OperandSize::Size64),
                ALUOp::Eor32 => ("eor", OperandSize::Size32),
                ALUOp::Eor64 => ("eor", OperandSize::Size64),
                ALUOp::AddS32 => ("adds", OperandSize::Size32),
                ALUOp::AddS64 => ("adds", OperandSize::Size64),
                ALUOp::SubS32 => ("subs", OperandSize::Size32),
                ALUOp::SubS64 => ("subs", OperandSize::Size64),
                ALUOp::SubS64XR => ("subs", OperandSize::Size64),
                ALUOp::MAdd32 => ("madd", OperandSize::Size32),
                ALUOp::MAdd64 => ("madd", OperandSize::Size64),
                ALUOp::MSub32 => ("msub", OperandSize::Size32),
                ALUOp::MSub64 => ("msub", OperandSize::Size64),
                ALUOp::SMulH => ("smulh", OperandSize::Size64),
                ALUOp::UMulH => ("umulh", OperandSize::Size64),
                ALUOp::SDiv64 => ("sdiv", OperandSize::Size64),
                ALUOp::UDiv64 => ("udiv", OperandSize::Size64),
                ALUOp::AndNot32 => ("bic", OperandSize::Size32),
                ALUOp::AndNot64 => ("bic", OperandSize::Size64),
                ALUOp::OrrNot32 => ("orn", OperandSize::Size32),
                ALUOp::OrrNot64 => ("orn", OperandSize::Size64),
                ALUOp::EorNot32 => ("eon", OperandSize::Size32),
                ALUOp::EorNot64 => ("eon", OperandSize::Size64),
                ALUOp::RotR32 => ("ror", OperandSize::Size32),
                ALUOp::RotR64 => ("ror", OperandSize::Size64),
                ALUOp::Lsr32 => ("lsr", OperandSize::Size32),
                ALUOp::Lsr64 => ("lsr", OperandSize::Size64),
                ALUOp::Asr32 => ("asr", OperandSize::Size32),
                ALUOp::Asr64 => ("asr", OperandSize::Size64),
                ALUOp::Lsl32 => ("lsl", OperandSize::Size32),
                ALUOp::Lsl64 => ("lsl", OperandSize::Size64),
            }
        }

        match self {
            &Inst::Nop0 => "nop-zero-len".to_string(),
            &Inst::Nop4 => "nop".to_string(),
            &Inst::AluRRR { alu_op, rd, rn, rm } => {
                let (op, size) = op_name_size(alu_op);
                let rd = show_ireg_sized(rd.to_reg(), mb_rru, size);
                let rn = show_ireg_sized(rn, mb_rru, size);
                let rm = show_ireg_sized(rm, mb_rru, size);
                format!("{} {}, {}, {}", op, rd, rn, rm)
            }
            &Inst::AluRRRR {
                alu_op,
                rd,
                rn,
                rm,
                ra,
            } => {
                let (op, size) = op_name_size(alu_op);
                let four_args = alu_op != ALUOp::SMulH && alu_op != ALUOp::UMulH;
                let rd = show_ireg_sized(rd.to_reg(), mb_rru, size);
                let rn = show_ireg_sized(rn, mb_rru, size);
                let rm = show_ireg_sized(rm, mb_rru, size);
                let ra = show_ireg_sized(ra, mb_rru, size);
                if four_args {
                    format!("{} {}, {}, {}, {}", op, rd, rn, rm, ra)
                } else {
                    // smulh and umulh have Ra "hard-wired" to the zero register
                    // and the canonical assembly form has only three regs.
                    format!("{} {}, {}, {}", op, rd, rn, rm)
                }
            }
            &Inst::AluRRImm12 {
                alu_op,
                rd,
                rn,
                ref imm12,
            } => {
                let (op, size) = op_name_size(alu_op);
                let rd = show_ireg_sized(rd.to_reg(), mb_rru, size);
                let rn = show_ireg_sized(rn, mb_rru, size);

                if imm12.bits == 0 && alu_op == ALUOp::Add64 {
                    // special-case MOV (used for moving into SP).
                    format!("mov {}, {}", rd, rn)
                } else {
                    let imm12 = imm12.show_rru(mb_rru);
                    format!("{} {}, {}, {}", op, rd, rn, imm12)
                }
            }
            &Inst::AluRRImmLogic {
                alu_op,
                rd,
                rn,
                ref imml,
            } => {
                let (op, size) = op_name_size(alu_op);
                let rd = show_ireg_sized(rd.to_reg(), mb_rru, size);
                let rn = show_ireg_sized(rn, mb_rru, size);
                let imml = imml.show_rru(mb_rru);
                format!("{} {}, {}, {}", op, rd, rn, imml)
            }
            &Inst::AluRRImmShift {
                alu_op,
                rd,
                rn,
                ref immshift,
            } => {
                let (op, size) = op_name_size(alu_op);
                let rd = show_ireg_sized(rd.to_reg(), mb_rru, size);
                let rn = show_ireg_sized(rn, mb_rru, size);
                let immshift = immshift.show_rru(mb_rru);
                format!("{} {}, {}, {}", op, rd, rn, immshift)
            }
            &Inst::AluRRRShift {
                alu_op,
                rd,
                rn,
                rm,
                ref shiftop,
            } => {
                let (op, size) = op_name_size(alu_op);
                let rd = show_ireg_sized(rd.to_reg(), mb_rru, size);
                let rn = show_ireg_sized(rn, mb_rru, size);
                let rm = show_ireg_sized(rm, mb_rru, size);
                let shiftop = shiftop.show_rru(mb_rru);
                format!("{} {}, {}, {}, {}", op, rd, rn, rm, shiftop)
            }
            &Inst::AluRRRExtend {
                alu_op,
                rd,
                rn,
                rm,
                ref extendop,
            } => {
                let (op, size) = op_name_size(alu_op);
                let rd = show_ireg_sized(rd.to_reg(), mb_rru, size);
                let rn = show_ireg_sized(rn, mb_rru, size);
                let rm = show_ireg_sized(rm, mb_rru, size);
                let extendop = extendop.show_rru(mb_rru);
                format!("{} {}, {}, {}, {}", op, rd, rn, rm, extendop)
            }
            &Inst::BitRR { op, rd, rn } => {
                let size = op.operand_size();
                let op = op.op_str();
                let rd = show_ireg_sized(rd.to_reg(), mb_rru, size);
                let rn = show_ireg_sized(rn, mb_rru, size);
                format!("{} {}, {}", op, rd, rn)
            }
            &Inst::ULoad8 {
                rd,
                ref mem,
                srcloc: _srcloc,
            }
            | &Inst::SLoad8 {
                rd,
                ref mem,
                srcloc: _srcloc,
            }
            | &Inst::ULoad16 {
                rd,
                ref mem,
                srcloc: _srcloc,
            }
            | &Inst::SLoad16 {
                rd,
                ref mem,
                srcloc: _srcloc,
            }
            | &Inst::ULoad32 {
                rd,
                ref mem,
                srcloc: _srcloc,
            }
            | &Inst::SLoad32 {
                rd,
                ref mem,
                srcloc: _srcloc,
            }
            | &Inst::ULoad64 {
                rd,
                ref mem,
                srcloc: _srcloc,
                ..
            } => {
                let (mem_str, mem) = mem_finalize_for_show(mem, mb_rru, state);

                let is_unscaled = match &mem {
                    &AMode::Unscaled(..) => true,
                    _ => false,
                };
                let (op, size) = match (self, is_unscaled) {
                    (&Inst::ULoad8 { .. }, false) => ("ldrb", OperandSize::Size32),
                    (&Inst::ULoad8 { .. }, true) => ("ldurb", OperandSize::Size32),
                    (&Inst::SLoad8 { .. }, false) => ("ldrsb", OperandSize::Size64),
                    (&Inst::SLoad8 { .. }, true) => ("ldursb", OperandSize::Size64),
                    (&Inst::ULoad16 { .. }, false) => ("ldrh", OperandSize::Size32),
                    (&Inst::ULoad16 { .. }, true) => ("ldurh", OperandSize::Size32),
                    (&Inst::SLoad16 { .. }, false) => ("ldrsh", OperandSize::Size64),
                    (&Inst::SLoad16 { .. }, true) => ("ldursh", OperandSize::Size64),
                    (&Inst::ULoad32 { .. }, false) => ("ldr", OperandSize::Size32),
                    (&Inst::ULoad32 { .. }, true) => ("ldur", OperandSize::Size32),
                    (&Inst::SLoad32 { .. }, false) => ("ldrsw", OperandSize::Size64),
                    (&Inst::SLoad32 { .. }, true) => ("ldursw", OperandSize::Size64),
                    (&Inst::ULoad64 { .. }, false) => ("ldr", OperandSize::Size64),
                    (&Inst::ULoad64 { .. }, true) => ("ldur", OperandSize::Size64),
                    _ => unreachable!(),
                };
                let rd = show_ireg_sized(rd.to_reg(), mb_rru, size);
                let mem = mem.show_rru(mb_rru);
                format!("{}{} {}, {}", mem_str, op, rd, mem)
            }
            &Inst::Store8 {
                rd,
                ref mem,
                srcloc: _srcloc,
            }
            | &Inst::Store16 {
                rd,
                ref mem,
                srcloc: _srcloc,
            }
            | &Inst::Store32 {
                rd,
                ref mem,
                srcloc: _srcloc,
            }
            | &Inst::Store64 {
                rd,
                ref mem,
                srcloc: _srcloc,
                ..
            } => {
                let (mem_str, mem) = mem_finalize_for_show(mem, mb_rru, state);

                let is_unscaled = match &mem {
                    &AMode::Unscaled(..) => true,
                    _ => false,
                };
                let (op, size) = match (self, is_unscaled) {
                    (&Inst::Store8 { .. }, false) => ("strb", OperandSize::Size32),
                    (&Inst::Store8 { .. }, true) => ("sturb", OperandSize::Size32),
                    (&Inst::Store16 { .. }, false) => ("strh", OperandSize::Size32),
                    (&Inst::Store16 { .. }, true) => ("sturh", OperandSize::Size32),
                    (&Inst::Store32 { .. }, false) => ("str", OperandSize::Size32),
                    (&Inst::Store32 { .. }, true) => ("stur", OperandSize::Size32),
                    (&Inst::Store64 { .. }, false) => ("str", OperandSize::Size64),
                    (&Inst::Store64 { .. }, true) => ("stur", OperandSize::Size64),
                    _ => unreachable!(),
                };
                let rd = show_ireg_sized(rd, mb_rru, size);
                let mem = mem.show_rru(mb_rru);
                format!("{}{} {}, {}", mem_str, op, rd, mem)
            }
            &Inst::StoreP64 { rt, rt2, ref mem } => {
                let rt = rt.show_rru(mb_rru);
                let rt2 = rt2.show_rru(mb_rru);
                let mem = mem.show_rru_sized(mb_rru, /* size = */ 8);
                format!("stp {}, {}, {}", rt, rt2, mem)
            }
            &Inst::LoadP64 { rt, rt2, ref mem } => {
                let rt = rt.to_reg().show_rru(mb_rru);
                let rt2 = rt2.to_reg().show_rru(mb_rru);
                let mem = mem.show_rru_sized(mb_rru, /* size = */ 8);
                format!("ldp {}, {}, {}", rt, rt2, mem)
            }
            &Inst::Mov { rd, rm } => {
                let rd = rd.to_reg().show_rru(mb_rru);
                let rm = rm.show_rru(mb_rru);
                format!("mov {}, {}", rd, rm)
            }
            &Inst::Mov32 { rd, rm } => {
                let rd = show_ireg_sized(rd.to_reg(), mb_rru, OperandSize::Size32);
                let rm = show_ireg_sized(rm, mb_rru, OperandSize::Size32);
                format!("mov {}, {}", rd, rm)
            }
            &Inst::MovZ { rd, ref imm } => {
                let rd = rd.to_reg().show_rru(mb_rru);
                let imm = imm.show_rru(mb_rru);
                format!("movz {}, {}", rd, imm)
            }
            &Inst::MovN { rd, ref imm } => {
                let rd = rd.to_reg().show_rru(mb_rru);
                let imm = imm.show_rru(mb_rru);
                format!("movn {}, {}", rd, imm)
            }
            &Inst::MovK { rd, ref imm } => {
                let rd = rd.to_reg().show_rru(mb_rru);
                let imm = imm.show_rru(mb_rru);
                format!("movk {}, {}", rd, imm)
            }
            &Inst::CSel { rd, rn, rm, cond } => {
                let rd = rd.to_reg().show_rru(mb_rru);
                let rn = rn.show_rru(mb_rru);
                let rm = rm.show_rru(mb_rru);
                let cond = cond.show_rru(mb_rru);
                format!("csel {}, {}, {}, {}", rd, rn, rm, cond)
            }
            &Inst::CSet { rd, cond } => {
                let rd = rd.to_reg().show_rru(mb_rru);
                let cond = cond.show_rru(mb_rru);
                format!("cset {}, {}", rd, cond)
            }
            &Inst::CCmpImm {
                size,
                rn,
                imm,
                nzcv,
                cond,
            } => {
                let rn = show_ireg_sized(rn, mb_rru, size);
                let imm = imm.show_rru(mb_rru);
                let nzcv = nzcv.show_rru(mb_rru);
                let cond = cond.show_rru(mb_rru);
                format!("ccmp {}, {}, {}, {}", rn, imm, nzcv, cond)
            }
            &Inst::AtomicRMW { ty, op, .. } => {
                format!(
                    "atomically {{ {}_bits_at_[x25]) {:?}= x26 ; x27 = old_value_at_[x25]; x24,x28 = trash }}",
                    ty.bits(), op)
            }
            &Inst::AtomicCAS { ty, .. } => {
                format!(
                    "atomically {{ compare-and-swap({}_bits_at_[x25], x26 -> x28), x27 = old_value_at_[x25]; x24 = trash }}",
                    ty.bits())
            }
            &Inst::AtomicLoad { ty, r_data, r_addr, .. } => {
                format!(
                    "atomically {{ {} = zero_extend_{}_bits_at[{}] }}",
                    r_data.show_rru(mb_rru), ty.bits(), r_addr.show_rru(mb_rru))
            }
            &Inst::AtomicStore { ty, r_data, r_addr, .. } => {
                format!(
                    "atomically {{ {}_bits_at[{}] = {} }}", ty.bits(), r_addr.show_rru(mb_rru), r_data.show_rru(mb_rru))
            }
            &Inst::Fence {} => {
                format!("dmb ish")
            }
            &Inst::FpuMove64 { rd, rn } => {
                let rd = rd.to_reg().show_rru(mb_rru);
                let rn = rn.show_rru(mb_rru);
                format!("mov {}.8b, {}.8b", rd, rn)
            }
            &Inst::FpuMove128 { rd, rn } => {
                let rd = rd.to_reg().show_rru(mb_rru);
                let rn = rn.show_rru(mb_rru);
                format!("mov {}.16b, {}.16b", rd, rn)
            }
            &Inst::FpuMoveFromVec { rd, rn, idx, size } => {
                let rd = show_vreg_scalar(rd.to_reg(), mb_rru, size.lane_size());
                let rn = show_vreg_element(rn, mb_rru, idx, size);
                format!("mov {}, {}", rd, rn)
            }
            &Inst::FpuRR { fpu_op, rd, rn } => {
                let (op, sizesrc, sizedest) = match fpu_op {
                    FPUOp1::Abs32 => ("fabs", ScalarSize::Size32, ScalarSize::Size32),
                    FPUOp1::Abs64 => ("fabs", ScalarSize::Size64, ScalarSize::Size64),
                    FPUOp1::Neg32 => ("fneg", ScalarSize::Size32, ScalarSize::Size32),
                    FPUOp1::Neg64 => ("fneg", ScalarSize::Size64, ScalarSize::Size64),
                    FPUOp1::Sqrt32 => ("fsqrt", ScalarSize::Size32, ScalarSize::Size32),
                    FPUOp1::Sqrt64 => ("fsqrt", ScalarSize::Size64, ScalarSize::Size64),
                    FPUOp1::Cvt32To64 => ("fcvt", ScalarSize::Size32, ScalarSize::Size64),
                    FPUOp1::Cvt64To32 => ("fcvt", ScalarSize::Size64, ScalarSize::Size32),
                };
                let rd = show_vreg_scalar(rd.to_reg(), mb_rru, sizedest);
                let rn = show_vreg_scalar(rn, mb_rru, sizesrc);
                format!("{} {}, {}", op, rd, rn)
            }
            &Inst::FpuRRR { fpu_op, rd, rn, rm } => {
                let (op, size) = match fpu_op {
                    FPUOp2::Add32 => ("fadd", ScalarSize::Size32),
                    FPUOp2::Add64 => ("fadd", ScalarSize::Size64),
                    FPUOp2::Sub32 => ("fsub", ScalarSize::Size32),
                    FPUOp2::Sub64 => ("fsub", ScalarSize::Size64),
                    FPUOp2::Mul32 => ("fmul", ScalarSize::Size32),
                    FPUOp2::Mul64 => ("fmul", ScalarSize::Size64),
                    FPUOp2::Div32 => ("fdiv", ScalarSize::Size32),
                    FPUOp2::Div64 => ("fdiv", ScalarSize::Size64),
                    FPUOp2::Max32 => ("fmax", ScalarSize::Size32),
                    FPUOp2::Max64 => ("fmax", ScalarSize::Size64),
                    FPUOp2::Min32 => ("fmin", ScalarSize::Size32),
                    FPUOp2::Min64 => ("fmin", ScalarSize::Size64),
                    FPUOp2::Sqadd64 => ("sqadd", ScalarSize::Size64),
                    FPUOp2::Uqadd64 => ("uqadd", ScalarSize::Size64),
                    FPUOp2::Sqsub64 => ("sqsub", ScalarSize::Size64),
                    FPUOp2::Uqsub64 => ("uqsub", ScalarSize::Size64),
                };
                let rd = show_vreg_scalar(rd.to_reg(), mb_rru, size);
                let rn = show_vreg_scalar(rn, mb_rru, size);
                let rm = show_vreg_scalar(rm, mb_rru, size);
                format!("{} {}, {}, {}", op, rd, rn, rm)
            }
            &Inst::FpuRRI { fpu_op, rd, rn } => {
                let (op, imm, vector) = match fpu_op {
                    FPUOpRI::UShr32(imm) => ("ushr", imm.show_rru(mb_rru), true),
                    FPUOpRI::UShr64(imm) => ("ushr", imm.show_rru(mb_rru), false),
                    FPUOpRI::Sli32(imm) => ("sli", imm.show_rru(mb_rru), true),
                    FPUOpRI::Sli64(imm) => ("sli", imm.show_rru(mb_rru), false),
                };

                let show_vreg_fn: fn(Reg, Option<&RealRegUniverse>) -> String = if vector {
                    |reg, mb_rru| show_vreg_vector(reg, mb_rru, VectorSize::Size32x2)
                } else {
                    |reg, mb_rru| show_vreg_scalar(reg, mb_rru, ScalarSize::Size64)
                };
                let rd = show_vreg_fn(rd.to_reg(), mb_rru);
                let rn = show_vreg_fn(rn, mb_rru);
                format!("{} {}, {}, {}", op, rd, rn, imm)
            }
            &Inst::FpuRRRR {
                fpu_op,
                rd,
                rn,
                rm,
                ra,
            } => {
                let (op, size) = match fpu_op {
                    FPUOp3::MAdd32 => ("fmadd", ScalarSize::Size32),
                    FPUOp3::MAdd64 => ("fmadd", ScalarSize::Size64),
                };
                let rd = show_vreg_scalar(rd.to_reg(), mb_rru, size);
                let rn = show_vreg_scalar(rn, mb_rru, size);
                let rm = show_vreg_scalar(rm, mb_rru, size);
                let ra = show_vreg_scalar(ra, mb_rru, size);
                format!("{} {}, {}, {}, {}", op, rd, rn, rm, ra)
            }
            &Inst::FpuCmp32 { rn, rm } => {
                let rn = show_vreg_scalar(rn, mb_rru, ScalarSize::Size32);
                let rm = show_vreg_scalar(rm, mb_rru, ScalarSize::Size32);
                format!("fcmp {}, {}", rn, rm)
            }
            &Inst::FpuCmp64 { rn, rm } => {
                let rn = show_vreg_scalar(rn, mb_rru, ScalarSize::Size64);
                let rm = show_vreg_scalar(rm, mb_rru, ScalarSize::Size64);
                format!("fcmp {}, {}", rn, rm)
            }
            &Inst::FpuLoad32 { rd, ref mem, .. } => {
                let rd = show_vreg_scalar(rd.to_reg(), mb_rru, ScalarSize::Size32);
                let (mem_str, mem) = mem_finalize_for_show(mem, mb_rru, state);
                let mem = mem.show_rru(mb_rru);
                format!("{}ldr {}, {}", mem_str, rd, mem)
            }
            &Inst::FpuLoad64 { rd, ref mem, .. } => {
                let rd = show_vreg_scalar(rd.to_reg(), mb_rru, ScalarSize::Size64);
                let (mem_str, mem) = mem_finalize_for_show(mem, mb_rru, state);
                let mem = mem.show_rru(mb_rru);
                format!("{}ldr {}, {}", mem_str, rd, mem)
            }
            &Inst::FpuLoad128 { rd, ref mem, .. } => {
                let rd = rd.to_reg().show_rru(mb_rru);
                let rd = "q".to_string() + &rd[1..];
                let (mem_str, mem) = mem_finalize_for_show(mem, mb_rru, state);
                let mem = mem.show_rru(mb_rru);
                format!("{}ldr {}, {}", mem_str, rd, mem)
            }
            &Inst::FpuStore32 { rd, ref mem, .. } => {
                let rd = show_vreg_scalar(rd, mb_rru, ScalarSize::Size32);
                let (mem_str, mem) = mem_finalize_for_show(mem, mb_rru, state);
                let mem = mem.show_rru(mb_rru);
                format!("{}str {}, {}", mem_str, rd, mem)
            }
            &Inst::FpuStore64 { rd, ref mem, .. } => {
                let rd = show_vreg_scalar(rd, mb_rru, ScalarSize::Size64);
                let (mem_str, mem) = mem_finalize_for_show(mem, mb_rru, state);
                let mem = mem.show_rru(mb_rru);
                format!("{}str {}, {}", mem_str, rd, mem)
            }
            &Inst::FpuStore128 { rd, ref mem, .. } => {
                let rd = rd.show_rru(mb_rru);
                let rd = "q".to_string() + &rd[1..];
                let (mem_str, mem) = mem_finalize_for_show(mem, mb_rru, state);
                let mem = mem.show_rru(mb_rru);
                format!("{}str {}, {}", mem_str, rd, mem)
            }
            &Inst::LoadFpuConst32 { rd, const_data } => {
                let rd = show_vreg_scalar(rd.to_reg(), mb_rru, ScalarSize::Size32);
                format!("ldr {}, pc+8 ; b 8 ; data.f32 {}", rd, const_data)
            }
            &Inst::LoadFpuConst64 { rd, const_data } => {
                let rd = show_vreg_scalar(rd.to_reg(), mb_rru, ScalarSize::Size64);
                format!("ldr {}, pc+8 ; b 12 ; data.f64 {}", rd, const_data)
            }
            &Inst::LoadFpuConst128 { rd, const_data } => {
                let rd = show_vreg_scalar(rd.to_reg(), mb_rru, ScalarSize::Size128);
                format!("ldr {}, pc+8 ; b 20 ; data.f128 0x{:032x}", rd, const_data)
            }
            &Inst::FpuToInt { op, rd, rn } => {
                let (op, sizesrc, sizedest) = match op {
                    FpuToIntOp::F32ToI32 => ("fcvtzs", ScalarSize::Size32, OperandSize::Size32),
                    FpuToIntOp::F32ToU32 => ("fcvtzu", ScalarSize::Size32, OperandSize::Size32),
                    FpuToIntOp::F32ToI64 => ("fcvtzs", ScalarSize::Size32, OperandSize::Size64),
                    FpuToIntOp::F32ToU64 => ("fcvtzu", ScalarSize::Size32, OperandSize::Size64),
                    FpuToIntOp::F64ToI32 => ("fcvtzs", ScalarSize::Size64, OperandSize::Size32),
                    FpuToIntOp::F64ToU32 => ("fcvtzu", ScalarSize::Size64, OperandSize::Size32),
                    FpuToIntOp::F64ToI64 => ("fcvtzs", ScalarSize::Size64, OperandSize::Size64),
                    FpuToIntOp::F64ToU64 => ("fcvtzu", ScalarSize::Size64, OperandSize::Size64),
                };
                let rd = show_ireg_sized(rd.to_reg(), mb_rru, sizedest);
                let rn = show_vreg_scalar(rn, mb_rru, sizesrc);
                format!("{} {}, {}", op, rd, rn)
            }
            &Inst::IntToFpu { op, rd, rn } => {
                let (op, sizesrc, sizedest) = match op {
                    IntToFpuOp::I32ToF32 => ("scvtf", OperandSize::Size32, ScalarSize::Size32),
                    IntToFpuOp::U32ToF32 => ("ucvtf", OperandSize::Size32, ScalarSize::Size32),
                    IntToFpuOp::I64ToF32 => ("scvtf", OperandSize::Size64, ScalarSize::Size32),
                    IntToFpuOp::U64ToF32 => ("ucvtf", OperandSize::Size64, ScalarSize::Size32),
                    IntToFpuOp::I32ToF64 => ("scvtf", OperandSize::Size32, ScalarSize::Size64),
                    IntToFpuOp::U32ToF64 => ("ucvtf", OperandSize::Size32, ScalarSize::Size64),
                    IntToFpuOp::I64ToF64 => ("scvtf", OperandSize::Size64, ScalarSize::Size64),
                    IntToFpuOp::U64ToF64 => ("ucvtf", OperandSize::Size64, ScalarSize::Size64),
                };
                let rd = show_vreg_scalar(rd.to_reg(), mb_rru, sizedest);
                let rn = show_ireg_sized(rn, mb_rru, sizesrc);
                format!("{} {}, {}", op, rd, rn)
            }
            &Inst::FpuCSel32 { rd, rn, rm, cond } => {
                let rd = show_vreg_scalar(rd.to_reg(), mb_rru, ScalarSize::Size32);
                let rn = show_vreg_scalar(rn, mb_rru, ScalarSize::Size32);
                let rm = show_vreg_scalar(rm, mb_rru, ScalarSize::Size32);
                let cond = cond.show_rru(mb_rru);
                format!("fcsel {}, {}, {}, {}", rd, rn, rm, cond)
            }
            &Inst::FpuCSel64 { rd, rn, rm, cond } => {
                let rd = show_vreg_scalar(rd.to_reg(), mb_rru, ScalarSize::Size64);
                let rn = show_vreg_scalar(rn, mb_rru, ScalarSize::Size64);
                let rm = show_vreg_scalar(rm, mb_rru, ScalarSize::Size64);
                let cond = cond.show_rru(mb_rru);
                format!("fcsel {}, {}, {}, {}", rd, rn, rm, cond)
            }
            &Inst::FpuRound { op, rd, rn } => {
                let (inst, size) = match op {
                    FpuRoundMode::Minus32 => ("frintm", ScalarSize::Size32),
                    FpuRoundMode::Minus64 => ("frintm", ScalarSize::Size64),
                    FpuRoundMode::Plus32 => ("frintp", ScalarSize::Size32),
                    FpuRoundMode::Plus64 => ("frintp", ScalarSize::Size64),
                    FpuRoundMode::Zero32 => ("frintz", ScalarSize::Size32),
                    FpuRoundMode::Zero64 => ("frintz", ScalarSize::Size64),
                    FpuRoundMode::Nearest32 => ("frintn", ScalarSize::Size32),
                    FpuRoundMode::Nearest64 => ("frintn", ScalarSize::Size64),
                };
                let rd = show_vreg_scalar(rd.to_reg(), mb_rru, size);
                let rn = show_vreg_scalar(rn, mb_rru, size);
                format!("{} {}, {}", inst, rd, rn)
            }
            &Inst::MovToFpu { rd, rn } => {
                let rd = show_vreg_scalar(rd.to_reg(), mb_rru, ScalarSize::Size64);
                let rn = show_ireg_sized(rn, mb_rru, OperandSize::Size64);
                format!("fmov {}, {}", rd, rn)
            }
            &Inst::MovToVec { rd, rn, idx, size } => {
                let rd = show_vreg_element(rd.to_reg(), mb_rru, idx, size);
                let rn = show_ireg_sized(rn, mb_rru, size.operand_size());
                format!("mov {}, {}", rd, rn)
            }
            &Inst::MovFromVec { rd, rn, idx, size } => {
                let op = match size {
                    VectorSize::Size8x16 => "umov",
                    VectorSize::Size16x8 => "umov",
                    VectorSize::Size32x4 => "mov",
                    VectorSize::Size64x2 => "mov",
                    _ => unimplemented!(),
                };
                let rd = show_ireg_sized(rd.to_reg(), mb_rru, size.operand_size());
                let rn = show_vreg_element(rn, mb_rru, idx, size);
                format!("{} {}, {}", op, rd, rn)
            }
            &Inst::MovFromVecSigned {
                rd,
                rn,
                idx,
                size,
                scalar_size,
            } => {
                let rd = show_ireg_sized(rd.to_reg(), mb_rru, scalar_size);
                let rn = show_vreg_element(rn, mb_rru, idx, size);
                format!("smov {}, {}", rd, rn)
            }
            &Inst::VecDup { rd, rn, size } => {
                let rd = show_vreg_vector(rd.to_reg(), mb_rru, size);
                let rn = show_ireg_sized(rn, mb_rru, size.operand_size());
                format!("dup {}, {}", rd, rn)
            }
            &Inst::VecDupFromFpu { rd, rn, size } => {
                let rd = show_vreg_vector(rd.to_reg(), mb_rru, size);
                let rn = show_vreg_element(rn, mb_rru, 0, size);
                format!("dup {}, {}", rd, rn)
            }
            &Inst::VecExtend { t, rd, rn } => {
                let (op, dest, src) = match t {
                    VecExtendOp::Sxtl8 => ("sxtl", VectorSize::Size16x8, VectorSize::Size8x8),
                    VecExtendOp::Sxtl16 => ("sxtl", VectorSize::Size32x4, VectorSize::Size16x4),
                    VecExtendOp::Sxtl32 => ("sxtl", VectorSize::Size64x2, VectorSize::Size32x2),
                    VecExtendOp::Uxtl8 => ("uxtl", VectorSize::Size16x8, VectorSize::Size8x8),
                    VecExtendOp::Uxtl16 => ("uxtl", VectorSize::Size32x4, VectorSize::Size16x4),
                    VecExtendOp::Uxtl32 => ("uxtl", VectorSize::Size64x2, VectorSize::Size32x2),
                };
                let rd = show_vreg_vector(rd.to_reg(), mb_rru, dest);
                let rn = show_vreg_vector(rn, mb_rru, src);
                format!("{} {}, {}", op, rd, rn)
            }
            &Inst::VecMovElement {
                rd,
                rn,
                idx1,
                idx2,
                size,
            } => {
                let rd = show_vreg_element(rd.to_reg(), mb_rru, idx1, size);
                let rn = show_vreg_element(rn, mb_rru, idx2, size);
                format!("mov {}, {}", rd, rn)
            }
            &Inst::VecRRR {
                rd,
                rn,
                rm,
                alu_op,
                size,
            } => {
                let (op, size) = match alu_op {
                    VecALUOp::Sqadd => ("sqadd", size),
                    VecALUOp::Uqadd => ("uqadd", size),
                    VecALUOp::Sqsub => ("sqsub", size),
                    VecALUOp::Uqsub => ("uqsub", size),
                    VecALUOp::Cmeq => ("cmeq", size),
                    VecALUOp::Cmge => ("cmge", size),
                    VecALUOp::Cmgt => ("cmgt", size),
                    VecALUOp::Cmhs => ("cmhs", size),
                    VecALUOp::Cmhi => ("cmhi", size),
                    VecALUOp::Fcmeq => ("fcmeq", size),
                    VecALUOp::Fcmgt => ("fcmgt", size),
                    VecALUOp::Fcmge => ("fcmge", size),
                    VecALUOp::And => ("and", VectorSize::Size8x16),
                    VecALUOp::Bic => ("bic", VectorSize::Size8x16),
                    VecALUOp::Orr => ("orr", VectorSize::Size8x16),
                    VecALUOp::Eor => ("eor", VectorSize::Size8x16),
                    VecALUOp::Bsl => ("bsl", VectorSize::Size8x16),
                    VecALUOp::Umaxp => ("umaxp", size),
                    VecALUOp::Add => ("add", size),
                    VecALUOp::Sub => ("sub", size),
                    VecALUOp::Mul => ("mul", size),
                    VecALUOp::Sshl => ("sshl", size),
                    VecALUOp::Ushl => ("ushl", size),
                    VecALUOp::Umin => ("umin", size),
                    VecALUOp::Smin => ("smin", size),
                    VecALUOp::Umax => ("umax", size),
                    VecALUOp::Smax => ("smax", size),
                    VecALUOp::Urhadd => ("urhadd", size),
                    VecALUOp::Fadd => ("fadd", size),
                    VecALUOp::Fsub => ("fsub", size),
                    VecALUOp::Fdiv => ("fdiv", size),
                    VecALUOp::Fmax => ("fmax", size),
                    VecALUOp::Fmin => ("fmin", size),
                    VecALUOp::Fmul => ("fmul", size),
                };
                let rd = show_vreg_vector(rd.to_reg(), mb_rru, size);
                let rn = show_vreg_vector(rn, mb_rru, size);
                let rm = show_vreg_vector(rm, mb_rru, size);
                format!("{} {}, {}, {}", op, rd, rn, rm)
            }
            &Inst::VecMisc { op, rd, rn, size } => {
                let (op, size) = match op {
                    VecMisc2::Not => ("mvn", VectorSize::Size8x16),
                    VecMisc2::Neg => ("neg", size),
                    VecMisc2::Abs => ("abs", size),
                    VecMisc2::Fabs => ("fabs", size),
                    VecMisc2::Fneg => ("fneg", size),
                    VecMisc2::Fsqrt => ("fsqrt", size),
                };

                let rd = show_vreg_vector(rd.to_reg(), mb_rru, size);
                let rn = show_vreg_vector(rn, mb_rru, size);
                format!("{} {}, {}", op, rd, rn)
            }
            &Inst::VecLanes { op, rd, rn, size } => {
                let op = match op {
                    VecLanesOp::Uminv => "uminv",
                };
                let rd = show_vreg_scalar(rd.to_reg(), mb_rru, size.lane_size());
                let rn = show_vreg_vector(rn, mb_rru, size);
                format!("{} {}, {}", op, rd, rn)
            }
            &Inst::VecTbl {
                rd,
                rn,
                rm,
                is_extension,
            } => {
                let op = if is_extension { "tbx" } else { "tbl" };
                let rd = show_vreg_vector(rd.to_reg(), mb_rru, VectorSize::Size8x16);
                let rn = show_vreg_vector(rn, mb_rru, VectorSize::Size8x16);
                let rm = show_vreg_vector(rm, mb_rru, VectorSize::Size8x16);
                format!("{} {}, {{ {} }}, {}", op, rd, rn, rm)
            }
            &Inst::VecTbl2 {
                rd,
                rn,
                rn2,
                rm,
                is_extension,
            } => {
                let op = if is_extension { "tbx" } else { "tbl" };
                let rd = show_vreg_vector(rd.to_reg(), mb_rru, VectorSize::Size8x16);
                let rn = show_vreg_vector(rn, mb_rru, VectorSize::Size8x16);
                let rn2 = show_vreg_vector(rn2, mb_rru, VectorSize::Size8x16);
                let rm = show_vreg_vector(rm, mb_rru, VectorSize::Size8x16);
                format!("{} {}, {{ {}, {} }}, {}", op, rd, rn, rn2, rm)
            }
            &Inst::MovToNZCV { rn } => {
                let rn = rn.show_rru(mb_rru);
                format!("msr nzcv, {}", rn)
            }
            &Inst::MovFromNZCV { rd } => {
                let rd = rd.to_reg().show_rru(mb_rru);
                format!("mrs {}, nzcv", rd)
            }
            &Inst::Extend {
                rd,
                rn,
                signed,
                from_bits,
                to_bits,
            } if from_bits >= 8 => {
                // Is the destination a 32-bit register? Corresponds to whether
                // extend-to width is <= 32 bits, *unless* we have an unsigned
                // 32-to-64-bit extension, which is implemented with a "mov" to a
                // 32-bit (W-reg) dest, because this zeroes the top 32 bits.
                let dest_size = if !signed && from_bits == 32 && to_bits == 64 {
                    OperandSize::Size32
                } else {
                    OperandSize::from_bits(to_bits)
                };
                let rd = show_ireg_sized(rd.to_reg(), mb_rru, dest_size);
                let rn = show_ireg_sized(rn, mb_rru, OperandSize::from_bits(from_bits));
                let op = match (signed, from_bits, to_bits) {
                    (false, 8, 32) => "uxtb",
                    (true, 8, 32) => "sxtb",
                    (false, 16, 32) => "uxth",
                    (true, 16, 32) => "sxth",
                    (false, 8, 64) => "uxtb",
                    (true, 8, 64) => "sxtb",
                    (false, 16, 64) => "uxth",
                    (true, 16, 64) => "sxth",
                    (false, 32, 64) => "mov", // special case (see above).
                    (true, 32, 64) => "sxtw",
                    _ => panic!("Unsupported Extend case: {:?}", self),
                };
                format!("{} {}, {}", op, rd, rn)
            }
            &Inst::Extend {
                rd,
                rn,
                signed,
                from_bits,
                to_bits,
            } if from_bits == 1 && signed => {
                let dest_size = OperandSize::from_bits(to_bits);
                let zr = if dest_size.is32() { "wzr" } else { "xzr" };
                let rd32 = show_ireg_sized(rd.to_reg(), mb_rru, OperandSize::Size32);
                let rd = show_ireg_sized(rd.to_reg(), mb_rru, dest_size);
                let rn = show_ireg_sized(rn, mb_rru, OperandSize::Size32);
                format!("and {}, {}, #1 ; sub {}, {}, {}", rd32, rn, rd, zr, rd)
            }
            &Inst::Extend {
                rd,
                rn,
                signed,
                from_bits,
                ..
            } if from_bits == 1 && !signed => {
                let rd = show_ireg_sized(rd.to_reg(), mb_rru, OperandSize::Size32);
                let rn = show_ireg_sized(rn, mb_rru, OperandSize::Size32);
                format!("and {}, {}, #1", rd, rn)
            }
            &Inst::Extend { .. } => {
                panic!("Unsupported Extend case");
            }
            &Inst::Call { .. } => format!("bl 0"),
            &Inst::CallInd { ref info, .. } => {
                let rn = info.rn.show_rru(mb_rru);
                format!("blr {}", rn)
            }
            &Inst::Ret => "ret".to_string(),
            &Inst::EpiloguePlaceholder => "epilogue placeholder".to_string(),
            &Inst::Jump { ref dest } => {
                let dest = dest.show_rru(mb_rru);
                format!("b {}", dest)
            }
            &Inst::CondBr {
                ref taken,
                ref not_taken,
                ref kind,
            } => {
                let taken = taken.show_rru(mb_rru);
                let not_taken = not_taken.show_rru(mb_rru);
                match kind {
                    &CondBrKind::Zero(reg) => {
                        let reg = reg.show_rru(mb_rru);
                        format!("cbz {}, {} ; b {}", reg, taken, not_taken)
                    }
                    &CondBrKind::NotZero(reg) => {
                        let reg = reg.show_rru(mb_rru);
                        format!("cbnz {}, {} ; b {}", reg, taken, not_taken)
                    }
                    &CondBrKind::Cond(c) => {
                        let c = c.show_rru(mb_rru);
                        format!("b.{} {} ; b {}", c, taken, not_taken)
                    }
                }
            }
            &Inst::IndirectBr { rn, .. } => {
                let rn = rn.show_rru(mb_rru);
                format!("br {}", rn)
            }
            &Inst::Brk => "brk #0".to_string(),
            &Inst::Udf { .. } => "udf".to_string(),
            &Inst::TrapIf { ref kind, .. } => match kind {
                &CondBrKind::Zero(reg) => {
                    let reg = reg.show_rru(mb_rru);
                    format!("cbnz {}, 8 ; udf", reg)
                }
                &CondBrKind::NotZero(reg) => {
                    let reg = reg.show_rru(mb_rru);
                    format!("cbz {}, 8 ; udf", reg)
                }
                &CondBrKind::Cond(c) => {
                    let c = c.invert().show_rru(mb_rru);
                    format!("b.{} 8 ; udf", c)
                }
            },
            &Inst::Adr { rd, off } => {
                let rd = rd.show_rru(mb_rru);
                format!("adr {}, pc+{}", rd, off)
            }
            &Inst::Word4 { data } => format!("data.i32 {}", data),
            &Inst::Word8 { data } => format!("data.i64 {}", data),
            &Inst::JTSequence {
                ref info,
                ridx,
                rtmp1,
                rtmp2,
                ..
            } => {
                let ridx = ridx.show_rru(mb_rru);
                let rtmp1 = rtmp1.show_rru(mb_rru);
                let rtmp2 = rtmp2.show_rru(mb_rru);
                let default_target = info.default_target.show_rru(mb_rru);
                format!(
                    concat!(
                        "b.hs {} ; ",
                        "adr {}, pc+16 ; ",
                        "ldrsw {}, [{}, {}, LSL 2] ; ",
                        "add {}, {}, {} ; ",
                        "br {} ; ",
                        "jt_entries {:?}"
                    ),
                    default_target,
                    rtmp1,
                    rtmp2,
                    rtmp1,
                    ridx,
                    rtmp1,
                    rtmp1,
                    rtmp2,
                    rtmp1,
                    info.targets
                )
            }
            &Inst::LoadConst64 { rd, const_data } => {
                let rd = rd.show_rru(mb_rru);
                format!("ldr {}, 8 ; b 12 ; data {:?}", rd, const_data)
            }
            &Inst::LoadExtName {
                rd,
                ref name,
                offset,
                srcloc: _srcloc,
            } => {
                let rd = rd.show_rru(mb_rru);
                format!("ldr {}, 8 ; b 12 ; data {:?} + {}", rd, name, offset)
            }
            &Inst::LoadAddr { rd, ref mem } => {
                // TODO: we really should find a better way to avoid duplication of
                // this logic between `emit()` and `show_rru()` -- a separate 1-to-N
                // expansion stage (i.e., legalization, but without the slow edit-in-place
                // of the existing legalization framework).
                let (mem_insts, mem) = mem_finalize(0, mem, state);
                let mut ret = String::new();
                for inst in mem_insts.into_iter() {
                    ret.push_str(&inst.show_rru(mb_rru));
                }
                let (reg, offset) = match mem {
                    AMode::Unscaled(r, simm9) => (r, simm9.value()),
                    AMode::UnsignedOffset(r, uimm12scaled) => (r, uimm12scaled.value() as i32),
                    _ => panic!("Unsupported case for LoadAddr: {:?}", mem),
                };
                let abs_offset = if offset < 0 {
                    -offset as u64
                } else {
                    offset as u64
                };
                let alu_op = if offset < 0 {
                    ALUOp::Sub64
                } else {
                    ALUOp::Add64
                };

                if offset == 0 {
                    let mov = Inst::mov(rd, reg);
                    ret.push_str(&mov.show_rru(mb_rru));
                } else if let Some(imm12) = Imm12::maybe_from_u64(abs_offset) {
                    let add = Inst::AluRRImm12 {
                        alu_op,
                        rd,
                        rn: reg,
                        imm12,
                    };
                    ret.push_str(&add.show_rru(mb_rru));
                } else {
                    let tmp = writable_spilltmp_reg();
                    for inst in Inst::load_constant(tmp, abs_offset).into_iter() {
                        ret.push_str(&inst.show_rru(mb_rru));
                    }
                    let add = Inst::AluRRR {
                        alu_op,
                        rd,
                        rn: reg,
                        rm: tmp.to_reg(),
                    };
                    ret.push_str(&add.show_rru(mb_rru));
                }
                ret
            }
            &Inst::VirtualSPOffsetAdj { offset } => {
                state.virtual_sp_offset += offset;
                format!("virtual_sp_offset_adjust {}", offset)
            }
            &Inst::EmitIsland { needed_space } => format!("emit_island {}", needed_space),
        }
    }
}

//=============================================================================
// Label fixups and jump veneers.

/// Different forms of label references for different instruction formats.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LabelUse {
    /// 19-bit branch offset (conditional branches). PC-rel, offset is imm << 2. Immediate is 19
    /// signed bits, in bits 23:5. Used by cbz, cbnz, b.cond.
    Branch19,
    /// 26-bit branch offset (unconditional branches). PC-rel, offset is imm << 2. Immediate is 26
    /// signed bits, in bits 25:0. Used by b, bl.
    Branch26,
    /// 19-bit offset for LDR (load literal). PC-rel, offset is imm << 2. Immediate is 19 signed bits,
    /// in bits 23:5.
    Ldr19,
    /// 21-bit offset for ADR (get address of label). PC-rel, offset is not shifted. Immediate is
    /// 21 signed bits, with high 19 bits in bits 23:5 and low 2 bits in bits 30:29.
    Adr21,
    /// 32-bit PC relative constant offset (from address of constant itself),
    /// signed. Used in jump tables.
    PCRel32,
}

impl MachInstLabelUse for LabelUse {
    /// Alignment for veneer code. Every AArch64 instruction must be 4-byte-aligned.
    const ALIGN: CodeOffset = 4;

    /// Maximum PC-relative range (positive), inclusive.
    fn max_pos_range(self) -> CodeOffset {
        match self {
            // 19-bit immediate, left-shifted by 2, for 21 bits of total range. Signed, so +2^20
            // from zero. Likewise for two other shifted cases below.
            LabelUse::Branch19 => (1 << 20) - 1,
            LabelUse::Branch26 => (1 << 27) - 1,
            LabelUse::Ldr19 => (1 << 20) - 1,
            // Adr does not shift its immediate, so the 21-bit immediate gives 21 bits of total
            // range.
            LabelUse::Adr21 => (1 << 20) - 1,
            LabelUse::PCRel32 => 0x7fffffff,
        }
    }

    /// Maximum PC-relative range (negative).
    fn max_neg_range(self) -> CodeOffset {
        // All forms are twos-complement signed offsets, so negative limit is one more than
        // positive limit.
        self.max_pos_range() + 1
    }

    /// Size of window into code needed to do the patch.
    fn patch_size(self) -> CodeOffset {
        // Patch is on one instruction only for all of these label reference types.
        4
    }

    /// Perform the patch.
    fn patch(self, buffer: &mut [u8], use_offset: CodeOffset, label_offset: CodeOffset) {
        let pc_rel = (label_offset as i64) - (use_offset as i64);
        debug_assert!(pc_rel <= self.max_pos_range() as i64);
        debug_assert!(pc_rel >= -(self.max_neg_range() as i64));
        let pc_rel = pc_rel as u32;
        let insn_word = u32::from_le_bytes([buffer[0], buffer[1], buffer[2], buffer[3]]);
        let mask = match self {
            LabelUse::Branch19 => 0x00ffffe0, // bits 23..5 inclusive
            LabelUse::Branch26 => 0x03ffffff, // bits 25..0 inclusive
            LabelUse::Ldr19 => 0x00ffffe0,    // bits 23..5 inclusive
            LabelUse::Adr21 => 0x60ffffe0,    // bits 30..29, 25..5 inclusive
            LabelUse::PCRel32 => 0xffffffff,
        };
        let pc_rel_shifted = match self {
            LabelUse::Adr21 | LabelUse::PCRel32 => pc_rel,
            _ => {
                debug_assert!(pc_rel & 3 == 0);
                pc_rel >> 2
            }
        };
        let pc_rel_inserted = match self {
            LabelUse::Branch19 | LabelUse::Ldr19 => (pc_rel_shifted & 0x7ffff) << 5,
            LabelUse::Branch26 => pc_rel_shifted & 0x3ffffff,
            LabelUse::Adr21 => (pc_rel_shifted & 0x7ffff) << 5 | (pc_rel_shifted & 0x180000) << 10,
            LabelUse::PCRel32 => pc_rel_shifted,
        };
        let is_add = match self {
            LabelUse::PCRel32 => true,
            _ => false,
        };
        let insn_word = if is_add {
            insn_word.wrapping_add(pc_rel_inserted)
        } else {
            (insn_word & !mask) | pc_rel_inserted
        };
        buffer[0..4].clone_from_slice(&u32::to_le_bytes(insn_word));
    }

    /// Is a veneer supported for this label reference type?
    fn supports_veneer(self) -> bool {
        match self {
            LabelUse::Branch19 => true, // veneer is a Branch26
            _ => false,
        }
    }

    /// How large is the veneer, if supported?
    fn veneer_size(self) -> CodeOffset {
        4
    }

    /// Generate a veneer into the buffer, given that this veneer is at `veneer_offset`, and return
    /// an offset and label-use for the veneer's use of the original label.
    fn generate_veneer(
        self,
        buffer: &mut [u8],
        veneer_offset: CodeOffset,
    ) -> (CodeOffset, LabelUse) {
        match self {
            LabelUse::Branch19 => {
                // veneer is a Branch26 (unconditional branch). Just encode directly here -- don't
                // bother with constructing an Inst.
                let insn_word = 0b000101 << 26;
                buffer[0..4].clone_from_slice(&u32::to_le_bytes(insn_word));
                (veneer_offset, LabelUse::Branch26)
            }
            _ => panic!("Unsupported label-reference type for veneer generation!"),
        }
    }
}
