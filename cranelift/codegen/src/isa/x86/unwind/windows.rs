//! Unwind information for Windows x64 ABI.

use crate::ir::{Function, InstructionData, Opcode, ValueLoc};
use crate::isa::x86::registers::{FPR, GPR, RU};
use crate::isa::{CallConv, RegUnit, TargetIsa};
use crate::result::{CodegenError, CodegenResult};
use alloc::vec::Vec;
use byteorder::{ByteOrder, LittleEndian};
use log::warn;

#[cfg(feature = "enable-serde")]
use serde::{Deserialize, Serialize};

/// Maximum (inclusive) size of a "small" stack allocation
const SMALL_ALLOC_MAX_SIZE: u32 = 128;
/// Maximum (inclusive) size of a "large" stack allocation that can represented in 16-bits
const LARGE_ALLOC_16BIT_MAX_SIZE: u32 = 524280;

struct Writer<'a> {
    buf: &'a mut [u8],
    offset: usize,
}

impl<'a> Writer<'a> {
    pub fn new(buf: &'a mut [u8]) -> Self {
        Self { buf, offset: 0 }
    }

    fn write_u8(&mut self, v: u8) {
        self.buf[self.offset] = v;
        self.offset += 1;
    }

    fn write_u16<T: ByteOrder>(&mut self, v: u16) {
        T::write_u16(&mut self.buf[self.offset..(self.offset + 2)], v);
        self.offset += 2;
    }

    fn write_u32<T: ByteOrder>(&mut self, v: u32) {
        T::write_u32(&mut self.buf[self.offset..(self.offset + 4)], v);
        self.offset += 4;
    }
}

/// The supported unwind codes for the x64 Windows ABI.
///
/// See: https://docs.microsoft.com/en-us/cpp/build/exception-handling-x64
/// Only what is needed to describe the prologues generated by the Cranelift x86 ISA are represented here.
/// Note: the Cranelift x86 ISA RU enum matches the Windows unwind GPR encoding values.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "enable-serde", derive(Serialize, Deserialize))]
enum UnwindCode {
    PushRegister {
        offset: u8,
        reg: u8,
    },
    SaveXmm {
        offset: u8,
        reg: u8,
        stack_offset: u32,
    },
    StackAlloc {
        offset: u8,
        size: u32,
    },
    SetFramePointer {
        offset: u8,
        sp_offset: u8,
    },
}

impl UnwindCode {
    fn emit(&self, writer: &mut Writer) {
        enum UnwindOperation {
            PushNonvolatileRegister = 0,
            LargeStackAlloc = 1,
            SmallStackAlloc = 2,
            SetFramePointer = 3,
            SaveXmm128 = 8,
            SaveXmm128Far = 9,
        }

        match self {
            Self::PushRegister { offset, reg } => {
                writer.write_u8(*offset);
                writer.write_u8((*reg << 4) | (UnwindOperation::PushNonvolatileRegister as u8));
            }
            Self::SaveXmm {
                offset,
                reg,
                stack_offset,
            } => {
                writer.write_u8(*offset);
                let stack_offset = stack_offset / 16;
                if stack_offset <= core::u16::MAX as u32 {
                    writer.write_u8((*reg << 4) | (UnwindOperation::SaveXmm128 as u8));
                    writer.write_u16::<LittleEndian>(stack_offset as u16);
                } else {
                    writer.write_u8((*reg << 4) | (UnwindOperation::SaveXmm128Far as u8));
                    writer.write_u16::<LittleEndian>(stack_offset as u16);
                    writer.write_u16::<LittleEndian>((stack_offset >> 16) as u16);
                }
            }
            Self::StackAlloc { offset, size } => {
                // Stack allocations on Windows must be a multiple of 8 and be at least 1 slot
                assert!(*size >= 8);
                assert!((*size % 8) == 0);

                writer.write_u8(*offset);
                if *size <= SMALL_ALLOC_MAX_SIZE {
                    writer.write_u8(
                        ((((*size - 8) / 8) as u8) << 4) | UnwindOperation::SmallStackAlloc as u8,
                    );
                } else if *size <= LARGE_ALLOC_16BIT_MAX_SIZE {
                    writer.write_u8(UnwindOperation::LargeStackAlloc as u8);
                    writer.write_u16::<LittleEndian>((*size / 8) as u16);
                } else {
                    writer.write_u8((1 << 4) | (UnwindOperation::LargeStackAlloc as u8));
                    writer.write_u32::<LittleEndian>(*size);
                }
            }
            Self::SetFramePointer { offset, sp_offset } => {
                writer.write_u8(*offset);
                writer.write_u8((*sp_offset << 4) | (UnwindOperation::SetFramePointer as u8));
            }
        };
    }

