//! Lowering rules for X64.

// ISLE integration glue.
pub(super) mod isle;

use crate::data_value::DataValue;
use crate::ir::{
    condcodes::FloatCC, types, ExternalName, Inst as IRInst, InstructionData, LibCall, Opcode, Type,
};
use crate::isa::x64::abi::*;
use crate::isa::x64::inst::args::*;
use crate::isa::x64::inst::*;
use crate::isa::{x64::settings as x64_settings, x64::X64Backend, CallConv};
use crate::machinst::lower::*;
use crate::machinst::*;
use crate::result::CodegenResult;
use crate::settings::{Flags, TlsModel};
use smallvec::SmallVec;
use std::convert::TryFrom;
use target_lexicon::Triple;

//=============================================================================
// Helpers for instruction lowering.

fn is_int_or_ref_ty(ty: Type) -> bool {
    match ty {
        types::I8 | types::I16 | types::I32 | types::I64 | types::R64 => true,
        types::B1 | types::B8 | types::B16 | types::B32 | types::B64 => true,
        types::R32 => panic!("shouldn't have 32-bits refs on x64"),
        _ => false,
    }
}

/// Returns whether the given specified `input` is a result produced by an instruction with Opcode
/// `op`.
// TODO investigate failures with checking against the result index.
fn matches_input(ctx: &mut Lower<Inst>, input: InsnInput, op: Opcode) -> Option<IRInst> {
    let inputs = ctx.get_input_as_source_or_const(input.insn, input.input);
    inputs.inst.as_inst().and_then(|(src_inst, _)| {
        let data = ctx.data(src_inst);
        if data.opcode() == op {
            return Some(src_inst);
        }
        None
    })
}

/// Emits instruction(s) to generate the given 64-bit constant value into a newly-allocated
/// temporary register, returning that register.
fn generate_constant(ctx: &mut Lower<Inst>, ty: Type, c: u64) -> ValueRegs<Reg> {
    let from_bits = ty_bits(ty);
    let masked = if from_bits < 64 {
        c & ((1u64 << from_bits) - 1)
    } else {
        c
    };

    let cst_copy = ctx.alloc_tmp(ty);
    for inst in Inst::gen_constant(cst_copy, masked as u128, ty, |ty| {
        ctx.alloc_tmp(ty).only_reg().unwrap()
    })
    .into_iter()
    {
        ctx.emit(inst);
    }
    non_writable_value_regs(cst_copy)
}

/// Put the given input into possibly multiple registers, and mark it as used (side-effect).
fn put_input_in_regs(ctx: &mut Lower<Inst>, spec: InsnInput) -> ValueRegs<Reg> {
    let ty = ctx.input_ty(spec.insn, spec.input);
    let input = ctx.get_input_as_source_or_const(spec.insn, spec.input);

    if let Some(c) = input.constant {
        // Generate constants fresh at each use to minimize long-range register pressure.
        generate_constant(ctx, ty, c)
    } else {
        ctx.put_input_in_regs(spec.insn, spec.input)
    }
}

/// Put the given input into a register, and mark it as used (side-effect).
fn put_input_in_reg(ctx: &mut Lower<Inst>, spec: InsnInput) -> Reg {
    put_input_in_regs(ctx, spec)
        .only_reg()
        .expect("Multi-register value not expected")
}

/// Determines whether a load operation (indicated by `src_insn`) can be merged
/// into the current lowering point. If so, returns the address-base source (as
/// an `InsnInput`) and an offset from that address from which to perform the
/// load.
fn is_mergeable_load(ctx: &mut Lower<Inst>, src_insn: IRInst) -> Option<(InsnInput, i32)> {
    let insn_data = ctx.data(src_insn);
    let inputs = ctx.num_inputs(src_insn);
    if inputs != 1 {
        return None;
    }

    let load_ty = ctx.output_ty(src_insn, 0);
    if ty_bits(load_ty) < 32 {
        // Narrower values are handled by ALU insts that are at least 32 bits
        // wide, which is normally OK as we ignore upper buts; but, if we
        // generate, e.g., a direct-from-memory 32-bit add for a byte value and
        // the byte is the last byte in a page, the extra data that we load is
        // incorrectly accessed. So we only allow loads to merge for
        // 32-bit-and-above widths.
        return None;
    }

    // SIMD instructions can only be load-coalesced when the loaded value comes
    // from an aligned address.
    if load_ty.is_vector() && !insn_data.memflags().map_or(false, |f| f.aligned()) {
        return None;
    }

    // Just testing the opcode is enough, because the width will always match if
    // the type does (and the type should match if the CLIF is properly
    // constructed).
    if insn_data.opcode() == Opcode::Load {
        let offset = insn_data
            .load_store_offset()
            .expect("load should have offset");
        Some((
            InsnInput {
                insn: src_insn,
                input: 0,
            },
            offset,
        ))
    } else {
        None
    }
}

/// Put the given input into a register or a memory operand.
/// Effectful: may mark the given input as used, when returning the register form.
fn input_to_reg_mem(ctx: &mut Lower<Inst>, spec: InsnInput) -> RegMem {
    let inputs = ctx.get_input_as_source_or_const(spec.insn, spec.input);

    if let Some(c) = inputs.constant {
        // Generate constants fresh at each use to minimize long-range register pressure.
        let ty = ctx.input_ty(spec.insn, spec.input);
        return RegMem::reg(generate_constant(ctx, ty, c).only_reg().unwrap());
    }

    if let InputSourceInst::UniqueUse(src_insn, 0) = inputs.inst {
        if let Some((addr_input, offset)) = is_mergeable_load(ctx, src_insn) {
            ctx.sink_inst(src_insn);
            let amode = lower_to_amode(ctx, addr_input, offset);
            return RegMem::mem(amode);
        }
    }

    RegMem::reg(
        ctx.put_input_in_regs(spec.insn, spec.input)
            .only_reg()
            .unwrap(),
    )
}

fn input_to_imm(ctx: &mut Lower<Inst>, spec: InsnInput) -> Option<u64> {
    ctx.get_input_as_source_or_const(spec.insn, spec.input)
        .constant
}

/// Emit an instruction to insert a value `src` into a lane of `dst`.
fn emit_insert_lane(ctx: &mut Lower<Inst>, src: RegMem, dst: Writable<Reg>, lane: u8, ty: Type) {
    if !ty.is_float() {
        let (sse_op, size) = match ty.lane_bits() {
            8 => (SseOpcode::Pinsrb, OperandSize::Size32),
            16 => (SseOpcode::Pinsrw, OperandSize::Size32),
            32 => (SseOpcode::Pinsrd, OperandSize::Size32),
            64 => (SseOpcode::Pinsrd, OperandSize::Size64),
            _ => panic!("Unable to insertlane for lane size: {}", ty.lane_bits()),
        };
        ctx.emit(Inst::xmm_rm_r_imm(sse_op, src, dst, lane, size));
    } else if ty == types::F32 {
        let sse_op = SseOpcode::Insertps;
        // Insert 32-bits from replacement (at index 00, bits 7:8) to vector (lane
        // shifted into bits 5:6).
        let lane = 0b00_00_00_00 | lane << 4;
        ctx.emit(Inst::xmm_rm_r_imm(
            sse_op,
            src,
            dst,
            lane,
            OperandSize::Size32,
        ));
    } else if ty == types::F64 {
        let sse_op = match lane {
            // Move the lowest quadword in replacement to vector without changing
            // the upper bits.
            0 => SseOpcode::Movsd,
            // Move the low 64 bits of replacement vector to the high 64 bits of the
            // vector.
            1 => SseOpcode::Movlhps,
            _ => unreachable!(),
        };
        // Here we use the `xmm_rm_r` encoding because it correctly tells the register
        // allocator how we are using `dst`: we are using `dst` as a `mod` whereas other
        // encoding formats like `xmm_unary_rm_r` treat it as a `def`.
        ctx.emit(Inst::xmm_rm_r(sse_op, src, dst));
    } else {
        panic!("unable to emit insertlane for type: {}", ty)
    }
}

/// Emit an instruction to extract a lane of `src` into `dst`.
fn emit_extract_lane(ctx: &mut Lower<Inst>, src: Reg, dst: Writable<Reg>, lane: u8, ty: Type) {
    if !ty.is_float() {
        let (sse_op, size) = match ty.lane_bits() {
            8 => (SseOpcode::Pextrb, OperandSize::Size32),
            16 => (SseOpcode::Pextrw, OperandSize::Size32),
            32 => (SseOpcode::Pextrd, OperandSize::Size32),
            64 => (SseOpcode::Pextrd, OperandSize::Size64),
            _ => panic!("Unable to extractlane for lane size: {}", ty.lane_bits()),
        };
        let src = RegMem::reg(src);
        ctx.emit(Inst::xmm_rm_r_imm(sse_op, src, dst, lane, size));
    } else if ty == types::F32 || ty == types::F64 {
        if lane == 0 {
            // Remove the extractlane instruction, leaving the float where it is. The upper
            // bits will remain unchanged; for correctness, this relies on Cranelift type
            // checking to avoid using those bits.
            ctx.emit(Inst::gen_move(dst, src, ty));
        } else {
            // Otherwise, shuffle the bits in `lane` to the lowest lane.
            let sse_op = SseOpcode::Pshufd;
            let mask = match ty {
                // Move the value at `lane` to lane 0, copying existing value at lane 0 to
                // other lanes. Again, this relies on Cranelift type checking to avoid
                // using those bits.
                types::F32 => {
                    assert!(lane > 0 && lane < 4);
                    0b00_00_00_00 | lane
                }
                // Move the value at `lane` 1 (we know it must be 1 because of the `if`
                // statement above) to lane 0 and leave lane 1 unchanged. The Cranelift type
                // checking assumption also applies here.
                types::F64 => {
                    assert!(lane == 1);
                    0b11_10_11_10
                }
                _ => unreachable!(),
            };
            let src = RegMem::reg(src);
            ctx.emit(Inst::xmm_rm_r_imm(
                sse_op,
                src,
                dst,
                mask,
                OperandSize::Size32,
            ));
        }
    } else {
        panic!("unable to emit extractlane for type: {}", ty)
    }
}

fn emit_vm_call(
    ctx: &mut Lower<Inst>,
    flags: &Flags,
    triple: &Triple,
    libcall: LibCall,
    inputs: &[Reg],
    outputs: &[Writable<Reg>],
) -> CodegenResult<()> {
    let extname = ExternalName::LibCall(libcall);

    let dist = if flags.use_colocated_libcalls() {
        RelocDistance::Near
    } else {
        RelocDistance::Far
    };

    // TODO avoid recreating signatures for every single Libcall function.
    let call_conv = CallConv::for_libcall(flags, CallConv::triple_default(triple));
    let sig = libcall.signature(call_conv);
    let caller_conv = ctx.abi().call_conv();

    let mut abi = X64Caller::from_func(&sig, &extname, dist, caller_conv, flags)?;

    abi.emit_stack_pre_adjust(ctx);

    assert_eq!(inputs.len(), abi.num_args());

    for (i, input) in inputs.iter().enumerate() {
        abi.emit_copy_regs_to_arg(ctx, i, ValueRegs::one(*input));
    }

    abi.emit_call(ctx);
    for (i, output) in outputs.iter().enumerate() {
        abi.emit_copy_retval_to_regs(ctx, i, ValueRegs::one(*output));
    }
    abi.emit_stack_post_adjust(ctx);

    Ok(())
}

/// Returns whether the given input is a shift by a constant value less or equal than 3.
/// The goal is to embed it within an address mode.
fn matches_small_constant_shift(ctx: &mut Lower<Inst>, spec: InsnInput) -> Option<(InsnInput, u8)> {
    matches_input(ctx, spec, Opcode::Ishl).and_then(|shift| {
        match input_to_imm(
            ctx,
            InsnInput {
                insn: shift,
                input: 1,
            },
        ) {
            Some(shift_amt) if shift_amt <= 3 => Some((
                InsnInput {
                    insn: shift,
                    input: 0,
                },
                shift_amt as u8,
            )),
            _ => None,
        }
    })
}

