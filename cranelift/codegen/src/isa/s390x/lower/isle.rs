//! ISLE integration glue code for s390x lowering.

// Pull in the ISLE generated code.
pub mod generated_code;

// Types that the generated ISLE code uses via `use super::*`.
use crate::isa::s390x::abi::S390xMachineDeps;
use crate::isa::s390x::inst::{
    stack_reg, writable_gpr, zero_reg, CallIndInfo, CallInfo, Cond, Inst as MInst, MemArg, UImm12,
    UImm16Shifted, UImm32Shifted,
};
use crate::isa::s390x::settings::Flags as IsaFlags;
use crate::machinst::isle::*;
use crate::machinst::{MachLabel, Reg};
use crate::settings::Flags;
use crate::{
    ir::{
        condcodes::*, immediates::*, types::*, AtomicRmwOp, Endianness, Inst, InstructionData,
        MemFlags, Opcode, TrapCode, Value, ValueList,
    },
    isa::unwind::UnwindInst,
    machinst::{InsnOutput, LowerCtx, VCodeConstant, VCodeConstantData},
};
use regalloc2::PReg;
use std::boxed::Box;
use std::cell::Cell;
use std::convert::TryFrom;
use std::vec::Vec;
use target_lexicon::Triple;

type BoxCallInfo = Box<CallInfo>;
type BoxCallIndInfo = Box<CallIndInfo>;
type VecMachLabel = Vec<MachLabel>;
type BoxExternalName = Box<ExternalName>;
type VecMInst = Vec<MInst>;
type VecMInstBuilder = Cell<Vec<MInst>>;

/// The main entry point for lowering with ISLE.
pub(crate) fn lower<C>(
    lower_ctx: &mut C,
    triple: &Triple,
    flags: &Flags,
    isa_flags: &IsaFlags,
    outputs: &[InsnOutput],
    inst: Inst,
) -> Result<(), ()>
where
    C: LowerCtx<I = MInst>,
{
    lower_common(
        lower_ctx,
        triple,
        flags,
        isa_flags,
        outputs,
        inst,
        |cx, insn| generated_code::constructor_lower(cx, insn),
    )
}

/// The main entry point for branch lowering with ISLE.
pub(crate) fn lower_branch<C>(
    lower_ctx: &mut C,
    triple: &Triple,
    flags: &Flags,
    isa_flags: &IsaFlags,
    branch: Inst,
    targets: &[MachLabel],
) -> Result<(), ()>
where
    C: LowerCtx<I = MInst>,
{
    lower_common(
        lower_ctx,
        triple,
        flags,
        isa_flags,
        &[],
        branch,
        |cx, insn| generated_code::constructor_lower_branch(cx, insn, &targets.to_vec()),
    )
}

