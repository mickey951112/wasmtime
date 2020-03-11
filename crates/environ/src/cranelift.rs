//! Support for compiling with Cranelift.

use crate::address_map::{FunctionAddressMap, InstructionAddressMap};
use crate::cache::{ModuleCacheDataTupleType, ModuleCacheEntry};
use crate::compilation::{
    Compilation, CompileError, CompiledFunction, CompiledFunctionUnwindInfo, Relocation,
    RelocationTarget, TrapInformation,
};
use crate::frame_layout::FrameLayout;
use crate::func_environ::{get_func_name, FuncEnvironment};
use crate::module::{Module, ModuleLocal};
use crate::module_environ::FunctionBodyData;
use crate::CacheConfig;
use cranelift_codegen::ir::{self, ExternalName};
use cranelift_codegen::print_errors::pretty_error;
use cranelift_codegen::{binemit, isa, Context};
use cranelift_entity::PrimaryMap;
use cranelift_wasm::{DefinedFuncIndex, FuncIndex, FuncTranslator, ModuleTranslationState};
use rayon::prelude::{IntoParallelRefIterator, ParallelIterator};
use std::hash::{Hash, Hasher};

/// Implementation of a relocation sink that just saves all the information for later
pub struct RelocSink {
    /// Current function index.
    func_index: FuncIndex,

    /// Relocations recorded for the function.
    pub func_relocs: Vec<Relocation>,
}

impl binemit::RelocSink for RelocSink {
    fn reloc_block(
        &mut self,
        _offset: binemit::CodeOffset,
        _reloc: binemit::Reloc,
        _block_offset: binemit::CodeOffset,
    ) {
        // This should use the `offsets` field of `ir::Function`.
        panic!("block headers not yet implemented");
    }
    fn reloc_external(
        &mut self,
        offset: binemit::CodeOffset,
        reloc: binemit::Reloc,
        name: &ExternalName,
        addend: binemit::Addend,
    ) {
        let reloc_target = if let ExternalName::User { namespace, index } = *name {
            debug_assert_eq!(namespace, 0);
            RelocationTarget::UserFunc(FuncIndex::from_u32(index))
        } else if let ExternalName::LibCall(libcall) = *name {
            RelocationTarget::LibCall(libcall)
        } else {
            panic!("unrecognized external name")
        };
        self.func_relocs.push(Relocation {
            reloc,
            reloc_target,
            offset,
            addend,
        });
    }

    fn reloc_constant(
        &mut self,
        _code_offset: binemit::CodeOffset,
        _reloc: binemit::Reloc,
        _constant_offset: ir::ConstantOffset,
    ) {
        // Do nothing for now: cranelift emits constant data after the function code and also emits
        // function code with correct relative offsets to the constant data.
    }

    fn reloc_jt(&mut self, offset: binemit::CodeOffset, reloc: binemit::Reloc, jt: ir::JumpTable) {
        self.func_relocs.push(Relocation {
            reloc,
            reloc_target: RelocationTarget::JumpTable(self.func_index, jt),
            offset,
            addend: 0,
        });
    }
}

impl RelocSink {
    /// Return a new `RelocSink` instance.
    pub fn new(func_index: FuncIndex) -> Self {
        Self {
            func_index,
            func_relocs: Vec::new(),
        }
    }
}

struct TrapSink {
    pub traps: Vec<TrapInformation>,
}

impl TrapSink {
    fn new() -> Self {
        Self { traps: Vec::new() }
    }
}

impl binemit::TrapSink for TrapSink {
    fn trap(
        &mut self,
        code_offset: binemit::CodeOffset,
        source_loc: ir::SourceLoc,
        trap_code: ir::TrapCode,
    ) {
        self.traps.push(TrapInformation {
            code_offset,
            source_loc,
            trap_code,
        });
    }
}

