use anyhow::{anyhow, Result};
use core::fmt::Formatter;
use cranelift_codegen::isa::CallConv;
use std::{
    error,
    fmt::{self, Debug, Display},
};
use target_lexicon::{Architecture, Triple};
use wasmparser::{FuncType, FuncValidator, FunctionBody, ValidatorResources};

#[cfg(feature = "x64")]
pub(crate) mod x64;

#[cfg(feature = "arm64")]
pub(crate) mod aarch64;

pub(crate) mod reg;

macro_rules! isa {
    ($name: ident, $cfg_terms: tt, $triple: ident) => {{
        #[cfg $cfg_terms]
        {
            Ok(Box::new($name::isa_from($triple)))
        }
        #[cfg(not $cfg_terms)]
        {
            Err(anyhow!(LookupError::SupportDisabled))
        }
    }};
}

/// Look for an ISA for the given target triple.
//
// The ISA, as it's currently implemented in Cranelift
// needs a builder since it adds settings
// depending on those available in the host architecture.
// I'm intentionally skipping the builder for now.
// The lookup method will return the ISA directly.
//
// Once features like SIMD are supported, returning a builder
// will make more sense.
pub fn lookup(triple: Triple) -> Result<Box<dyn TargetIsa>> {
    match triple.architecture {
        Architecture::X86_64 => {
            isa!(x64, (feature = "x64"), triple)
        }
        Architecture::Aarch64 { .. } => {
            isa!(aarch64, (feature = "arm64"), triple)
        }

        _ => Err(anyhow!(LookupError::Unsupported)),
    }
}

impl error::Error for LookupError {}
impl Display for LookupError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            LookupError::Unsupported => write!(f, "This target is not supported yet"),
            LookupError::SupportDisabled => write!(f, "Support for this target was disabled"),
        }
    }
}

#[derive(Debug)]
pub(crate) enum LookupError {
    Unsupported,
    // This directive covers the case in which the consumer
    // enables the `all-arch` feature; in such case, this variant
    // will never be used. This is most likely going to change
    // in the future; this is one of the simplest options for now.
    #[allow(dead_code)]
    SupportDisabled,
}

/// A trait representing commonalities between the supported
/// instruction set architectures.
pub trait TargetIsa: Send + Sync {
    /// Get the name of the ISA.
    fn name(&self) -> &'static str;

    /// Get the target triple of the ISA.
    fn triple(&self) -> &Triple;

    fn compile_function(
        &self,
        sig: &FuncType,
        body: &FunctionBody,
        validator: FuncValidator<ValidatorResources>,
    ) -> Result<Vec<String>>;

    /// Get the default calling convention of the underlying target triple.
    fn call_conv(&self) -> CallConv {
        CallConv::triple_default(&self.triple())
    }

    /// Get the endianess of the underlying target triple.
    fn endianness(&self) -> target_lexicon::Endianness {
        self.triple().endianness().unwrap()
    }
}

impl Debug for &dyn TargetIsa {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Target ISA {{ triple: {:?}, calling convention: {:?} }}",
            self.triple(),
            self.call_conv()
        )
    }
}
