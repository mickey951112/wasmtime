//! JIT compilation.

use crate::instantiate::SetupError;
use crate::object::{build_object, ObjectUnwindInfo};
use cranelift_codegen::ir;
use wasmtime_debug::{emit_dwarf, DebugInfoData, DwarfSection};
use wasmtime_environ::entity::{EntityRef, PrimaryMap};
use wasmtime_environ::isa::{TargetFrontendConfig, TargetIsa};
use wasmtime_environ::wasm::{DefinedFuncIndex, DefinedMemoryIndex, MemoryIndex};
use wasmtime_environ::{
    CacheConfig, Compiler as _C, Module, ModuleAddressMap, ModuleMemoryOffset, ModuleTranslation,
    ModuleVmctxInfo, StackMaps, Traps, Tunables, VMOffsets, ValueLabelsRanges,
};
use wasmtime_runtime::InstantiationError;

/// Select which kind of compilation to use.
#[derive(Copy, Clone, Debug)]
pub enum CompilationStrategy {
    /// Let Wasmtime pick the strategy.
    Auto,

    /// Compile all functions with Cranelift.
    Cranelift,

    /// Compile all functions with Lightbeam.
    #[cfg(feature = "lightbeam")]
    Lightbeam,
}

/// A WebAssembly code JIT compiler.
///
/// A `Compiler` instance owns the executable memory that it allocates.
///
/// TODO: Evolve this to support streaming rather than requiring a `&[u8]`
/// containing a whole wasm module at once.
///
/// TODO: Consider using cranelift-module.
pub struct Compiler {
    isa: Box<dyn TargetIsa>,
    strategy: CompilationStrategy,
    cache_config: CacheConfig,
    tunables: Tunables,
}

impl Compiler {
    /// Construct a new `Compiler`.
    pub fn new(
        isa: Box<dyn TargetIsa>,
        strategy: CompilationStrategy,
        cache_config: CacheConfig,
        tunables: Tunables,
    ) -> Self {
        Self {
            isa,
            strategy,
            cache_config,
            tunables,
        }
    }
}

fn _assert_compiler_send_sync() {
    fn _assert<T: Send + Sync>() {}
    _assert::<Compiler>();
}

fn transform_dwarf_data(
    isa: &dyn TargetIsa,
    module: &Module,
    debug_data: &DebugInfoData,
    address_transform: &ModuleAddressMap,
    value_ranges: &ValueLabelsRanges,
    stack_slots: PrimaryMap<DefinedFuncIndex, ir::StackSlots>,
    compilation: &wasmtime_environ::Compilation,
) -> Result<Vec<DwarfSection>, SetupError> {
    let target_config = isa.frontend_config();
    let ofs = VMOffsets::new(target_config.pointer_bytes(), &module.local);

    let module_vmctx_info = {
        ModuleVmctxInfo {
            memory_offset: if ofs.num_imported_memories > 0 {
                ModuleMemoryOffset::Imported(ofs.vmctx_vmmemory_import(MemoryIndex::new(0)))
            } else if ofs.num_defined_memories > 0 {
                ModuleMemoryOffset::Defined(
                    ofs.vmctx_vmmemory_definition_base(DefinedMemoryIndex::new(0)),
                )
            } else {
                ModuleMemoryOffset::None
            },
            stack_slots,
        }
    };
    emit_dwarf(
        isa,
        debug_data,
        &address_transform,
        &module_vmctx_info,
        &value_ranges,
        &compilation,
    )
    .map_err(SetupError::DebugInfo)
}

#[allow(missing_docs)]
pub struct Compilation {
    pub obj: Vec<u8>,
    pub unwind_info: Vec<ObjectUnwindInfo>,
    pub traps: Traps,
    pub stack_maps: StackMaps,
    pub address_transform: ModuleAddressMap,
}

impl Compiler {
    /// Return the isa.
    pub fn isa(&self) -> &dyn TargetIsa {
        self.isa.as_ref()
    }

    /// Return the target's frontend configuration settings.
    pub fn frontend_config(&self) -> TargetFrontendConfig {
        self.isa.frontend_config()
    }

    /// Return the tunables in use by this engine.
    pub fn tunables(&self) -> &Tunables {
        &self.tunables
    }

    /// Compile the given function bodies.
    pub(crate) fn compile<'data>(
        &self,
        translation: &ModuleTranslation,
        debug_data: Option<DebugInfoData>,
    ) -> Result<Compilation, SetupError> {
        let (
            compilation,
            relocations,
            address_transform,
            value_ranges,
            stack_slots,
            traps,
            stack_maps,
        ) = match self.strategy {
            // For now, interpret `Auto` as `Cranelift` since that's the most stable
            // implementation.
            CompilationStrategy::Auto | CompilationStrategy::Cranelift => {
                wasmtime_environ::cranelift::Cranelift::compile_module(
                    translation,
                    &*self.isa,
                    &self.cache_config,
                )
            }
            #[cfg(feature = "lightbeam")]
            CompilationStrategy::Lightbeam => {
                wasmtime_environ::lightbeam::Lightbeam::compile_module(
                    translation,
                    &*self.isa,
                    &self.cache_config,
                )
            }
        }
        .map_err(SetupError::Compile)?;

        let dwarf_sections = if debug_data.is_some() && !compilation.is_empty() {
            transform_dwarf_data(
                &*self.isa,
                &translation.module,
                debug_data.as_ref().unwrap(),
                &address_transform,
                &value_ranges,
                stack_slots,
                &compilation,
            )?
        } else {
            vec![]
        };

        let (obj, unwind_info) = build_object(
            &*self.isa,
            &compilation,
            &relocations,
            &translation.module,
            &dwarf_sections,
        )?;
        let obj = obj.write().map_err(|_| {
            SetupError::Instantiate(InstantiationError::Resource(
                "failed to create image memory".to_string(),
            ))
        })?;

        Ok(Compilation {
            obj,
            unwind_info,
            traps,
            stack_maps,
            address_transform,
        })
    }
}
