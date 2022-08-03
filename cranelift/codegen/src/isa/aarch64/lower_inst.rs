//! Lower a single Cranelift instruction into vcode.

use super::lower::*;
use crate::binemit::CodeOffset;
use crate::ir::condcodes::FloatCC;
use crate::ir::types::*;
use crate::ir::Inst as IRInst;
use crate::ir::{InstructionData, Opcode, TrapCode};
use crate::isa::aarch64::abi::*;
use crate::isa::aarch64::inst::*;
use crate::isa::aarch64::settings as aarch64_settings;
use crate::machinst::lower::*;
use crate::machinst::*;
use crate::settings::{Flags, TlsModel};
use crate::{CodegenError, CodegenResult};
use alloc::boxed::Box;
use alloc::vec::Vec;
use core::convert::TryFrom;
use target_lexicon::Triple;

/// Actually codegen an instruction's results into registers.
pub(crate) fn lower_insn_to_regs<C: LowerCtx<I = Inst>>(
    ctx: &mut C,
    insn: IRInst,
    triple: &Triple,
    flags: &Flags,
    isa_flags: &aarch64_settings::Flags,
) -> CodegenResult<()> {
    let op = ctx.data(insn).opcode();
    let inputs = insn_inputs(ctx, insn);
    let outputs = insn_outputs(ctx, insn);
    let ty = if outputs.len() > 0 {
        Some(ctx.output_ty(insn, 0))
    } else {
        None
    };

    if let Ok(()) = super::lower::isle::lower(ctx, triple, flags, isa_flags, &outputs, insn) {
        return Ok(());
    }

    let implemented_in_isle = |ctx: &mut C| -> ! {
        unreachable!(
            "implemented in ISLE: inst = `{}`, type = `{:?}`",
            ctx.dfg().display_inst(insn),
            ty
        );
    };

    match op {
        Opcode::Iconst | Opcode::Bconst | Opcode::Null => implemented_in_isle(ctx),

        Opcode::F32const | Opcode::F64const => unreachable!(
            "Should never see constant ops at top level lowering entry
            point, as constants are rematerialized at use-sites"
        ),

        Opcode::GetFramePointer | Opcode::GetStackPointer | Opcode::GetReturnAddress => {
            implemented_in_isle(ctx)
        }

        Opcode::Iadd => implemented_in_isle(ctx),
        Opcode::Isub => implemented_in_isle(ctx),
        Opcode::UaddSat | Opcode::SaddSat | Opcode::UsubSat | Opcode::SsubSat => {
            implemented_in_isle(ctx)
        }

        Opcode::Ineg => implemented_in_isle(ctx),

        Opcode::Imul => implemented_in_isle(ctx),

        Opcode::Umulhi | Opcode::Smulhi => implemented_in_isle(ctx),

        Opcode::Udiv | Opcode::Sdiv | Opcode::Urem | Opcode::Srem => implemented_in_isle(ctx),

        Opcode::Uextend | Opcode::Sextend => implemented_in_isle(ctx),

        Opcode::Bnot => implemented_in_isle(ctx),

        Opcode::Band
        | Opcode::Bor
        | Opcode::Bxor
        | Opcode::BandNot
        | Opcode::BorNot
        | Opcode::BxorNot => implemented_in_isle(ctx),

        Opcode::Ishl | Opcode::Ushr | Opcode::Sshr => implemented_in_isle(ctx),

        Opcode::Rotr | Opcode::Rotl => implemented_in_isle(ctx),

        Opcode::Bitrev | Opcode::Clz | Opcode::Cls | Opcode::Ctz => implemented_in_isle(ctx),

        Opcode::Popcnt => implemented_in_isle(ctx),

        Opcode::Load
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
        | Opcode::Uload32x2 => {
            let sign_extend = match op {
                Opcode::Sload8 | Opcode::Sload16 | Opcode::Sload32 => true,
                _ => false,
            };
            let flags = ctx
                .memflags(insn)
                .expect("Load instruction should have memflags");

            let out_ty = ctx.output_ty(insn, 0);
            if out_ty == I128 {
                let off = ctx.data(insn).load_store_offset().unwrap();
                let mem = lower_pair_address(ctx, &inputs[..], off);
                let dst = get_output_reg(ctx, outputs[0]);
                ctx.emit(Inst::LoadP64 {
                    rt: dst.regs()[0],
                    rt2: dst.regs()[1],
                    mem,
                    flags,
                });
            } else {
                lower_load(
                    ctx,
                    insn,
                    &inputs[..],
                    outputs[0],
                    |ctx, dst, mut elem_ty, mem| {
                        if elem_ty.is_dynamic_vector() {
                            elem_ty = dynamic_to_fixed(elem_ty);
                        }
                        let rd = dst.only_reg().unwrap();
                        let is_float = ty_has_float_or_vec_representation(elem_ty);
                        ctx.emit(match (ty_bits(elem_ty), sign_extend, is_float) {
                            (1, _, _) => Inst::ULoad8 { rd, mem, flags },
                            (8, false, _) => Inst::ULoad8 { rd, mem, flags },
                            (8, true, _) => Inst::SLoad8 { rd, mem, flags },
                            (16, false, _) => Inst::ULoad16 { rd, mem, flags },
                            (16, true, _) => Inst::SLoad16 { rd, mem, flags },
                            (32, false, false) => Inst::ULoad32 { rd, mem, flags },
                            (32, true, false) => Inst::SLoad32 { rd, mem, flags },
                            (32, _, true) => Inst::FpuLoad32 { rd, mem, flags },
                            (64, _, false) => Inst::ULoad64 { rd, mem, flags },
                            // Note that we treat some of the vector loads as scalar floating-point loads,
                            // which is correct in a little endian environment.
                            (64, _, true) => Inst::FpuLoad64 { rd, mem, flags },
                            (128, _, true) => Inst::FpuLoad128 { rd, mem, flags },
                            _ => {
                                return Err(CodegenError::Unsupported(format!(
                                    "Unsupported type in load: {:?}",
                                    elem_ty
                                )))
                            }
                        });

                        let vec_extend = match op {
                            Opcode::Sload8x8 => Some(VecExtendOp::Sxtl8),
                            Opcode::Uload8x8 => Some(VecExtendOp::Uxtl8),
                            Opcode::Sload16x4 => Some(VecExtendOp::Sxtl16),
                            Opcode::Uload16x4 => Some(VecExtendOp::Uxtl16),
                            Opcode::Sload32x2 => Some(VecExtendOp::Sxtl32),
                            Opcode::Uload32x2 => Some(VecExtendOp::Uxtl32),
                            _ => None,
                        };

                        if let Some(t) = vec_extend {
                            let rd = dst.only_reg().unwrap();
                            ctx.emit(Inst::VecExtend {
                                t,
                                rd,
                                rn: rd.to_reg(),
                                high_half: false,
                            });
                        }

                        Ok(())
                    },
                )?;
            }
        }

        Opcode::Store | Opcode::Istore8 | Opcode::Istore16 | Opcode::Istore32 => {
            let off = ctx.data(insn).load_store_offset().unwrap();
            let mut elem_ty = match op {
                Opcode::Istore8 => I8,
                Opcode::Istore16 => I16,
                Opcode::Istore32 => I32,
                Opcode::Store => ctx.input_ty(insn, 0),
                _ => unreachable!(),
            };
            let is_float = ty_has_float_or_vec_representation(elem_ty);
            let flags = ctx
                .memflags(insn)
                .expect("Store instruction should have memflags");

            let dst = put_input_in_regs(ctx, inputs[0]);

            if elem_ty == I128 {
                let mem = lower_pair_address(ctx, &inputs[1..], off);
                ctx.emit(Inst::StoreP64 {
                    rt: dst.regs()[0],
                    rt2: dst.regs()[1],
                    mem,
                    flags,
                });
            } else {
                if elem_ty.is_dynamic_vector() {
                    elem_ty = dynamic_to_fixed(elem_ty);
                }
                let rd = dst.only_reg().unwrap();
                let mem = lower_address(ctx, elem_ty, &inputs[1..], off);
                ctx.emit(match (ty_bits(elem_ty), is_float) {
                    (1, _) | (8, _) => Inst::Store8 { rd, mem, flags },
                    (16, _) => Inst::Store16 { rd, mem, flags },
                    (32, false) => Inst::Store32 { rd, mem, flags },
                    (32, true) => Inst::FpuStore32 { rd, mem, flags },
                    (64, false) => Inst::Store64 { rd, mem, flags },
                    (64, true) => Inst::FpuStore64 { rd, mem, flags },
                    (128, _) => Inst::FpuStore128 { rd, mem, flags },
                    _ => {
                        return Err(CodegenError::Unsupported(format!(
                            "Unsupported type in store: {:?}",
                            elem_ty
                        )))
                    }
                });
            }
        }

        Opcode::StackAddr => {
            let (stack_slot, offset) = match *ctx.data(insn) {
                InstructionData::StackLoad {
                    opcode: Opcode::StackAddr,
                    stack_slot,
                    offset,
                } => (stack_slot, offset),
                _ => unreachable!(),
            };
            let rd = get_output_reg(ctx, outputs[0]).only_reg().unwrap();
            let offset: i32 = offset.into();
            assert!(ctx.abi().sized_stackslot_offsets().is_valid(stack_slot));
            let inst =
                ctx.abi()
                    .sized_stackslot_addr(stack_slot, u32::try_from(offset).unwrap(), rd);
            ctx.emit(inst);
        }

        Opcode::DynamicStackAddr => implemented_in_isle(ctx),

        Opcode::AtomicRmw => implemented_in_isle(ctx),

        Opcode::AtomicCas => implemented_in_isle(ctx),

        Opcode::AtomicLoad => implemented_in_isle(ctx),

        Opcode::AtomicStore => implemented_in_isle(ctx),

        Opcode::Fence => implemented_in_isle(ctx),

        Opcode::StackLoad
        | Opcode::StackStore
        | Opcode::DynamicStackStore
        | Opcode::DynamicStackLoad => {
            panic!("Direct stack memory access not supported; should not be used by Wasm");
        }

        Opcode::HeapAddr => {
            panic!("heap_addr should have been removed by legalization!");
        }

        Opcode::TableAddr => {
            panic!("table_addr should have been removed by legalization!");
        }

        Opcode::Nop => {
            // Nothing.
        }

        Opcode::Select => {
            let flag_input = inputs[0];
            let cond = if let Some(icmp_insn) =
                maybe_input_insn_via_conv(ctx, flag_input, Opcode::Icmp, Opcode::Bint)
            {
                let condcode = ctx.data(icmp_insn).cond_code().unwrap();
                lower_icmp(ctx, icmp_insn, condcode, IcmpOutput::CondCode)?.unwrap_cond()
            } else if let Some(fcmp_insn) =
                maybe_input_insn_via_conv(ctx, flag_input, Opcode::Fcmp, Opcode::Bint)
            {
                let condcode = ctx.data(fcmp_insn).fp_cond_code().unwrap();
                let cond = lower_fp_condcode(condcode);
                lower_fcmp_or_ffcmp_to_flags(ctx, fcmp_insn);
                cond
            } else {
                let (size, narrow_mode) = if ty_bits(ctx.input_ty(insn, 0)) > 32 {
                    (OperandSize::Size64, NarrowValueMode::ZeroExtend64)
                } else {
                    (OperandSize::Size32, NarrowValueMode::ZeroExtend32)
                };

                let rcond = put_input_in_reg(ctx, inputs[0], narrow_mode);
                // cmp rcond, #0
                ctx.emit(Inst::AluRRR {
                    alu_op: ALUOp::SubS,
                    size,
                    rd: writable_zero_reg(),
                    rn: rcond,
                    rm: zero_reg(),
                });
                Cond::Ne
            };

            // csel.cond rd, rn, rm
            let ty = ctx.output_ty(insn, 0);
            let bits = ty_bits(ty);
            let is_float = ty_has_float_or_vec_representation(ty);

            let dst = get_output_reg(ctx, outputs[0]);
            let lhs = put_input_in_regs(ctx, inputs[1]);
            let rhs = put_input_in_regs(ctx, inputs[2]);

            let rd = dst.regs()[0];
            let rn = lhs.regs()[0];
            let rm = rhs.regs()[0];

            match (is_float, bits) {
                (true, 32) => ctx.emit(Inst::FpuCSel32 { cond, rd, rn, rm }),
                (true, 64) => ctx.emit(Inst::FpuCSel64 { cond, rd, rn, rm }),
                (true, 128) => ctx.emit(Inst::VecCSel { cond, rd, rn, rm }),
                (false, 128) => {
                    ctx.emit(Inst::CSel {
                        cond,
                        rd: dst.regs()[0],
                        rn: lhs.regs()[0],
                        rm: rhs.regs()[0],
                    });
                    ctx.emit(Inst::CSel {
                        cond,
                        rd: dst.regs()[1],
                        rn: lhs.regs()[1],
                        rm: rhs.regs()[1],
                    });
                }
                (false, bits) if bits <= 64 => ctx.emit(Inst::CSel { cond, rd, rn, rm }),
                _ => {
                    return Err(CodegenError::Unsupported(format!(
                        "Select: Unsupported type: {:?}",
                        ty
                    )));
                }
            }
        }

        Opcode::Selectif | Opcode::SelectifSpectreGuard => {
            let condcode = ctx.data(insn).cond_code().unwrap();
            // Verification ensures that the input is always a
            // single-def ifcmp.
            let ifcmp_insn = maybe_input_insn(ctx, inputs[0], Opcode::Ifcmp).unwrap();
            let cond = lower_icmp(ctx, ifcmp_insn, condcode, IcmpOutput::CondCode)?.unwrap_cond();

            // csel.COND rd, rn, rm
            let rd = get_output_reg(ctx, outputs[0]).only_reg().unwrap();
            let rn = put_input_in_reg(ctx, inputs[1], NarrowValueMode::None);
            let rm = put_input_in_reg(ctx, inputs[2], NarrowValueMode::None);
            let ty = ctx.output_ty(insn, 0);
            let bits = ty_bits(ty);
            let is_float = ty_has_float_or_vec_representation(ty);
            if is_float && bits == 32 {
                ctx.emit(Inst::FpuCSel32 { cond, rd, rn, rm });
            } else if is_float && bits == 64 {
                ctx.emit(Inst::FpuCSel64 { cond, rd, rn, rm });
            } else if !is_float && bits <= 64 {
                ctx.emit(Inst::CSel { cond, rd, rn, rm });
            } else {
                return Err(CodegenError::Unsupported(format!(
                    "{}: Unsupported type: {:?}",
                    op, ty
                )));
            }

            if op == Opcode::SelectifSpectreGuard {
                ctx.emit(Inst::Csdb);
            }
        }

        Opcode::Bitselect | Opcode::Vselect => implemented_in_isle(ctx),

        Opcode::Trueif => {
            let condcode = ctx.data(insn).cond_code().unwrap();
            // Verification ensures that the input is always a
            // single-def ifcmp.
            let ifcmp_insn = maybe_input_insn(ctx, inputs[0], Opcode::Ifcmp).unwrap();
            let rd = get_output_reg(ctx, outputs[0]).only_reg().unwrap();
            lower_icmp(ctx, ifcmp_insn, condcode, IcmpOutput::Register(rd))?;
        }

        Opcode::Trueff => {
            let condcode = ctx.data(insn).fp_cond_code().unwrap();
            let cond = lower_fp_condcode(condcode);
            let ffcmp_insn = maybe_input_insn(ctx, inputs[0], Opcode::Ffcmp).unwrap();
            lower_fcmp_or_ffcmp_to_flags(ctx, ffcmp_insn);
            let rd = get_output_reg(ctx, outputs[0]).only_reg().unwrap();
            materialize_bool_result(ctx, insn, rd, cond);
        }

        Opcode::IsNull | Opcode::IsInvalid => implemented_in_isle(ctx),

        Opcode::Copy => {
            let rd = get_output_reg(ctx, outputs[0]).only_reg().unwrap();
            let rn = put_input_in_reg(ctx, inputs[0], NarrowValueMode::None);
            let ty = ctx.input_ty(insn, 0);
            ctx.emit(Inst::gen_move(rd, rn, ty));
        }

        Opcode::Breduce | Opcode::Ireduce => implemented_in_isle(ctx),

        Opcode::Bextend | Opcode::Bmask => implemented_in_isle(ctx),

        Opcode::Bint => implemented_in_isle(ctx),

        Opcode::Bitcast => {
            let rd = get_output_reg(ctx, outputs[0]).only_reg().unwrap();
            let ity = ctx.input_ty(insn, 0);
            let oty = ctx.output_ty(insn, 0);
            let ity_bits = ty_bits(ity);
            let ity_vec_reg = ty_has_float_or_vec_representation(ity);
            let oty_bits = ty_bits(oty);
            let oty_vec_reg = ty_has_float_or_vec_representation(oty);

            debug_assert_eq!(ity_bits, oty_bits);

            match (ity_vec_reg, oty_vec_reg) {
                (true, true) => {
                    let narrow_mode = if ity_bits <= 32 {
                        NarrowValueMode::ZeroExtend32
                    } else {
                        NarrowValueMode::ZeroExtend64
                    };
                    let rm = put_input_in_reg(ctx, inputs[0], narrow_mode);
                    ctx.emit(Inst::gen_move(rd, rm, oty));
                }
                (false, false) => {
                    let rm = put_input_in_reg(ctx, inputs[0], NarrowValueMode::None);
                    ctx.emit(Inst::gen_move(rd, rm, oty));
                }
                (false, true) => {
                    let rn = put_input_in_reg(ctx, inputs[0], NarrowValueMode::ZeroExtend64);
                    ctx.emit(Inst::MovToFpu {
                        rd,
                        rn,
                        size: ScalarSize::Size64,
                    });
                }
                (true, false) => {
                    let rn = put_input_in_reg(ctx, inputs[0], NarrowValueMode::None);
                    let size = VectorSize::from_lane_size(ScalarSize::from_bits(oty_bits), true);

                    ctx.emit(Inst::MovFromVec {
                        rd,
                        rn,
                        idx: 0,
                        size,
                    });
                }
            }
        }

        Opcode::Return => {
            for (i, input) in inputs.iter().enumerate() {
                // N.B.: according to the AArch64 ABI, the top bits of a register
                // (above the bits for the value's type) are undefined, so we
                // need not extend the return values.
                let src_regs = put_input_in_regs(ctx, *input);
                let retval_regs = ctx.retval(i);

                assert_eq!(src_regs.len(), retval_regs.len());
                let ty = ctx.input_ty(insn, i);
                let (_, tys) = Inst::rc_for_type(ty)?;

                src_regs
                    .regs()
                    .iter()
                    .zip(retval_regs.regs().iter())
                    .zip(tys.iter())
                    .for_each(|((&src, &dst), &ty)| {
                        ctx.emit(Inst::gen_move(dst, src, ty));
                    });
            }
            // N.B.: the Ret itself is generated by the ABI.
        }

        Opcode::Ifcmp | Opcode::Ffcmp => {
            // An Ifcmp/Ffcmp must always be seen as a use of a brif/brff or trueif/trueff
            // instruction. This will always be the case as long as the IR uses an Ifcmp/Ffcmp from
            // the same block, or a dominating block. In other words, it cannot pass through a BB
            // param (phi). The flags pass of the verifier will ensure this.
            panic!("Should never reach ifcmp as isel root!");
        }

        Opcode::Icmp => {
            let condcode = ctx.data(insn).cond_code().unwrap();
            let rd = get_output_reg(ctx, outputs[0]).only_reg().unwrap();
            lower_icmp(ctx, insn, condcode, IcmpOutput::Register(rd))?;
        }

        Opcode::Fcmp => {
            let condcode = ctx.data(insn).fp_cond_code().unwrap();
            let cond = lower_fp_condcode(condcode);
            let ty = ctx.input_ty(insn, 0);
            let rn = put_input_in_reg(ctx, inputs[0], NarrowValueMode::None);
            let rm = put_input_in_reg(ctx, inputs[1], NarrowValueMode::None);
            let rd = get_output_reg(ctx, outputs[0]).only_reg().unwrap();

            if !ty.is_vector() {
                ctx.emit(Inst::FpuCmp {
                    size: ScalarSize::from_ty(ty),
                    rn,
                    rm,
                });
                materialize_bool_result(ctx, insn, rd, cond);
            } else {
                lower_vector_compare(ctx, rd, rn, rm, ty, cond)?;
            }
        }

        Opcode::Debugtrap => implemented_in_isle(ctx),

        Opcode::Trap | Opcode::ResumableTrap => implemented_in_isle(ctx),

        Opcode::Trapif | Opcode::Trapff => {
            let trap_code = ctx.data(insn).trap_code().unwrap();

            let cond = if maybe_input_insn(ctx, inputs[0], Opcode::IaddIfcout).is_some() {
                let condcode = ctx.data(insn).cond_code().unwrap();
                let cond = lower_condcode(condcode);
                // The flags must not have been clobbered by any other
                // instruction between the iadd_ifcout and this instruction, as
                // verified by the CLIF validator; so we can simply use the
                // flags here.
                cond
            } else if op == Opcode::Trapif {
                let condcode = ctx.data(insn).cond_code().unwrap();

                // Verification ensures that the input is always a single-def ifcmp.
                let ifcmp_insn = maybe_input_insn(ctx, inputs[0], Opcode::Ifcmp).unwrap();
                lower_icmp(ctx, ifcmp_insn, condcode, IcmpOutput::CondCode)?.unwrap_cond()
            } else {
                let condcode = ctx.data(insn).fp_cond_code().unwrap();
                let cond = lower_fp_condcode(condcode);

                // Verification ensures that the input is always a
                // single-def ffcmp.
                let ffcmp_insn = maybe_input_insn(ctx, inputs[0], Opcode::Ffcmp).unwrap();
                lower_fcmp_or_ffcmp_to_flags(ctx, ffcmp_insn);
                cond
            };

            ctx.emit(Inst::TrapIf {
                trap_code,
                kind: CondBrKind::Cond(cond),
            });
        }

        Opcode::Trapz | Opcode::Trapnz | Opcode::ResumableTrapnz => {
            panic!("trapz / trapnz / resumable_trapnz should have been removed by legalization!");
        }

        Opcode::FuncAddr => {
            let rd = get_output_reg(ctx, outputs[0]).only_reg().unwrap();
            let (extname, _) = ctx.call_target(insn).unwrap();
            let extname = extname.clone();
            ctx.emit(Inst::LoadExtName {
                rd,
                name: Box::new(extname),
                offset: 0,
            });
        }

        Opcode::GlobalValue => {
            panic!("global_value should have been removed by legalization!");
        }

        Opcode::SymbolValue => {
            let rd = get_output_reg(ctx, outputs[0]).only_reg().unwrap();
            let (extname, _, offset) = ctx.symbol_value(insn).unwrap();
            let extname = extname.clone();
            ctx.emit(Inst::LoadExtName {
                rd,
                name: Box::new(extname),
                offset,
            });
        }

        Opcode::Call | Opcode::CallIndirect => {
            let caller_conv = ctx.abi().call_conv();
            let (mut abi, inputs) = match op {
                Opcode::Call => {
                    let (extname, dist) = ctx.call_target(insn).unwrap();
                    let extname = extname.clone();
                    let sig = ctx.call_sig(insn).unwrap();
                    assert!(inputs.len() == sig.params.len());
                    assert!(outputs.len() == sig.returns.len());
                    (
                        AArch64ABICaller::from_func(sig, &extname, dist, caller_conv, flags)?,
                        &inputs[..],
                    )
                }
                Opcode::CallIndirect => {
                    let ptr = put_input_in_reg(ctx, inputs[0], NarrowValueMode::ZeroExtend64);
                    let sig = ctx.call_sig(insn).unwrap();
                    assert!(inputs.len() - 1 == sig.params.len());
                    assert!(outputs.len() == sig.returns.len());
                    (
                        AArch64ABICaller::from_ptr(sig, ptr, op, caller_conv, flags)?,
                        &inputs[1..],
                    )
                }
                _ => unreachable!(),
            };

            abi.emit_stack_pre_adjust(ctx);
            assert!(inputs.len() == abi.num_args());
            let mut arg_regs = vec![];
            for input in inputs {
                arg_regs.push(put_input_in_regs(ctx, *input))
            }
            for (i, arg_regs) in arg_regs.iter().enumerate() {
                abi.emit_copy_regs_to_buffer(ctx, i, *arg_regs);
            }
            for (i, arg_regs) in arg_regs.iter().enumerate() {
                abi.emit_copy_regs_to_arg(ctx, i, *arg_regs);
            }
            abi.emit_call(ctx);
            for (i, output) in outputs.iter().enumerate() {
                let retval_regs = get_output_reg(ctx, *output);
                abi.emit_copy_retval_to_regs(ctx, i, retval_regs);
            }
            abi.emit_stack_post_adjust(ctx);
        }

        Opcode::GetPinnedReg => {
            let rd = get_output_reg(ctx, outputs[0]).only_reg().unwrap();
            ctx.emit(Inst::gen_move(rd, xreg(PINNED_REG), I64));
        }

        Opcode::SetPinnedReg => {
            let rm = put_input_in_reg(ctx, inputs[0], NarrowValueMode::None);
            ctx.emit(Inst::gen_move(writable_xreg(PINNED_REG), rm, I64));
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

        Opcode::Vconst => {
            let value = const_param_to_u128(ctx, insn).expect("Invalid immediate bytes");
            let rd = get_output_reg(ctx, outputs[0]).only_reg().unwrap();
            lower_constant_f128(ctx, rd, value);
        }

        Opcode::RawBitcast => {
            let rm = put_input_in_reg(ctx, inputs[0], NarrowValueMode::None);
            let rd = get_output_reg(ctx, outputs[0]).only_reg().unwrap();
            let ty = ctx.input_ty(insn, 0);
            ctx.emit(Inst::gen_move(rd, rm, ty));
        }

        Opcode::Extractlane => {
            if let InstructionData::BinaryImm8 { imm, .. } = ctx.data(insn) {
                let idx = *imm;
                let rd = get_output_reg(ctx, outputs[0]).only_reg().unwrap();
                let rn = put_input_in_reg(ctx, inputs[0], NarrowValueMode::None);
                let input_ty = ctx.input_ty(insn, 0);
                let size = VectorSize::from_ty(input_ty);
                let ty = ty.unwrap();

                if ty_has_int_representation(ty) {
                    ctx.emit(Inst::MovFromVec { rd, rn, idx, size });
                // Plain moves are faster on some processors.
                } else if idx == 0 {
                    ctx.emit(Inst::gen_move(rd, rn, ty));
                } else {
                    ctx.emit(Inst::FpuMoveFromVec { rd, rn, idx, size });
                }
            } else {
                unreachable!();
            }
        }

        Opcode::Insertlane => {
            let idx = if let InstructionData::TernaryImm8 { imm, .. } = ctx.data(insn) {
                *imm
            } else {
                unreachable!();
            };
            let input_ty = ctx.input_ty(insn, 1);
            let rd = get_output_reg(ctx, outputs[0]).only_reg().unwrap();
            let rm = put_input_in_reg(ctx, inputs[0], NarrowValueMode::None);
            let rn = put_input_in_reg(ctx, inputs[1], NarrowValueMode::None);
            let ty = ty.unwrap();
            let size = VectorSize::from_ty(ty);

            ctx.emit(Inst::gen_move(rd, rm, ty));

            if ty_has_int_representation(input_ty) {
                ctx.emit(Inst::MovToVec { rd, rn, idx, size });
            } else {
                ctx.emit(Inst::VecMovElement {
                    rd,
                    rn,
                    dest_idx: idx,
                    src_idx: 0,
                    size,
                });
            }
        }

        Opcode::Splat => implemented_in_isle(ctx),

        Opcode::ScalarToVector => implemented_in_isle(ctx),

        Opcode::VallTrue if ctx.input_ty(insn, 0).lane_bits() == 64 => {
            let input_ty = ctx.input_ty(insn, 0);

            if input_ty.lane_count() != 2 {
                return Err(CodegenError::Unsupported(format!(
                    "VallTrue: unsupported type {:?}",
                    input_ty
                )));
            }

            let rd = get_output_reg(ctx, outputs[0]).only_reg().unwrap();
            let rm = put_input_in_reg(ctx, inputs[0], NarrowValueMode::None);
            let tmp = ctx.alloc_tmp(I64X2).only_reg().unwrap();

            // cmeq vtmp.2d, vm.2d, #0
            // addp dtmp, vtmp.2d
            // fcmp dtmp, dtmp
            // cset xd, eq
            //
            // Note that after the ADDP the value of the temporary register will
            // be either 0 when all input elements are true, i.e. non-zero, or a
            // NaN otherwise (either -1 or -2 when represented as an integer);
            // NaNs are the only floating-point numbers that compare unequal to
            // themselves.

            ctx.emit(Inst::VecMisc {
                op: VecMisc2::Cmeq0,
                rd: tmp,
                rn: rm,
                size: VectorSize::Size64x2,
            });
            ctx.emit(Inst::VecRRPair {
                op: VecPairOp::Addp,
                rd: tmp,
                rn: tmp.to_reg(),
            });
            ctx.emit(Inst::FpuCmp {
                size: ScalarSize::Size64,
                rn: tmp.to_reg(),
                rm: tmp.to_reg(),
            });
            materialize_bool_result(ctx, insn, rd, Cond::Eq);
        }

        Opcode::VanyTrue | Opcode::VallTrue => {
            let rd = get_output_reg(ctx, outputs[0]).only_reg().unwrap();
            let rm = put_input_in_reg(ctx, inputs[0], NarrowValueMode::None);
            let src_ty = ctx.input_ty(insn, 0);
            let tmp = ctx.alloc_tmp(src_ty).only_reg().unwrap();

            // This operation is implemented by using umaxp or uminv to
            // create a scalar value, which is then compared against zero.
            //
            // umaxp vn.16b, vm.16, vm.16 / uminv bn, vm.16b
            // mov xm, vn.d[0]
            // cmp xm, #0
            // cset xm, ne

            let s = VectorSize::from_ty(src_ty);
            let size = if s == VectorSize::Size64x2 {
                // `vall_true` with 64-bit elements is handled elsewhere.
                debug_assert_ne!(op, Opcode::VallTrue);

                VectorSize::Size32x4
            } else {
                s
            };

            if op == Opcode::VanyTrue {
                ctx.emit(Inst::VecRRR {
                    alu_op: VecALUOp::Umaxp,
                    rd: tmp,
                    rn: rm,
                    rm,
                    size,
                });
            } else {
                if size == VectorSize::Size32x2 {
                    return Err(CodegenError::Unsupported(format!(
                        "VallTrue: Unsupported type: {:?}",
                        src_ty
                    )));
                }

                ctx.emit(Inst::VecLanes {
                    op: VecLanesOp::Uminv,
                    rd: tmp,
                    rn: rm,
                    size,
                });
            };

            ctx.emit(Inst::MovFromVec {
                rd,
                rn: tmp.to_reg(),
                idx: 0,
                size: VectorSize::Size64x2,
            });

            ctx.emit(Inst::AluRRImm12 {
                alu_op: ALUOp::SubS,
                size: OperandSize::Size64,
                rd: writable_zero_reg(),
                rn: rd.to_reg(),
                imm12: Imm12::zero(),
            });

            materialize_bool_result(ctx, insn, rd, Cond::Ne);
        }

        Opcode::VhighBits => {
            let dst_r = get_output_reg(ctx, outputs[0]).only_reg().unwrap();
            let src_v = put_input_in_reg(ctx, inputs[0], NarrowValueMode::None);
            let ty = ctx.input_ty(insn, 0);
            // All three sequences use one integer temporary and two vector temporaries.  The
            // shift is done early so as to give the register allocator the possibility of using
            // the same reg for `tmp_v1` and `src_v` in the case that this is the last use of
            // `src_v`.  See https://github.com/WebAssembly/simd/pull/201 for the background and
            // derivation of these sequences.  Alternative sequences are discussed in
            // https://github.com/bytecodealliance/wasmtime/issues/2296, although they are not
            // used here.
            let tmp_r0 = ctx.alloc_tmp(I64).only_reg().unwrap();
            let tmp_v0 = ctx.alloc_tmp(I8X16).only_reg().unwrap();
            let tmp_v1 = ctx.alloc_tmp(I8X16).only_reg().unwrap();
            match ty {
                I8X16 => {
                    // sshr  tmp_v1.16b, src_v.16b, #7
                    // mov   tmp_r0, #0x0201
                    // movk  tmp_r0, #0x0804, lsl 16
                    // movk  tmp_r0, #0x2010, lsl 32
                    // movk  tmp_r0, #0x8040, lsl 48
                    // dup   tmp_v0.2d, tmp_r0
                    // and   tmp_v1.16b, tmp_v1.16b, tmp_v0.16b
                    // ext   tmp_v0.16b, tmp_v1.16b, tmp_v1.16b, #8
                    // zip1  tmp_v0.16b, tmp_v1.16b, tmp_v0.16b
                    // addv  tmp_v0h, tmp_v0.8h
                    // mov   dst_r, tmp_v0.h[0]
                    ctx.emit(Inst::VecShiftImm {
                        op: VecShiftImmOp::Sshr,
                        rd: tmp_v1,
                        rn: src_v,
                        size: VectorSize::Size8x16,
                        imm: 7,
                    });
                    lower_splat_const(ctx, tmp_v0, 0x8040201008040201u64, VectorSize::Size64x2);
                    ctx.emit(Inst::VecRRR {
                        alu_op: VecALUOp::And,
                        rd: tmp_v1,
                        rn: tmp_v1.to_reg(),
                        rm: tmp_v0.to_reg(),
                        size: VectorSize::Size8x16,
                    });
                    ctx.emit(Inst::VecExtract {
                        rd: tmp_v0,
                        rn: tmp_v1.to_reg(),
                        rm: tmp_v1.to_reg(),
                        imm4: 8,
                    });
                    ctx.emit(Inst::VecRRR {
                        alu_op: VecALUOp::Zip1,
                        rd: tmp_v0,
                        rn: tmp_v1.to_reg(),
                        rm: tmp_v0.to_reg(),
                        size: VectorSize::Size8x16,
                    });
                    ctx.emit(Inst::VecLanes {
                        op: VecLanesOp::Addv,
                        rd: tmp_v0,
                        rn: tmp_v0.to_reg(),
                        size: VectorSize::Size16x8,
                    });
                    ctx.emit(Inst::MovFromVec {
                        rd: dst_r,
                        rn: tmp_v0.to_reg(),
                        idx: 0,
                        size: VectorSize::Size16x8,
                    });
                }
                I16X8 => {
                    // sshr  tmp_v1.8h, src_v.8h, #15
                    // mov   tmp_r0, #0x1
                    // movk  tmp_r0, #0x2, lsl 16
                    // movk  tmp_r0, #0x4, lsl 32
                    // movk  tmp_r0, #0x8, lsl 48
                    // dup   tmp_v0.2d, tmp_r0
                    // shl   tmp_r0, tmp_r0, #4
                    // mov   tmp_v0.d[1], tmp_r0
                    // and   tmp_v0.16b, tmp_v1.16b, tmp_v0.16b
                    // addv  tmp_v0h, tmp_v0.8h
                    // mov   dst_r, tmp_v0.h[0]
                    ctx.emit(Inst::VecShiftImm {
                        op: VecShiftImmOp::Sshr,
                        rd: tmp_v1,
                        rn: src_v,
                        size: VectorSize::Size16x8,
                        imm: 15,
                    });
                    lower_constant_u64(ctx, tmp_r0, 0x0008000400020001u64);
                    ctx.emit(Inst::VecDup {
                        rd: tmp_v0,
                        rn: tmp_r0.to_reg(),
                        size: VectorSize::Size64x2,
                    });
                    ctx.emit(Inst::AluRRImmShift {
                        alu_op: ALUOp::Lsl,
                        size: OperandSize::Size64,
                        rd: tmp_r0,
                        rn: tmp_r0.to_reg(),
                        immshift: ImmShift { imm: 4 },
                    });
                    ctx.emit(Inst::MovToVec {
                        rd: tmp_v0,
                        rn: tmp_r0.to_reg(),
                        idx: 1,
                        size: VectorSize::Size64x2,
                    });
                    ctx.emit(Inst::VecRRR {
                        alu_op: VecALUOp::And,
                        rd: tmp_v0,
                        rn: tmp_v1.to_reg(),
                        rm: tmp_v0.to_reg(),
                        size: VectorSize::Size8x16,
                    });
                    ctx.emit(Inst::VecLanes {
                        op: VecLanesOp::Addv,
                        rd: tmp_v0,
                        rn: tmp_v0.to_reg(),
                        size: VectorSize::Size16x8,
                    });
                    ctx.emit(Inst::MovFromVec {
                        rd: dst_r,
                        rn: tmp_v0.to_reg(),
                        idx: 0,
                        size: VectorSize::Size16x8,
                    });
                }
                I32X4 => {
                    // sshr  tmp_v1.4s, src_v.4s, #31
                    // mov   tmp_r0, #0x1
                    // movk  tmp_r0, #0x2, lsl 32
                    // dup   tmp_v0.2d, tmp_r0
                    // shl   tmp_r0, tmp_r0, #2
                    // mov   tmp_v0.d[1], tmp_r0
                    // and   tmp_v0.16b, tmp_v1.16b, tmp_v0.16b
                    // addv  tmp_v0s, tmp_v0.4s
                    // mov   dst_r, tmp_v0.s[0]
                    ctx.emit(Inst::VecShiftImm {
                        op: VecShiftImmOp::Sshr,
                        rd: tmp_v1,
                        rn: src_v,
                        size: VectorSize::Size32x4,
                        imm: 31,
                    });
                    lower_constant_u64(ctx, tmp_r0, 0x0000000200000001u64);
                    ctx.emit(Inst::VecDup {
                        rd: tmp_v0,
                        rn: tmp_r0.to_reg(),
                        size: VectorSize::Size64x2,
                    });
                    ctx.emit(Inst::AluRRImmShift {
                        alu_op: ALUOp::Lsl,
                        size: OperandSize::Size64,
                        rd: tmp_r0,
                        rn: tmp_r0.to_reg(),
                        immshift: ImmShift { imm: 2 },
                    });
                    ctx.emit(Inst::MovToVec {
                        rd: tmp_v0,
                        rn: tmp_r0.to_reg(),
                        idx: 1,
                        size: VectorSize::Size64x2,
                    });
                    ctx.emit(Inst::VecRRR {
                        alu_op: VecALUOp::And,
                        rd: tmp_v0,
                        rn: tmp_v1.to_reg(),
                        rm: tmp_v0.to_reg(),
                        size: VectorSize::Size8x16,
                    });
                    ctx.emit(Inst::VecLanes {
                        op: VecLanesOp::Addv,
                        rd: tmp_v0,
                        rn: tmp_v0.to_reg(),
                        size: VectorSize::Size32x4,
                    });
                    ctx.emit(Inst::MovFromVec {
                        rd: dst_r,
                        rn: tmp_v0.to_reg(),
                        idx: 0,
                        size: VectorSize::Size32x4,
                    });
                }
                I64X2 => {
                    // mov dst_r, src_v.d[0]
                    // mov tmp_r0, src_v.d[1]
                    // lsr dst_r, dst_r, #63
                    // lsr tmp_r0, tmp_r0, #63
                    // add dst_r, dst_r, tmp_r0, lsl #1
                    ctx.emit(Inst::MovFromVec {
                        rd: dst_r,
                        rn: src_v,
                        idx: 0,
                        size: VectorSize::Size64x2,
                    });
                    ctx.emit(Inst::MovFromVec {
                        rd: tmp_r0,
                        rn: src_v,
                        idx: 1,
                        size: VectorSize::Size64x2,
                    });
                    ctx.emit(Inst::AluRRImmShift {
                        alu_op: ALUOp::Lsr,
                        size: OperandSize::Size64,
                        rd: dst_r,
                        rn: dst_r.to_reg(),
                        immshift: ImmShift::maybe_from_u64(63).unwrap(),
                    });
                    ctx.emit(Inst::AluRRImmShift {
                        alu_op: ALUOp::Lsr,
                        size: OperandSize::Size64,
                        rd: tmp_r0,
                        rn: tmp_r0.to_reg(),
                        immshift: ImmShift::maybe_from_u64(63).unwrap(),
                    });
                    ctx.emit(Inst::AluRRRShift {
                        alu_op: ALUOp::Add,
                        size: OperandSize::Size32,
                        rd: dst_r,
                        rn: dst_r.to_reg(),
                        rm: tmp_r0.to_reg(),
                        shiftop: ShiftOpAndAmt::new(
                            ShiftOp::LSL,
                            ShiftOpShiftImm::maybe_from_shift(1).unwrap(),
                        ),
                    });
                }
                _ => {
                    return Err(CodegenError::Unsupported(format!(
                        "VhighBits: Unsupported type: {:?}",
                        ty
                    )))
                }
            }
        }

        Opcode::Shuffle => {
            let mask = const_param_to_u128(ctx, insn).expect("Invalid immediate mask bytes");
            let rd = get_output_reg(ctx, outputs[0]).only_reg().unwrap();
            let rn = put_input_in_reg(ctx, inputs[0], NarrowValueMode::None);
            let rn2 = put_input_in_reg(ctx, inputs[1], NarrowValueMode::None);
            // 2 register table vector lookups require consecutive table registers;
            // we satisfy this constraint by hardcoding the usage of v29 and v30.
            let temp = writable_vreg(29);
            let temp2 = writable_vreg(30);
            let input_ty = ctx.input_ty(insn, 0);
            assert_eq!(input_ty, ctx.input_ty(insn, 1));
            // Make sure that both inputs are in virtual registers, since it is
            // not guaranteed that we can get them safely to the temporaries if
            // either is in a real register.
            let rn = ctx.ensure_in_vreg(rn, input_ty);
            let rn2 = ctx.ensure_in_vreg(rn2, input_ty);

            lower_constant_f128(ctx, rd, mask);
            ctx.emit(Inst::gen_move(temp, rn, input_ty));
            ctx.emit(Inst::gen_move(temp2, rn2, input_ty));
            ctx.emit(Inst::VecTbl2 {
                rd,
                rn: temp.to_reg(),
                rn2: temp2.to_reg(),
                rm: rd.to_reg(),
                is_extension: false,
            });
        }

        Opcode::Swizzle => implemented_in_isle(ctx),

        Opcode::Isplit => implemented_in_isle(ctx),

        Opcode::Iconcat => implemented_in_isle(ctx),

        Opcode::Imax | Opcode::Umax | Opcode::Umin | Opcode::Imin => implemented_in_isle(ctx),

        Opcode::IaddPairwise => implemented_in_isle(ctx),

        Opcode::WideningPairwiseDotProductS => {
            let r_y = get_output_reg(ctx, outputs[0]).only_reg().unwrap();
            let r_a = put_input_in_reg(ctx, inputs[0], NarrowValueMode::None);
            let r_b = put_input_in_reg(ctx, inputs[1], NarrowValueMode::None);
            let ty = ty.unwrap();
            if ty == I32X4 {
                let tmp = ctx.alloc_tmp(I8X16).only_reg().unwrap();
                // The args have type I16X8.
                // "y = i32x4.dot_i16x8_s(a, b)"
                // => smull  tmp, a, b
                //    smull2 y,   a, b
                //    addp   y,   tmp, y
                ctx.emit(Inst::VecRRRLong {
                    alu_op: VecRRRLongOp::Smull16,
                    rd: tmp,
                    rn: r_a,
                    rm: r_b,
                    high_half: false,
                });
                ctx.emit(Inst::VecRRRLong {
                    alu_op: VecRRRLongOp::Smull16,
                    rd: r_y,
                    rn: r_a,
                    rm: r_b,
                    high_half: true,
                });
                ctx.emit(Inst::VecRRR {
                    alu_op: VecALUOp::Addp,
                    rd: r_y,
                    rn: tmp.to_reg(),
                    rm: r_y.to_reg(),
                    size: VectorSize::Size32x4,
                });
            } else {
                return Err(CodegenError::Unsupported(format!(
                    "Opcode::WideningPairwiseDotProductS: unsupported laneage: {:?}",
                    ty
                )));
            }
        }

        Opcode::Fadd | Opcode::Fsub | Opcode::Fmul | Opcode::Fdiv | Opcode::Fmin | Opcode::Fmax => {
            implemented_in_isle(ctx)
        }

        Opcode::FminPseudo | Opcode::FmaxPseudo => implemented_in_isle(ctx),

        Opcode::Sqrt | Opcode::Fneg | Opcode::Fabs | Opcode::Fpromote | Opcode::Fdemote => {
            implemented_in_isle(ctx)
        }

        Opcode::Ceil | Opcode::Floor | Opcode::Trunc | Opcode::Nearest => implemented_in_isle(ctx),

        Opcode::Fma => {
            let ty = ty.unwrap();
            let bits = ty_bits(ty);
            let fpu_op = match bits {
                32 => FPUOp3::MAdd32,
                64 => FPUOp3::MAdd64,
                _ => {
                    return Err(CodegenError::Unsupported(format!(
                        "Fma: Unsupported type: {:?}",
                        ty
                    )))
                }
            };
            let rn = put_input_in_reg(ctx, inputs[0], NarrowValueMode::None);
            let rm = put_input_in_reg(ctx, inputs[1], NarrowValueMode::None);
            let ra = put_input_in_reg(ctx, inputs[2], NarrowValueMode::None);
            let rd = get_output_reg(ctx, outputs[0]).only_reg().unwrap();
            ctx.emit(Inst::FpuRRRR {
                fpu_op,
                rn,
                rm,
                ra,
                rd,
            });
        }

        Opcode::Fcopysign => {
            // Copy the sign bit from inputs[1] to inputs[0]. We use the following sequence:
            //
            // This is a scalar Fcopysign.
            // This uses scalar NEON operations for 64-bit and vector operations (2S) for 32-bit.
            // In the latter case it still sets all bits except the lowest 32 to 0.
            //
            //  mov vd, vn
            //  ushr vtmp, vm, #63 / #31
            //  sli vd, vtmp, #63 / #31

            let ty = ctx.output_ty(insn, 0);

            if ty != F32 && ty != F64 {
                return Err(CodegenError::Unsupported(format!(
                    "Fcopysign: Unsupported type: {:?}",
                    ty
                )));
            }

            let bits = ty_bits(ty) as u8;
            let rn = put_input_in_reg(ctx, inputs[0], NarrowValueMode::None);
            let rm = put_input_in_reg(ctx, inputs[1], NarrowValueMode::None);
            let rd = get_output_reg(ctx, outputs[0]).only_reg().unwrap();
            let tmp = ctx.alloc_tmp(F64).only_reg().unwrap();

            // Copy LHS to rd.
            ctx.emit(Inst::gen_move(rd, rn, ty));

            // Copy the sign bit to the lowest bit in tmp.
            let imm = FPURightShiftImm::maybe_from_u8(bits - 1, bits).unwrap();
            ctx.emit(Inst::FpuRRI {
                fpu_op: choose_32_64(ty, FPUOpRI::UShr32(imm), FPUOpRI::UShr64(imm)),
                rd: tmp,
                rn: rm,
            });

            // Insert the bit from tmp into the sign bit of rd.
            let imm = FPULeftShiftImm::maybe_from_u8(bits - 1, bits).unwrap();
            ctx.emit(Inst::FpuRRI {
                fpu_op: choose_32_64(ty, FPUOpRI::Sli32(imm), FPUOpRI::Sli64(imm)),
                rd,
                rn: tmp.to_reg(),
            });
        }

        Opcode::FcvtToUint | Opcode::FcvtToSint => {
            let input_ty = ctx.input_ty(insn, 0);
            let in_bits = ty_bits(input_ty);
            let output_ty = ty.unwrap();
            let out_bits = ty_bits(output_ty);
            let signed = op == Opcode::FcvtToSint;
            let op = match (signed, in_bits, out_bits) {
                (false, 32, 8) | (false, 32, 16) | (false, 32, 32) => FpuToIntOp::F32ToU32,
                (true, 32, 8) | (true, 32, 16) | (true, 32, 32) => FpuToIntOp::F32ToI32,
                (false, 32, 64) => FpuToIntOp::F32ToU64,
                (true, 32, 64) => FpuToIntOp::F32ToI64,
                (false, 64, 8) | (false, 64, 16) | (false, 64, 32) => FpuToIntOp::F64ToU32,
                (true, 64, 8) | (true, 64, 16) | (true, 64, 32) => FpuToIntOp::F64ToI32,
                (false, 64, 64) => FpuToIntOp::F64ToU64,
                (true, 64, 64) => FpuToIntOp::F64ToI64,
                _ => {
                    return Err(CodegenError::Unsupported(format!(
                        "{}: Unsupported types: {:?} -> {:?}",
                        op, input_ty, output_ty
                    )))
                }
            };

            let rn = put_input_in_reg(ctx, inputs[0], NarrowValueMode::None);
            let rd = get_output_reg(ctx, outputs[0]).only_reg().unwrap();

            // First, check the output: it's important to carry the NaN conversion before the
            // in-bounds conversion, per wasm semantics.

            // Check that the input is not a NaN.
            ctx.emit(Inst::FpuCmp {
                size: ScalarSize::from_ty(input_ty),
                rn,
                rm: rn,
            });
            let trap_code = TrapCode::BadConversionToInteger;
            ctx.emit(Inst::TrapIf {
                trap_code,
                kind: CondBrKind::Cond(lower_fp_condcode(FloatCC::Unordered)),
            });

            let tmp = ctx.alloc_tmp(I8X16).only_reg().unwrap();

            // Check that the input is in range, with "truncate towards zero" semantics. This means
            // we allow values that are slightly out of range:
            // - for signed conversions, we allow values strictly greater than INT_MIN-1 (when this
            // can be represented), and strictly less than INT_MAX+1 (when this can be
            // represented).
            // - for unsigned conversions, we allow values strictly greater than -1, and strictly
            // less than UINT_MAX+1 (when this can be represented).

            if in_bits == 32 {
                // From float32.
                let (low_bound, low_cond, high_bound) = match (signed, out_bits) {
                    (true, 8) => (
                        i8::min_value() as f32 - 1.,
                        FloatCC::GreaterThan,
                        i8::max_value() as f32 + 1.,
                    ),
                    (true, 16) => (
                        i16::min_value() as f32 - 1.,
                        FloatCC::GreaterThan,
                        i16::max_value() as f32 + 1.,
                    ),
                    (true, 32) => (
                        i32::min_value() as f32, // I32_MIN - 1 isn't precisely representable as a f32.
                        FloatCC::GreaterThanOrEqual,
                        i32::max_value() as f32 + 1.,
                    ),
                    (true, 64) => (
                        i64::min_value() as f32, // I64_MIN - 1 isn't precisely representable as a f32.
                        FloatCC::GreaterThanOrEqual,
                        i64::max_value() as f32 + 1.,
                    ),
                    (false, 8) => (-1., FloatCC::GreaterThan, u8::max_value() as f32 + 1.),
                    (false, 16) => (-1., FloatCC::GreaterThan, u16::max_value() as f32 + 1.),
                    (false, 32) => (-1., FloatCC::GreaterThan, u32::max_value() as f32 + 1.),
                    (false, 64) => (-1., FloatCC::GreaterThan, u64::max_value() as f32 + 1.),
                    _ => unreachable!(),
                };

                // >= low_bound
                lower_constant_f32(ctx, tmp, low_bound);
                ctx.emit(Inst::FpuCmp {
                    size: ScalarSize::Size32,
                    rn,
                    rm: tmp.to_reg(),
                });
                let trap_code = TrapCode::IntegerOverflow;
                ctx.emit(Inst::TrapIf {
                    trap_code,
                    kind: CondBrKind::Cond(lower_fp_condcode(low_cond).invert()),
                });

                // <= high_bound
                lower_constant_f32(ctx, tmp, high_bound);
                ctx.emit(Inst::FpuCmp {
                    size: ScalarSize::Size32,
                    rn,
                    rm: tmp.to_reg(),
                });
                let trap_code = TrapCode::IntegerOverflow;
                ctx.emit(Inst::TrapIf {
                    trap_code,
                    kind: CondBrKind::Cond(lower_fp_condcode(FloatCC::LessThan).invert()),
                });
            } else {
                // From float64.
                let (low_bound, low_cond, high_bound) = match (signed, out_bits) {
                    (true, 8) => (
                        i8::min_value() as f64 - 1.,
                        FloatCC::GreaterThan,
                        i8::max_value() as f64 + 1.,
                    ),
                    (true, 16) => (
                        i16::min_value() as f64 - 1.,
                        FloatCC::GreaterThan,
                        i16::max_value() as f64 + 1.,
                    ),
                    (true, 32) => (
                        i32::min_value() as f64 - 1.,
                        FloatCC::GreaterThan,
                        i32::max_value() as f64 + 1.,
                    ),
                    (true, 64) => (
                        i64::min_value() as f64, // I64_MIN - 1 is not precisely representable as an i64.
                        FloatCC::GreaterThanOrEqual,
                        i64::max_value() as f64 + 1.,
                    ),
                    (false, 8) => (-1., FloatCC::GreaterThan, u8::max_value() as f64 + 1.),
                    (false, 16) => (-1., FloatCC::GreaterThan, u16::max_value() as f64 + 1.),
                    (false, 32) => (-1., FloatCC::GreaterThan, u32::max_value() as f64 + 1.),
                    (false, 64) => (-1., FloatCC::GreaterThan, u64::max_value() as f64 + 1.),
                    _ => unreachable!(),
                };

                // >= low_bound
                lower_constant_f64(ctx, tmp, low_bound);
                ctx.emit(Inst::FpuCmp {
                    size: ScalarSize::Size64,
                    rn,
                    rm: tmp.to_reg(),
                });
                let trap_code = TrapCode::IntegerOverflow;
                ctx.emit(Inst::TrapIf {
                    trap_code,
                    kind: CondBrKind::Cond(lower_fp_condcode(low_cond).invert()),
                });

                // <= high_bound
                lower_constant_f64(ctx, tmp, high_bound);
                ctx.emit(Inst::FpuCmp {
                    size: ScalarSize::Size64,
                    rn,
                    rm: tmp.to_reg(),
                });
                let trap_code = TrapCode::IntegerOverflow;
                ctx.emit(Inst::TrapIf {
                    trap_code,
                    kind: CondBrKind::Cond(lower_fp_condcode(FloatCC::LessThan).invert()),
                });
            };

            // Do the conversion.
            ctx.emit(Inst::FpuToInt { op, rd, rn });
        }

        Opcode::FcvtFromUint | Opcode::FcvtFromSint => {
            let input_ty = ctx.input_ty(insn, 0);
            let ty = ty.unwrap();
            let signed = op == Opcode::FcvtFromSint;
            let rd = get_output_reg(ctx, outputs[0]).only_reg().unwrap();

            if ty.is_vector() {
                if input_ty.lane_bits() != ty.lane_bits() {
                    return Err(CodegenError::Unsupported(format!(
                        "{}: Unsupported types: {:?} -> {:?}",
                        op, input_ty, ty
                    )));
                }

                let op = if signed {
                    VecMisc2::Scvtf
                } else {
                    VecMisc2::Ucvtf
                };
                let rn = put_input_in_reg(ctx, inputs[0], NarrowValueMode::None);

                ctx.emit(Inst::VecMisc {
                    op,
                    rd,
                    rn,
                    size: VectorSize::from_ty(ty),
                });
            } else {
                let in_bits = ty_bits(input_ty);
                let out_bits = ty_bits(ty);
                let op = match (signed, in_bits, out_bits) {
                    (false, 8, 32) | (false, 16, 32) | (false, 32, 32) => IntToFpuOp::U32ToF32,
                    (true, 8, 32) | (true, 16, 32) | (true, 32, 32) => IntToFpuOp::I32ToF32,
                    (false, 8, 64) | (false, 16, 64) | (false, 32, 64) => IntToFpuOp::U32ToF64,
                    (true, 8, 64) | (true, 16, 64) | (true, 32, 64) => IntToFpuOp::I32ToF64,
                    (false, 64, 32) => IntToFpuOp::U64ToF32,
                    (true, 64, 32) => IntToFpuOp::I64ToF32,
                    (false, 64, 64) => IntToFpuOp::U64ToF64,
                    (true, 64, 64) => IntToFpuOp::I64ToF64,
                    _ => {
                        return Err(CodegenError::Unsupported(format!(
                            "{}: Unsupported types: {:?} -> {:?}",
                            op, input_ty, ty
                        )))
                    }
                };
                let narrow_mode = match (signed, in_bits) {
                    (false, 8) | (false, 16) | (false, 32) => NarrowValueMode::ZeroExtend32,
                    (true, 8) | (true, 16) | (true, 32) => NarrowValueMode::SignExtend32,
                    (false, 64) => NarrowValueMode::ZeroExtend64,
                    (true, 64) => NarrowValueMode::SignExtend64,
                    _ => unreachable!(),
                };
                let rn = put_input_in_reg(ctx, inputs[0], narrow_mode);
                ctx.emit(Inst::IntToFpu { op, rd, rn });
            }
        }

        Opcode::FcvtToUintSat | Opcode::FcvtToSintSat => {
            let in_ty = ctx.input_ty(insn, 0);
            let ty = ty.unwrap();
            let out_signed = op == Opcode::FcvtToSintSat;
            let rn = put_input_in_reg(ctx, inputs[0], NarrowValueMode::None);
            let rd = get_output_reg(ctx, outputs[0]).only_reg().unwrap();

            if ty.is_vector() {
                if in_ty.lane_bits() != ty.lane_bits() {
                    return Err(CodegenError::Unsupported(format!(
                        "{}: Unsupported types: {:?} -> {:?}",
                        op, in_ty, ty
                    )));
                }

                let op = if out_signed {
                    VecMisc2::Fcvtzs
                } else {
                    VecMisc2::Fcvtzu
                };

                ctx.emit(Inst::VecMisc {
                    op,
                    rd,
                    rn,
                    size: VectorSize::from_ty(ty),
                });
            } else {
                let in_bits = ty_bits(in_ty);
                let out_bits = ty_bits(ty);
                // FIMM Vtmp1, u32::MAX or u64::MAX or i32::MAX or i64::MAX
                // FMIN Vtmp2, Vin, Vtmp1
                // FIMM Vtmp1, 0 or 0 or i32::MIN or i64::MIN
                // FMAX Vtmp2, Vtmp2, Vtmp1
                // (if signed) FIMM Vtmp1, 0
                // FCMP Vin, Vin
                // FCSEL Vtmp2, Vtmp1, Vtmp2, NE  // on NaN, select 0
                // convert Rout, Vtmp2

                assert!(in_ty.is_float() && (in_bits == 32 || in_bits == 64));
                assert!(out_bits == 32 || out_bits == 64);

                let min: f64 = match (out_bits, out_signed) {
                    (32, true) => std::i32::MIN as f64,
                    (32, false) => 0.0,
                    (64, true) => std::i64::MIN as f64,
                    (64, false) => 0.0,
                    _ => unreachable!(),
                };

                let max = match (out_bits, out_signed) {
                    (32, true) => std::i32::MAX as f64,
                    (32, false) => std::u32::MAX as f64,
                    (64, true) => std::i64::MAX as f64,
                    (64, false) => std::u64::MAX as f64,
                    _ => unreachable!(),
                };

                let rtmp1 = ctx.alloc_tmp(in_ty).only_reg().unwrap();
                let rtmp2 = ctx.alloc_tmp(in_ty).only_reg().unwrap();

                if in_bits == 32 {
                    lower_constant_f32(ctx, rtmp1, max as f32);
                } else {
                    lower_constant_f64(ctx, rtmp1, max);
                }
                ctx.emit(Inst::FpuRRR {
                    fpu_op: FPUOp2::Min,
                    size: ScalarSize::from_ty(in_ty),
                    rd: rtmp2,
                    rn,
                    rm: rtmp1.to_reg(),
                });
                if in_bits == 32 {
                    lower_constant_f32(ctx, rtmp1, min as f32);
                } else {
                    lower_constant_f64(ctx, rtmp1, min);
                }
                ctx.emit(Inst::FpuRRR {
                    fpu_op: FPUOp2::Max,
                    size: ScalarSize::from_ty(in_ty),
                    rd: rtmp2,
                    rn: rtmp2.to_reg(),
                    rm: rtmp1.to_reg(),
                });
                if out_signed {
                    if in_bits == 32 {
                        lower_constant_f32(ctx, rtmp1, 0.0);
                    } else {
                        lower_constant_f64(ctx, rtmp1, 0.0);
                    }
                }
                ctx.emit(Inst::FpuCmp {
                    size: ScalarSize::from_ty(in_ty),
                    rn,
                    rm: rn,
                });
                if in_bits == 32 {
                    ctx.emit(Inst::FpuCSel32 {
                        rd: rtmp2,
                        rn: rtmp1.to_reg(),
                        rm: rtmp2.to_reg(),
                        cond: Cond::Ne,
                    });
                } else {
                    ctx.emit(Inst::FpuCSel64 {
                        rd: rtmp2,
                        rn: rtmp1.to_reg(),
                        rm: rtmp2.to_reg(),
                        cond: Cond::Ne,
                    });
                }

                let cvt = match (in_bits, out_bits, out_signed) {
                    (32, 32, false) => FpuToIntOp::F32ToU32,
                    (32, 32, true) => FpuToIntOp::F32ToI32,
                    (32, 64, false) => FpuToIntOp::F32ToU64,
                    (32, 64, true) => FpuToIntOp::F32ToI64,
                    (64, 32, false) => FpuToIntOp::F64ToU32,
                    (64, 32, true) => FpuToIntOp::F64ToI32,
                    (64, 64, false) => FpuToIntOp::F64ToU64,
                    (64, 64, true) => FpuToIntOp::F64ToI64,
                    _ => unreachable!(),
                };
                ctx.emit(Inst::FpuToInt {
                    op: cvt,
                    rd,
                    rn: rtmp2.to_reg(),
                });
            }
        }

        Opcode::IaddIfcout => {
            // This is a two-output instruction that is needed for the
            // legalizer's explicit heap-check sequence, among possible other
            // uses. Its second output is a flags output only ever meant to
            // check for overflow using the
            // `backend.unsigned_add_overflow_condition()` condition.
            //
            // Note that the CLIF validation will ensure that no flag-setting
            // operation comes between this IaddIfcout and its use (e.g., a
            // Trapif). Thus, we can rely on implicit communication through the
            // processor flags rather than explicitly generating flags into a
            // register. We simply use the variant of the add instruction that
            // sets flags (`adds`) here.

            // Note that the second output (the flags) need not be generated,
            // because flags are never materialized into a register; the only
            // instructions that can use a value of type `iflags` or `fflags`
            // will look directly for the flags-producing instruction (which can
            // always be found, by construction) and merge it.

            // Now handle the iadd as above, except use an AddS opcode that sets
            // flags.
            let rd = get_output_reg(ctx, outputs[0]).only_reg().unwrap();
            let rn = put_input_in_reg(ctx, inputs[0], NarrowValueMode::None);
            let rm = put_input_in_rse_imm12(ctx, inputs[1], NarrowValueMode::None);
            let ty = ty.unwrap();
            ctx.emit(alu_inst_imm12(ALUOp::AddS, ty, rd, rn, rm));
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

        Opcode::Iabs => implemented_in_isle(ctx),
        Opcode::AvgRound => {
            let ty = ty.unwrap();

            if ty.lane_bits() == 64 {
                return Err(CodegenError::Unsupported(format!(
                    "AvgRound: Unsupported type: {:?}",
                    ty
                )));
            }

            let rd = get_output_reg(ctx, outputs[0]).only_reg().unwrap();
            let rn = put_input_in_reg(ctx, inputs[0], NarrowValueMode::None);
            let rm = put_input_in_reg(ctx, inputs[1], NarrowValueMode::None);
            ctx.emit(Inst::VecRRR {
                alu_op: VecALUOp::Urhadd,
                rd,
                rn,
                rm,
                size: VectorSize::from_ty(ty),
            });
        }

        Opcode::Snarrow | Opcode::Unarrow | Opcode::Uunarrow => implemented_in_isle(ctx),

        Opcode::SwidenLow | Opcode::SwidenHigh | Opcode::UwidenLow | Opcode::UwidenHigh => {
            let rd = get_output_reg(ctx, outputs[0]).only_reg().unwrap();
            let rn = put_input_in_reg(ctx, inputs[0], NarrowValueMode::None);
            let ty = ty.unwrap();
            let ty = if ty.is_dynamic_vector() {
                ty.dynamic_to_vector()
                    .unwrap_or_else(|| panic!("Unsupported dynamic type: {}?", ty))
            } else {
                ty
            };
            let (t, high_half) = match (ty, op) {
                (I16X8, Opcode::SwidenLow) => (VecExtendOp::Sxtl8, false),
                (I16X8, Opcode::SwidenHigh) => (VecExtendOp::Sxtl8, true),
                (I16X8, Opcode::UwidenLow) => (VecExtendOp::Uxtl8, false),
                (I16X8, Opcode::UwidenHigh) => (VecExtendOp::Uxtl8, true),
                (I32X4, Opcode::SwidenLow) => (VecExtendOp::Sxtl16, false),
                (I32X4, Opcode::SwidenHigh) => (VecExtendOp::Sxtl16, true),
                (I32X4, Opcode::UwidenLow) => (VecExtendOp::Uxtl16, false),
                (I32X4, Opcode::UwidenHigh) => (VecExtendOp::Uxtl16, true),
                (I64X2, Opcode::SwidenLow) => (VecExtendOp::Sxtl32, false),
                (I64X2, Opcode::SwidenHigh) => (VecExtendOp::Sxtl32, true),
                (I64X2, Opcode::UwidenLow) => (VecExtendOp::Uxtl32, false),
                (I64X2, Opcode::UwidenHigh) => (VecExtendOp::Uxtl32, true),
                (ty, _) => {
                    return Err(CodegenError::Unsupported(format!(
                        "{}: Unsupported type: {:?}",
                        op, ty
                    )));
                }
            };

            ctx.emit(Inst::VecExtend {
                t,
                rd,
                rn,
                high_half,
            });
        }

        Opcode::TlsValue => match flags.tls_model() {
            TlsModel::ElfGd => {
                let dst = get_output_reg(ctx, outputs[0]).only_reg().unwrap();
                let (name, _, _) = ctx.symbol_value(insn).unwrap();
                let symbol = name.clone();
                ctx.emit(Inst::ElfTlsGetAddr { symbol });

                let x0 = xreg(0);
                ctx.emit(Inst::gen_move(dst, x0, I64));
            }
            _ => {
                return Err(CodegenError::Unsupported(format!(
                    "Unimplemented TLS model in AArch64 backend: {:?}",
                    flags.tls_model()
                )));
            }
        },

        Opcode::SqmulRoundSat => {
            let ty = ty.unwrap();

            if !ty.is_vector() || (ty.lane_type() != I16 && ty.lane_type() != I32) {
                return Err(CodegenError::Unsupported(format!(
                    "SqmulRoundSat: Unsupported type: {:?}",
                    ty
                )));
            }

            let rd = get_output_reg(ctx, outputs[0]).only_reg().unwrap();
            let rn = put_input_in_reg(ctx, inputs[0], NarrowValueMode::None);
            let rm = put_input_in_reg(ctx, inputs[1], NarrowValueMode::None);

            ctx.emit(Inst::VecRRR {
                alu_op: VecALUOp::Sqrdmulh,
                rd,
                rn,
                rm,
                size: VectorSize::from_ty(ty),
            });
        }

        Opcode::FcvtLowFromSint => {
            let ty = ty.unwrap();

            if ty != F64X2 {
                return Err(CodegenError::Unsupported(format!(
                    "FcvtLowFromSint: Unsupported type: {:?}",
                    ty
                )));
            }

            let rd = get_output_reg(ctx, outputs[0]).only_reg().unwrap();
            let rn = put_input_in_reg(ctx, inputs[0], NarrowValueMode::None);

            ctx.emit(Inst::VecExtend {
                t: VecExtendOp::Sxtl32,
                rd,
                rn,
                high_half: false,
            });
            ctx.emit(Inst::VecMisc {
                op: VecMisc2::Scvtf,
                rd,
                rn: rd.to_reg(),
                size: VectorSize::Size64x2,
            });
        }

        Opcode::FvpromoteLow => {
            debug_assert_eq!(ty.unwrap(), F64X2);

            let rd = get_output_reg(ctx, outputs[0]).only_reg().unwrap();
            let rn = put_input_in_reg(ctx, inputs[0], NarrowValueMode::None);

            ctx.emit(Inst::VecRRLong {
                op: VecRRLongOp::Fcvtl32,
                rd,
                rn,
                high_half: false,
            });
        }

        Opcode::Fvdemote => implemented_in_isle(ctx),

        Opcode::ExtractVector => implemented_in_isle(ctx),

        Opcode::ConstAddr | Opcode::Vconcat | Opcode::Vsplit => {
            return Err(CodegenError::Unsupported(format!(
                "Unimplemented lowering: {}",
                op
            )));
        }
    }

    Ok(())
}

pub(crate) fn lower_branch<C: LowerCtx<I = Inst>>(
    ctx: &mut C,
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
        // Must be a conditional branch followed by an unconditional branch.
        let op0 = ctx.data(branches[0]).opcode();
        let op1 = ctx.data(branches[1]).opcode();

        assert!(op1 == Opcode::Jump);
        let taken = BranchTarget::Label(targets[0]);
        // not_taken target is the target of the second branch, even if it is a Fallthrough
        // instruction: because we reorder blocks while we lower, the fallthrough in the new
        // order is not (necessarily) the same as the fallthrough in CLIF. So we use the
        // explicitly-provided target.
        let not_taken = BranchTarget::Label(targets[1]);

        match op0 {
            Opcode::Brz | Opcode::Brnz => {
                let ty = ctx.input_ty(branches[0], 0);
                let flag_input = InsnInput {
                    insn: branches[0],
                    input: 0,
                };
                if let Some(icmp_insn) =
                    maybe_input_insn_via_conv(ctx, flag_input, Opcode::Icmp, Opcode::Bint)
                {
                    let condcode = ctx.data(icmp_insn).cond_code().unwrap();
                    let cond =
                        lower_icmp(ctx, icmp_insn, condcode, IcmpOutput::CondCode)?.unwrap_cond();
                    let negated = op0 == Opcode::Brz;
                    let cond = if negated { cond.invert() } else { cond };

                    ctx.emit(Inst::CondBr {
                        taken,
                        not_taken,
                        kind: CondBrKind::Cond(cond),
                    });
                } else if let Some(fcmp_insn) =
                    maybe_input_insn_via_conv(ctx, flag_input, Opcode::Fcmp, Opcode::Bint)
                {
                    let condcode = ctx.data(fcmp_insn).fp_cond_code().unwrap();
                    let cond = lower_fp_condcode(condcode);
                    let negated = op0 == Opcode::Brz;
                    let cond = if negated { cond.invert() } else { cond };

                    lower_fcmp_or_ffcmp_to_flags(ctx, fcmp_insn);
                    ctx.emit(Inst::CondBr {
                        taken,
                        not_taken,
                        kind: CondBrKind::Cond(cond),
                    });
                } else {
                    let rt = if ty == I128 {
                        let tmp = ctx.alloc_tmp(I64).only_reg().unwrap();
                        let input = put_input_in_regs(ctx, flag_input);
                        ctx.emit(Inst::AluRRR {
                            alu_op: ALUOp::Orr,
                            size: OperandSize::Size64,
                            rd: tmp,
                            rn: input.regs()[0],
                            rm: input.regs()[1],
                        });
                        tmp.to_reg()
                    } else {
                        put_input_in_reg(ctx, flag_input, NarrowValueMode::ZeroExtend64)
                    };
                    let kind = match op0 {
                        Opcode::Brz => CondBrKind::Zero(rt),
                        Opcode::Brnz => CondBrKind::NotZero(rt),
                        _ => unreachable!(),
                    };
                    ctx.emit(Inst::CondBr {
                        taken,
                        not_taken,
                        kind,
                    });
                }
            }
            Opcode::BrIcmp => {
                let condcode = ctx.data(branches[0]).cond_code().unwrap();
                let cond =
                    lower_icmp(ctx, branches[0], condcode, IcmpOutput::CondCode)?.unwrap_cond();

                ctx.emit(Inst::CondBr {
                    taken,
                    not_taken,
                    kind: CondBrKind::Cond(cond),
                });
            }

            Opcode::Brif => {
                let condcode = ctx.data(branches[0]).cond_code().unwrap();

                let flag_input = InsnInput {
                    insn: branches[0],
                    input: 0,
                };
                if let Some(ifcmp_insn) = maybe_input_insn(ctx, flag_input, Opcode::Ifcmp) {
                    let cond =
                        lower_icmp(ctx, ifcmp_insn, condcode, IcmpOutput::CondCode)?.unwrap_cond();
                    ctx.emit(Inst::CondBr {
                        taken,
                        not_taken,
                        kind: CondBrKind::Cond(cond),
                    });
                } else {
                    // If the ifcmp result is actually placed in a
                    // register, we need to move it back into the flags.
                    let rn = put_input_in_reg(ctx, flag_input, NarrowValueMode::None);
                    ctx.emit(Inst::MovToNZCV { rn });
                    ctx.emit(Inst::CondBr {
                        taken,
                        not_taken,
                        kind: CondBrKind::Cond(lower_condcode(condcode)),
                    });
                }
            }

            Opcode::Brff => {
                let condcode = ctx.data(branches[0]).fp_cond_code().unwrap();
                let cond = lower_fp_condcode(condcode);
                let kind = CondBrKind::Cond(cond);
                let flag_input = InsnInput {
                    insn: branches[0],
                    input: 0,
                };
                if let Some(ffcmp_insn) = maybe_input_insn(ctx, flag_input, Opcode::Ffcmp) {
                    lower_fcmp_or_ffcmp_to_flags(ctx, ffcmp_insn);
                    ctx.emit(Inst::CondBr {
                        taken,
                        not_taken,
                        kind,
                    });
                } else {
                    // If the ffcmp result is actually placed in a
                    // register, we need to move it back into the flags.
                    let rn = put_input_in_reg(ctx, flag_input, NarrowValueMode::None);
                    ctx.emit(Inst::MovToNZCV { rn });
                    ctx.emit(Inst::CondBr {
                        taken,
                        not_taken,
                        kind,
                    });
                }
            }

            _ => unimplemented!(),
        }
    } else {
        // Must be an unconditional branch or an indirect branch.
        let op = ctx.data(branches[0]).opcode();
        match op {
            Opcode::Jump => {
                assert!(branches.len() == 1);
                ctx.emit(Inst::Jump {
                    dest: BranchTarget::Label(targets[0]),
                });
            }

            Opcode::BrTable => {
                // Expand `br_table index, default, JT` to:
                //
                //   emit_island  // this forces an island at this point
                //                // if the jumptable would push us past
                //                // the deadline
                //   subs idx, #jt_size
                //   b.hs default
                //   adr vTmp1, PC+16
                //   ldr vTmp2, [vTmp1, idx, lsl #2]
                //   add vTmp2, vTmp2, vTmp1
                //   br vTmp2
                //   [jumptable offsets relative to JT base]
                let jt_size = targets.len() - 1;
                assert!(jt_size <= std::u32::MAX as usize);

                ctx.emit(Inst::EmitIsland {
                    needed_space: 4 * (6 + jt_size) as CodeOffset,
                });

                let ridx = put_input_in_reg(
                    ctx,
                    InsnInput {
                        insn: branches[0],
                        input: 0,
                    },
                    NarrowValueMode::ZeroExtend32,
                );

                let rtmp1 = ctx.alloc_tmp(I32).only_reg().unwrap();
                let rtmp2 = ctx.alloc_tmp(I32).only_reg().unwrap();

                // Bounds-check, leaving condition codes for JTSequence's
                // branch to default target below.
                if let Some(imm12) = Imm12::maybe_from_u64(jt_size as u64) {
                    ctx.emit(Inst::AluRRImm12 {
                        alu_op: ALUOp::SubS,
                        size: OperandSize::Size32,
                        rd: writable_zero_reg(),
                        rn: ridx,
                        imm12,
                    });
                } else {
                    lower_constant_u64(ctx, rtmp1, jt_size as u64);
                    ctx.emit(Inst::AluRRR {
                        alu_op: ALUOp::SubS,
                        size: OperandSize::Size32,
                        rd: writable_zero_reg(),
                        rn: ridx,
                        rm: rtmp1.to_reg(),
                    });
                }

                // Emit the compound instruction that does:
                //
                // b.hs default
                // adr rA, jt
                // ldrsw rB, [rA, rIndex, UXTW 2]
                // add rA, rA, rB
                // br rA
                // [jt entries]
                //
                // This must be *one* instruction in the vcode because
                // we cannot allow regalloc to insert any spills/fills
                // in the middle of the sequence; otherwise, the ADR's
                // PC-rel offset to the jumptable would be incorrect.
                // (The alternative is to introduce a relocation pass
                // for inlined jumptables, which is much worse, IMHO.)

                let jt_targets: Vec<BranchTarget> = targets
                    .iter()
                    .skip(1)
                    .map(|bix| BranchTarget::Label(*bix))
                    .collect();
                let default_target = BranchTarget::Label(targets[0]);
                ctx.emit(Inst::JTSequence {
                    ridx,
                    rtmp1,
                    rtmp2,
                    info: Box::new(JTSequenceInfo {
                        targets: jt_targets,
                        default_target,
                    }),
                });
            }

            _ => panic!("Unknown branch type!"),
        }
    }

    Ok(())
}
