//! Binary machine code emission.
//!
//! The `binemit` module contains code for translating Cranelift's intermediate representation into
//! binary machine code.

mod memorysink;
mod stack_map;

pub use self::memorysink::{
    MemoryCodeSink, NullRelocSink, NullStackMapSink, NullTrapSink, RelocSink, StackMapSink,
    TrapSink,
};
pub use self::stack_map::StackMap;
use crate::ir::{ExternalName, Opcode, SourceLoc, TrapCode};
use core::fmt;
#[cfg(feature = "enable-serde")]
use serde::{Deserialize, Serialize};

/// Offset in bytes from the beginning of the function.
///
/// Cranelift can be used as a cross compiler, so we don't want to use a type like `usize` which
/// depends on the *host* platform, not the *target* platform.
pub type CodeOffset = u32;

/// Addend to add to the symbol value.
pub type Addend = i64;

/// Relocation kinds for every ISA
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "enable-serde", derive(Serialize, Deserialize))]
pub enum Reloc {
    /// absolute 4-byte
    Abs4,
    /// absolute 8-byte
    Abs8,
    /// x86 PC-relative 4-byte
    X86PCRel4,
    /// x86 call to PC-relative 4-byte
    X86CallPCRel4,
    /// x86 call to PLT-relative 4-byte
    X86CallPLTRel4,
    /// x86 GOT PC-relative 4-byte
    X86GOTPCRel4,
    /// Arm32 call target
    Arm32Call,
    /// Arm64 call target. Encoded as bottom 26 bits of instruction. This
    /// value is sign-extended, multiplied by 4, and added to the PC of
    /// the call instruction to form the destination address.
    Arm64Call,
    /// s390x PC-relative 4-byte offset
    S390xPCRel32Dbl,

    /// Elf x86_64 32 bit signed PC relative offset to two GOT entries for GD symbol.
    ElfX86_64TlsGd,

    /// Mach-O x86_64 32 bit signed PC relative offset to a `__thread_vars` entry.
    MachOX86_64Tlv,

    /// AArch64 TLS GD
    /// Set an ADRP immediate field to the top 21 bits of the final address. Checks for overflow.
    /// This is equivalent to `R_AARCH64_TLSGD_ADR_PAGE21` in the [aaelf64](https://github.com/ARM-software/abi-aa/blob/2bcab1e3b22d55170c563c3c7940134089176746/aaelf64/aaelf64.rst#relocations-for-thread-local-storage)
    Aarch64TlsGdAdrPage21,

    /// AArch64 TLS GD
    /// Set the add immediate field to the low 12 bits of the final address. Does not check for overflow.
    /// This is equivalent to `R_AARCH64_TLSGD_ADD_LO12_NC` in the [aaelf64](https://github.com/ARM-software/abi-aa/blob/2bcab1e3b22d55170c563c3c7940134089176746/aaelf64/aaelf64.rst#relocations-for-thread-local-storage)
    Aarch64TlsGdAddLo12Nc,
}

impl fmt::Display for Reloc {
    /// Display trait implementation drops the arch, since its used in contexts where the arch is
    /// already unambiguous, e.g. clif syntax with isa specified. In other contexts, use Debug.
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            Self::Abs4 => write!(f, "Abs4"),
            Self::Abs8 => write!(f, "Abs8"),
            Self::S390xPCRel32Dbl => write!(f, "PCRel32Dbl"),
            Self::X86PCRel4 => write!(f, "PCRel4"),
            Self::X86CallPCRel4 => write!(f, "CallPCRel4"),
            Self::X86CallPLTRel4 => write!(f, "CallPLTRel4"),
            Self::X86GOTPCRel4 => write!(f, "GOTPCRel4"),
            Self::Arm32Call | Self::Arm64Call => write!(f, "Call"),

            Self::ElfX86_64TlsGd => write!(f, "ElfX86_64TlsGd"),
            Self::MachOX86_64Tlv => write!(f, "MachOX86_64Tlv"),
            Self::Aarch64TlsGdAdrPage21 => write!(f, "Aarch64TlsGdAdrPage21"),
            Self::Aarch64TlsGdAddLo12Nc => write!(f, "Aarch64TlsGdAddLo12Nc"),
        }
    }
}

/// Container for information about a vector of compiled code and its supporting read-only data.
///
/// The code starts at offset 0 and is followed optionally by relocatable jump tables and copyable
/// (raw binary) read-only data.  Any padding between sections is always part of the section that
/// precedes the boundary between the sections.
#[derive(PartialEq)]
pub struct CodeInfo {
    /// Number of bytes of machine code (the code starts at offset 0).
    pub code_size: CodeOffset,

    /// Number of bytes of jumptables.
    pub jumptables_size: CodeOffset,

    /// Number of bytes of rodata.
    pub rodata_size: CodeOffset,

    /// Number of bytes in total.
    pub total_size: CodeOffset,
}

impl CodeInfo {
    /// Offset of any relocatable jump tables, or equal to rodata if there are no jump tables.
    pub fn jumptables(&self) -> CodeOffset {
        self.code_size
    }

    /// Offset of any copyable read-only data, or equal to total_size if there are no rodata.
    pub fn rodata(&self) -> CodeOffset {
        self.code_size + self.jumptables_size
    }
}

/// Abstract interface for adding bytes to the code segment.
///
/// A `CodeSink` will receive all of the machine code for a function. It also accepts relocations
/// which are locations in the code section that need to be fixed up when linking.
pub trait CodeSink {
    /// Get the current position.
    fn offset(&self) -> CodeOffset;

    /// Add 1 byte to the code section.
    fn put1(&mut self, _: u8);

    /// Add 2 bytes to the code section.
    fn put2(&mut self, _: u16);

    /// Add 4 bytes to the code section.
    fn put4(&mut self, _: u32);

    /// Add 8 bytes to the code section.
    fn put8(&mut self, _: u64);

    /// Add a relocation referencing an external symbol plus the addend at the current offset.
    fn reloc_external(&mut self, _: SourceLoc, _: Reloc, _: &ExternalName, _: Addend);

    /// Add trap information for the current offset.
    fn trap(&mut self, _: TrapCode, _: SourceLoc);

    /// Machine code output is complete, jump table data may follow.
    fn begin_jumptables(&mut self);

    /// Jump table output is complete, raw read-only data may follow.
    fn begin_rodata(&mut self);

    /// Read-only data output is complete, we're done.
    fn end_codegen(&mut self);

    /// Add a call site for a call with the given opcode, returning at the current offset.
    fn add_call_site(&mut self, _: Opcode, _: SourceLoc) {
        // Default implementation doesn't need to do anything.
    }
}
