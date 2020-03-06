//! Performs autodetection of the host for the purposes of running
//! Cranelift to generate code to run on the same machine.

#![deny(
    missing_docs,
    trivial_numeric_casts,
    unused_extern_crates,
    unstable_features
)]
#![warn(unused_import_braces)]
#![cfg_attr(feature = "clippy", plugin(clippy(conf_file = "../../clippy.toml")))]
#![cfg_attr(feature = "cargo-clippy", allow(clippy::new_without_default))]
#![cfg_attr(
    feature = "cargo-clippy",
    warn(
        clippy::float_arithmetic,
        clippy::mut_mut,
        clippy::nonminimal_bool,
        clippy::option_map_unwrap_or,
        clippy::option_map_unwrap_or_else,
        clippy::print_stdout,
        clippy::unicode_not_nfc,
        clippy::use_self
    )
)]
#![no_std]

use cranelift_codegen::isa;
use target_lexicon::Triple;

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
use raw_cpuid::CpuId;

/// Return an `isa` builder configured for the current host
/// machine, or `Err(())` if the host machine is not supported
/// in the current configuration.
pub fn builder() -> Result<isa::Builder, &'static str> {
    let mut isa_builder = isa::lookup(Triple::host()).map_err(|err| match err {
        isa::LookupError::SupportDisabled => "support for architecture disabled at compile time",
        isa::LookupError::Unsupported => "unsupported architecture",
    })?;

    if cfg!(any(target_arch = "x86", target_arch = "x86_64")) {
        parse_x86_cpuid(&mut isa_builder)?;
    }

    Ok(isa_builder)
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
fn parse_x86_cpuid(isa_builder: &mut isa::Builder) -> Result<(), &'static str> {
    use cranelift_codegen::settings::Configurable;
    let cpuid = CpuId::new();

    if let Some(info) = cpuid.get_feature_info() {
        if !info.has_sse2() {
            return Err("x86 support requires SSE2");
        }
        if info.has_sse3() {
            isa_builder.enable("has_sse3").unwrap();
        }
        if info.has_ssse3() {
            isa_builder.enable("has_ssse3").unwrap();
        }
        if info.has_sse41() {
            isa_builder.enable("has_sse41").unwrap();
        }
        if info.has_sse42() {
            isa_builder.enable("has_sse42").unwrap();
        }
        if info.has_popcnt() {
            isa_builder.enable("has_popcnt").unwrap();
        }
        if info.has_avx() {
            isa_builder.enable("has_avx").unwrap();
        }
    }
    if let Some(info) = cpuid.get_extended_feature_info() {
        if info.has_bmi1() {
            isa_builder.enable("has_bmi1").unwrap();
        }
        if info.has_bmi2() {
            isa_builder.enable("has_bmi2").unwrap();
        }
        if info.has_avx2() {
            isa_builder.enable("has_avx2").unwrap();
        }
        if info.has_avx512dq() {
            isa_builder.enable("has_avx512dq").unwrap();
        }
        if info.has_avx512vl() {
            isa_builder.enable("has_avx512vl").unwrap();
        }
    }
    if let Some(info) = cpuid.get_extended_function_info() {
        if info.has_lzcnt() {
            isa_builder.enable("has_lzcnt").unwrap();
        }
    }
    Ok(())
}

#[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
fn parse_x86_cpuid(_isa_builder: &mut isa::Builder) -> Result<(), &'static str> {
    unreachable!();
}

#[cfg(test)]
mod tests {
    use super::builder;
    use cranelift_codegen::isa::CallConv;
    use cranelift_codegen::settings;

    #[test]
    fn test() {
        if let Ok(isa_builder) = builder() {
            let flag_builder = settings::builder();
            let isa = isa_builder.finish(settings::Flags::new(flag_builder));
            if cfg!(any(unix, target_os = "nebulet")) {
                assert_eq!(isa.default_call_conv(), CallConv::SystemV);
            } else if cfg!(windows) {
                assert_eq!(isa.default_call_conv(), CallConv::WindowsFastcall);
            }
            if cfg!(target_pointer_width = "64") {
                assert_eq!(isa.pointer_bits(), 64);
            }
            if cfg!(target_pointer_width = "32") {
                assert_eq!(isa.pointer_bits(), 32);
            }
            if cfg!(target_pointer_width = "16") {
                assert_eq!(isa.pointer_bits(), 16);
            }
        }
    }
}

/// Version number of this crate.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
