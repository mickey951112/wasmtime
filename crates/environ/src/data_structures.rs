#![doc(hidden)]

pub mod ir {
    pub use cranelift_codegen::binemit::{Reloc, StackMap};
    pub use cranelift_codegen::ir::{
        types, AbiParam, ArgumentPurpose, JumpTableOffsets, LibCall, Signature, SourceLoc,
        StackSlots, TrapCode, Type, ValueLabel, ValueLoc,
    };
    pub use cranelift_codegen::{ValueLabelsRanges, ValueLocRange};
}

pub mod settings {
    pub use cranelift_codegen::settings::{builder, Builder, Configurable, Flags, SetError};
}

pub mod isa {
    pub use cranelift_codegen::isa::{
        unwind, Builder, CallConv, RegUnit, TargetFrontendConfig, TargetIsa,
    };
}

pub mod entity {
    pub use cranelift_entity::{packed_option, BoxedSlice, EntityRef, PrimaryMap};
}

pub mod wasm {
    pub use cranelift_wasm::*;
}