impl<C> generated_code::Context for IsleContext<'_, C, Flags, IsaFlags, 6>
where
    C: LowerCtx<I = MInst>,
{
    isle_prelude_methods!();

    fn abi_sig(&mut self, sig_ref: SigRef) -> ABISig {
        let sig = &self.lower_ctx.dfg().signatures[sig_ref];
        ABISig::from_func_sig::<S390xMachineDeps>(sig, self.flags).unwrap()
    }

    fn abi_accumulate_outgoing_args_size(&mut self, abi: &ABISig) -> Unit {
        let off = abi.sized_stack_arg_space() + abi.sized_stack_ret_space();
        self.lower_ctx
            .abi()
            .accumulate_outgoing_args_size(off as u32);
    }

    fn abi_call_info(&mut self, abi: &ABISig, name: ExternalName, opcode: &Opcode) -> BoxCallInfo {
        let (uses, defs, clobbers) = abi.call_uses_defs_clobbers::<S390xMachineDeps>();
        Box::new(CallInfo {
            dest: name.clone(),
            uses,
            defs,
            clobbers,
            opcode: *opcode,
            caller_callconv: self.lower_ctx.abi().call_conv(),
            callee_callconv: abi.call_conv(),
        })
    }

    fn abi_call_ind_info(&mut self, abi: &ABISig, target: Reg, opcode: &Opcode) -> BoxCallIndInfo {
        let (uses, defs, clobbers) = abi.call_uses_defs_clobbers::<S390xMachineDeps>();
        Box::new(CallIndInfo {
            rn: target,
            uses,
            defs,
            clobbers,
            opcode: *opcode,
            caller_callconv: self.lower_ctx.abi().call_conv(),
            callee_callconv: abi.call_conv(),
        })
    }

    #[inline]
    fn allow_div_traps(&mut self, _: Type) -> Option<()> {
        if !self.flags.avoid_div_traps() {
            Some(())
        } else {
            None
        }
    }

    #[inline]
    fn mie2_enabled(&mut self, _: Type) -> Option<()> {
        if self.isa_flags.has_mie2() {
            Some(())
        } else {
            None
        }
    }

    #[inline]
    fn mie2_disabled(&mut self, _: Type) -> Option<()> {
        if !self.isa_flags.has_mie2() {
            Some(())
        } else {
            None
        }
    }

    #[inline]
    fn vxrs_ext2_enabled(&mut self, _: Type) -> Option<()> {
        if self.isa_flags.has_vxrs_ext2() {
            Some(())
        } else {
            None
        }
    }

    #[inline]
    fn vxrs_ext2_disabled(&mut self, _: Type) -> Option<()> {
        if !self.isa_flags.has_vxrs_ext2() {
            Some(())
        } else {
            None
        }
    }

    #[inline]
    fn writable_gpr(&mut self, regno: u8) -> WritableReg {
        writable_gpr(regno)
    }

    #[inline]
    fn zero_reg(&mut self) -> Reg {
        zero_reg()
    }

    #[inline]
    fn gpr32_ty(&mut self, ty: Type) -> Option<Type> {
        match ty {
            I8 | I16 | I32 | B1 | B8 | B16 | B32 => Some(ty),
            _ => None,
        }
    }

    #[inline]
    fn gpr64_ty(&mut self, ty: Type) -> Option<Type> {
        match ty {
            I64 | B64 | R64 => Some(ty),
            _ => None,
        }
    }

    #[inline]
    fn uimm32shifted(&mut self, n: u32, shift: u8) -> UImm32Shifted {
        UImm32Shifted::maybe_with_shift(n, shift).unwrap()
    }

    #[inline]
    fn uimm16shifted(&mut self, n: u16, shift: u8) -> UImm16Shifted {
        UImm16Shifted::maybe_with_shift(n, shift).unwrap()
    }

    #[inline]
    fn i64_nonequal(&mut self, val: i64, cmp: i64) -> Option<i64> {
        if val != cmp {
            Some(val)
        } else {
            None
        }
    }

    #[inline]
    fn u64_pair_split(&mut self, n: u128) -> (u64, u64) {
        ((n >> 64) as u64, n as u64)
    }

    #[inline]
    fn u64_pair_concat(&mut self, hi: u64, lo: u64) -> u128 {
        (hi as u128) << 64 | (lo as u128)
    }

    #[inline]
    fn u32_pair_split(&mut self, n: u64) -> (u32, u32) {
        ((n >> 32) as u32, n as u32)
    }

    #[inline]
    fn u32_pair_concat(&mut self, hi: u32, lo: u32) -> u64 {
        (hi as u64) << 32 | (lo as u64)
    }

    #[inline]
    fn u16_pair_split(&mut self, n: u32) -> (u16, u16) {
        ((n >> 16) as u16, n as u16)
    }

    #[inline]
    fn u16_pair_concat(&mut self, hi: u16, lo: u16) -> u32 {
        (hi as u32) << 16 | (lo as u32)
    }

    #[inline]
    fn u8_pair_split(&mut self, n: u16) -> (u8, u8) {
        ((n >> 8) as u8, n as u8)
    }

    #[inline]
    fn u8_pair_concat(&mut self, hi: u8, lo: u8) -> u16 {
        (hi as u16) << 8 | (lo as u16)
    }

    #[inline]
    fn u8_as_u16(&mut self, n: u8) -> u16 {
        n as u16
    }

    #[inline]
    fn u64_as_u32(&mut self, n: u64) -> u32 {
        n as u32
    }

    #[inline]
    fn u64_as_i16(&mut self, n: u64) -> i16 {
        n as i16
    }

    #[inline]
    fn u64_nonzero_hipart(&mut self, n: u64) -> Option<u64> {
        let part = n & 0xffff_ffff_0000_0000;
        if part != 0 {
            Some(part)
        } else {
            None
        }
    }

    #[inline]
    fn u64_nonzero_lopart(&mut self, n: u64) -> Option<u64> {
        let part = n & 0x0000_0000_ffff_ffff;
        if part != 0 {
            Some(part)
        } else {
            None
        }
    }

    #[inline]
    fn i32_from_u64(&mut self, n: u64) -> Option<i32> {
        if let Ok(imm) = i32::try_from(n as i64) {
            Some(imm)
        } else {
            None
        }
    }

    #[inline]
    fn i16_from_u64(&mut self, n: u64) -> Option<i16> {
        if let Ok(imm) = i16::try_from(n as i64) {
            Some(imm)
        } else {
            None
        }
    }

    #[inline]
    fn i16_from_u32(&mut self, n: u32) -> Option<i16> {
        if let Ok(imm) = i16::try_from(n as i32) {
            Some(imm)
        } else {
            None
        }
    }

    #[inline]
    fn uimm32shifted_from_u64(&mut self, n: u64) -> Option<UImm32Shifted> {
        UImm32Shifted::maybe_from_u64(n)
    }

    #[inline]
    fn uimm16shifted_from_u64(&mut self, n: u64) -> Option<UImm16Shifted> {
        UImm16Shifted::maybe_from_u64(n)
    }

    #[inline]
    fn be_lane_idx(&mut self, ty: Type, idx: u8) -> u8 {
        ty.lane_count() as u8 - 1 - idx
    }

    #[inline]
    fn lane_byte_mask(&mut self, ty: Type, idx: u8) -> u16 {
        let lane_bytes = (ty.lane_bits() / 8) as u8;
        let lane_mask = (1u16 << lane_bytes) - 1;
        lane_mask << (16 - ((idx + 1) * lane_bytes))
    }

    #[inline]
    fn shuffle_mask_from_u128(&mut self, idx: u128) -> (u128, u16) {
        let bytes = idx.to_be_bytes();
        let and_mask = bytes.iter().fold(0, |acc, &x| (acc << 1) | (x < 32) as u16);
        let bytes = bytes.map(|x| {
            if x < 16 {
                15 - x
            } else if x < 32 {
                47 - x
            } else {
                128
            }
        });
        let permute_mask = u128::from_be_bytes(bytes);
        (permute_mask, and_mask)
    }

    #[inline]
    fn u64_from_value(&mut self, val: Value) -> Option<u64> {
        let inst = self.lower_ctx.dfg().value_def(val).inst()?;
        let constant = self.lower_ctx.get_constant(inst)?;
        let ty = self.lower_ctx.output_ty(inst, 0);
        Some(zero_extend_to_u64(constant, self.ty_bits(ty).unwrap()))
    }

    #[inline]
    fn u64_from_inverted_value(&mut self, val: Value) -> Option<u64> {
        let inst = self.lower_ctx.dfg().value_def(val).inst()?;
        let constant = self.lower_ctx.get_constant(inst)?;
        let ty = self.lower_ctx.output_ty(inst, 0);
        Some(zero_extend_to_u64(!constant, self.ty_bits(ty).unwrap()))
    }

    #[inline]
    fn u32_from_value(&mut self, val: Value) -> Option<u32> {
        let constant = self.u64_from_value(val)?;
        let imm = u32::try_from(constant).ok()?;
        Some(imm)
    }

    #[inline]
    fn u8_from_value(&mut self, val: Value) -> Option<u8> {
        let constant = self.u64_from_value(val)?;
        let imm = u8::try_from(constant).ok()?;
        Some(imm)
    }

    #[inline]
    fn u64_from_signed_value(&mut self, val: Value) -> Option<u64> {
        let inst = self.lower_ctx.dfg().value_def(val).inst()?;
        let constant = self.lower_ctx.get_constant(inst)?;
        let ty = self.lower_ctx.output_ty(inst, 0);
        Some(sign_extend_to_u64(constant, self.ty_bits(ty).unwrap()))
    }

    #[inline]
    fn i64_from_value(&mut self, val: Value) -> Option<i64> {
        let constant = self.u64_from_signed_value(val)? as i64;
        Some(constant)
    }

    #[inline]
    fn i32_from_value(&mut self, val: Value) -> Option<i32> {
        let constant = self.u64_from_signed_value(val)? as i64;
        let imm = i32::try_from(constant).ok()?;
        Some(imm)
    }

    #[inline]
    fn i16_from_value(&mut self, val: Value) -> Option<i16> {
        let constant = self.u64_from_signed_value(val)? as i64;
        let imm = i16::try_from(constant).ok()?;
        Some(imm)
    }

    #[inline]
    fn i16_from_swapped_value(&mut self, val: Value) -> Option<i16> {
        let constant = self.u64_from_signed_value(val)? as i64;
        let imm = i16::try_from(constant).ok()?;
        Some(imm.swap_bytes())
    }

    #[inline]
    fn i64_from_negated_value(&mut self, val: Value) -> Option<i64> {
        let constant = self.u64_from_signed_value(val)? as i64;
        let imm = -constant;
        Some(imm)
    }

    #[inline]
    fn i32_from_negated_value(&mut self, val: Value) -> Option<i32> {
        let constant = self.u64_from_signed_value(val)? as i64;
        let imm = i32::try_from(-constant).ok()?;
        Some(imm)
    }

    #[inline]
    fn i16_from_negated_value(&mut self, val: Value) -> Option<i16> {
        let constant = self.u64_from_signed_value(val)? as i64;
        let imm = i16::try_from(-constant).ok()?;
        Some(imm)
    }

    #[inline]
    fn uimm16shifted_from_value(&mut self, val: Value) -> Option<UImm16Shifted> {
        let constant = self.u64_from_value(val)?;
        UImm16Shifted::maybe_from_u64(constant)
    }

    #[inline]
    fn uimm32shifted_from_value(&mut self, val: Value) -> Option<UImm32Shifted> {
        let constant = self.u64_from_value(val)?;
        UImm32Shifted::maybe_from_u64(constant)
    }

    #[inline]
    fn uimm16shifted_from_inverted_value(&mut self, val: Value) -> Option<UImm16Shifted> {
        let constant = self.u64_from_inverted_value(val)?;
        let imm = UImm16Shifted::maybe_from_u64(constant)?;
        Some(imm.negate_bits())
    }

    #[inline]
    fn uimm32shifted_from_inverted_value(&mut self, val: Value) -> Option<UImm32Shifted> {
        let constant = self.u64_from_inverted_value(val)?;
        let imm = UImm32Shifted::maybe_from_u64(constant)?;
        Some(imm.negate_bits())
    }

    #[inline]
    fn mask_amt_imm(&mut self, ty: Type, amt: i64) -> u8 {
        let mask = ty.lane_bits() - 1;
        (amt as u8) & (mask as u8)
    }

    #[inline]
    fn mask_as_cond(&mut self, mask: u8) -> Cond {
        Cond::from_mask(mask)
    }

    #[inline]
    fn intcc_as_cond(&mut self, cc: &IntCC) -> Cond {
        Cond::from_intcc(*cc)
    }

    #[inline]
    fn floatcc_as_cond(&mut self, cc: &FloatCC) -> Cond {
        Cond::from_floatcc(*cc)
    }

    #[inline]
    fn invert_cond(&mut self, cond: &Cond) -> Cond {
        Cond::invert(*cond)
    }

    #[inline]
    fn signed(&mut self, cc: &IntCC) -> Option<()> {
        if condcode_is_signed(*cc) {
            Some(())
        } else {
            None
        }
    }

    #[inline]
    fn unsigned(&mut self, cc: &IntCC) -> Option<()> {
        if !condcode_is_signed(*cc) {
            Some(())
        } else {
            None
        }
    }

    #[inline]
    fn vec_length_minus1(&mut self, vec: &VecMachLabel) -> u32 {
        u32::try_from(vec.len()).unwrap() - 1
    }

    #[inline]
    fn vec_element(&mut self, vec: &VecMachLabel, index: u8) -> MachLabel {
        vec[usize::from(index)]
    }

    #[inline]
    fn zero_offset(&mut self) -> Offset32 {
        Offset32::new(0)
    }

    #[inline]
    fn i64_from_offset(&mut self, off: Offset32) -> i64 {
        i64::from(off)
    }

    #[inline]
    fn fcvt_to_uint_ub32(&mut self, size: u8) -> u64 {
        (2.0_f32).powi(size.into()).to_bits() as u64
    }

    #[inline]
    fn fcvt_to_uint_lb32(&mut self) -> u64 {
        (-1.0_f32).to_bits() as u64
    }

    #[inline]
    fn fcvt_to_uint_ub64(&mut self, size: u8) -> u64 {
        (2.0_f64).powi(size.into()).to_bits()
    }

    #[inline]
    fn fcvt_to_uint_lb64(&mut self) -> u64 {
        (-1.0_f64).to_bits()
    }

    #[inline]
    fn fcvt_to_sint_ub32(&mut self, size: u8) -> u64 {
        (2.0_f32).powi((size - 1).into()).to_bits() as u64
    }

    #[inline]
    fn fcvt_to_sint_lb32(&mut self, size: u8) -> u64 {
        let lb = (-2.0_f32).powi((size - 1).into());
        std::cmp::max(lb.to_bits() + 1, (lb - 1.0).to_bits()) as u64
    }

    #[inline]
    fn fcvt_to_sint_ub64(&mut self, size: u8) -> u64 {
        (2.0_f64).powi((size - 1).into()).to_bits()
    }

    #[inline]
    fn fcvt_to_sint_lb64(&mut self, size: u8) -> u64 {
        let lb = (-2.0_f64).powi((size - 1).into());
        std::cmp::max(lb.to_bits() + 1, (lb - 1.0).to_bits())
    }

    #[inline]
    fn littleendian(&mut self, flags: MemFlags) -> Option<()> {
        let endianness = flags.endianness(Endianness::Big);
        if endianness == Endianness::Little {
            Some(())
        } else {
            None
        }
    }

    #[inline]
    fn bigendian(&mut self, flags: MemFlags) -> Option<()> {
        let endianness = flags.endianness(Endianness::Big);
        if endianness == Endianness::Big {
            Some(())
        } else {
            None
        }
    }

    #[inline]
    fn memflags_trusted(&mut self) -> MemFlags {
        MemFlags::trusted()
    }

    #[inline]
    fn memarg_reg_plus_reg(&mut self, x: Reg, y: Reg, bias: u8, flags: MemFlags) -> MemArg {
        MemArg::BXD12 {
            base: x,
            index: y,
            disp: UImm12::maybe_from_u64(bias as u64).unwrap(),
            flags,
        }
    }

    #[inline]
    fn memarg_reg_plus_off(&mut self, reg: Reg, off: i64, bias: u8, flags: MemFlags) -> MemArg {
        MemArg::reg_plus_off(reg, off + (bias as i64), flags)
    }

    #[inline]
    fn memarg_stack_off(&mut self, base: i64, off: i64) -> MemArg {
        MemArg::reg_plus_off(stack_reg(), base + off, MemFlags::trusted())
    }

    #[inline]
    fn memarg_initial_sp_offset(&mut self, off: i64) -> MemArg {
        MemArg::InitialSPOffset { off }
    }

    #[inline]
    fn memarg_symbol(&mut self, name: ExternalName, offset: i32, flags: MemFlags) -> MemArg {
        MemArg::Symbol {
            name: Box::new(name),
            offset,
            flags,
        }
    }

    #[inline]
    fn memarg_symbol_offset_sum(&mut self, off1: i64, off2: i64) -> Option<i32> {
        let off = i32::try_from(off1 + off2).ok()?;
        if off & 1 == 0 {
            Some(off)
        } else {
            None
        }
    }

    #[inline]
    fn inst_builder_new(&mut self) -> VecMInstBuilder {
        Cell::new(Vec::<MInst>::new())
    }

    #[inline]
    fn inst_builder_push(&mut self, builder: &VecMInstBuilder, inst: &MInst) -> Unit {
        let mut vec = builder.take();
        vec.push(inst.clone());
        builder.set(vec);
    }

    #[inline]
    fn inst_builder_finish(&mut self, builder: &VecMInstBuilder) -> Vec<MInst> {
        builder.take()
    }

    #[inline]
    fn real_reg(&mut self, reg: WritableReg) -> Option<WritableReg> {
        if reg.to_reg().is_real() {
            Some(reg)
        } else {
            None
        }
    }

    #[inline]
    fn same_reg(&mut self, dst: WritableReg, src: Reg) -> Option<Reg> {
        if dst.to_reg() == src {
            Some(src)
        } else {
            None
        }
    }

    #[inline]
    fn sinkable_inst(&mut self, val: Value) -> Option<Inst> {
        let input = self.lower_ctx.get_value_as_source_or_const(val);
        if let Some((inst, 0)) = input.inst.as_inst() {
            return Some(inst);
        }
        None
    }

    #[inline]
    fn emit(&mut self, inst: &MInst) -> Unit {
        self.lower_ctx.emit(inst.clone());
    }

    #[inline]
    fn preg_stack(&mut self) -> PReg {
        stack_reg().to_real_reg().unwrap().into()
    }
}