    fn node_count(&self) -> usize {
        match self {
            Self::StackAlloc { size, .. } => {
                if *size <= SMALL_ALLOC_MAX_SIZE {
                    1
                } else if *size <= LARGE_ALLOC_16BIT_MAX_SIZE {
                    2
                } else {
                    3
                }
            }
            Self::SaveXmm { stack_offset, .. } => {
                if *stack_offset <= core::u16::MAX as u32 {
                    2
                } else {
                    3
                }
            }
            _ => 1,
        }
    }
}

pub(crate) fn create_unwind_info(
    func: &Function,
    isa: &dyn TargetIsa,
    frame_register: Option<RegUnit>,
) -> CodegenResult<Option<UnwindInfo>> {
    // Only Windows fastcall is supported for unwind information
    if func.signature.call_conv != CallConv::WindowsFastcall || func.prologue_end.is_none() {
        return Ok(None);
    }

    let prologue_end = func.prologue_end.unwrap();
    let entry_block = func.layout.entry_block().expect("missing entry block");

    // Stores the stack size when SP is not adjusted via an immediate value
    let mut stack_size = None;
    let mut prologue_size = 0;
    let mut unwind_codes = Vec::new();
    let mut found_end = false;

    // Have we saved at least one FPR? if so, we might have to check additional constraints.
    let mut saved_fpr = false;

    // In addition to the min offset for a callee-save, we need to know the offset from the
    // frame base to the stack pointer, so that we can record an unwind offset that spans only
    // to the end of callee-save space.
    let mut static_frame_allocation_size = 0u32;

    // For the time being, FPR preservation is split into a stack_addr and later store/load.
    // Store the register used for stack store and ensure it is the same register with no
    // intervening changes to the frame size.
    let mut callee_save_region_reg = None;
    // Also record the callee-save region's offset from RSP, because it must be added to FPR
    // save offsets to compute an offset from the frame base.
    let mut callee_save_offset = None;

    for (offset, inst, size) in func.inst_offsets(entry_block, &isa.encoding_info()) {
        // x64 ABI prologues cannot exceed 255 bytes in length
        if (offset + size) > 255 {
            warn!("function prologues cannot exceed 255 bytes in size for Windows x64");
            return Err(CodegenError::CodeTooLarge);
        }

        prologue_size += size;

        let unwind_offset = (offset + size) as u8;

        match func.dfg[inst] {
            InstructionData::Unary { opcode, arg } => {
                match opcode {
                    Opcode::X86Push => {
                        static_frame_allocation_size += 8;

                        unwind_codes.push(UnwindCode::PushRegister {
                            offset: unwind_offset,
                            reg: GPR.index_of(func.locations[arg].unwrap_reg()) as u8,
                        });
                    }
                    Opcode::AdjustSpDown => {
                        let stack_size =
                            stack_size.expect("expected a previous stack size instruction");
                        static_frame_allocation_size += stack_size;

                        // This is used when calling a stack check function
                        // We need to track the assignment to RAX which has the size of the stack
                        unwind_codes.push(UnwindCode::StackAlloc {
                            offset: unwind_offset,
                            size: stack_size,
                        });
                    }
                    _ => {}
                }
            }
            InstructionData::CopySpecial { src, dst, .. } => {
                if let Some(frame_register) = frame_register {
                    if src == (RU::rsp as RegUnit) && dst == frame_register {
                        // Constructing an rbp-based stack frame, so the static frame
                        // allocation restarts at 0 from here.
                        static_frame_allocation_size = 0;

                        unwind_codes.push(UnwindCode::SetFramePointer {
                            offset: unwind_offset,
                            sp_offset: 0,
                        });
                    }
                }
            }
            InstructionData::UnaryImm { opcode, imm } => {
                match opcode {
                    Opcode::Iconst => {
                        let imm: i64 = imm.into();
                        assert!(imm <= core::u32::MAX as i64);
                        assert!(stack_size.is_none());

                        // This instruction should only appear in a prologue to pass an
                        // argument of the stack size to a stack check function.
                        // Record the stack size so we know what it is when we encounter the adjustment
                        // instruction (which will adjust via the register assigned to this instruction).
                        stack_size = Some(imm as u32);
                    }
                    Opcode::AdjustSpDownImm => {
                        let imm: i64 = imm.into();
                        assert!(imm <= core::u32::MAX as i64);

                        static_frame_allocation_size += imm as u32;

                        unwind_codes.push(UnwindCode::StackAlloc {
                            offset: unwind_offset,
                            size: imm as u32,
                        });
                    }
                    _ => {}
                }
            }
            InstructionData::StackLoad {
                opcode: Opcode::StackAddr,
                stack_slot,
                offset: _,
            } => {
                let result = func.dfg.inst_results(inst).get(0).unwrap();
                if let ValueLoc::Reg(frame_reg) = func.locations[*result] {
                    callee_save_region_reg = Some(frame_reg);

                    // Figure out the offset in the call frame that `frame_reg` will have.
                    let frame_size = func
                        .stack_slots
                        .layout_info
                        .expect("func's stack slots have layout info if stack operations exist")
                        .frame_size;
                    // Because we're well after the prologue has been constructed, stack slots
                    // must have been laid out...
                    let slot_offset = func.stack_slots[stack_slot]
                        .offset
                        .expect("callee-save slot has an offset computed");
                    let frame_offset = frame_size as i32 + slot_offset;

                    callee_save_offset = Some(frame_offset as u32);
                }
            }
            InstructionData::Store {
                opcode: Opcode::Store,
                args: [arg1, arg2],
                flags: _flags,
                offset,
            } => {
                if let (ValueLoc::Reg(ru), ValueLoc::Reg(base_ru)) =
                    (func.locations[arg1], func.locations[arg2])
                {
                    if Some(base_ru) == callee_save_region_reg {
                        let offset_int: i32 = offset.into();
                        assert!(offset_int >= 0, "negative fpr offset would store outside the stack frame, and is almost certainly an error");
                        let offset_int: u32 = offset_int as u32 + callee_save_offset.expect("FPR presevation requires an FPR save region, which has some stack offset");
                        if FPR.contains(ru) {
                            saved_fpr = true;
                            unwind_codes.push(UnwindCode::SaveXmm {
                                offset: unwind_offset,
                                reg: ru as u8,
                                stack_offset: offset_int,
                            });
                        }
                    }
                }
            }
            _ => {}
        };

        if inst == prologue_end {
            found_end = true;
            break;
        }
    }

    assert!(found_end);

    if saved_fpr {
        if static_frame_allocation_size > 240 && saved_fpr {
            warn!("stack frame is too large ({} bytes) to use with Windows x64 SEH when preserving FPRs. \
                This is a Cranelift implementation limit, see \
                https://github.com/bytecodealliance/wasmtime/issues/1475",
                static_frame_allocation_size);
            return Err(CodegenError::ImplLimitExceeded);
        }
        // Only test static frame size is 16-byte aligned when an FPR is saved to avoid
        // panicking when alignment is elided because no FPRs are saved and no child calls are
        // made.
        assert!(
            static_frame_allocation_size % 16 == 0,
            "static frame allocation must be a multiple of 16"
        );
    }

    // Hack to avoid panicking unnecessarily. Because Cranelift generates prologues with RBP at
    // one end of the call frame, and RSP at the other, required offsets are arbitrarily large.
    // Windows x64 SEH only allows this offset be up to 240 bytes, however, meaning large
    // frames are inexpressible, and we cannot actually compile the function. In case there are
    // no preserved FPRs, we can lie without error and claim the offset to RBP is 0 - nothing
    // will actually check it. This, then, avoids panics when compiling functions with large
    // call frames.
    let reported_frame_offset = if saved_fpr {
        (static_frame_allocation_size / 16) as u8
    } else {
        0
    };

    Ok(Some(UnwindInfo {
        flags: 0, // this assumes cranelift functions have no SEH handlers
        prologue_size: prologue_size as u8,
        frame_register: frame_register.map(|r| GPR.index_of(r) as u8),
        frame_register_offset: reported_frame_offset,
        unwind_codes,
    }))
}