/// Lowers an instruction to one of the x86 addressing modes.
///
/// Note: the 32-bit offset in Cranelift has to be sign-extended, which maps x86's behavior.
fn lower_to_amode(ctx: &mut Lower<Inst>, spec: InsnInput, offset: i32) -> Amode {
    let flags = ctx
        .memflags(spec.insn)
        .expect("Instruction with amode should have memflags");

    // We now either have an add that we must materialize, or some other input; as well as the
    // final offset.
    if let Some(add) = matches_input(ctx, spec, Opcode::Iadd) {
        debug_assert_eq!(ctx.output_ty(add, 0), types::I64);
        let add_inputs = &[
            InsnInput {
                insn: add,
                input: 0,
            },
            InsnInput {
                insn: add,
                input: 1,
            },
        ];

        // TODO heap_addr legalization generates a uext64 *after* the shift, so these optimizations
        // aren't happening in the wasm case. We could do better, given some range analysis.
        let (base, index, shift) = if let Some((shift_input, shift_amt)) =
            matches_small_constant_shift(ctx, add_inputs[0])
        {
            (
                put_input_in_reg(ctx, add_inputs[1]),
                put_input_in_reg(ctx, shift_input),
                shift_amt,
            )
        } else if let Some((shift_input, shift_amt)) =
            matches_small_constant_shift(ctx, add_inputs[1])
        {
            (
                put_input_in_reg(ctx, add_inputs[0]),
                put_input_in_reg(ctx, shift_input),
                shift_amt,
            )
        } else {
            for i in 0..=1 {
                // Try to pierce through uextend.
                if let Some(uextend) = matches_input(
                    ctx,
                    InsnInput {
                        insn: add,
                        input: i,
                    },
                    Opcode::Uextend,
                ) {
                    if let Some(cst) = ctx.get_input_as_source_or_const(uextend, 0).constant {
                        // Zero the upper bits.
                        let input_size = ctx.input_ty(uextend, 0).bits() as u64;
                        let shift: u64 = 64 - input_size;
                        let uext_cst: u64 = (cst << shift) >> shift;

                        let final_offset = (offset as i64).wrapping_add(uext_cst as i64);
                        if low32_will_sign_extend_to_64(final_offset as u64) {
                            let base = put_input_in_reg(ctx, add_inputs[1 - i]);
                            return Amode::imm_reg(final_offset as u32, base).with_flags(flags);
                        }
                    }
                }

                // If it's a constant, add it directly!
                if let Some(cst) = ctx.get_input_as_source_or_const(add, i).constant {
                    let final_offset = (offset as i64).wrapping_add(cst as i64);
                    if low32_will_sign_extend_to_64(final_offset as u64) {
                        let base = put_input_in_reg(ctx, add_inputs[1 - i]);
                        return Amode::imm_reg(final_offset as u32, base).with_flags(flags);
                    }
                }
            }

            (
                put_input_in_reg(ctx, add_inputs[0]),
                put_input_in_reg(ctx, add_inputs[1]),
                0,
            )
        };

        return Amode::imm_reg_reg_shift(
            offset as u32,
            Gpr::new(base).unwrap(),
            Gpr::new(index).unwrap(),
            shift,
        )
        .with_flags(flags);
    }

    let input = put_input_in_reg(ctx, spec);
    Amode::imm_reg(offset as u32, input).with_flags(flags)
}

//=============================================================================
// Top-level instruction lowering entry point, for one instruction.