/// Zero-extend the low `from_bits` bits of `value` to a full u64.
#[inline]
fn zero_extend_to_u64(value: u64, from_bits: u8) -> u64 {
    assert!(from_bits <= 64);
    if from_bits >= 64 {
        value
    } else {
        value & ((1u64 << from_bits) - 1)
    }
}

/// Sign-extend the low `from_bits` bits of `value` to a full u64.
#[inline]
fn sign_extend_to_u64(value: u64, from_bits: u8) -> u64 {
    assert!(from_bits <= 64);
    if from_bits >= 64 {
        value
    } else {
        (((value << (64 - from_bits)) as i64) >> (64 - from_bits)) as u64
    }
}

/// Determines whether this condcode interprets inputs as signed or
/// unsigned.  See the documentation for the `icmp` instruction in
/// cranelift-codegen/meta/src/shared/instructions.rs for further insights
/// into this.
#[inline]
fn condcode_is_signed(cc: IntCC) -> bool {
    match cc {
        IntCC::Equal => false,
        IntCC::NotEqual => false,
        IntCC::SignedGreaterThanOrEqual => true,
        IntCC::SignedGreaterThan => true,
        IntCC::SignedLessThanOrEqual => true,
        IntCC::SignedLessThan => true,
        IntCC::UnsignedGreaterThanOrEqual => false,
        IntCC::UnsignedGreaterThan => false,
        IntCC::UnsignedLessThanOrEqual => false,
        IntCC::UnsignedLessThan => false,
        IntCC::Overflow => true,
        IntCC::NotOverflow => true,
    }
}