/// Represents Windows x64 unwind information.
///
/// For information about Windows x64 unwind info, see:
/// https://docs.microsoft.com/en-us/cpp/build/exception-handling-x64
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "enable-serde", derive(Serialize, Deserialize))]
pub struct UnwindInfo {
    flags: u8,
    prologue_size: u8,
    frame_register: Option<u8>,
    frame_register_offset: u8,
    unwind_codes: Vec<UnwindCode>,
}

impl UnwindInfo {
    /// Gets the emit size of the unwind information, in bytes.
    pub fn emit_size(&self) -> usize {
        let node_count = self.node_count();

        // Calculation of the size requires no SEH handler or chained info
        assert!(self.flags == 0);

        // Size of fixed part of UNWIND_INFO is 4 bytes
        // Then comes the UNWIND_CODE nodes (2 bytes each)
        // Then comes 2 bytes of padding for the unwind codes if necessary
        // Next would come the SEH data, but we assert above that the function doesn't have SEH data

        4 + (node_count * 2) + if (node_count & 1) == 1 { 2 } else { 0 }
    }

    /// Emits the unwind information into the given mutable byte slice.
    ///
    /// This function will panic if the slice is not at least `emit_size` in length.
    pub fn emit(&self, buf: &mut [u8]) {
        const UNWIND_INFO_VERSION: u8 = 1;

        let node_count = self.node_count();
        assert!(node_count <= 256);

        let mut writer = Writer::new(buf);

        writer.write_u8((self.flags << 3) | UNWIND_INFO_VERSION);
        writer.write_u8(self.prologue_size);
        writer.write_u8(node_count as u8);

        if let Some(reg) = self.frame_register {
            writer.write_u8((self.frame_register_offset << 4) | reg);
        } else {
            writer.write_u8(0);
        }

        // Unwind codes are written in reverse order (prologue offset descending)
        for code in self.unwind_codes.iter().rev() {
            code.emit(&mut writer);
        }

        // To keep a 32-bit alignment, emit 2 bytes of padding if there's an odd number of 16-bit nodes
        if (node_count & 1) == 1 {
            writer.write_u16::<LittleEndian>(0);
        }

        // Ensure the correct number of bytes was emitted
        assert_eq!(writer.offset, self.emit_size());
    }