fn get_function_address_map<'data>(
    context: &Context,
    data: &FunctionBodyData<'data>,
    body_len: usize,
    isa: &dyn isa::TargetIsa,
) -> FunctionAddressMap {
    let mut instructions = Vec::new();

    let func = &context.func;
    let mut blocks = func.layout.blocks().collect::<Vec<_>>();
    blocks.sort_by_key(|block| func.offsets[*block]); // Ensure inst offsets always increase

    let encinfo = isa.encoding_info();
    for block in blocks {
        for (offset, inst, size) in func.inst_offsets(block, &encinfo) {
            let srcloc = func.srclocs[inst];
            instructions.push(InstructionAddressMap {
                srcloc,
                code_offset: offset as usize,
                code_len: size as usize,
            });
        }
    }

    // Generate artificial srcloc for function start/end to identify boundary
    // within module. Similar to FuncTranslator::cur_srcloc(): it will wrap around
    // if byte code is larger than 4 GB.
    let start_srcloc = ir::SourceLoc::new(data.module_offset as u32);
    let end_srcloc = ir::SourceLoc::new((data.module_offset + data.data.len()) as u32);

    FunctionAddressMap {
        instructions,
        start_srcloc,
        end_srcloc,
        body_offset: 0,
        body_len,
    }
}

fn get_frame_layout(
    context: &Context,
    isa: &dyn isa::TargetIsa,
) -> (
    Box<[ir::FrameLayoutChange]>,
    Box<[(usize, ir::FrameLayoutChange)]>,
) {
    let func = &context.func;
    assert!(func.frame_layout.is_some(), "expected func.frame_layout");

    let mut blocks = func.layout.blocks().collect::<Vec<_>>();
    blocks.sort_by_key(|b| func.offsets[*b]); // Ensure inst offsets always increase

    let encinfo = isa.encoding_info();
    let mut last_offset = 0;
    let mut commands = Vec::new();
    for b in blocks {
        for (offset, inst, size) in func.inst_offsets(b, &encinfo) {
            if let Some(cmds) = func.frame_layout.as_ref().unwrap().instructions.get(&inst) {
                let address_offset = (offset + size) as usize;
                assert!(last_offset < address_offset);
                for cmd in cmds.iter() {
                    commands.push((address_offset, cmd.clone()));
                }
                last_offset = address_offset;
            }
        }
    }
    let initial = func.frame_layout.as_ref().unwrap().initial.clone();
    (initial, commands.into_boxed_slice())
}

/// A compiler that compiles a WebAssembly module with Cranelift, translating the Wasm to Cranelift IR,
/// optimizing it and then translating to assembly.
pub struct Cranelift;

impl crate::compilation::Compiler for Cranelift {
    /// Compile the module using Cranelift, producing a compilation result with
    /// associated relocations.
    fn compile_module(
        module: &Module,
        module_translation: &ModuleTranslationState,
        function_body_inputs: PrimaryMap<DefinedFuncIndex, FunctionBodyData<'_>>,
        isa: &dyn isa::TargetIsa,
        generate_debug_info: bool,
        cache_config: &CacheConfig,
    ) -> Result<ModuleCacheDataTupleType, CompileError> {
        let cache_entry = ModuleCacheEntry::new("cranelift", cache_config);

        let data = cache_entry.get_data(
            (
                &module.local,
                HashedModuleTranslationState(module_translation),
                function_body_inputs,
                Isa(isa),
                generate_debug_info,
            ),
            compile,
        )?;
        Ok(data.into_tuple())
    }
}