/// Actually codegen an instruction's results into registers.
fn lower_insn_to_regs(
    ctx: &mut Lower<Inst>,
    insn: IRInst,
    flags: &Flags,
    isa_flags: &x64_settings::Flags,
    triple: &Triple,
) -> CodegenResult<()> {
    let op = ctx.data(insn).opcode();

    let inputs: SmallVec<[InsnInput; 4]> = (0..ctx.num_inputs(insn))
        .map(|i| InsnInput { insn, input: i })
        .collect();
    let outputs: SmallVec<[InsnOutput; 2]> = (0..ctx.num_outputs(insn))
        .map(|i| InsnOutput { insn, output: i })
        .collect();

    let ty = if outputs.len() > 0 {
        Some(ctx.output_ty(insn, 0))
    } else {
        None
    };

    if let Ok(()) = isle::lower(ctx, triple, flags, isa_flags, &outputs, insn) {
        return Ok(());
    }

    let implemented_in_isle = |ctx: &mut Lower<Inst>| {
        unreachable!(
            "implemented in ISLE: inst = `{}`, type = `{:?}`",
            ctx.dfg().display_inst(insn),
            ty
        )
    };

    match op {
        Opcode::Iconst
        | Opcode::Bconst
        | Opcode::F32const
        | Opcode::F64const
        | Opcode::Null
        | Opcode::Iadd
        | Opcode::IaddIfcout
        | Opcode::SaddSat
        | Opcode::UaddSat
        | Opcode::Isub
        | Opcode::SsubSat
        | Opcode::UsubSat
        | Opcode::AvgRound
        | Opcode::Band
        | Opcode::Bor
        | Opcode::Bxor
        | Opcode::Imul
        | Opcode::BandNot
        | Opcode::Iabs
        | Opcode::Imax
        | Opcode::Umax
        | Opcode::Imin
        | Opcode::Umin
        | Opcode::Bnot
        | Opcode::Bitselect
        | Opcode::Vselect
        | Opcode::Ushr
        | Opcode::Sshr
        | Opcode::Ishl
        | Opcode::Rotl
        | Opcode::Rotr
        | Opcode::Ineg
        | Opcode::Trap
        | Opcode::ResumableTrap
        | Opcode::Clz
        | Opcode::Ctz
        | Opcode::Popcnt
        | Opcode::Bitrev
        | Opcode::IsNull
        | Opcode::IsInvalid
        | Opcode::Uextend
        | Opcode::Sextend
        | Opcode::Breduce
        | Opcode::Bextend
        | Opcode::Ireduce
        | Opcode::Bint
        | Opcode::Debugtrap
        | Opcode::WideningPairwiseDotProductS
        | Opcode::Fadd
        | Opcode::Fsub
        | Opcode::Fmul
        | Opcode::Fdiv
        | Opcode::Fmin
        | Opcode::Fmax
        | Opcode::FminPseudo
        | Opcode::FmaxPseudo
        | Opcode::Sqrt
        | Opcode::Fpromote
        | Opcode::FvpromoteLow
        | Opcode::Fdemote
        | Opcode::Fvdemote
        | Opcode::Fma
        | Opcode::Icmp
        | Opcode::Fcmp
        | Opcode::Load
        | Opcode::Uload8
        | Opcode::Sload8
        | Opcode::Uload16
        | Opcode::Sload16
        | Opcode::Uload32
        | Opcode::Sload32
        | Opcode::Sload8x8
        | Opcode::Uload8x8
        | Opcode::Sload16x4
        | Opcode::Uload16x4
        | Opcode::Sload32x2
        | Opcode::Uload32x2
        | Opcode::Store
        | Opcode::Istore8
        | Opcode::Istore16
        | Opcode::Istore32
        | Opcode::AtomicRmw
        | Opcode::AtomicCas
        | Opcode::AtomicLoad
        | Opcode::AtomicStore
        | Opcode::Fence
        | Opcode::FuncAddr
        | Opcode::SymbolValue
        | Opcode::Return
        | Opcode::Call
        | Opcode::CallIndirect
        | Opcode::Trapif
        | Opcode::Trapff
        | Opcode::GetFramePointer
        | Opcode::GetStackPointer
        | Opcode::GetReturnAddress
        | Opcode::Select
        | Opcode::Selectif
        | Opcode::SelectifSpectreGuard
        | Opcode::FcvtFromSint
        | Opcode::FcvtLowFromSint
        | Opcode::FcvtFromUint => {
            implemented_in_isle(ctx);
        }

        Opcode::FcvtToUint | Opcode::FcvtToUintSat | Opcode::FcvtToSint | Opcode::FcvtToSintSat => {
            let src = put_input_in_reg(ctx, inputs[0]);
            let dst = get_output_reg(ctx, outputs[0]).only_reg().unwrap();

            let input_ty = ctx.input_ty(insn, 0);
            if !input_ty.is_vector() {
                let src_size = if input_ty == types::F32 {
                    OperandSize::Size32
                } else {
                    assert_eq!(input_ty, types::F64);
                    OperandSize::Size64
                };

                let output_ty = ty.unwrap();
                let dst_size = if output_ty == types::I32 {
                    OperandSize::Size32
                } else {
                    assert_eq!(output_ty, types::I64);
                    OperandSize::Size64
                };

                let to_signed = op == Opcode::FcvtToSint || op == Opcode::FcvtToSintSat;
                let is_sat = op == Opcode::FcvtToUintSat || op == Opcode::FcvtToSintSat;

                let src_copy = ctx.alloc_tmp(input_ty).only_reg().unwrap();
                ctx.emit(Inst::gen_move(src_copy, src, input_ty));

                let tmp_xmm = ctx.alloc_tmp(input_ty).only_reg().unwrap();
                let tmp_gpr = ctx.alloc_tmp(output_ty).only_reg().unwrap();

                if to_signed {
                    ctx.emit(Inst::cvt_float_to_sint_seq(
                        src_size, dst_size, is_sat, src_copy, dst, tmp_gpr, tmp_xmm,
                    ));
                } else {
                    ctx.emit(Inst::cvt_float_to_uint_seq(
                        src_size, dst_size, is_sat, src_copy, dst, tmp_gpr, tmp_xmm,
                    ));
                }
            } else {
                if op == Opcode::FcvtToSintSat {
                    // Sets destination to zero if float is NaN
                    assert_eq!(types::F32X4, ctx.input_ty(insn, 0));
                    let tmp = ctx.alloc_tmp(types::I32X4).only_reg().unwrap();
                    ctx.emit(Inst::xmm_unary_rm_r(
                        SseOpcode::Movapd,
                        RegMem::reg(src),
                        tmp,
                    ));
                    ctx.emit(Inst::gen_move(dst, src, input_ty));
                    let cond = FcmpImm::from(FloatCC::Equal);
                    ctx.emit(Inst::xmm_rm_r_imm(
                        SseOpcode::Cmpps,
                        RegMem::reg(tmp.to_reg()),
                        tmp,
                        cond.encode(),
                        OperandSize::Size32,
                    ));
                    ctx.emit(Inst::xmm_rm_r(
                        SseOpcode::Andps,
                        RegMem::reg(tmp.to_reg()),
                        dst,
                    ));

                    // Sets top bit of tmp if float is positive
                    // Setting up to set top bit on negative float values
                    ctx.emit(Inst::xmm_rm_r(
                        SseOpcode::Pxor,
                        RegMem::reg(dst.to_reg()),
                        tmp,
                    ));

                    // Convert the packed float to packed doubleword.
                    ctx.emit(Inst::xmm_unary_rm_r(
                        SseOpcode::Cvttps2dq,
                        RegMem::reg(dst.to_reg()),
                        dst,
                    ));

                    // Set top bit only if < 0
                    // Saturate lane with sign (top) bit.
                    ctx.emit(Inst::xmm_rm_r(
                        SseOpcode::Pand,
                        RegMem::reg(dst.to_reg()),
                        tmp,
                    ));
                    ctx.emit(Inst::xmm_rmi_reg(SseOpcode::Psrad, RegMemImm::imm(31), tmp));

                    // On overflow 0x80000000 is returned to a lane.
                    // Below sets positive overflow lanes to 0x7FFFFFFF
                    // Keeps negative overflow lanes as is.
                    ctx.emit(Inst::xmm_rm_r(
                        SseOpcode::Pxor,
                        RegMem::reg(tmp.to_reg()),
                        dst,
                    ));
                } else if op == Opcode::FcvtToUintSat {
                    // The algorithm for converting floats to unsigned ints is a little tricky. The
                    // complication arises because we are converting from a signed 64-bit int with a positive
                    // integer range from 1..INT_MAX (0x1..0x7FFFFFFF) to an unsigned integer with an extended
                    // range from (INT_MAX+1)..UINT_MAX. It's this range from (INT_MAX+1)..UINT_MAX
                    // (0x80000000..0xFFFFFFFF) that needs to be accounted for as a special case since our
                    // conversion instruction (cvttps2dq) only converts as high as INT_MAX (0x7FFFFFFF), but
                    // which conveniently setting underflows and overflows (smaller than MIN_INT or larger than
                    // MAX_INT) to be INT_MAX+1 (0x80000000). Nothing that the range (INT_MAX+1)..UINT_MAX includes
                    // precisely INT_MAX values we can correctly account for and convert every value in this range
                    // if we simply subtract INT_MAX+1 before doing the cvttps2dq conversion. After the subtraction
                    // every value originally (INT_MAX+1)..UINT_MAX is now the range (0..INT_MAX).
                    // After the conversion we add INT_MAX+1 back to this converted value, noting again that
                    // values we are trying to account for were already set to INT_MAX+1 during the original conversion.
                    // We simply have to create a mask and make sure we are adding together only the lanes that need
                    // to be accounted for. Digesting it all the steps then are:
                    //
                    // Step 1 - Account for NaN and negative floats by setting these src values to zero.
                    // Step 2 - Make a copy (tmp1) of the src value since we need to convert twice for
                    //          reasons described above.
                    // Step 3 - Convert the original src values. This will convert properly all floats up to INT_MAX
                    // Step 4 - Subtract INT_MAX from the copy set (tmp1). Note, all zero and negative values are those
                    //          values that were originally in the range (0..INT_MAX). This will come in handy during
                    //          step 7 when we zero negative lanes.
                    // Step 5 - Create a bit mask for tmp1 that will correspond to all lanes originally less than
                    //          UINT_MAX that are now less than INT_MAX thanks to the subtraction.
                    // Step 6 - Convert the second set of values (tmp1)
                    // Step 7 - Prep the converted second set by zeroing out negative lanes (these have already been
                    //          converted correctly with the first set) and by setting overflow lanes to 0x7FFFFFFF
                    //          as this will allow us to properly saturate overflow lanes when adding to 0x80000000
                    // Step 8 - Add the orginal converted src and the converted tmp1 where float values originally less
                    //          than and equal to INT_MAX will be unchanged, float values originally between INT_MAX+1 and
                    //          UINT_MAX will add together (INT_MAX) + (SRC - INT_MAX), and float values originally
                    //          greater than UINT_MAX will be saturated to UINT_MAX (0xFFFFFFFF) after adding (0x8000000 + 0x7FFFFFFF).
                    //
                    //
                    // The table below illustrates the result after each step where it matters for the converted set.
                    // Note the original value range (original src set) is the final dst in Step 8:
                    //
                    // Original src set:
                    // | Original Value Range |    Step 1    |         Step 3         |          Step 8           |
                    // |  -FLT_MIN..FLT_MAX   | 0.0..FLT_MAX | 0..INT_MAX(w/overflow) | 0..UINT_MAX(w/saturation) |
                    //
                    // Copied src set (tmp1):
                    // |    Step 2    |                  Step 4                  |
                    // | 0.0..FLT_MAX | (0.0-(INT_MAX+1))..(FLT_MAX-(INT_MAX+1)) |
                    //
                    // |                       Step 6                        |                 Step 7                 |
                    // | (0-(INT_MAX+1))..(UINT_MAX-(INT_MAX+1))(w/overflow) | ((INT_MAX+1)-(INT_MAX+1))..(INT_MAX+1) |

                    // Create temporaries
                    assert_eq!(types::F32X4, ctx.input_ty(insn, 0));
                    let tmp1 = ctx.alloc_tmp(types::I32X4).only_reg().unwrap();
                    let tmp2 = ctx.alloc_tmp(types::I32X4).only_reg().unwrap();

                    // Converting to unsigned int so if float src is negative or NaN
                    // will first set to zero.
                    ctx.emit(Inst::xmm_rm_r(SseOpcode::Pxor, RegMem::from(tmp2), tmp2));
                    ctx.emit(Inst::gen_move(dst, src, input_ty));
                    ctx.emit(Inst::xmm_rm_r(SseOpcode::Maxps, RegMem::from(tmp2), dst));

                    // Set tmp2 to INT_MAX+1. It is important to note here that after it looks
                    // like we are only converting INT_MAX (0x7FFFFFFF) but in fact because
                    // single precision IEEE-754 floats can only accurately represent contingous
                    // integers up to 2^23 and outside of this range it rounds to the closest
                    // integer that it can represent. In the case of INT_MAX, this value gets
                    // represented as 0x4f000000 which is the integer value (INT_MAX+1).

                    ctx.emit(Inst::xmm_rm_r(SseOpcode::Pcmpeqd, RegMem::from(tmp2), tmp2));
                    ctx.emit(Inst::xmm_rmi_reg(SseOpcode::Psrld, RegMemImm::imm(1), tmp2));
                    ctx.emit(Inst::xmm_unary_rm_r(
                        SseOpcode::Cvtdq2ps,
                        RegMem::from(tmp2),
                        tmp2,
                    ));

                    // Make a copy of these lanes and then do the first conversion.
                    // Overflow lanes greater than the maximum allowed signed value will
                    // set to 0x80000000. Negative and NaN lanes will be 0x0
                    ctx.emit(Inst::xmm_mov(SseOpcode::Movaps, RegMem::from(dst), tmp1));
                    ctx.emit(Inst::xmm_unary_rm_r(
                        SseOpcode::Cvttps2dq,
                        RegMem::from(dst),
                        dst,
                    ));

                    // Set lanes to src - max_signed_int
                    ctx.emit(Inst::xmm_rm_r(SseOpcode::Subps, RegMem::from(tmp2), tmp1));

                    // Create mask for all positive lanes to saturate (i.e. greater than
                    // or equal to the maxmimum allowable unsigned int).
                    let cond = FcmpImm::from(FloatCC::LessThanOrEqual);
                    ctx.emit(Inst::xmm_rm_r_imm(
                        SseOpcode::Cmpps,
                        RegMem::from(tmp1),
                        tmp2,
                        cond.encode(),
                        OperandSize::Size32,
                    ));

                    // Convert those set of lanes that have the max_signed_int factored out.
                    ctx.emit(Inst::xmm_unary_rm_r(
                        SseOpcode::Cvttps2dq,
                        RegMem::from(tmp1),
                        tmp1,
                    ));

                    // Prepare converted lanes by zeroing negative lanes and prepping lanes
                    // that have positive overflow (based on the mask) by setting these lanes
                    // to 0x7FFFFFFF
                    ctx.emit(Inst::xmm_rm_r(SseOpcode::Pxor, RegMem::from(tmp2), tmp1));
                    ctx.emit(Inst::xmm_rm_r(SseOpcode::Pxor, RegMem::from(tmp2), tmp2));
                    ctx.emit(Inst::xmm_rm_r(SseOpcode::Pmaxsd, RegMem::from(tmp2), tmp1));

                    // Add this second set of converted lanes to the original to properly handle
                    // values greater than max signed int.
                    ctx.emit(Inst::xmm_rm_r(SseOpcode::Paddd, RegMem::from(tmp1), dst));
                } else {
                    // Since this branch is also guarded by a check for vector types
                    // neither Opcode::FcvtToUint nor Opcode::FcvtToSint can reach here
                    // due to vector varients not existing. The first two branches will
                    // cover all reachable cases.
                    unreachable!();
                }
            }
        }
        Opcode::IaddPairwise => {
            if let (Some(swiden_low), Some(swiden_high)) = (
                matches_input(ctx, inputs[0], Opcode::SwidenLow),
                matches_input(ctx, inputs[1], Opcode::SwidenHigh),
            ) {
                let swiden_input = &[
                    InsnInput {
                        insn: swiden_low,
                        input: 0,
                    },
                    InsnInput {
                        insn: swiden_high,
                        input: 0,
                    },
                ];

                let input_ty = ctx.input_ty(swiden_low, 0);
                let output_ty = ctx.output_ty(insn, 0);
                let src0 = put_input_in_reg(ctx, swiden_input[0]);
                let src1 = put_input_in_reg(ctx, swiden_input[1]);
                let dst = get_output_reg(ctx, outputs[0]).only_reg().unwrap();
                if src0 != src1 {
                    unimplemented!(
                        "iadd_pairwise not implemented for general case with different inputs"
                    );
                }
                match (input_ty, output_ty) {
                    (types::I8X16, types::I16X8) => {
                        static MUL_CONST: [u8; 16] = [0x01; 16];
                        let mul_const = ctx.use_constant(VCodeConstantData::WellKnown(&MUL_CONST));
                        let mul_const_reg = ctx.alloc_tmp(types::I8X16).only_reg().unwrap();
                        ctx.emit(Inst::xmm_load_const(mul_const, mul_const_reg, types::I8X16));
                        ctx.emit(Inst::xmm_mov(
                            SseOpcode::Movdqa,
                            RegMem::reg(mul_const_reg.to_reg()),
                            dst,
                        ));
                        ctx.emit(Inst::xmm_rm_r(SseOpcode::Pmaddubsw, RegMem::reg(src0), dst));
                    }
                    (types::I16X8, types::I32X4) => {
                        static MUL_CONST: [u8; 16] = [
                            0x01, 0x00, 0x01, 0x00, 0x01, 0x00, 0x01, 0x00, 0x01, 0x00, 0x01, 0x00,
                            0x01, 0x00, 0x01, 0x00,
                        ];
                        let mul_const = ctx.use_constant(VCodeConstantData::WellKnown(&MUL_CONST));
                        let mul_const_reg = ctx.alloc_tmp(types::I16X8).only_reg().unwrap();
                        ctx.emit(Inst::xmm_load_const(mul_const, mul_const_reg, types::I16X8));
                        ctx.emit(Inst::xmm_mov(SseOpcode::Movdqa, RegMem::reg(src0), dst));
                        ctx.emit(Inst::xmm_rm_r(
                            SseOpcode::Pmaddwd,
                            RegMem::reg(mul_const_reg.to_reg()),
                            dst,
                        ));
                    }
                    _ => {
                        unimplemented!("Type not supported for {:?}", op);
                    }
                }
            } else if let (Some(uwiden_low), Some(uwiden_high)) = (
                matches_input(ctx, inputs[0], Opcode::UwidenLow),
                matches_input(ctx, inputs[1], Opcode::UwidenHigh),
            ) {
                let uwiden_input = &[
                    InsnInput {
                        insn: uwiden_low,
                        input: 0,
                    },
                    InsnInput {
                        insn: uwiden_high,
                        input: 0,
                    },
                ];

                let input_ty = ctx.input_ty(uwiden_low, 0);
                let output_ty = ctx.output_ty(insn, 0);
                let src0 = put_input_in_reg(ctx, uwiden_input[0]);
                let src1 = put_input_in_reg(ctx, uwiden_input[1]);
                let dst = get_output_reg(ctx, outputs[0]).only_reg().unwrap();
                if src0 != src1 {
                    unimplemented!(
                        "iadd_pairwise not implemented for general case with different inputs"
                    );
                }
                match (input_ty, output_ty) {
                    (types::I8X16, types::I16X8) => {
                        static MUL_CONST: [u8; 16] = [0x01; 16];
                        let mul_const = ctx.use_constant(VCodeConstantData::WellKnown(&MUL_CONST));
                        let mul_const_reg = ctx.alloc_tmp(types::I8X16).only_reg().unwrap();
                        ctx.emit(Inst::xmm_load_const(mul_const, mul_const_reg, types::I8X16));
                        ctx.emit(Inst::xmm_mov(SseOpcode::Movdqa, RegMem::reg(src0), dst));
                        ctx.emit(Inst::xmm_rm_r(
                            SseOpcode::Pmaddubsw,
                            RegMem::reg(mul_const_reg.to_reg()),
                            dst,
                        ));
                    }
                    (types::I16X8, types::I32X4) => {
                        static PXOR_CONST: [u8; 16] = [
                            0x00, 0x80, 0x00, 0x80, 0x00, 0x80, 0x00, 0x80, 0x00, 0x80, 0x00, 0x80,
                            0x00, 0x80, 0x00, 0x80,
                        ];
                        let pxor_const =
                            ctx.use_constant(VCodeConstantData::WellKnown(&PXOR_CONST));
                        let pxor_const_reg = ctx.alloc_tmp(types::I16X8).only_reg().unwrap();
                        ctx.emit(Inst::xmm_load_const(
                            pxor_const,
                            pxor_const_reg,
                            types::I16X8,
                        ));
                        ctx.emit(Inst::xmm_mov(SseOpcode::Movdqa, RegMem::reg(src0), dst));
                        ctx.emit(Inst::xmm_rm_r(
                            SseOpcode::Pxor,
                            RegMem::reg(pxor_const_reg.to_reg()),
                            dst,
                        ));

                        static MADD_CONST: [u8; 16] = [
                            0x01, 0x00, 0x01, 0x00, 0x01, 0x00, 0x01, 0x00, 0x01, 0x00, 0x01, 0x00,
                            0x01, 0x00, 0x01, 0x00,
                        ];
                        let madd_const =
                            ctx.use_constant(VCodeConstantData::WellKnown(&MADD_CONST));
                        let madd_const_reg = ctx.alloc_tmp(types::I8X16).only_reg().unwrap();
                        ctx.emit(Inst::xmm_load_const(
                            madd_const,
                            madd_const_reg,
                            types::I16X8,
                        ));
                        ctx.emit(Inst::xmm_rm_r(
                            SseOpcode::Pmaddwd,
                            RegMem::reg(madd_const_reg.to_reg()),
                            dst,
                        ));
                        static ADDD_CONST2: [u8; 16] = [
                            0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00,
                            0x00, 0x00, 0x01, 0x00,
                        ];
                        let addd_const2 =
                            ctx.use_constant(VCodeConstantData::WellKnown(&ADDD_CONST2));
                        let addd_const2_reg = ctx.alloc_tmp(types::I8X16).only_reg().unwrap();
                        ctx.emit(Inst::xmm_load_const(
                            addd_const2,
                            addd_const2_reg,
                            types::I16X8,
                        ));
                        ctx.emit(Inst::xmm_rm_r(
                            SseOpcode::Paddd,
                            RegMem::reg(addd_const2_reg.to_reg()),
                            dst,
                        ));
                    }
                    _ => {
                        unimplemented!("Type not supported for {:?}", op);
                    }
                }
            } else {
                unimplemented!("Operands not supported for {:?}", op);
            }
        }
        Opcode::UwidenHigh | Opcode::UwidenLow | Opcode::SwidenHigh | Opcode::SwidenLow => {
            let input_ty = ctx.input_ty(insn, 0);
            let output_ty = ctx.output_ty(insn, 0);
            let src = put_input_in_reg(ctx, inputs[0]);
            let dst = get_output_reg(ctx, outputs[0]).only_reg().unwrap();
            if output_ty.is_vector() {
                match op {
                    Opcode::SwidenLow => match (input_ty, output_ty) {
                        (types::I8X16, types::I16X8) => {
                            ctx.emit(Inst::xmm_mov(SseOpcode::Pmovsxbw, RegMem::reg(src), dst));
                        }
                        (types::I16X8, types::I32X4) => {
                            ctx.emit(Inst::xmm_mov(SseOpcode::Pmovsxwd, RegMem::reg(src), dst));
                        }
                        (types::I32X4, types::I64X2) => {
                            ctx.emit(Inst::xmm_mov(SseOpcode::Pmovsxdq, RegMem::reg(src), dst));
                        }
                        _ => unreachable!(),
                    },
                    Opcode::SwidenHigh => match (input_ty, output_ty) {
                        (types::I8X16, types::I16X8) => {
                            ctx.emit(Inst::gen_move(dst, src, output_ty));
                            ctx.emit(Inst::xmm_rm_r_imm(
                                SseOpcode::Palignr,
                                RegMem::reg(src),
                                dst,
                                8,
                                OperandSize::Size32,
                            ));
                            ctx.emit(Inst::xmm_mov(SseOpcode::Pmovsxbw, RegMem::from(dst), dst));
                        }
                        (types::I16X8, types::I32X4) => {
                            ctx.emit(Inst::gen_move(dst, src, output_ty));
                            ctx.emit(Inst::xmm_rm_r_imm(
                                SseOpcode::Palignr,
                                RegMem::reg(src),
                                dst,
                                8,
                                OperandSize::Size32,
                            ));
                            ctx.emit(Inst::xmm_mov(SseOpcode::Pmovsxwd, RegMem::from(dst), dst));
                        }
                        (types::I32X4, types::I64X2) => {
                            ctx.emit(Inst::xmm_rm_r_imm(
                                SseOpcode::Pshufd,
                                RegMem::reg(src),
                                dst,
                                0xEE,
                                OperandSize::Size32,
                            ));
                            ctx.emit(Inst::xmm_mov(SseOpcode::Pmovsxdq, RegMem::from(dst), dst));
                        }
                        _ => unreachable!(),
                    },
                    Opcode::UwidenLow => match (input_ty, output_ty) {
                        (types::I8X16, types::I16X8) => {
                            ctx.emit(Inst::xmm_mov(SseOpcode::Pmovzxbw, RegMem::reg(src), dst));
                        }
                        (types::I16X8, types::I32X4) => {
                            ctx.emit(Inst::xmm_mov(SseOpcode::Pmovzxwd, RegMem::reg(src), dst));
                        }
                        (types::I32X4, types::I64X2) => {
                            ctx.emit(Inst::xmm_mov(SseOpcode::Pmovzxdq, RegMem::reg(src), dst));
                        }
                        _ => unreachable!(),
                    },
                    Opcode::UwidenHigh => match (input_ty, output_ty) {
                        (types::I8X16, types::I16X8) => {
                            ctx.emit(Inst::gen_move(dst, src, output_ty));
                            ctx.emit(Inst::xmm_rm_r_imm(
                                SseOpcode::Palignr,
                                RegMem::reg(src),
                                dst,
                                8,
                                OperandSize::Size32,
                            ));
                            ctx.emit(Inst::xmm_mov(SseOpcode::Pmovzxbw, RegMem::from(dst), dst));
                        }
                        (types::I16X8, types::I32X4) => {
                            ctx.emit(Inst::gen_move(dst, src, output_ty));
                            ctx.emit(Inst::xmm_rm_r_imm(
                                SseOpcode::Palignr,
                                RegMem::reg(src),
                                dst,
                                8,
                                OperandSize::Size32,
                            ));
                            ctx.emit(Inst::xmm_mov(SseOpcode::Pmovzxwd, RegMem::from(dst), dst));
                        }
                        (types::I32X4, types::I64X2) => {
                            ctx.emit(Inst::xmm_rm_r_imm(
                                SseOpcode::Pshufd,
                                RegMem::reg(src),
                                dst,
                                0xEE,
                                OperandSize::Size32,
                            ));
                            ctx.emit(Inst::xmm_mov(SseOpcode::Pmovzxdq, RegMem::from(dst), dst));
                        }
                        _ => unreachable!(),
                    },
                    _ => unreachable!(),
                }
            } else {
                panic!("Unsupported non-vector type for widen instruction {:?}", ty);
            }
        }
        Opcode::Snarrow | Opcode::Unarrow => {
            let input_ty = ctx.input_ty(insn, 0);
            let output_ty = ctx.output_ty(insn, 0);
            let dst = get_output_reg(ctx, outputs[0]).only_reg().unwrap();
            if output_ty.is_vector() {
                match op {
                    Opcode::Snarrow => match (input_ty, output_ty) {
                        (types::I16X8, types::I8X16) => {
                            let src1 = put_input_in_reg(ctx, inputs[0]);
                            let src2 = put_input_in_reg(ctx, inputs[1]);
                            ctx.emit(Inst::gen_move(dst, src1, input_ty));
                            ctx.emit(Inst::xmm_rm_r(SseOpcode::Packsswb, RegMem::reg(src2), dst));
                        }
                        (types::I32X4, types::I16X8) => {
                            let src1 = put_input_in_reg(ctx, inputs[0]);
                            let src2 = put_input_in_reg(ctx, inputs[1]);
                            ctx.emit(Inst::gen_move(dst, src1, input_ty));
                            ctx.emit(Inst::xmm_rm_r(SseOpcode::Packssdw, RegMem::reg(src2), dst));
                        }
                        // TODO: The type we are expecting as input as actually an F64X2 but the instruction is only defined
                        // for integers so here we use I64X2. This is a separate issue that needs to be fixed in instruction.rs.
                        (types::I64X2, types::I32X4) => {
                            if let Some(fcvt_inst) =
                                matches_input(ctx, inputs[0], Opcode::FcvtToSintSat)
                            {
                                //y = i32x4.trunc_sat_f64x2_s_zero(x) is lowered to:
                                //MOVE xmm_tmp, xmm_x
                                //CMPEQPD xmm_tmp, xmm_x
                                //MOVE xmm_y, xmm_x
                                //ANDPS xmm_tmp, [wasm_f64x2_splat(2147483647.0)]
                                //MINPD xmm_y, xmm_tmp
                                //CVTTPD2DQ xmm_y, xmm_y

                                let fcvt_input = InsnInput {
                                    insn: fcvt_inst,
                                    input: 0,
                                };
                                let src = put_input_in_reg(ctx, fcvt_input);
                                ctx.emit(Inst::gen_move(dst, src, input_ty));
                                let tmp1 = ctx.alloc_tmp(output_ty).only_reg().unwrap();
                                ctx.emit(Inst::gen_move(tmp1, src, input_ty));
                                let cond = FcmpImm::from(FloatCC::Equal);
                                ctx.emit(Inst::xmm_rm_r_imm(
                                    SseOpcode::Cmppd,
                                    RegMem::reg(src),
                                    tmp1,
                                    cond.encode(),
                                    OperandSize::Size32,
                                ));

                                // 2147483647.0 is equivalent to 0x41DFFFFFFFC00000
                                static UMAX_MASK: [u8; 16] = [
                                    0x00, 0x00, 0xC0, 0xFF, 0xFF, 0xFF, 0xDF, 0x41, 0x00, 0x00,
                                    0xC0, 0xFF, 0xFF, 0xFF, 0xDF, 0x41,
                                ];
                                let umax_const =
                                    ctx.use_constant(VCodeConstantData::WellKnown(&UMAX_MASK));
                                let umax_mask = ctx.alloc_tmp(types::F64X2).only_reg().unwrap();
                                ctx.emit(Inst::xmm_load_const(umax_const, umax_mask, types::F64X2));

                                //ANDPD xmm_y, [wasm_f64x2_splat(2147483647.0)]
                                ctx.emit(Inst::xmm_rm_r(
                                    SseOpcode::Andps,
                                    RegMem::from(umax_mask),
                                    tmp1,
                                ));
                                ctx.emit(Inst::xmm_rm_r(SseOpcode::Minpd, RegMem::from(tmp1), dst));
                                ctx.emit(Inst::xmm_unary_rm_r(
                                    SseOpcode::Cvttpd2dq,
                                    RegMem::from(dst),
                                    dst,
                                ));
                            } else {
                                unreachable!();
                            }
                        }
                        _ => unreachable!(),
                    },
                    Opcode::Unarrow => match (input_ty, output_ty) {
                        (types::I16X8, types::I8X16) => {
                            let src1 = put_input_in_reg(ctx, inputs[0]);
                            let src2 = put_input_in_reg(ctx, inputs[1]);
                            ctx.emit(Inst::gen_move(dst, src1, input_ty));
                            ctx.emit(Inst::xmm_rm_r(SseOpcode::Packuswb, RegMem::reg(src2), dst));
                        }
                        (types::I32X4, types::I16X8) => {
                            let src1 = put_input_in_reg(ctx, inputs[0]);
                            let src2 = put_input_in_reg(ctx, inputs[1]);
                            ctx.emit(Inst::gen_move(dst, src1, input_ty));
                            ctx.emit(Inst::xmm_rm_r(SseOpcode::Packusdw, RegMem::reg(src2), dst));
                        }
                        _ => unreachable!(),
                    },
                    _ => unreachable!(),
                }
            } else {
                panic!("Unsupported non-vector type for widen instruction {:?}", ty);
            }
        }
        Opcode::Bitcast => {
            let input_ty = ctx.input_ty(insn, 0);
            let output_ty = ctx.output_ty(insn, 0);
            match (input_ty, output_ty) {
                (types::F32, types::I32) => {
                    let src = put_input_in_reg(ctx, inputs[0]);
                    let dst = get_output_reg(ctx, outputs[0]).only_reg().unwrap();
                    ctx.emit(Inst::xmm_to_gpr(
                        SseOpcode::Movd,
                        src,
                        dst,
                        OperandSize::Size32,
                    ));
                }
                (types::I32, types::F32) => {
                    let src = input_to_reg_mem(ctx, inputs[0]);
                    let dst = get_output_reg(ctx, outputs[0]).only_reg().unwrap();
                    ctx.emit(Inst::gpr_to_xmm(
                        SseOpcode::Movd,
                        src,
                        OperandSize::Size32,
                        dst,
                    ));
                }
                (types::F64, types::I64) => {
                    let src = put_input_in_reg(ctx, inputs[0]);
                    let dst = get_output_reg(ctx, outputs[0]).only_reg().unwrap();
                    ctx.emit(Inst::xmm_to_gpr(
                        SseOpcode::Movq,
                        src,
                        dst,
                        OperandSize::Size64,
                    ));
                }
                (types::I64, types::F64) => {
                    let src = input_to_reg_mem(ctx, inputs[0]);
                    let dst = get_output_reg(ctx, outputs[0]).only_reg().unwrap();
                    ctx.emit(Inst::gpr_to_xmm(
                        SseOpcode::Movq,
                        src,
                        OperandSize::Size64,
                        dst,
                    ));
                }
                _ => unreachable!("invalid bitcast from {:?} to {:?}", input_ty, output_ty),
            }
        }

        Opcode::Fabs | Opcode::Fneg => {
            let src = RegMem::reg(put_input_in_reg(ctx, inputs[0]));
            let dst = get_output_reg(ctx, outputs[0]).only_reg().unwrap();

            // In both cases, generate a constant and apply a single binary instruction:
            // - to compute the absolute value, set all bits to 1 but the MSB to 0, and bit-AND the
            // src with it.
            // - to compute the negated value, set all bits to 0 but the MSB to 1, and bit-XOR the
            // src with it.
            let output_ty = ty.unwrap();
            if !output_ty.is_vector() {
                let (val, opcode): (u64, _) = match output_ty {
                    types::F32 => match op {
                        Opcode::Fabs => (0x7fffffff, SseOpcode::Andps),
                        Opcode::Fneg => (0x80000000, SseOpcode::Xorps),
                        _ => unreachable!(),
                    },
                    types::F64 => match op {
                        Opcode::Fabs => (0x7fffffffffffffff, SseOpcode::Andpd),
                        Opcode::Fneg => (0x8000000000000000, SseOpcode::Xorpd),
                        _ => unreachable!(),
                    },
                    _ => panic!("unexpected type {:?} for Fabs", output_ty),
                };

                for inst in Inst::gen_constant(ValueRegs::one(dst), val as u128, output_ty, |ty| {
                    ctx.alloc_tmp(ty).only_reg().unwrap()
                }) {
                    ctx.emit(inst);
                }

                ctx.emit(Inst::xmm_rm_r(opcode, src, dst));
            } else {
                // Eventually vector constants should be available in `gen_constant` and this block
                // can be merged with the one above (TODO).
                if output_ty.bits() == 128 {
                    // Move the `lhs` to the same register as `dst`; this may not emit an actual move
                    // but ensures that the registers are the same to match x86's read-write operand
                    // encoding.
                    let src = put_input_in_reg(ctx, inputs[0]);
                    ctx.emit(Inst::gen_move(dst, src, output_ty));

                    // Generate an all 1s constant in an XMM register. This uses CMPPS but could
                    // have used CMPPD with the same effect. Note, we zero the temp we allocate
                    // because if not, there is a chance that the register we use could be initialized
                    // with NaN .. in which case the CMPPS would fail since NaN != NaN.
                    let tmp = ctx.alloc_tmp(output_ty).only_reg().unwrap();
                    ctx.emit(Inst::xmm_rm_r(SseOpcode::Xorps, RegMem::from(tmp), tmp));
                    let cond = FcmpImm::from(FloatCC::Equal);
                    let cmpps = Inst::xmm_rm_r_imm(
                        SseOpcode::Cmpps,
                        RegMem::reg(tmp.to_reg()),
                        tmp,
                        cond.encode(),
                        OperandSize::Size32,
                    );
                    ctx.emit(cmpps);

                    // Shift the all 1s constant to generate the mask.
                    let lane_bits = output_ty.lane_bits();
                    let (shift_opcode, opcode, shift_by) = match (op, lane_bits) {
                        (Opcode::Fabs, _) => {
                            unreachable!(
                                "implemented in ISLE: inst = `{}`, type = `{:?}`",
                                ctx.dfg().display_inst(insn),
                                ty
                            );
                        }
                        (Opcode::Fneg, 32) => (SseOpcode::Pslld, SseOpcode::Xorps, 31),
                        (Opcode::Fneg, 64) => (SseOpcode::Psllq, SseOpcode::Xorpd, 63),
                        _ => unreachable!(
                            "unexpected opcode and lane size: {:?}, {} bits",
                            op, lane_bits
                        ),
                    };
                    let shift = Inst::xmm_rmi_reg(shift_opcode, RegMemImm::imm(shift_by), tmp);
                    ctx.emit(shift);

                    // Apply shifted mask (XOR or AND).
                    let mask = Inst::xmm_rm_r(opcode, RegMem::reg(tmp.to_reg()), dst);
                    ctx.emit(mask);
                } else {
                    panic!("unexpected type {:?} for Fabs", output_ty);
                }
            }
        }

        Opcode::Fcopysign => {
            let dst = get_output_reg(ctx, outputs[0]).only_reg().unwrap();
            let lhs = put_input_in_reg(ctx, inputs[0]);
            let rhs = put_input_in_reg(ctx, inputs[1]);

            let ty = ty.unwrap();

            // We're going to generate the following sequence:
            //
            // movabs     $INT_MIN, tmp_gpr1
            // mov{d,q}   tmp_gpr1, tmp_xmm1
            // movap{s,d} tmp_xmm1, dst
            // andnp{s,d} src_1, dst
            // movap{s,d} src_2, tmp_xmm2
            // andp{s,d}  tmp_xmm1, tmp_xmm2
            // orp{s,d}   tmp_xmm2, dst

            let tmp_xmm1 = ctx.alloc_tmp(types::F32).only_reg().unwrap();
            let tmp_xmm2 = ctx.alloc_tmp(types::F32).only_reg().unwrap();

            let (sign_bit_cst, mov_op, and_not_op, and_op, or_op) = match ty {
                types::F32 => (
                    0x8000_0000,
                    SseOpcode::Movaps,
                    SseOpcode::Andnps,
                    SseOpcode::Andps,
                    SseOpcode::Orps,
                ),
                types::F64 => (
                    0x8000_0000_0000_0000,
                    SseOpcode::Movapd,
                    SseOpcode::Andnpd,
                    SseOpcode::Andpd,
                    SseOpcode::Orpd,
                ),
                _ => {
                    panic!("unexpected type {:?} for copysign", ty);
                }
            };

            for inst in Inst::gen_constant(ValueRegs::one(tmp_xmm1), sign_bit_cst, ty, |ty| {
                ctx.alloc_tmp(ty).only_reg().unwrap()
            }) {
                ctx.emit(inst);
            }
            ctx.emit(Inst::xmm_mov(mov_op, RegMem::reg(tmp_xmm1.to_reg()), dst));
            ctx.emit(Inst::xmm_rm_r(and_not_op, RegMem::reg(lhs), dst));
            ctx.emit(Inst::xmm_mov(mov_op, RegMem::reg(rhs), tmp_xmm2));
            ctx.emit(Inst::xmm_rm_r(
                and_op,
                RegMem::reg(tmp_xmm1.to_reg()),
                tmp_xmm2,
            ));
            ctx.emit(Inst::xmm_rm_r(or_op, RegMem::reg(tmp_xmm2.to_reg()), dst));
        }

        Opcode::Ceil | Opcode::Floor | Opcode::Nearest | Opcode::Trunc => {
            let ty = ty.unwrap();
            if isa_flags.use_sse41() {
                let mode = match op {
                    Opcode::Ceil => RoundImm::RoundUp,
                    Opcode::Floor => RoundImm::RoundDown,
                    Opcode::Nearest => RoundImm::RoundNearest,
                    Opcode::Trunc => RoundImm::RoundZero,
                    _ => panic!("unexpected opcode {:?} in Ceil/Floor/Nearest/Trunc", op),
                };
                let op = match ty {
                    types::F32 => SseOpcode::Roundss,
                    types::F64 => SseOpcode::Roundsd,
                    types::F32X4 => SseOpcode::Roundps,
                    types::F64X2 => SseOpcode::Roundpd,
                    _ => panic!("unexpected type {:?} in Ceil/Floor/Nearest/Trunc", ty),
                };
                let src = input_to_reg_mem(ctx, inputs[0]);
                let dst = get_output_reg(ctx, outputs[0]).only_reg().unwrap();
                ctx.emit(Inst::xmm_rm_r_imm(
                    op,
                    src,
                    dst,
                    mode.encode(),
                    OperandSize::Size32,
                ));
            } else {
                // Lower to VM calls when there's no access to SSE4.1.
                // Note, for vector types on platforms that don't support sse41
                // the execution will panic here.
                let libcall = match (op, ty) {
                    (Opcode::Ceil, types::F32) => LibCall::CeilF32,
                    (Opcode::Ceil, types::F64) => LibCall::CeilF64,
                    (Opcode::Floor, types::F32) => LibCall::FloorF32,
                    (Opcode::Floor, types::F64) => LibCall::FloorF64,
                    (Opcode::Nearest, types::F32) => LibCall::NearestF32,
                    (Opcode::Nearest, types::F64) => LibCall::NearestF64,
                    (Opcode::Trunc, types::F32) => LibCall::TruncF32,
                    (Opcode::Trunc, types::F64) => LibCall::TruncF64,
                    _ => panic!(
                        "unexpected type/opcode {:?}/{:?} in Ceil/Floor/Nearest/Trunc",
                        ty, op
                    ),
                };

                let input = put_input_in_reg(ctx, inputs[0]);
                let dst = get_output_reg(ctx, outputs[0]).only_reg().unwrap();

                emit_vm_call(ctx, flags, triple, libcall, &[input], &[dst])?;
            }
        }

        Opcode::DynamicStackAddr => unimplemented!("DynamicStackAddr"),

        Opcode::StackAddr => {
            let (stack_slot, offset) = match *ctx.data(insn) {
                InstructionData::StackLoad {
                    opcode: Opcode::StackAddr,
                    stack_slot,
                    offset,
                } => (stack_slot, offset),
                _ => unreachable!(),
            };
            let dst = get_output_reg(ctx, outputs[0]).only_reg().unwrap();
            let offset: i32 = offset.into();
            let inst =
                ctx.abi()
                    .sized_stackslot_addr(stack_slot, u32::try_from(offset).unwrap(), dst);
            ctx.emit(inst);
        }

        Opcode::Udiv | Opcode::Urem | Opcode::Sdiv | Opcode::Srem => {
            let kind = match op {
                Opcode::Udiv => DivOrRemKind::UnsignedDiv,
                Opcode::Sdiv => DivOrRemKind::SignedDiv,
                Opcode::Urem => DivOrRemKind::UnsignedRem,
                Opcode::Srem => DivOrRemKind::SignedRem,
                _ => unreachable!(),
            };
            let is_div = kind.is_div();

            let input_ty = ctx.input_ty(insn, 0);
            let size = OperandSize::from_ty(input_ty);

            let dividend = put_input_in_reg(ctx, inputs[0]);
            let dst = get_output_reg(ctx, outputs[0]).only_reg().unwrap();

            ctx.emit(Inst::gen_move(
                Writable::from_reg(regs::rax()),
                dividend,
                input_ty,
            ));

            // Always do explicit checks for `srem`: otherwise, INT_MIN % -1 is not handled properly.
            if flags.avoid_div_traps() || op == Opcode::Srem {
                // A vcode meta-instruction is used to lower the inline checks, since they embed
                // pc-relative offsets that must not change, thus requiring regalloc to not
                // interfere by introducing spills and reloads.
                //
                // Note it keeps the result in $rax (for divide) or $rdx (for rem), so that
                // regalloc is aware of the coalescing opportunity between rax/rdx and the
                // destination register.
                let divisor = put_input_in_reg(ctx, inputs[1]);

                let divisor_copy = ctx.alloc_tmp(types::I64).only_reg().unwrap();
                ctx.emit(Inst::gen_move(divisor_copy, divisor, types::I64));

                let tmp = if op == Opcode::Sdiv && size == OperandSize::Size64 {
                    Some(ctx.alloc_tmp(types::I64).only_reg().unwrap())
                } else {
                    None
                };
                // TODO use xor
                ctx.emit(Inst::imm(
                    OperandSize::Size32,
                    0,
                    Writable::from_reg(regs::rdx()),
                ));
                ctx.emit(Inst::checked_div_or_rem_seq(kind, size, divisor_copy, tmp));
            } else {
                // We don't want more than one trap record for a single instruction,
                // so let's not allow the "mem" case (load-op merging) here; force
                // divisor into a register instead.
                let divisor = RegMem::reg(put_input_in_reg(ctx, inputs[1]));

                // Fill in the high parts:
                if kind.is_signed() {
                    // sign-extend the sign-bit of al into ah for size 1, or rax into rdx, for
                    // signed opcodes.
                    ctx.emit(Inst::sign_extend_data(size));
                } else if input_ty == types::I8 {
                    ctx.emit(Inst::movzx_rm_r(
                        ExtMode::BL,
                        RegMem::reg(regs::rax()),
                        Writable::from_reg(regs::rax()),
                    ));
                } else {
                    // zero for unsigned opcodes.
                    ctx.emit(Inst::imm(
                        OperandSize::Size64,
                        0,
                        Writable::from_reg(regs::rdx()),
                    ));
                }

                // Emit the actual idiv.
                ctx.emit(Inst::div(size, kind.is_signed(), divisor));
            }

            // Move the result back into the destination reg.
            if is_div {
                // The quotient is in rax.
                ctx.emit(Inst::gen_move(dst, regs::rax(), input_ty));
            } else {
                if size == OperandSize::Size8 {
                    // The remainder is in AH. Right-shift by 8 bits then move from rax.
                    ctx.emit(Inst::shift_r(
                        OperandSize::Size64,
                        ShiftKind::ShiftRightLogical,
                        Some(8),
                        Writable::from_reg(regs::rax()),
                    ));
                    ctx.emit(Inst::gen_move(dst, regs::rax(), input_ty));
                } else {
                    // The remainder is in rdx.
                    ctx.emit(Inst::gen_move(dst, regs::rdx(), input_ty));
                }
            }
        }

        Opcode::Umulhi | Opcode::Smulhi => {
            let input_ty = ctx.input_ty(insn, 0);

            let lhs = put_input_in_reg(ctx, inputs[0]);
            let rhs = input_to_reg_mem(ctx, inputs[1]);
            let dst = get_output_reg(ctx, outputs[0]).only_reg().unwrap();

            // Move lhs in %rax.
            ctx.emit(Inst::gen_move(
                Writable::from_reg(regs::rax()),
                lhs,
                input_ty,
            ));

            // Emit the actual mul or imul.
            let signed = op == Opcode::Smulhi;
            ctx.emit(Inst::mul_hi(OperandSize::from_ty(input_ty), signed, rhs));

            // Read the result from the high part (stored in %rdx).
            ctx.emit(Inst::gen_move(dst, regs::rdx(), input_ty));
        }

        Opcode::GetPinnedReg => {
            let dst = get_output_reg(ctx, outputs[0]).only_reg().unwrap();
            ctx.emit(Inst::gen_move(dst, regs::pinned_reg(), types::I64));
        }

        Opcode::SetPinnedReg => {
            let src = put_input_in_reg(ctx, inputs[0]);
            ctx.emit(Inst::gen_move(
                Writable::from_reg(regs::pinned_reg()),
                src,
                types::I64,
            ));
        }

        Opcode::Vconst => {
            let used_constant = if let &InstructionData::UnaryConst {
                constant_handle, ..
            } = ctx.data(insn)
            {
                ctx.use_constant(VCodeConstantData::Pool(
                    constant_handle,
                    ctx.get_constant_data(constant_handle).clone(),
                ))
            } else {
                unreachable!("vconst should always have unary_const format")
            };
            // TODO use Inst::gen_constant() instead.
            let dst = get_output_reg(ctx, outputs[0]).only_reg().unwrap();
            let ty = ty.unwrap();
            ctx.emit(Inst::xmm_load_const(used_constant, dst, ty));
        }

        Opcode::RawBitcast => {
            // A raw_bitcast is just a mechanism for correcting the type of V128 values (see
            // https://github.com/bytecodealliance/wasmtime/issues/1147). As such, this IR
            // instruction should emit no machine code but a move is necessary to give the register
            // allocator a definition for the output virtual register.
            let src = put_input_in_reg(ctx, inputs[0]);
            let dst = get_output_reg(ctx, outputs[0]).only_reg().unwrap();
            let ty = ty.unwrap();
            ctx.emit(Inst::gen_move(dst, src, ty));
        }

        Opcode::Shuffle => {
            let ty = ty.unwrap();
            let dst = get_output_reg(ctx, outputs[0]).only_reg().unwrap();
            let lhs_ty = ctx.input_ty(insn, 0);
            let lhs = put_input_in_reg(ctx, inputs[0]);
            let rhs = put_input_in_reg(ctx, inputs[1]);
            let mask = match ctx.get_immediate(insn) {
                Some(DataValue::V128(bytes)) => bytes.to_vec(),
                _ => unreachable!("shuffle should always have a 16-byte immediate"),
            };

            // A mask-building helper: in 128-bit SIMD, 0-15 indicate which lane to read from and a
            // 1 in the most significant position zeroes the lane.
            let zero_unknown_lane_index = |b: u8| if b > 15 { 0b10000000 } else { b };

            ctx.emit(Inst::gen_move(dst, rhs, ty));
            if rhs == lhs {
                // If `lhs` and `rhs` are the same we can use a single PSHUFB to shuffle the XMM
                // register. We statically build `constructed_mask` to zero out any unknown lane
                // indices (may not be completely necessary: verification could fail incorrect mask
                // values) and fix the indexes to all point to the `dst` vector.
                let constructed_mask = mask
                    .iter()
                    // If the mask is greater than 15 it still may be referring to a lane in b.
                    .map(|&b| if b > 15 { b.wrapping_sub(16) } else { b })
                    .map(zero_unknown_lane_index)
                    .collect();
                let constant = ctx.use_constant(VCodeConstantData::Generated(constructed_mask));
                let tmp = ctx.alloc_tmp(types::I8X16).only_reg().unwrap();
                ctx.emit(Inst::xmm_load_const(constant, tmp, ty));
                // After loading the constructed mask in a temporary register, we use this to
                // shuffle the `dst` register (remember that, in this case, it is the same as
                // `src` so we disregard this register).
                ctx.emit(Inst::xmm_rm_r(SseOpcode::Pshufb, RegMem::from(tmp), dst));
            } else {
                if isa_flags.use_avx512vl_simd() && isa_flags.use_avx512vbmi_simd() {
                    assert!(
                        mask.iter().all(|b| *b < 32),
                        "shuffle mask values must be between 0 and 31"
                    );

                    // Load the mask into the destination register.
                    let constant = ctx.use_constant(VCodeConstantData::Generated(mask.into()));
                    ctx.emit(Inst::xmm_load_const(constant, dst, ty));

                    // VPERMI2B has the exact semantics of Wasm's shuffle:
                    // permute the bytes in `src1` and `src2` using byte indexes
                    // in `dst` and store the byte results in `dst`.
                    ctx.emit(Inst::xmm_rm_r_evex(
                        Avx512Opcode::Vpermi2b,
                        RegMem::reg(rhs),
                        lhs,
                        dst,
                    ));
                } else {
                    // If `lhs` and `rhs` are different, we must shuffle each separately and then OR
                    // them together. This is necessary due to PSHUFB semantics. As in the case above,
                    // we build the `constructed_mask` for each case statically.

                    // PSHUFB the `lhs` argument into `tmp0`, placing zeroes for unused lanes.
                    let tmp0 = ctx.alloc_tmp(lhs_ty).only_reg().unwrap();
                    ctx.emit(Inst::gen_move(tmp0, lhs, lhs_ty));
                    let constructed_mask =
                        mask.iter().cloned().map(zero_unknown_lane_index).collect();
                    let constant = ctx.use_constant(VCodeConstantData::Generated(constructed_mask));
                    let tmp1 = ctx.alloc_tmp(types::I8X16).only_reg().unwrap();
                    ctx.emit(Inst::xmm_load_const(constant, tmp1, ty));
                    ctx.emit(Inst::xmm_rm_r(SseOpcode::Pshufb, RegMem::from(tmp1), tmp0));

                    // PSHUFB the second argument, placing zeroes for unused lanes.
                    let constructed_mask = mask
                        .iter()
                        .map(|b| b.wrapping_sub(16))
                        .map(zero_unknown_lane_index)
                        .collect();
                    let constant = ctx.use_constant(VCodeConstantData::Generated(constructed_mask));
                    let tmp2 = ctx.alloc_tmp(types::I8X16).only_reg().unwrap();
                    ctx.emit(Inst::xmm_load_const(constant, tmp2, ty));
                    ctx.emit(Inst::xmm_rm_r(SseOpcode::Pshufb, RegMem::from(tmp2), dst));

                    // OR the shuffled registers (the mechanism and lane-size for OR-ing the registers
                    // is not important).
                    ctx.emit(Inst::xmm_rm_r(SseOpcode::Orps, RegMem::from(tmp0), dst));
                }
            }
        }

        Opcode::Swizzle => {
            // SIMD swizzle; the following inefficient implementation is due to the Wasm SIMD spec
            // requiring mask indexes greater than 15 to have the same semantics as a 0 index. For
            // the spec discussion, see https://github.com/WebAssembly/simd/issues/93. The CLIF
            // semantics match the Wasm SIMD semantics for this instruction.
            // The instruction format maps to variables like: %dst = swizzle %src, %mask
            let ty = ty.unwrap();
            let dst = get_output_reg(ctx, outputs[0]).only_reg().unwrap();
            let src = put_input_in_reg(ctx, inputs[0]);
            let swizzle_mask = put_input_in_reg(ctx, inputs[1]);

            // Inform the register allocator that `src` and `dst` should be in the same register.
            ctx.emit(Inst::gen_move(dst, src, ty));

            // Create a mask for zeroing out-of-bounds lanes of the swizzle mask.
            let zero_mask = ctx.alloc_tmp(types::I8X16).only_reg().unwrap();
            static ZERO_MASK_VALUE: [u8; 16] = [
                0x70, 0x70, 0x70, 0x70, 0x70, 0x70, 0x70, 0x70, 0x70, 0x70, 0x70, 0x70, 0x70, 0x70,
                0x70, 0x70,
            ];
            let constant = ctx.use_constant(VCodeConstantData::WellKnown(&ZERO_MASK_VALUE));
            ctx.emit(Inst::xmm_load_const(constant, zero_mask, ty));

            // Use the `zero_mask` on a writable `swizzle_mask`.
            let swizzle_mask_tmp = ctx.alloc_tmp(types::I8X16).only_reg().unwrap();
            ctx.emit(Inst::gen_move(swizzle_mask_tmp, swizzle_mask, ty));
            ctx.emit(Inst::xmm_rm_r(
                SseOpcode::Paddusb,
                RegMem::from(zero_mask),
                swizzle_mask_tmp,
            ));

            // Shuffle `dst` using the fixed-up `swizzle_mask`.
            ctx.emit(Inst::xmm_rm_r(
                SseOpcode::Pshufb,
                RegMem::from(swizzle_mask_tmp),
                dst,
            ));
        }

        Opcode::Insertlane => {
            unreachable!(
                "implemented in ISLE: inst = `{}`, type = `{:?}`",
                ctx.dfg().display_inst(insn),
                ty
            );
        }

        Opcode::Extractlane => {
            // The instruction format maps to variables like: %dst = extractlane %src, %lane
            let ty = ty.unwrap();
            let dst = get_output_reg(ctx, outputs[0]).only_reg().unwrap();
            let src_ty = ctx.input_ty(insn, 0);
            assert_eq!(src_ty.bits(), 128);
            let src = put_input_in_reg(ctx, inputs[0]);
            let lane = if let InstructionData::BinaryImm8 { imm, .. } = ctx.data(insn) {
                *imm
            } else {
                unreachable!();
            };
            debug_assert!(lane < src_ty.lane_count() as u8);

            emit_extract_lane(ctx, src, dst, lane, ty);
        }

        Opcode::ScalarToVector => {
            // When moving a scalar value to a vector register, we must be handle several
            // situations:
            //  1. a scalar float is already in an XMM register, so we simply move it
            //  2. a scalar of any other type resides in a GPR register: MOVD moves the bits to an
            //     XMM register and zeroes the upper bits
            //  3. a scalar (float or otherwise) that has previously been loaded from memory (e.g.
            //     the default lowering of Wasm's `load[32|64]_zero`) can be lowered to a single
            //     MOVSS/MOVSD instruction; to do this, we rely on `input_to_reg_mem` to sink the
            //     unused load.
            let src = input_to_reg_mem(ctx, inputs[0]);
            let src_ty = ctx.input_ty(insn, 0);
            let dst = get_output_reg(ctx, outputs[0]).only_reg().unwrap();
            let dst_ty = ty.unwrap();
            assert!(src_ty == dst_ty.lane_type() && dst_ty.bits() == 128);
            match src {
                RegMem::Reg { reg } => {
                    if src_ty.is_float() {
                        // Case 1: when moving a scalar float, we simply move from one XMM register
                        // to another, expecting the register allocator to elide this. Here we
                        // assume that the upper bits of a scalar float have not been munged with
                        // (the same assumption the old backend makes).
                        ctx.emit(Inst::gen_move(dst, reg, dst_ty));
                    } else {
                        // Case 2: when moving a scalar value of any other type, use MOVD to zero
                        // the upper lanes.
                        let src_size = match src_ty.bits() {
                            32 => OperandSize::Size32,
                            64 => OperandSize::Size64,
                            _ => unimplemented!("invalid source size for type: {}", src_ty),
                        };
                        ctx.emit(Inst::gpr_to_xmm(SseOpcode::Movd, src, src_size, dst));
                    }
                }
                RegMem::Mem { .. } => {
                    // Case 3: when presented with `load + scalar_to_vector`, coalesce into a single
                    // MOVSS/MOVSD instruction.
                    let opcode = match src_ty.bits() {
                        32 => SseOpcode::Movss,
                        64 => SseOpcode::Movsd,
                        _ => unimplemented!("unable to move scalar to vector for type: {}", src_ty),
                    };
                    ctx.emit(Inst::xmm_mov(opcode, src, dst));
                }
            }
        }

        Opcode::Splat => {
            let ty = ty.unwrap();
            assert_eq!(ty.bits(), 128);
            let src_ty = ctx.input_ty(insn, 0);
            assert!(src_ty.bits() < 128);

            let src = input_to_reg_mem(ctx, inputs[0]);
            let dst = get_output_reg(ctx, outputs[0]).only_reg().unwrap();

            // We know that splat will overwrite all of the lanes of `dst` but it takes several
            // instructions to do so. Because of the multiple instructions, there is no good way to
            // declare `dst` a `def` except with the following pseudo-instruction.
            ctx.emit(Inst::xmm_uninit_value(dst));

            // TODO: eventually many of these sequences could be optimized with AVX's VBROADCAST*
            // and VPBROADCAST*.
            match ty.lane_bits() {
                8 => {
                    emit_insert_lane(ctx, src, dst, 0, ty.lane_type());
                    // Initialize a register with all 0s.
                    let tmp = ctx.alloc_tmp(ty).only_reg().unwrap();
                    ctx.emit(Inst::xmm_rm_r(SseOpcode::Pxor, RegMem::from(tmp), tmp));
                    // Shuffle the lowest byte lane to all other lanes.
                    ctx.emit(Inst::xmm_rm_r(SseOpcode::Pshufb, RegMem::from(tmp), dst))
                }
                16 => {
                    emit_insert_lane(ctx, src.clone(), dst, 0, ty.lane_type());
                    emit_insert_lane(ctx, src, dst, 1, ty.lane_type());
                    // Shuffle the lowest two lanes to all other lanes.
                    ctx.emit(Inst::xmm_rm_r_imm(
                        SseOpcode::Pshufd,
                        RegMem::from(dst),
                        dst,
                        0,
                        OperandSize::Size32,
                    ))
                }
                32 => {
                    emit_insert_lane(ctx, src, dst, 0, ty.lane_type());
                    // Shuffle the lowest lane to all other lanes.
                    ctx.emit(Inst::xmm_rm_r_imm(
                        SseOpcode::Pshufd,
                        RegMem::from(dst),
                        dst,
                        0,
                        OperandSize::Size32,
                    ))
                }
                64 => {
                    emit_insert_lane(ctx, src.clone(), dst, 0, ty.lane_type());
                    emit_insert_lane(ctx, src, dst, 1, ty.lane_type());
                }
                _ => panic!("Invalid type to splat: {}", ty),
            }
        }

        Opcode::VanyTrue => {
            let dst = get_output_reg(ctx, outputs[0]).only_reg().unwrap();
            let src_ty = ctx.input_ty(insn, 0);
            assert_eq!(src_ty.bits(), 128);
            let src = put_input_in_reg(ctx, inputs[0]);
            // Set the ZF if the result is all zeroes.
            ctx.emit(Inst::xmm_cmp_rm_r(SseOpcode::Ptest, RegMem::reg(src), src));
            // If the ZF is not set, place a 1 in `dst`.
            ctx.emit(Inst::setcc(CC::NZ, dst));
        }

        Opcode::VallTrue => {
            let dst = get_output_reg(ctx, outputs[0]).only_reg().unwrap();
            let src_ty = ctx.input_ty(insn, 0);
            assert_eq!(src_ty.bits(), 128);
            let src = input_to_reg_mem(ctx, inputs[0]);

            let eq = |ty: Type| match ty.lane_bits() {
                8 => SseOpcode::Pcmpeqb,
                16 => SseOpcode::Pcmpeqw,
                32 => SseOpcode::Pcmpeqd,
                64 => SseOpcode::Pcmpeqq,
                _ => panic!("Unable to find an instruction for {} for type: {}", op, ty),
            };

            // Initialize a register with all 0s.
            let tmp = ctx.alloc_tmp(src_ty).only_reg().unwrap();
            ctx.emit(Inst::xmm_rm_r(SseOpcode::Pxor, RegMem::from(tmp), tmp));
            // Compare to see what lanes are filled with all 1s.
            ctx.emit(Inst::xmm_rm_r(eq(src_ty), src, tmp));
            // Set the ZF if the result is all zeroes.
            ctx.emit(Inst::xmm_cmp_rm_r(
                SseOpcode::Ptest,
                RegMem::from(tmp),
                tmp.to_reg(),
            ));
            // If the ZF is set, place a 1 in `dst`.
            ctx.emit(Inst::setcc(CC::Z, dst));
        }

        Opcode::VhighBits => {
            let src = put_input_in_reg(ctx, inputs[0]);
            let src_ty = ctx.input_ty(insn, 0);
            debug_assert!(src_ty.is_vector() && src_ty.bits() == 128);
            let dst = get_output_reg(ctx, outputs[0]).only_reg().unwrap();
            debug_assert!(dst.to_reg().class() == RegClass::Int);

            // The Intel specification allows using both 32-bit and 64-bit GPRs as destination for
            // the "move mask" instructions. This is controlled by the REX.R bit: "In 64-bit mode,
            // the instruction can access additional registers when used with a REX.R prefix. The
            // default operand size is 64-bit in 64-bit mode" (PMOVMSKB in IA Software Development
            // Manual, vol. 2). This being the case, we will always clear REX.W since its use is
            // unnecessary (`OperandSize` is used for setting/clearing REX.W).
            let size = OperandSize::Size32;

            match src_ty {
                types::I8X16 | types::B8X16 => {
                    ctx.emit(Inst::xmm_to_gpr(SseOpcode::Pmovmskb, src, dst, size))
                }
                types::I32X4 | types::B32X4 | types::F32X4 => {
                    ctx.emit(Inst::xmm_to_gpr(SseOpcode::Movmskps, src, dst, size))
                }
                types::I64X2 | types::B64X2 | types::F64X2 => {
                    ctx.emit(Inst::xmm_to_gpr(SseOpcode::Movmskpd, src, dst, size))
                }
                types::I16X8 | types::B16X8 => {
                    // There is no x86 instruction for extracting the high bit of 16-bit lanes so
                    // here we:
                    // - duplicate the 16-bit lanes of `src` into 8-bit lanes:
                    //     PACKSSWB([x1, x2, ...], [x1, x2, ...]) = [x1', x2', ..., x1', x2', ...]
                    // - use PMOVMSKB to gather the high bits; now we have duplicates, though
                    // - shift away the bottom 8 high bits to remove the duplicates.
                    let tmp = ctx.alloc_tmp(src_ty).only_reg().unwrap();
                    ctx.emit(Inst::gen_move(tmp, src, src_ty));
                    ctx.emit(Inst::xmm_rm_r(SseOpcode::Packsswb, RegMem::reg(src), tmp));
                    ctx.emit(Inst::xmm_to_gpr(
                        SseOpcode::Pmovmskb,
                        tmp.to_reg(),
                        dst,
                        size,
                    ));
                    ctx.emit(Inst::shift_r(
                        OperandSize::Size64,
                        ShiftKind::ShiftRightLogical,
                        Some(8),
                        dst,
                    ));
                }
                _ => unimplemented!("unknown input type {} for {}", src_ty, op),
            }
        }

        Opcode::Iconcat => {
            let ty = ctx.output_ty(insn, 0);
            assert_eq!(
                ty,
                types::I128,
                "Iconcat not expected to be used for non-128-bit type"
            );
            assert_eq!(ctx.input_ty(insn, 0), types::I64);
            assert_eq!(ctx.input_ty(insn, 1), types::I64);
            let lo = put_input_in_reg(ctx, inputs[0]);
            let hi = put_input_in_reg(ctx, inputs[1]);
            let dst = get_output_reg(ctx, outputs[0]);
            ctx.emit(Inst::gen_move(dst.regs()[0], lo, types::I64));
            ctx.emit(Inst::gen_move(dst.regs()[1], hi, types::I64));
        }

        Opcode::Isplit => {
            let ty = ctx.input_ty(insn, 0);
            assert_eq!(
                ty,
                types::I128,
                "Isplit not expected to be used for non-128-bit type"
            );
            assert_eq!(ctx.output_ty(insn, 0), types::I64);
            assert_eq!(ctx.output_ty(insn, 1), types::I64);
            let src = put_input_in_regs(ctx, inputs[0]);
            let dst_lo = get_output_reg(ctx, outputs[0]).only_reg().unwrap();
            let dst_hi = get_output_reg(ctx, outputs[1]).only_reg().unwrap();
            ctx.emit(Inst::gen_move(dst_lo, src.regs()[0], types::I64));
            ctx.emit(Inst::gen_move(dst_hi, src.regs()[1], types::I64));
        }

        Opcode::TlsValue => {
            let dst = get_output_reg(ctx, outputs[0]).only_reg().unwrap();
            let (name, _, _) = ctx.symbol_value(insn).unwrap();
            let symbol = name.clone();

            match flags.tls_model() {
                TlsModel::ElfGd => {
                    ctx.emit(Inst::ElfTlsGetAddr { symbol });
                    ctx.emit(Inst::gen_move(dst, regs::rax(), types::I64));
                }
                TlsModel::Macho => {
                    ctx.emit(Inst::MachOTlsGetAddr { symbol });
                    ctx.emit(Inst::gen_move(dst, regs::rax(), types::I64));
                }
                TlsModel::Coff => {
                    ctx.emit(Inst::CoffTlsGetAddr { symbol });
                    ctx.emit(Inst::gen_move(dst, regs::rax(), types::I64));
                }
                _ => todo!(
                    "Unimplemented TLS model in x64 backend: {:?}",
                    flags.tls_model()
                ),
            }
        }

        Opcode::SqmulRoundSat => {
            // Lane-wise saturating rounding multiplication in Q15 format
            // Optimal lowering taken from instruction proposal https://github.com/WebAssembly/simd/pull/365
            // y = i16x8.q15mulr_sat_s(a, b) is lowered to:
            //MOVDQA xmm_y, xmm_a
            //MOVDQA xmm_tmp, wasm_i16x8_splat(0x8000)
            //PMULHRSW xmm_y, xmm_b
            //PCMPEQW xmm_tmp, xmm_y
            //PXOR xmm_y, xmm_tmp
            let input_ty = ctx.input_ty(insn, 0);
            let src1 = put_input_in_reg(ctx, inputs[0]);
            let src2 = put_input_in_reg(ctx, inputs[1]);
            let dst = get_output_reg(ctx, outputs[0]).only_reg().unwrap();

            ctx.emit(Inst::gen_move(dst, src1, input_ty));
            static SAT_MASK: [u8; 16] = [
                0x00, 0x80, 0x00, 0x80, 0x00, 0x80, 0x00, 0x80, 0x00, 0x80, 0x00, 0x80, 0x00, 0x80,
                0x00, 0x80,
            ];
            let mask_const = ctx.use_constant(VCodeConstantData::WellKnown(&SAT_MASK));
            let mask = ctx.alloc_tmp(types::I16X8).only_reg().unwrap();
            ctx.emit(Inst::xmm_load_const(mask_const, mask, types::I16X8));

            ctx.emit(Inst::xmm_rm_r(SseOpcode::Pmulhrsw, RegMem::reg(src2), dst));
            ctx.emit(Inst::xmm_rm_r(
                SseOpcode::Pcmpeqw,
                RegMem::reg(dst.to_reg()),
                mask,
            ));
            ctx.emit(Inst::xmm_rm_r(
                SseOpcode::Pxor,
                RegMem::reg(mask.to_reg()),
                dst,
            ));
        }

        Opcode::Uunarrow => {
            if let Some(fcvt_inst) = matches_input(ctx, inputs[0], Opcode::FcvtToUintSat) {
                //y = i32x4.trunc_sat_f64x2_u_zero(x) is lowered to:
                //MOVAPD xmm_y, xmm_x
                //XORPD xmm_tmp, xmm_tmp
                //MAXPD xmm_y, xmm_tmp
                //MINPD xmm_y, [wasm_f64x2_splat(4294967295.0)]
                //ROUNDPD xmm_y, xmm_y, 0x0B
                //ADDPD xmm_y, [wasm_f64x2_splat(0x1.0p+52)]
                //SHUFPS xmm_y, xmm_xmp, 0x88

                let fcvt_input = InsnInput {
                    insn: fcvt_inst,
                    input: 0,
                };
                let input_ty = ctx.input_ty(fcvt_inst, 0);
                let output_ty = ctx.output_ty(insn, 0);
                let src = put_input_in_reg(ctx, fcvt_input);
                let dst = get_output_reg(ctx, outputs[0]).only_reg().unwrap();

                ctx.emit(Inst::gen_move(dst, src, input_ty));
                let tmp1 = ctx.alloc_tmp(output_ty).only_reg().unwrap();
                ctx.emit(Inst::xmm_rm_r(SseOpcode::Xorpd, RegMem::from(tmp1), tmp1));
                ctx.emit(Inst::xmm_rm_r(SseOpcode::Maxpd, RegMem::from(tmp1), dst));

                // 4294967295.0 is equivalent to 0x41EFFFFFFFE00000
                static UMAX_MASK: [u8; 16] = [
                    0x00, 0x00, 0xE0, 0xFF, 0xFF, 0xFF, 0xEF, 0x41, 0x00, 0x00, 0xE0, 0xFF, 0xFF,
                    0xFF, 0xEF, 0x41,
                ];
                let umax_const = ctx.use_constant(VCodeConstantData::WellKnown(&UMAX_MASK));
                let umax_mask = ctx.alloc_tmp(types::F64X2).only_reg().unwrap();
                ctx.emit(Inst::xmm_load_const(umax_const, umax_mask, types::F64X2));

                //MINPD xmm_y, [wasm_f64x2_splat(4294967295.0)]
                ctx.emit(Inst::xmm_rm_r(
                    SseOpcode::Minpd,
                    RegMem::from(umax_mask),
                    dst,
                ));
                //ROUNDPD xmm_y, xmm_y, 0x0B
                ctx.emit(Inst::xmm_rm_r_imm(
                    SseOpcode::Roundpd,
                    RegMem::reg(dst.to_reg()),
                    dst,
                    RoundImm::RoundZero.encode(),
                    OperandSize::Size32,
                ));
                //ADDPD xmm_y, [wasm_f64x2_splat(0x1.0p+52)]
                static UINT_MASK: [u8; 16] = [
                    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x30, 0x43, 0x00, 0x00, 0x00, 0x00, 0x00,
                    0x00, 0x30, 0x43,
                ];
                let uint_mask_const = ctx.use_constant(VCodeConstantData::WellKnown(&UINT_MASK));
                let uint_mask = ctx.alloc_tmp(types::F64X2).only_reg().unwrap();
                ctx.emit(Inst::xmm_load_const(
                    uint_mask_const,
                    uint_mask,
                    types::F64X2,
                ));
                ctx.emit(Inst::xmm_rm_r(
                    SseOpcode::Addpd,
                    RegMem::from(uint_mask),
                    dst,
                ));

                //SHUFPS xmm_y, xmm_xmp, 0x88
                ctx.emit(Inst::xmm_rm_r_imm(
                    SseOpcode::Shufps,
                    RegMem::reg(tmp1.to_reg()),
                    dst,
                    0x88,
                    OperandSize::Size32,
                ));
            } else {
                println!("Did not match fcvt input!");
            }
        }

        // Unimplemented opcodes below. These are not currently used by Wasm
        // lowering or other known embeddings, but should be either supported or
        // removed eventually
        Opcode::ExtractVector => {
            unimplemented!("ExtractVector not supported");
        }

        Opcode::Cls => unimplemented!("Cls not supported"),

        Opcode::BorNot | Opcode::BxorNot => {
            unimplemented!("or-not / xor-not opcodes not implemented");
        }

        Opcode::Bmask => unimplemented!("Bmask not implemented"),

        Opcode::Trueif | Opcode::Trueff => unimplemented!("trueif / trueff not implemented"),

        Opcode::ConstAddr => unimplemented!("ConstAddr not implemented"),

        Opcode::Vsplit | Opcode::Vconcat => {
            unimplemented!("Vector split/concat ops not implemented.");
        }

        // Opcodes that should be removed by legalization. These should
        // eventually be removed if/when we replace in-situ legalization with
        // something better.
        Opcode::Ifcmp | Opcode::Ffcmp => {
            panic!("Should never reach ifcmp/ffcmp as isel root!");
        }

        Opcode::IaddImm
        | Opcode::ImulImm
        | Opcode::UdivImm
        | Opcode::SdivImm
        | Opcode::UremImm
        | Opcode::SremImm
        | Opcode::IrsubImm
        | Opcode::IaddCin
        | Opcode::IaddIfcin
        | Opcode::IaddCout
        | Opcode::IaddCarry
        | Opcode::IaddIfcarry
        | Opcode::IsubBin
        | Opcode::IsubIfbin
        | Opcode::IsubBout
        | Opcode::IsubIfbout
        | Opcode::IsubBorrow
        | Opcode::IsubIfborrow
        | Opcode::BandImm
        | Opcode::BorImm
        | Opcode::BxorImm
        | Opcode::RotlImm
        | Opcode::RotrImm
        | Opcode::IshlImm
        | Opcode::UshrImm
        | Opcode::SshrImm
        | Opcode::IcmpImm
        | Opcode::IfcmpImm => {
            panic!("ALU+imm and ALU+carry ops should not appear here!");
        }

        Opcode::StackLoad
        | Opcode::StackStore
        | Opcode::DynamicStackStore
        | Opcode::DynamicStackLoad => {
            panic!("Direct stack memory access not supported; should have been legalized");
        }

        Opcode::GlobalValue => {
            panic!("global_value should have been removed by legalization!");
        }

        Opcode::HeapAddr => {
            panic!("heap_addr should have been removed by legalization!");
        }

        Opcode::TableAddr => {
            panic!("table_addr should have been removed by legalization!");
        }

        Opcode::Copy => {
            panic!("Unused opcode should not be encountered.");
        }

        Opcode::Trapz | Opcode::Trapnz | Opcode::ResumableTrapnz => {
            panic!("trapz / trapnz / resumable_trapnz should have been removed by legalization!");
        }

        Opcode::Jump
        | Opcode::Brz
        | Opcode::Brnz
        | Opcode::BrIcmp
        | Opcode::Brif
        | Opcode::Brff
        | Opcode::BrTable => {
            panic!("Branch opcode reached non-branch lowering logic!");
        }

        Opcode::Nop => {
            // Nothing.
        }
    }

    Ok(())
}

