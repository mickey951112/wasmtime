use crate::abi::{align_to, ty_size, ABIArg, ABISig, LocalSlot, ABI};
use anyhow::Result;
use smallvec::SmallVec;
use std::ops::Range;
use wasmparser::{BinaryReader, FuncValidator, ValidatorResources};
use wasmtime_environ::{ModuleTranslation, TypeConvert};

// TODO:
// SpiderMonkey's implementation uses 16;
// (ref: https://searchfox.org/mozilla-central/source/js/src/wasm/WasmBCFrame.h#585)
// during instrumentation we should measure to verify if this is a good default.
pub(crate) type Locals = SmallVec<[LocalSlot; 16]>;

/// Function defined locals start and end in the frame.
pub(crate) struct DefinedLocalsRange(Range<u32>);

impl DefinedLocalsRange {
    /// Get a reference to the inner range.
    pub fn as_range(&self) -> &Range<u32> {
        &self.0
    }
}

/// An abstraction to read the defined locals from the Wasm binary for a function.
#[derive(Default)]
pub(crate) struct DefinedLocals {
    /// The defined locals for a function.
    pub defined_locals: Locals,
    /// The size of the defined locals.
    pub stack_size: u32,
}

impl DefinedLocals {
    /// Compute the local slots for a Wasm function.
    pub fn new(
        translation: &ModuleTranslation<'_>,
        reader: &mut BinaryReader<'_>,
        validator: &mut FuncValidator<ValidatorResources>,
    ) -> Result<Self> {
        let mut next_stack = 0;
        // The first 32 bits of a Wasm binary function describe the number of locals.
        let local_count = reader.read_var_u32()?;
        let mut slots: Locals = Default::default();

        for _ in 0..local_count {
            let position = reader.original_position();
            let count = reader.read_var_u32()?;
            let ty = reader.read()?;
            validator.define_locals(position, count, ty)?;

            let ty = translation.module.convert_valtype(ty);
            for _ in 0..count {
                let ty_size = ty_size(&ty);
                next_stack = align_to(next_stack, ty_size) + ty_size;
                slots.push(LocalSlot::new(ty, next_stack));
            }
        }

        Ok(Self {
            defined_locals: slots,
            stack_size: next_stack,
        })
    }
}

/// Frame handler abstraction.
pub(crate) struct Frame {
    /// The size of the entire local area; the arguments plus the function defined locals.
    pub locals_size: u32,

    /// The range in the frame corresponding to the defined locals range.
    pub defined_locals_range: DefinedLocalsRange,

    /// The local slots for the current function.
    ///
    /// Locals get calculated when allocating a frame and are readonly
    /// through the function compilation lifetime.
    pub locals: Locals,

    /// The offset to the slot containing the `VMContext`.
    pub vmctx_slot: LocalSlot,
}

impl Frame {
    /// Allocate a new Frame.
    pub fn new<A: ABI>(sig: &ABISig, defined_locals: &DefinedLocals) -> Result<Self> {
        let (mut locals, defined_locals_start) = Self::compute_arg_slots::<A>(sig)?;

        // The defined locals have a zero-based offset by default
        // so we need to add the defined locals start to the offset.
        locals.extend(
            defined_locals
                .defined_locals
                .iter()
                .map(|l| LocalSlot::new(l.ty, l.offset + defined_locals_start)),
        );

        let vmctx_slots_size = <A as ABI>::word_bytes();
        let vmctx_offset = defined_locals_start + defined_locals.stack_size + vmctx_slots_size;

        let locals_size = align_to(vmctx_offset, <A as ABI>::stack_align().into());

        Ok(Self {
            locals,
            locals_size,
            vmctx_slot: LocalSlot::i64(vmctx_offset),
            defined_locals_range: DefinedLocalsRange(
                defined_locals_start..defined_locals.stack_size,
            ),
        })
    }

    /// Get a local slot.
    pub fn get_local(&self, index: u32) -> Option<&LocalSlot> {
        self.locals.get(index as usize)
    }

    fn compute_arg_slots<A: ABI>(sig: &ABISig) -> Result<(Locals, u32)> {
        // Go over the function ABI-signature and
        // calculate the stack slots.
        //
        //  for each parameter p; when p
        //
        //  Stack =>
        //      The slot offset is calculated from the ABIArg offset
        //      relative the to the frame pointer (and its inclusions, e.g.
        //      return address).
        //
        //  Register =>
        //     The slot is calculated by accumulating into the `next_frame_size`
        //     the size + alignment of the type that the register is holding.
        //
        //  NOTE
        //      This implementation takes inspiration from SpiderMonkey's implementation
        //      to calculate local slots for function arguments
        //      (https://searchfox.org/mozilla-central/source/js/src/wasm/WasmBCFrame.cpp#83).
        //      The main difference is that SpiderMonkey's implementation
        //      doesn't append any sort of metadata to the locals regarding stack
        //      addressing mode (stack pointer or frame pointer), the offset is
        //      declared negative if the local belongs to a stack argument;
        //      that's enough to later calculate address of the local later on.
        //
        //      Winch appends an addressing mode to each slot, in the end
        //      we want positive addressing from the stack pointer
        //      for both locals and stack arguments.

        let arg_base_offset = <A as ABI>::arg_base_offset().into();
        let mut next_stack = 0u32;
        let slots: Locals = sig
            .params
            .iter()
            .map(|arg| Self::abi_arg_slot(&arg, &mut next_stack, arg_base_offset))
            .collect();

        Ok((slots, next_stack))
    }

    fn abi_arg_slot(arg: &ABIArg, next_stack: &mut u32, arg_base_offset: u32) -> LocalSlot {
        match arg {
            // Create a local slot, for input register spilling,
            // with type-size aligned access.
            ABIArg::Reg { ty, reg: _ } => {
                let ty_size = ty_size(&ty);
                *next_stack = align_to(*next_stack, ty_size) + ty_size;
                LocalSlot::new(*ty, *next_stack)
            }
            // Create a local slot, with an offset from the arguments base in
            // the stack; which is the frame pointer + return address.
            ABIArg::Stack { ty, offset } => LocalSlot::stack_arg(*ty, offset + arg_base_offset),
        }
    }
}