fn compile(
    (
        module,
        HashedModuleTranslationState(module_translation),
        function_body_inputs,
        Isa(isa),
        generate_debug_info,
    ): (
        &ModuleLocal,
        HashedModuleTranslationState<'_>,
        PrimaryMap<DefinedFuncIndex, FunctionBodyData<'_>>,
        Isa<'_, '_>,
        bool,
    ),
) -> Result<ModuleCacheDataTupleType, CompileError> {
    let mut functions = PrimaryMap::with_capacity(function_body_inputs.len());
    let mut relocations = PrimaryMap::with_capacity(function_body_inputs.len());
    let mut address_transforms = PrimaryMap::with_capacity(function_body_inputs.len());
    let mut value_ranges = PrimaryMap::with_capacity(function_body_inputs.len());
    let mut stack_slots = PrimaryMap::with_capacity(function_body_inputs.len());
    let mut traps = PrimaryMap::with_capacity(function_body_inputs.len());
    let mut frame_layouts = PrimaryMap::with_capacity(function_body_inputs.len());

    function_body_inputs
        .into_iter()
        .collect::<Vec<(DefinedFuncIndex, &FunctionBodyData<'_>)>>()
        .par_iter()
        .map_init(FuncTranslator::new, |func_translator, (i, input)| {
            let func_index = module.func_index(*i);
            let mut context = Context::new();
            context.func.name = get_func_name(func_index);
            context.func.signature = module.signatures[module.functions[func_index]].clone();
            context.func.collect_frame_layout_info();
            if generate_debug_info {
                context.func.collect_debug_info();
            }

            func_translator.translate(
                module_translation,
                input.data,
                input.module_offset,
                &mut context.func,
                &mut FuncEnvironment::new(isa.frontend_config(), module),
            )?;

            let mut code_buf: Vec<u8> = Vec::new();
            let mut reloc_sink = RelocSink::new(func_index);
            let mut trap_sink = TrapSink::new();
            let mut stackmap_sink = binemit::NullStackmapSink {};
            context
                .compile_and_emit(
                    isa,
                    &mut code_buf,
                    &mut reloc_sink,
                    &mut trap_sink,
                    &mut stackmap_sink,
                )
                .map_err(|error| {
                    CompileError::Codegen(pretty_error(&context.func, Some(isa), error))
                })?;

            let unwind_info = CompiledFunctionUnwindInfo::new(isa, &context);

            let address_transform = if generate_debug_info {
                let body_len = code_buf.len();
                Some(get_function_address_map(&context, input, body_len, isa))
            } else {
                None
            };

            let frame_layout = if generate_debug_info {
                let (initial_commands, commands) = get_frame_layout(&context, isa);
                Some(FrameLayout {
                    call_conv: context.func.signature.call_conv,
                    initial_commands,
                    commands,
                })
            } else {
                None
            };

            let ranges = if generate_debug_info {
                let ranges = context.build_value_labels_ranges(isa).map_err(|error| {
                    CompileError::Codegen(pretty_error(&context.func, Some(isa), error))
                })?;
                Some(ranges)
            } else {
                None
            };

            Ok((
                code_buf,
                context.func.jt_offsets,
                reloc_sink.func_relocs,
                address_transform,
                frame_layout,
                ranges,
                context.func.stack_slots,
                trap_sink.traps,
                unwind_info,
            ))
        })
        .collect::<Result<Vec<_>, CompileError>>()?
        .into_iter()
        .for_each(
            |(
                function,
                func_jt_offsets,
                relocs,
                address_transform,
                frame_layout,
                ranges,
                sss,
                function_traps,
                unwind_info,
            )| {
                functions.push(CompiledFunction {
                    body: function,
                    jt_offsets: func_jt_offsets,
                    unwind_info,
                });
                relocations.push(relocs);
                if let Some(address_transform) = address_transform {
                    address_transforms.push(address_transform);
                }
                value_ranges.push(ranges.unwrap_or_default());
                stack_slots.push(sss);
                traps.push(function_traps);
                if let Some(frame_layout) = frame_layout {
                    frame_layouts.push(frame_layout);
                }
            },
        );

    // TODO: Reorganize where we create the Vec for the resolved imports.

    Ok((
        Compilation::new(functions),
        relocations,
        address_transforms,
        value_ranges,
        stack_slots,
        traps,
        frame_layouts,
    ))
}

/// This is a wrapper struct to hash the specific bits of `TargetIsa` that
/// affect the output we care about. The trait itself can't implement `Hash`
/// (it's not object safe) so we have to implement our own hashing.
struct Isa<'a, 'b>(&'a (dyn isa::TargetIsa + 'b));

impl Hash for Isa<'_, '_> {
    fn hash<H: Hasher>(&self, hasher: &mut H) {
        self.0.triple().hash(hasher);

        // TODO: if this `to_string()` is too expensive then we should upstream
        // a native hashing ability of flags into cranelift itself, but
        // compilation and/or cache loading is relatively expensive so seems
        // unlikely.
        self.0.flags().to_string().hash(hasher);

        // TODO: ... and should we hash anything else? There's a lot of stuff in
        // `TargetIsa`, like registers/encodings/etc. Should we be hashing that
        // too? It seems like wasmtime doesn't configure it too too much, but
        // this may become an issue at some point.
    }
}

/// A wrapper struct around cranelift's `ModuleTranslationState` to implement
/// `Hash` since it's not `Hash` upstream yet.
///
/// TODO: we should upstream a `Hash` implementation, it would be very small! At
/// this moment though based on the definition it should be fine to not hash it
/// since we'll re-hash the signatures later.
struct HashedModuleTranslationState<'a>(&'a ModuleTranslationState);

impl Hash for HashedModuleTranslationState<'_> {
    fn hash<H: Hasher>(&self, _hasher: &mut H) {
        // nothing to hash right now
    }
}