    fn node_count(&self) -> usize {
        self.unwind_codes
            .iter()
            .fold(0, |nodes, c| nodes + c.node_count())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cursor::{Cursor, FuncCursor};
    use crate::ir::{ExternalName, InstBuilder, Signature, StackSlotData, StackSlotKind};
    use crate::isa::{lookup, CallConv};
    use crate::settings::{builder, Flags};
    use crate::Context;
    use std::str::FromStr;
    use target_lexicon::triple;

    #[test]
    fn test_wrong_calling_convention() {
        let isa = lookup(triple!("x86_64"))
            .expect("expect x86 ISA")
            .finish(Flags::new(builder()));

        let mut context = Context::for_function(create_function(CallConv::SystemV, None));

        context.compile(&*isa).expect("expected compilation");

        assert_eq!(
            create_unwind_info(&context.func, &*isa, None).expect("can create unwind info"),
            None
        );
    }

    #[test]
    fn test_small_alloc() {
        let isa = lookup(triple!("x86_64"))
            .expect("expect x86 ISA")
            .finish(Flags::new(builder()));

        let mut context = Context::for_function(create_function(
            CallConv::WindowsFastcall,
            Some(StackSlotData::new(StackSlotKind::ExplicitSlot, 64)),
        ));

        context.compile(&*isa).expect("expected compilation");

        let unwind = create_unwind_info(&context.func, &*isa, Some(RU::rbp.into()))
            .expect("can create unwind info")
            .expect("expected unwind info");

        assert_eq!(
            unwind,
            UnwindInfo {
                flags: 0,
                prologue_size: 9,
                frame_register: Some(GPR.index_of(RU::rbp.into()) as u8),
                frame_register_offset: 0,
                unwind_codes: vec![
                    UnwindCode::PushRegister {
                        offset: 2,
                        reg: GPR.index_of(RU::rbp.into()) as u8
                    },
                    UnwindCode::SetFramePointer {
                        offset: 5,
                        sp_offset: 0
                    },
                    UnwindCode::StackAlloc {
                        offset: 9,
                        size: 64 + 32
                    }
                ]
            }
        );

        assert_eq!(unwind.emit_size(), 12);

        let mut buf = [0u8; 12];
        unwind.emit(&mut buf);

        assert_eq!(
            buf,
            [
                0x01, // Version and flags (version 1, no flags)
                0x09, // Prologue size
                0x03, // Unwind code count (1 for stack alloc, 1 for save frame reg, 1 for push reg)
                0x05, // Frame register + offset (RBP with 0 offset)
                0x09, // Prolog offset
                0xB2, // Operation 2 (small stack alloc), size = 0xB slots (e.g. (0xB * 8) + 8 = 96 (64 + 32) bytes)
                0x05, // Prolog offset
                0x03, // Operation 3 (save frame register), stack pointer offset = 0
                0x02, // Prolog offset
                0x50, // Operation 0 (save nonvolatile register), reg = 5 (RBP)
                0x00, // Padding byte
                0x00, // Padding byte
            ]
        );
    }

    #[test]
    fn test_medium_alloc() {
        let isa = lookup(triple!("x86_64"))
            .expect("expect x86 ISA")
            .finish(Flags::new(builder()));

        let mut context = Context::for_function(create_function(
            CallConv::WindowsFastcall,
            Some(StackSlotData::new(StackSlotKind::ExplicitSlot, 10000)),
        ));

        context.compile(&*isa).expect("expected compilation");

        let unwind = create_unwind_info(&context.func, &*isa, Some(RU::rbp.into()))
            .expect("can create unwind info")
            .expect("expected unwind info");

        assert_eq!(
            unwind,
            UnwindInfo {
                flags: 0,
                prologue_size: 27,
                frame_register: Some(GPR.index_of(RU::rbp.into()) as u8),
                frame_register_offset: 0,
                unwind_codes: vec![
                    UnwindCode::PushRegister {
                        offset: 2,
                        reg: GPR.index_of(RU::rbp.into()) as u8
                    },
                    UnwindCode::SetFramePointer {
                        offset: 5,
                        sp_offset: 0
                    },
                    UnwindCode::StackAlloc {
                        offset: 27,
                        size: 10000 + 32
                    }
                ]
            }
        );

        assert_eq!(unwind.emit_size(), 12);

        let mut buf = [0u8; 12];
        unwind.emit(&mut buf);

        assert_eq!(
            buf,
            [
                0x01, // Version and flags (version 1, no flags)
                0x1B, // Prologue size
                0x04, // Unwind code count (2 for stack alloc, 1 for save frame reg, 1 for push reg)
                0x05, // Frame register + offset (RBP with 0 offset)
                0x1B, // Prolog offset
                0x01, // Operation 1 (large stack alloc), size is scaled 16-bits (info = 0)
                0xE6, // Low size byte
                0x04, // High size byte (e.g. 0x04E6 * 8 = 100032 (10000 + 32) bytes)
                0x05, // Prolog offset
                0x03, // Operation 3 (save frame register), stack pointer offset = 0
                0x02, // Prolog offset
                0x50, // Operation 0 (push nonvolatile register), reg = 5 (RBP)
            ]
        );
    }

    #[test]
    fn test_large_alloc() {
        let isa = lookup(triple!("x86_64"))
            .expect("expect x86 ISA")
            .finish(Flags::new(builder()));

        let mut context = Context::for_function(create_function(
            CallConv::WindowsFastcall,
            Some(StackSlotData::new(StackSlotKind::ExplicitSlot, 1000000)),
        ));

        context.compile(&*isa).expect("expected compilation");

        let unwind = create_unwind_info(&context.func, &*isa, Some(RU::rbp.into()))
            .expect("can create unwind info")
            .expect("expected unwind info");

        assert_eq!(
            unwind,
            UnwindInfo {
                flags: 0,
                prologue_size: 27,
                frame_register: Some(GPR.index_of(RU::rbp.into()) as u8),
                frame_register_offset: 0,
                unwind_codes: vec![
                    UnwindCode::PushRegister {
                        offset: 2,
                        reg: GPR.index_of(RU::rbp.into()) as u8
                    },
                    UnwindCode::SetFramePointer {
                        offset: 5,
                        sp_offset: 0
                    },
                    UnwindCode::StackAlloc {
                        offset: 27,
                        size: 1000000 + 32
                    }
                ]
            }
        );

        assert_eq!(unwind.emit_size(), 16);

        let mut buf = [0u8; 16];
        unwind.emit(&mut buf);

        assert_eq!(
            buf,
            [
                0x01, // Version and flags (version 1, no flags)
                0x1B, // Prologue size
                0x05, // Unwind code count (3 for stack alloc, 1 for save frame reg, 1 for push reg)
                0x05, // Frame register + offset (RBP with 0 offset)
                0x1B, // Prolog offset
                0x11, // Operation 1 (large stack alloc), size is unscaled 32-bits (info = 1)
                0x60, // Byte 1 of size
                0x42, // Byte 2 of size
                0x0F, // Byte 3 of size
                0x00, // Byte 4 of size (size is 0xF4260 = 1000032 (1000000 + 32) bytes)
                0x05, // Prolog offset
                0x03, // Operation 3 (save frame register), stack pointer offset = 0
                0x02, // Prolog offset
                0x50, // Operation 0 (push nonvolatile register), reg = 5 (RBP)
                0x00, // Padding byte
                0x00, // Padding byte
            ]
        );
    }

    fn create_function(call_conv: CallConv, stack_slot: Option<StackSlotData>) -> Function {
        let mut func =
            Function::with_name_signature(ExternalName::user(0, 0), Signature::new(call_conv));

        let block0 = func.dfg.make_block();
        let mut pos = FuncCursor::new(&mut func);
        pos.insert_block(block0);
        pos.ins().return_(&[]);

        if let Some(stack_slot) = stack_slot {
            func.stack_slots.push(stack_slot);
        }

        func
    }
}