//=============================================================================
// Lowering-backend trait implementation.

impl LowerBackend for X64Backend {
    type MInst = Inst;

    fn lower(&self, ctx: &mut Lower<Inst>, ir_inst: IRInst) -> CodegenResult<()> {
        lower_insn_to_regs(ctx, ir_inst, &self.flags, &self.x64_flags, &self.triple)
    }

    fn lower_branch_group(
        &self,
        ctx: &mut Lower<Inst>,
        branches: &[IRInst],
        targets: &[MachLabel],
    ) -> CodegenResult<()> {
        // A block should end with at most two branches. The first may be a
        // conditional branch; a conditional branch can be followed only by an
        // unconditional branch or fallthrough. Otherwise, if only one branch,
        // it may be an unconditional branch, a fallthrough, a return, or a
        // trap. These conditions are verified by `is_ebb_basic()` during the
        // verifier pass.
        assert!(branches.len() <= 2);
        if branches.len() == 2 {
            let op1 = ctx.data(branches[1]).opcode();
            assert!(op1 == Opcode::Jump);
        }

        if let Ok(()) = isle::lower_branch(
            ctx,
            &self.triple,
            &self.flags,
            &self.x64_flags,
            branches[0],
            targets,
        ) {
            return Ok(());
        }

        unreachable!(
            "implemented in ISLE: branch = `{}`",
            ctx.dfg().display_inst(branches[0]),
        );
    }

    fn maybe_pinned_reg(&self) -> Option<Reg> {
        Some(regs::pinned_reg())
    }
}
