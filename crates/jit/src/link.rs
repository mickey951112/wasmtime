//! Linking for JIT-compiled code.

use object::read::{Object, ObjectSection, Relocation, RelocationTarget};
use object::{elf, File, ObjectSymbol, RelocationEncoding, RelocationKind};
use std::ptr::{read_unaligned, write_unaligned};
use wasmtime_runtime::libcalls;
use wasmtime_runtime::VMFunctionBody;

/// Links a module that has been compiled with `compiled_module` in `wasmtime-environ`.
///
/// Performs all required relocations inside the function code, provided the necessary metadata.
/// The relocations data provided in the object file, see object.rs for details.
///
/// Currently, the produced ELF image can be trusted.
/// TODO refactor logic to remove panics and add defensive code the image data
/// becomes untrusted.
pub fn link_module(obj: &File, code_range: &mut [u8]) {
    // Read the ".text" section and process its relocations.
    let text_section = obj.section_by_name(".text").unwrap();
    let body = code_range.as_ptr() as *const VMFunctionBody;

    for (offset, r) in text_section.relocations() {
        apply_reloc(obj, body, offset, r);
    }
}

fn apply_reloc(obj: &File, body: *const VMFunctionBody, offset: u64, r: Relocation) {
    let target_func_address: usize = match r.target() {
        RelocationTarget::Symbol(i) => {
            // Processing relocation target is a named symbols that is compiled
            // wasm function or runtime libcall.
            let sym = obj.symbol_by_index(i).unwrap();
            if sym.is_local() {
                unsafe { body.add(sym.address() as usize) as usize }
            } else {
                match sym.name() {
                    Ok(name) => {
                        if let Some(addr) = to_libcall_address(name) {
                            addr
                        } else {
                            panic!("unknown function to link: {}", name);
                        }
                    }
                    Err(_) => panic!("unexpected relocation target: not a symbol"),
                }
            }
        }
        _ => panic!("unexpected relocation target"),
    };

    match (r.kind(), r.encoding(), r.size()) {
        #[cfg(target_pointer_width = "64")]
        (RelocationKind::Absolute, RelocationEncoding::Generic, 64) => unsafe {
            let reloc_address = body.add(offset as usize) as usize;
            let reloc_addend = r.addend() as isize;
            let reloc_abs = (target_func_address as u64)
                .checked_add(reloc_addend as u64)
                .unwrap();
            write_unaligned(reloc_address as *mut u64, reloc_abs);
        },
        #[cfg(target_pointer_width = "32")]
        (RelocationKind::Relative, RelocationEncoding::Generic, 32) => unsafe {
            let reloc_address = body.add(offset as usize) as usize;
            let reloc_addend = r.addend() as isize;
            let reloc_delta_u32 = (target_func_address as u32)
                .wrapping_sub(reloc_address as u32)
                .checked_add(reloc_addend as u32)
                .unwrap();
            write_unaligned(reloc_address as *mut u32, reloc_delta_u32);
        },
        #[cfg(target_pointer_width = "32")]
        (RelocationKind::Relative, RelocationEncoding::X86Branch, 32) => unsafe {
            let reloc_address = body.add(offset as usize) as usize;
            let reloc_addend = r.addend() as isize;
            let reloc_delta_u32 = (target_func_address as u32)
                .wrapping_sub(reloc_address as u32)
                .wrapping_add(reloc_addend as u32);
            write_unaligned(reloc_address as *mut u32, reloc_delta_u32);
        },
        #[cfg(target_pointer_width = "64")]
        (RelocationKind::Relative, RelocationEncoding::Generic, 32) => unsafe {
            let reloc_address = body.add(offset as usize) as usize;
            let reloc_addend = r.addend() as isize;
            let reloc_delta_u64 = (target_func_address as u64)
                .wrapping_sub(reloc_address as u64)
                .wrapping_add(reloc_addend as u64);
            // TODO implement far calls mode in x64 new backend.
            assert!(
                reloc_delta_u64 as isize <= i32::max_value() as isize,
                "relocation too large to fit in i32"
            );
            write_unaligned(reloc_address as *mut u32, reloc_delta_u64 as u32);
        },
        #[cfg(target_pointer_width = "64")]
        (RelocationKind::Relative, RelocationEncoding::S390xDbl, 32) => unsafe {
            let reloc_address = body.add(offset as usize) as usize;
            let reloc_addend = r.addend() as isize;
            let reloc_delta_u64 = (target_func_address as u64)
                .wrapping_sub(reloc_address as u64)
                .wrapping_add(reloc_addend as u64);
            assert!(
                (reloc_delta_u64 as isize) >> 1 <= i32::max_value() as isize,
                "relocation too large to fit in i32"
            );
            write_unaligned(reloc_address as *mut u32, (reloc_delta_u64 >> 1) as u32);
        },
        (RelocationKind::Elf(elf::R_AARCH64_CALL26), RelocationEncoding::Generic, 32) => unsafe {
            let reloc_address = body.add(offset as usize) as usize;
            let reloc_addend = r.addend() as isize;
            let reloc_delta = (target_func_address as u64).wrapping_sub(reloc_address as u64);
            // TODO: come up with a PLT-like solution for longer calls. We can't extend the
            // code segment at this point, but we could conservatively allocate space at the
            // end of the function during codegen, a fixed amount per call, to allow for
            // potential branch islands.
            assert!((reloc_delta as i64) < (1 << 27));
            assert!((reloc_delta as i64) >= -(1 << 27));
            let reloc_delta = reloc_delta as u32;
            let reloc_delta = reloc_delta.wrapping_add(reloc_addend as u32);
            let delta_bits = reloc_delta >> 2;
            let insn = read_unaligned(reloc_address as *const u32);
            let new_insn = (insn & 0xfc00_0000) | (delta_bits & 0x03ff_ffff);
            write_unaligned(reloc_address as *mut u32, new_insn);
        },
        other => panic!("unsupported reloc kind: {:?}", other),
    }
}

fn to_libcall_address(name: &str) -> Option<usize> {
    use self::libcalls::*;
    use wasmtime_environ::for_each_libcall;
    macro_rules! add_libcall_symbol {
        [$(($libcall:ident, $export:ident)),*] => {
            Some(match name {
                $(
                    stringify!($export) => $export as usize,
                )+
                _ => {
                    return None;
                }
            })
        };
    }
    for_each_libcall!(add_libcall_symbol)
}
