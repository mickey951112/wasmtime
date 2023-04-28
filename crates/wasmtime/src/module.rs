use crate::code::CodeObject;
use crate::{
    signatures::SignatureCollection,
    types::{ExportType, ExternType, ImportType},
    Engine,
};
use anyhow::{bail, Context, Result};
use once_cell::sync::OnceCell;
use std::any::Any;
use std::collections::BTreeMap;
use std::fs;
use std::mem;
use std::ops::Range;
use std::path::Path;
use std::ptr::NonNull;
use std::sync::Arc;
use wasmparser::{Parser, ValidPayload, Validator};
use wasmtime_environ::{
    DefinedFuncIndex, DefinedMemoryIndex, FuncIndex, FunctionLoc, HostPtr, ModuleEnvironment,
    ModuleTranslation, ModuleType, ModuleTypes, ObjectKind, PrimaryMap, SignatureIndex, VMOffsets,
    WasmFunctionInfo,
};
use wasmtime_jit::{
    CodeMemory, CompiledFunctionInfo, CompiledModule, CompiledModuleInfo, ObjectBuilder,
};
use wasmtime_runtime::{
    CompiledModuleId, MemoryImage, MmapVec, ModuleMemoryImages, VMArrayCallFunction,
    VMNativeCallFunction, VMSharedSignatureIndex, VMWasmCallFunction,
};

mod registry;

pub use registry::{is_wasm_trap_pc, register_code, unregister_code, ModuleRegistry};

/// A compiled WebAssembly module, ready to be instantiated.
///
/// A `Module` is a compiled in-memory representation of an input WebAssembly
/// binary. A `Module` is then used to create an [`Instance`](crate::Instance)
/// through an instantiation process. You cannot call functions or fetch
/// globals, for example, on a `Module` because it's purely a code
/// representation. Instead you'll need to create an
/// [`Instance`](crate::Instance) to interact with the wasm module.
///
/// Creating a `Module` currently involves compiling code, meaning that it can
/// be an expensive operation. All `Module` instances are compiled according to
/// the configuration in [`Config`], but typically they're JIT-compiled. If
/// you'd like to instantiate a module multiple times you can do so with
/// compiling the original wasm module only once with a single [`Module`]
/// instance.
///
/// The `Module` is thread-safe and safe to share across threads.
///
/// ## Modules and `Clone`
///
/// Using `clone` on a `Module` is a cheap operation. It will not create an
/// entirely new module, but rather just a new reference to the existing module.
/// In other words it's a shallow copy, not a deep copy.
///
/// ## Examples
///
/// There are a number of ways you can create a `Module`, for example pulling
/// the bytes from a number of locations. One example is loading a module from
/// the filesystem:
///
/// ```no_run
/// # use wasmtime::*;
/// # fn main() -> anyhow::Result<()> {
/// let engine = Engine::default();
/// let module = Module::from_file(&engine, "path/to/foo.wasm")?;
/// # Ok(())
/// # }
/// ```
///
/// You can also load the wasm text format if more convenient too:
///
/// ```no_run
/// # use wasmtime::*;
/// # fn main() -> anyhow::Result<()> {
/// let engine = Engine::default();
/// // Now we're using the WebAssembly text extension: `.wat`!
/// let module = Module::from_file(&engine, "path/to/foo.wat")?;
/// # Ok(())
/// # }
/// ```
///
/// And if you've already got the bytes in-memory you can use the
/// [`Module::new`] constructor:
///
/// ```no_run
/// # use wasmtime::*;
/// # fn main() -> anyhow::Result<()> {
/// let engine = Engine::default();
/// # let wasm_bytes: Vec<u8> = Vec::new();
/// let module = Module::new(&engine, &wasm_bytes)?;
///
/// // It also works with the text format!
/// let module = Module::new(&engine, "(module (func))")?;
/// # Ok(())
/// # }
/// ```
///
/// [`Config`]: crate::Config
#[derive(Clone)]
pub struct Module {
    inner: Arc<ModuleInner>,
}

struct ModuleInner {
    engine: Engine,
    /// The compiled artifacts for this module that will be instantiated and
    /// executed.
    module: CompiledModule,

    /// Runtime information such as the underlying mmap, type information, etc.
    ///
    /// Note that this `Arc` is used to share information between compiled
    /// modules within a component. For bare core wasm modules created with
    /// `Module::new`, for example, this is a uniquely owned `Arc`.
    code: Arc<CodeObject>,

    /// A set of initialization images for memories, if any.
    ///
    /// Note that this is behind a `OnceCell` to lazily create this image. On
    /// Linux where `memfd_create` may be used to create the backing memory
    /// image this is a pretty expensive operation, so by deferring it this
    /// improves memory usage for modules that are created but may not ever be
    /// instantiated.
    memory_images: OnceCell<Option<ModuleMemoryImages>>,

    /// Flag indicating whether this module can be serialized or not.
    serializable: bool,

    /// Runtime offset information for `VMContext`.
    offsets: VMOffsets<HostPtr>,
}

pub(crate) struct CompileFunctionResult {
    info: WasmFunctionInfo,
    function: Box<dyn Any + Send>,
    // These trampolines are only present if the function can escape.
    array_to_wasm_trampoline: Option<Box<dyn Any + Send>>,
    native_to_wasm_trampoline: Option<Box<dyn Any + Send>>,
}

pub(crate) struct ModuleFunctionIndices<'a> {
    translation: ModuleTranslation<'a>,
    func_infos: PrimaryMap<DefinedFuncIndex, WasmFunctionInfo>,

    // Indices within the associated `compiled_funcs` for various types of code.
    func_indices: Vec<usize>,
    array_to_wasm_trampoline_indices: Vec<(usize, DefinedFuncIndex)>,
    native_to_wasm_trampoline_indices: Vec<(usize, DefinedFuncIndex)>,
}

impl<'a> ModuleFunctionIndices<'a> {
    pub(crate) fn new(
        translation: ModuleTranslation<'a>,
        function_compilations: Vec<CompileFunctionResult>,
        symbol_prefix: &str,
        compiled_funcs: &mut Vec<(String, Box<dyn Any + Send>)>,
    ) -> Self {
        let mut func_infos = PrimaryMap::with_capacity(function_compilations.len());
        let mut func_indices = Vec::with_capacity(function_compilations.len());

        // Place all wasm-compiled functions, in order, into the final object.
        // This should help keep a sense of locality between functions, if
        // necessary.
        //
        // Trampolines are deferred to get inserted after all wasm functions
        // since they don't need the same locality and also don't require
        // alignment since they're not hot.
        let mut array_to_wasm_trampolines = Vec::new();
        let mut native_to_wasm_trampolines = Vec::new();
        for CompileFunctionResult {
            info,
            function,
            array_to_wasm_trampoline,
            native_to_wasm_trampoline,
        } in function_compilations
        {
            let def_idx = func_infos.push(info);
            let idx = translation.module.func_index(def_idx).as_u32();

            if let Some(trampoline) = array_to_wasm_trampoline {
                let sym = format!("{symbol_prefix}_array_to_wasm_trampoline_{idx}");
                array_to_wasm_trampolines.push((def_idx, (sym, trampoline)));
            }

            if let Some(trampoline) = native_to_wasm_trampoline {
                let sym = format!("{symbol_prefix}_native_to_wasm_trampoline_{idx}");
                native_to_wasm_trampolines.push((def_idx, (sym, trampoline)));
            }

            let sym = format!("{symbol_prefix}_function_{idx}");
            func_indices.push(compiled_funcs.len());
            compiled_funcs.push((sym, function));
        }

        let mut array_to_wasm_trampoline_indices = vec![];
        for (def_idx, pair) in array_to_wasm_trampolines {
            array_to_wasm_trampoline_indices.push((compiled_funcs.len(), def_idx));
            compiled_funcs.push(pair);
        }

        let mut native_to_wasm_trampoline_indices = vec![];
        for (def_idx, pair) in native_to_wasm_trampolines {
            native_to_wasm_trampoline_indices.push((compiled_funcs.len(), def_idx));
            compiled_funcs.push(pair);
        }

        ModuleFunctionIndices {
            translation,
            func_infos,
            func_indices,
            array_to_wasm_trampoline_indices,
            native_to_wasm_trampoline_indices,
        }
    }

    pub(crate) fn resolve_reloc(&self, idx: FuncIndex) -> usize {
        let defined = self.translation.module.defined_func_index(idx).unwrap();
        self.func_indices[defined.as_u32() as usize]
    }

    pub(crate) fn append_to_object(
        self,
        locs: &[(object::write::SymbolId, FunctionLoc)],
        wasm_to_native_trampoline_indices: &[(usize, SignatureIndex)],
        object: &mut ObjectBuilder,
    ) -> Result<CompiledModuleInfo> {
        let funcs: PrimaryMap<DefinedFuncIndex, CompiledFunctionInfo> = self
            .func_infos
            .into_iter()
            .enumerate()
            .zip(self.func_indices.iter().copied().map(|i| locs[i].1))
            .map(
                |((defined_func_index, (_id, wasm_func_info)), wasm_func_loc)| {
                    let defined_func_index =
                        DefinedFuncIndex::from_u32(u32::try_from(defined_func_index).unwrap());

                    let array_to_wasm_trampoline_index = self
                        .array_to_wasm_trampoline_indices
                        .binary_search_by_key(&defined_func_index, |(_i, def_func_idx)| {
                            *def_func_idx
                        })
                        .ok();
                    let array_to_wasm_trampoline = array_to_wasm_trampoline_index.map(|i| {
                        let compiled_func_index = self.array_to_wasm_trampoline_indices[i].0;
                        locs[compiled_func_index].1
                    });

                    let native_to_wasm_trampoline_index = self
                        .native_to_wasm_trampoline_indices
                        .binary_search_by_key(&defined_func_index, |(_i, def_func_idx)| {
                            *def_func_idx
                        })
                        .ok();
                    let native_to_wasm_trampoline = native_to_wasm_trampoline_index.map(|i| {
                        let compiled_func_index = self.native_to_wasm_trampoline_indices[i].0;
                        locs[compiled_func_index].1
                    });

                    CompiledFunctionInfo::new(
                        wasm_func_info,
                        wasm_func_loc,
                        array_to_wasm_trampoline,
                        native_to_wasm_trampoline,
                    )
                },
            )
            .collect();

        let wasm_to_native_trampolines = wasm_to_native_trampoline_indices
            .iter()
            .map(|&(i, sig_idx)| (sig_idx, locs[i].1))
            .collect();

        object.append(self.translation, funcs, wasm_to_native_trampolines)
    }
}

impl Module {
    /// Creates a new WebAssembly `Module` from the given in-memory `bytes`.
    ///
    /// The `bytes` provided must be in one of the following formats:
    ///
    /// * A [binary-encoded][binary] WebAssembly module. This is always supported.
    /// * A [text-encoded][text] instance of the WebAssembly text format.
    ///   This is only supported when the `wat` feature of this crate is enabled.
    ///   If this is supplied then the text format will be parsed before validation.
    ///   Note that the `wat` feature is enabled by default.
    ///
    /// The data for the wasm module must be loaded in-memory if it's present
    /// elsewhere, for example on disk. This requires that the entire binary is
    /// loaded into memory all at once, this API does not support streaming
    /// compilation of a module.
    ///
    /// If the module has not been already been compiled, the WebAssembly binary will
    /// be decoded and validated. It will also be compiled according to the
    /// configuration of the provided `engine`.
    ///
    /// # Errors
    ///
    /// This function may fail and return an error. Errors may include
    /// situations such as:
    ///
    /// * The binary provided could not be decoded because it's not a valid
    ///   WebAssembly binary
    /// * The WebAssembly binary may not validate (e.g. contains type errors)
    /// * Implementation-specific limits were exceeded with a valid binary (for
    ///   example too many locals)
    /// * The wasm binary may use features that are not enabled in the
    ///   configuration of `engine`
    /// * If the `wat` feature is enabled and the input is text, then it may be
    ///   rejected if it fails to parse.
    ///
    /// The error returned should contain full information about why module
    /// creation failed if one is returned.
    ///
    /// [binary]: https://webassembly.github.io/spec/core/binary/index.html
    /// [text]: https://webassembly.github.io/spec/core/text/index.html
    ///
    /// # Examples
    ///
    /// The `new` function can be invoked with a in-memory array of bytes:
    ///
    /// ```no_run
    /// # use wasmtime::*;
    /// # fn main() -> anyhow::Result<()> {
    /// # let engine = Engine::default();
    /// # let wasm_bytes: Vec<u8> = Vec::new();
    /// let module = Module::new(&engine, &wasm_bytes)?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// Or you can also pass in a string to be parsed as the wasm text
    /// format:
    ///
    /// ```
    /// # use wasmtime::*;
    /// # fn main() -> anyhow::Result<()> {
    /// # let engine = Engine::default();
    /// let module = Module::new(&engine, "(module (func))")?;
    /// # Ok(())
    /// # }
    /// ```
    #[cfg(compiler)]
    #[cfg_attr(nightlydoc, doc(cfg(any(feature = "cranelift", feature = "winch"))))] // see build.rs
    pub fn new(engine: &Engine, bytes: impl AsRef<[u8]>) -> Result<Module> {
        let bytes = bytes.as_ref();
        #[cfg(feature = "wat")]
        let bytes = wat::parse_bytes(bytes)?;
        Self::from_binary(engine, &bytes)
    }

    /// Creates a new WebAssembly `Module` from the contents of the given
    /// `file` on disk.
    ///
    /// This is a convenience function that will read the `file` provided and
    /// pass the bytes to the [`Module::new`] function. For more information
    /// see [`Module::new`]
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use wasmtime::*;
    /// # fn main() -> anyhow::Result<()> {
    /// let engine = Engine::default();
    /// let module = Module::from_file(&engine, "./path/to/foo.wasm")?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// The `.wat` text format is also supported:
    ///
    /// ```no_run
    /// # use wasmtime::*;
    /// # fn main() -> anyhow::Result<()> {
    /// # let engine = Engine::default();
    /// let module = Module::from_file(&engine, "./path/to/foo.wat")?;
    /// # Ok(())
    /// # }
    /// ```
    #[cfg(compiler)]
    #[cfg_attr(nightlydoc, doc(cfg(any(feature = "cranelift", feature = "winch"))))] // see build.rs
    pub fn from_file(engine: &Engine, file: impl AsRef<Path>) -> Result<Module> {
        match Self::new(
            engine,
            &fs::read(&file).with_context(|| "failed to read input file")?,
        ) {
            Ok(m) => Ok(m),
            Err(e) => {
                cfg_if::cfg_if! {
                    if #[cfg(feature = "wat")] {
                        let mut e = e.downcast::<wat::Error>()?;
                        e.set_path(file);
                        bail!(e)
                    } else {
                        Err(e)
                    }
                }
            }
        }
    }

    /// Creates a new WebAssembly `Module` from the given in-memory `binary`
    /// data.
    ///
    /// This is similar to [`Module::new`] except that it requires that the
    /// `binary` input is a WebAssembly binary, the text format is not supported
    /// by this function. It's generally recommended to use [`Module::new`], but
    /// if it's required to not support the text format this function can be
    /// used instead.
    ///
    /// # Examples
    ///
    /// ```
    /// # use wasmtime::*;
    /// # fn main() -> anyhow::Result<()> {
    /// # let engine = Engine::default();
    /// let wasm = b"\0asm\x01\0\0\0";
    /// let module = Module::from_binary(&engine, wasm)?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// Note that the text format is **not** accepted by this function:
    ///
    /// ```
    /// # use wasmtime::*;
    /// # fn main() -> anyhow::Result<()> {
    /// # let engine = Engine::default();
    /// assert!(Module::from_binary(&engine, b"(module)").is_err());
    /// # Ok(())
    /// # }
    /// ```
    #[cfg(compiler)]
    #[cfg_attr(nightlydoc, doc(cfg(any(feature = "cranelift", feature = "winch"))))] // see build.rs
    pub fn from_binary(engine: &Engine, binary: &[u8]) -> Result<Module> {
        engine
            .check_compatible_with_native_host()
            .context("compilation settings are not compatible with the native host")?;

        cfg_if::cfg_if! {
            if #[cfg(feature = "cache")] {
                let state = (HashedEngineCompileEnv(engine), binary);
                let (code, info_and_types) = wasmtime_cache::ModuleCacheEntry::new(
                    "wasmtime",
                    engine.cache_config(),
                )
                .get_data_raw(
                    &state,

                    // Cache miss, compute the actual artifacts
                    |(engine, wasm)| -> Result<_> {
                        let (mmap, info) = Module::build_artifacts(engine.0, wasm)?;
                        let code = publish_mmap(mmap)?;
                        Ok((code, info))
                    },

                    // Implementation of how to serialize artifacts
                    |(_engine, _wasm), (code, _info_and_types)| {
                        Some(code.mmap().to_vec())
                    },

                    // Cache hit, deserialize the provided artifacts
                    |(engine, _wasm), serialized_bytes| {
                        let code = engine.0.load_code_bytes(&serialized_bytes, ObjectKind::Module).ok()?;
                        Some((code, None))
                    },
                )?;
            } else {
                let (mmap, info_and_types) = Module::build_artifacts(engine, binary)?;
                let code = publish_mmap(mmap)?;
            }
        };

        let info_and_types = info_and_types.map(|(info, types)| (info, types.into()));
        return Self::from_parts(engine, code, info_and_types);

        fn publish_mmap(mmap: MmapVec) -> Result<Arc<CodeMemory>> {
            let mut code = CodeMemory::new(mmap)?;
            code.publish()?;
            Ok(Arc::new(code))
        }
    }

    /// Creates a new WebAssembly `Module` from the contents of the given `file`
    /// on disk, but with assumptions that the file is from a trusted source.
    /// The file should be a binary- or text-format WebAssembly module, or a
    /// precompiled artifact generated by the same version of Wasmtime.
    ///
    /// # Unsafety
    ///
    /// All of the reasons that [`deserialize`] is `unsafe` apply to this
    /// function as well. Arbitrary data loaded from a file may trick Wasmtime
    /// into arbitrary code execution since the contents of the file are not
    /// validated to be a valid precompiled module.
    ///
    /// [`deserialize`]: Module::deserialize
    ///
    /// Additionally though this function is also `unsafe` because the file
    /// referenced must remain unchanged and a valid precompiled module for the
    /// entire lifetime of the [`Module`] returned. Any changes to the file on
    /// disk may change future instantiations of the module to be incorrect.
    /// This is because the file is mapped into memory and lazily loaded pages
    /// reflect the current state of the file, not necessarily the origianl
    /// state of the file.
    #[cfg(compiler)]
    #[cfg_attr(nightlydoc, doc(cfg(any(feature = "cranelift", feature = "winch"))))] // see build.rs
    pub unsafe fn from_trusted_file(engine: &Engine, file: impl AsRef<Path>) -> Result<Module> {
        let mmap = MmapVec::from_file(file.as_ref())?;
        if &mmap[0..4] == b"\x7fELF" {
            let code = engine.load_code(mmap, ObjectKind::Module)?;
            return Module::from_parts(engine, code, None);
        }

        Module::new(engine, &*mmap)
    }

    #[cfg(compiler)]
    pub(crate) fn compile_wasm_to_native_trampolines(
        engine: &Engine,
        translations: &[ModuleTranslation<'_>],
        types: &ModuleTypes,
        compiled_funcs: &mut Vec<(String, Box<dyn Any + Send>)>,
    ) -> Result<Vec<(usize, SignatureIndex)>> {
        let mut sigs = BTreeMap::new();
        for trans in translations.iter() {
            sigs.extend(trans.module.types.iter().filter_map(|(_, ty)| match ty {
                ModuleType::Function(ty) => Some((*ty, trans)),
            }));
        }

        let trampolines = engine.run_maybe_parallel(
            sigs.into_iter().collect(),
            |(sig_index, translation)| -> Result<_> {
                let wasm_func_ty = &types[sig_index];
                Ok((
                    format!("wasm_to_native_trampoline[{}]", sig_index.as_u32()),
                    sig_index,
                    engine
                        .compiler()
                        .compile_wasm_to_native_trampoline(&translation, wasm_func_ty)?,
                ))
            },
        )?;

        let mut indices = Vec::with_capacity(trampolines.len());
        for (symbol, sig, trampoline) in trampolines {
            let idx = compiled_funcs.len();
            indices.push((idx, sig));
            compiled_funcs.push((symbol, trampoline));
        }

        Ok(indices)
    }

    /// Converts an input binary-encoded WebAssembly module to compilation
    /// artifacts and type information.
    ///
    /// This is where compilation actually happens of WebAssembly modules and
    /// translation/parsing/validation of the binary input occurs. The binary
    /// artifact represented in the `MmapVec` returned here is an in-memory ELF
    /// file in an owned area of virtual linear memory where permissions (such
    /// as the executable bit) can be applied.
    ///
    /// Additionally compilation returns an `Option` here which is always
    /// `Some`, notably compiled metadata about the module in addition to the
    /// type information found within.
    #[cfg(compiler)]
    pub(crate) fn build_artifacts(
        engine: &Engine,
        wasm: &[u8],
    ) -> Result<(MmapVec, Option<(CompiledModuleInfo, ModuleTypes)>)> {
        let tunables = &engine.config().tunables;
        let compiler = engine.compiler();

        // First a `ModuleEnvironment` is created which records type information
        // about the wasm module. This is where the WebAssembly is parsed and
        // validated. Afterwards `types` will have all the type information for
        // this module.
        let mut validator =
            wasmparser::Validator::new_with_features(engine.config().features.clone());
        let parser = wasmparser::Parser::new(0);
        let mut types = Default::default();
        let mut translation = ModuleEnvironment::new(tunables, &mut validator, &mut types)
            .translate(parser, wasm)
            .context("failed to parse WebAssembly module")?;
        let types = types.finish();

        // Afterwards compile all functions and trampolines required by the
        // module. Note that this is also where the actual validation of all
        // function bodies happens as well.
        let funcs = Self::compile_functions(engine, &mut translation, &types)?;
        let mut compiled_funcs = vec![];

        let wasm_to_native_trampolines = Module::compile_wasm_to_native_trampolines(
            engine,
            std::slice::from_ref(&translation),
            &types,
            &mut compiled_funcs,
        )?;

        let module_func_indices =
            ModuleFunctionIndices::new(translation, funcs, "wasm", &mut compiled_funcs);

        // Emplace all compiled functions into the object file with any other
        // sections associated with code as well.
        let mut obj = engine.compiler().object(ObjectKind::Module)?;
        let locs = compiler.append_code(&mut obj, &compiled_funcs, tunables, &|_i, idx| {
            module_func_indices.resolve_reloc(idx)
        })?;

        // If requested, generate and add dwarf information.
        if tunables.generate_native_debuginfo && !module_func_indices.func_indices.is_empty() {
            let funcs = module_func_indices
                .func_indices
                .iter()
                .copied()
                .map(|i| (locs[i].0, &*compiled_funcs[i].1))
                .collect();
            compiler.append_dwarf(&mut obj, &module_func_indices.translation, &funcs)?;
        }

        // Insert `Engine` and type-level information into the compiled
        // artifact so if this module is deserialized later it contains all
        // information necessary.
        //
        // Note that `append_compiler_info` and `append_types` here in theory
        // can both be skipped if this module will never get serialized.
        // They're only used during deserialization and not during runtime for
        // the module itself. Currently there's no need for that, however, so
        // it's left as an exercise for later.
        engine.append_compiler_info(&mut obj);
        engine.append_bti(&mut obj);

        let mut obj = wasmtime_jit::ObjectBuilder::new(obj, tunables);
        let info =
            module_func_indices.append_to_object(&locs, &wasm_to_native_trampolines, &mut obj)?;
        obj.serialize_info(&(&info, &types));
        let mmap = obj.finish()?;

        Ok((mmap, Some((info, types))))
    }

    #[cfg(compiler)]
    pub(crate) fn compile_functions(
        engine: &Engine,
        translation: &mut ModuleTranslation<'_>,
        types: &ModuleTypes,
    ) -> Result<Vec<CompileFunctionResult>> {
        let tunables = &engine.config().tunables;
        let functions = mem::take(&mut translation.function_body_inputs);
        let functions = functions.into_iter().collect::<Vec<_>>();
        let compiler = engine.compiler();
        let funcs =
            engine.run_maybe_parallel(functions, |(def_func_index, func)| -> Result<_> {
                let func_index = translation.module.func_index(def_func_index);
                let offset = func.body.range().start;

                let (info, function) = compiler
                    .compile_function(&translation, def_func_index, func, tunables, types)
                    .with_context(|| {
                        let name = match translation
                            .debuginfo
                            .name_section
                            .func_names
                            .get(&func_index)
                        {
                            Some(name) => format!(" (`{}`)", name),
                            None => String::new(),
                        };
                        let func_index = func_index.as_u32();
                        format!(
                        "failed to compile wasm function {func_index}{name} at offset {offset:#x}"
                    )
                    })?;

                let (array_to_wasm_trampoline, native_to_wasm_trampoline) =
                    if translation.module.functions[func_index].is_escaping() {
                        (
                            Some(compiler.compile_array_to_wasm_trampoline(
                                &translation,
                                types,
                                def_func_index,
                            )?),
                            Some(compiler.compile_native_to_wasm_trampoline(
                                &translation,
                                types,
                                def_func_index,
                            )?),
                        )
                    } else {
                        (None, None)
                    };

                Ok(CompileFunctionResult {
                    info,
                    function,
                    array_to_wasm_trampoline,
                    native_to_wasm_trampoline,
                })
            })?;

        // If configured attempt to use static memory initialization which
        // can either at runtime be implemented as a single memcpy to
        // initialize memory or otherwise enabling virtual-memory-tricks
        // such as mmap'ing from a file to get copy-on-write.
        if engine.config().memory_init_cow {
            let align = engine.compiler().page_size_align();
            let max_always_allowed = engine.config().memory_guaranteed_dense_image_size;
            translation.try_static_init(align, max_always_allowed);
        }

        // Attempt to convert table initializer segments to
        // FuncTable representation where possible, to enable
        // table lazy init.
        translation.try_func_table_init();

        Ok(funcs)
    }

    /// Deserializes an in-memory compiled module previously created with
    /// [`Module::serialize`] or [`Engine::precompile_module`].
    ///
    /// This function will deserialize the binary blobs emitted by
    /// [`Module::serialize`] and [`Engine::precompile_module`] back into an
    /// in-memory [`Module`] that's ready to be instantiated.
    ///
    /// Note that the [`Module::deserialize_file`] method is more optimized than
    /// this function, so if the serialized module is already present in a file
    /// it's recommended to use that method instead.
    ///
    /// # Unsafety
    ///
    /// This function is marked as `unsafe` because if fed invalid input or used
    /// improperly this could lead to memory safety vulnerabilities. This method
    /// should not, for example, be exposed to arbitrary user input.
    ///
    /// The structure of the binary blob read here is only lightly validated
    /// internally in `wasmtime`. This is intended to be an efficient
    /// "rehydration" for a [`Module`] which has very few runtime checks beyond
    /// deserialization. Arbitrary input could, for example, replace valid
    /// compiled code with any other valid compiled code, meaning that this can
    /// trivially be used to execute arbitrary code otherwise.
    ///
    /// For these reasons this function is `unsafe`. This function is only
    /// designed to receive the previous input from [`Module::serialize`] and
    /// [`Engine::precompile_module`]. If the exact output of those functions
    /// (unmodified) is passed to this function then calls to this function can
    /// be considered safe. It is the caller's responsibility to provide the
    /// guarantee that only previously-serialized bytes are being passed in
    /// here.
    ///
    /// Note that this function is designed to be safe receiving output from
    /// *any* compiled version of `wasmtime` itself. This means that it is safe
    /// to feed output from older versions of Wasmtime into this function, in
    /// addition to newer versions of wasmtime (from the future!). These inputs
    /// will deterministically and safely produce an `Err`. This function only
    /// successfully accepts inputs from the same version of `wasmtime`, but the
    /// safety guarantee only applies to externally-defined blobs of bytes, not
    /// those defined by any version of wasmtime. (this means that if you cache
    /// blobs across versions of wasmtime you can be safely guaranteed that
    /// future versions of wasmtime will reject old cache entries).
    pub unsafe fn deserialize(engine: &Engine, bytes: impl AsRef<[u8]>) -> Result<Module> {
        let code = engine.load_code_bytes(bytes.as_ref(), ObjectKind::Module)?;
        Module::from_parts(engine, code, None)
    }

    /// Same as [`deserialize`], except that the contents of `path` are read to
    /// deserialize into a [`Module`].
    ///
    /// This method is provided because it can be faster than [`deserialize`]
    /// since the data doesn't need to be copied around, but rather the module
    /// can be used directly from an mmap'd view of the file provided.
    ///
    /// [`deserialize`]: Module::deserialize
    ///
    /// # Unsafety
    ///
    /// All of the reasons that [`deserialize`] is `unsafe` applies to this
    /// function as well. Arbitrary data loaded from a file may trick Wasmtime
    /// into arbitrary code execution since the contents of the file are not
    /// validated to be a valid precompiled module.
    ///
    /// Additionally though this function is also `unsafe` because the file
    /// referenced must remain unchanged and a valid precompiled module for the
    /// entire lifetime of the [`Module`] returned. Any changes to the file on
    /// disk may change future instantiations of the module to be incorrect.
    /// This is because the file is mapped into memory and lazily loaded pages
    /// reflect the current state of the file, not necessarily the origianl
    /// state of the file.
    pub unsafe fn deserialize_file(engine: &Engine, path: impl AsRef<Path>) -> Result<Module> {
        let code = engine.load_code_file(path.as_ref(), ObjectKind::Module)?;
        Module::from_parts(engine, code, None)
    }

    /// Entrypoint for creating a `Module` for all above functions, both
    /// of the AOT and jit-compiled cateogries.
    ///
    /// In all cases the compilation artifact, `code_memory`, is provided here.
    /// The `info_and_types` argument is `None` when a module is being
    /// deserialized from a precompiled artifact or it's `Some` if it was just
    /// compiled and the values are already available.
    fn from_parts(
        engine: &Engine,
        code_memory: Arc<CodeMemory>,
        info_and_types: Option<(CompiledModuleInfo, ModuleTypes)>,
    ) -> Result<Self> {
        // Acquire this module's metadata and type information, deserializing
        // it from the provided artifact if it wasn't otherwise provided
        // already.
        let (info, types) = match info_and_types {
            Some((info, types)) => (info, types),
            None => bincode::deserialize(code_memory.wasmtime_info())?,
        };

        // Register function type signatures into the engine for the lifetime
        // of the `Module` that will be returned. This notably also builds up
        // maps for trampolines to be used for this module when inserted into
        // stores.
        //
        // Note that the unsafety here should be ok since the `trampolines`
        // field should only point to valid trampoline function pointers
        // within the text section.
        let signatures = SignatureCollection::new_for_module(engine.signatures(), &types);

        // Package up all our data into a `CodeObject` and delegate to the final
        // step of module compilation.
        let code = Arc::new(CodeObject::new(code_memory, signatures, types.into()));
        Module::from_parts_raw(engine, code, info, true)
    }

    pub(crate) fn from_parts_raw(
        engine: &Engine,
        code: Arc<CodeObject>,
        info: CompiledModuleInfo,
        serializable: bool,
    ) -> Result<Self> {
        let module = CompiledModule::from_artifacts(
            code.code_memory().clone(),
            info,
            engine.profiler(),
            engine.unique_id_allocator(),
        )?;

        // Validate the module can be used with the current allocator
        let offsets = VMOffsets::new(HostPtr, module.module());
        engine.allocator().validate(module.module(), &offsets)?;

        Ok(Self {
            inner: Arc::new(ModuleInner {
                engine: engine.clone(),
                code,
                memory_images: OnceCell::new(),
                module,
                serializable,
                offsets,
            }),
        })
    }

    /// Validates `binary` input data as a WebAssembly binary given the
    /// configuration in `engine`.
    ///
    /// This function will perform a speedy validation of the `binary` input
    /// WebAssembly module (which is in [binary form][binary], the text format
    /// is not accepted by this function) and return either `Ok` or `Err`
    /// depending on the results of validation. The `engine` argument indicates
    /// configuration for WebAssembly features, for example, which are used to
    /// indicate what should be valid and what shouldn't be.
    ///
    /// Validation automatically happens as part of [`Module::new`].
    ///
    /// # Errors
    ///
    /// If validation fails for any reason (type check error, usage of a feature
    /// that wasn't enabled, etc) then an error with a description of the
    /// validation issue will be returned.
    ///
    /// [binary]: https://webassembly.github.io/spec/core/binary/index.html
    pub fn validate(engine: &Engine, binary: &[u8]) -> Result<()> {
        let mut validator = Validator::new_with_features(engine.config().features);

        let mut functions = Vec::new();
        for payload in Parser::new(0).parse_all(binary) {
            let payload = payload?;
            if let ValidPayload::Func(a, b) = validator.payload(&payload)? {
                functions.push((a, b));
            }
            if let wasmparser::Payload::Version { encoding, .. } = &payload {
                if let wasmparser::Encoding::Component = encoding {
                    bail!("component passed to module validation");
                }
            }
        }

        engine.run_maybe_parallel(functions, |(validator, body)| {
            // FIXME: it would be best here to use a rayon-specific parallel
            // iterator that maintains state-per-thread to share the function
            // validator allocations (`Default::default` here) across multiple
            // functions.
            validator.into_validator(Default::default()).validate(&body)
        })?;
        Ok(())
    }

    /// Serializes this module to a vector of bytes.
    ///
    /// This function is similar to the [`Engine::precompile_module`] method
    /// where it produces an artifact of Wasmtime which is suitable to later
    /// pass into [`Module::deserialize`]. If a module is never instantiated
    /// then it's recommended to use [`Engine::precompile_module`] instead of
    /// this method, but if a module is both instantiated and serialized then
    /// this method can be useful to get the serialized version without
    /// compiling twice.
    #[cfg(compiler)]
    #[cfg_attr(nightlydoc, doc(cfg(any(feature = "cranelift", feature = "winch"))))] // see build.rs
    pub fn serialize(&self) -> Result<Vec<u8>> {
        // The current representation of compiled modules within a compiled
        // component means that it cannot be serialized. The mmap returned here
        // is the mmap for the entire component and while it contains all
        // necessary data to deserialize this particular module it's all
        // embedded within component-specific information.
        //
        // It's not the hardest thing in the world to support this but it's
        // expected that there's not much of a use case at this time. In theory
        // all that needs to be done is to edit the `.wasmtime.info` section
        // to contains this module's metadata instead of the metadata for the
        // whole component. The metadata itself is fairly trivially
        // recreateable here it's more that there's no easy one-off API for
        // editing the sections of an ELF object to use here.
        //
        // Overall for now this simply always returns an error in this
        // situation. If you're reading this and feel that the situation should
        // be different please feel free to open an issue.
        if !self.inner.serializable {
            bail!("cannot serialize a module exported from a component");
        }
        Ok(self.compiled_module().mmap().to_vec())
    }

    pub(crate) fn compiled_module(&self) -> &CompiledModule {
        &self.inner.module
    }

    fn code_object(&self) -> &Arc<CodeObject> {
        &self.inner.code
    }

    pub(crate) fn env_module(&self) -> &wasmtime_environ::Module {
        self.compiled_module().module()
    }

    pub(crate) fn types(&self) -> &ModuleTypes {
        self.inner.code.module_types()
    }

    pub(crate) fn signatures(&self) -> &SignatureCollection {
        self.inner.code.signatures()
    }

    /// Returns identifier/name that this [`Module`] has. This name
    /// is used in traps/backtrace details.
    ///
    /// Note that most LLVM/clang/Rust-produced modules do not have a name
    /// associated with them, but other wasm tooling can be used to inject or
    /// add a name.
    ///
    /// # Examples
    ///
    /// ```
    /// # use wasmtime::*;
    /// # fn main() -> anyhow::Result<()> {
    /// # let engine = Engine::default();
    /// let module = Module::new(&engine, "(module $foo)")?;
    /// assert_eq!(module.name(), Some("foo"));
    ///
    /// let module = Module::new(&engine, "(module)")?;
    /// assert_eq!(module.name(), None);
    ///
    /// # Ok(())
    /// # }
    /// ```
    pub fn name(&self) -> Option<&str> {
        self.compiled_module().module().name.as_deref()
    }

    /// Returns the list of imports that this [`Module`] has and must be
    /// satisfied.
    ///
    /// This function returns the list of imports that the wasm module has, but
    /// only the types of each import. The type of each import is used to
    /// typecheck the [`Instance::new`](crate::Instance::new) method's `imports`
    /// argument. The arguments to that function must match up 1-to-1 with the
    /// entries in the array returned here.
    ///
    /// The imports returned reflect the order of the imports in the wasm module
    /// itself, and note that no form of deduplication happens.
    ///
    /// # Examples
    ///
    /// Modules with no imports return an empty list here:
    ///
    /// ```
    /// # use wasmtime::*;
    /// # fn main() -> anyhow::Result<()> {
    /// # let engine = Engine::default();
    /// let module = Module::new(&engine, "(module)")?;
    /// assert_eq!(module.imports().len(), 0);
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// and modules with imports will have a non-empty list:
    ///
    /// ```
    /// # use wasmtime::*;
    /// # fn main() -> anyhow::Result<()> {
    /// # let engine = Engine::default();
    /// let wat = r#"
    ///     (module
    ///         (import "host" "foo" (func))
    ///     )
    /// "#;
    /// let module = Module::new(&engine, wat)?;
    /// assert_eq!(module.imports().len(), 1);
    /// let import = module.imports().next().unwrap();
    /// assert_eq!(import.module(), "host");
    /// assert_eq!(import.name(), "foo");
    /// match import.ty() {
    ///     ExternType::Func(_) => { /* ... */ }
    ///     _ => panic!("unexpected import type!"),
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn imports<'module>(
        &'module self,
    ) -> impl ExactSizeIterator<Item = ImportType<'module>> + 'module {
        let module = self.compiled_module().module();
        let types = self.types();
        module
            .imports()
            .map(move |(module, field, ty)| ImportType::new(module, field, ty, types))
            .collect::<Vec<_>>()
            .into_iter()
    }

    /// Returns the list of exports that this [`Module`] has and will be
    /// available after instantiation.
    ///
    /// This function will return the type of each item that will be returned
    /// from [`Instance::exports`](crate::Instance::exports). Each entry in this
    /// list corresponds 1-to-1 with that list, and the entries here will
    /// indicate the name of the export along with the type of the export.
    ///
    /// # Examples
    ///
    /// Modules might not have any exports:
    ///
    /// ```
    /// # use wasmtime::*;
    /// # fn main() -> anyhow::Result<()> {
    /// # let engine = Engine::default();
    /// let module = Module::new(&engine, "(module)")?;
    /// assert!(module.exports().next().is_none());
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// When the exports are not empty, you can inspect each export:
    ///
    /// ```
    /// # use wasmtime::*;
    /// # fn main() -> anyhow::Result<()> {
    /// # let engine = Engine::default();
    /// let wat = r#"
    ///     (module
    ///         (func (export "foo"))
    ///         (memory (export "memory") 1)
    ///     )
    /// "#;
    /// let module = Module::new(&engine, wat)?;
    /// assert_eq!(module.exports().len(), 2);
    ///
    /// let mut exports = module.exports();
    /// let foo = exports.next().unwrap();
    /// assert_eq!(foo.name(), "foo");
    /// match foo.ty() {
    ///     ExternType::Func(_) => { /* ... */ }
    ///     _ => panic!("unexpected export type!"),
    /// }
    ///
    /// let memory = exports.next().unwrap();
    /// assert_eq!(memory.name(), "memory");
    /// match memory.ty() {
    ///     ExternType::Memory(_) => { /* ... */ }
    ///     _ => panic!("unexpected export type!"),
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn exports<'module>(
        &'module self,
    ) -> impl ExactSizeIterator<Item = ExportType<'module>> + 'module {
        let module = self.compiled_module().module();
        let types = self.types();
        module.exports.iter().map(move |(name, entity_index)| {
            ExportType::new(name, module.type_of(*entity_index), types)
        })
    }

    /// Looks up an export in this [`Module`] by name.
    ///
    /// This function will return the type of an export with the given name.
    ///
    /// # Examples
    ///
    /// There may be no export with that name:
    ///
    /// ```
    /// # use wasmtime::*;
    /// # fn main() -> anyhow::Result<()> {
    /// # let engine = Engine::default();
    /// let module = Module::new(&engine, "(module)")?;
    /// assert!(module.get_export("foo").is_none());
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// When there is an export with that name, it is returned:
    ///
    /// ```
    /// # use wasmtime::*;
    /// # fn main() -> anyhow::Result<()> {
    /// # let engine = Engine::default();
    /// let wat = r#"
    ///     (module
    ///         (func (export "foo"))
    ///         (memory (export "memory") 1)
    ///     )
    /// "#;
    /// let module = Module::new(&engine, wat)?;
    /// let foo = module.get_export("foo");
    /// assert!(foo.is_some());
    ///
    /// let foo = foo.unwrap();
    /// match foo {
    ///     ExternType::Func(_) => { /* ... */ }
    ///     _ => panic!("unexpected export type!"),
    /// }
    ///
    /// # Ok(())
    /// # }
    /// ```
    pub fn get_export(&self, name: &str) -> Option<ExternType> {
        let module = self.compiled_module().module();
        let entity_index = module.exports.get(name)?;
        Some(ExternType::from_wasmtime(
            self.types(),
            &module.type_of(*entity_index),
        ))
    }

    /// Returns the [`Engine`] that this [`Module`] was compiled by.
    pub fn engine(&self) -> &Engine {
        &self.inner.engine
    }

    /// Returns the `ModuleInner` cast as `ModuleRuntimeInfo` for use
    /// by the runtime.
    pub(crate) fn runtime_info(&self) -> Arc<dyn wasmtime_runtime::ModuleRuntimeInfo> {
        // N.B.: this needs to return a clone because we cannot
        // statically cast the &Arc<ModuleInner> to &Arc<dyn Trait...>.
        self.inner.clone()
    }

    pub(crate) fn module_info(&self) -> &dyn wasmtime_runtime::ModuleInfo {
        &*self.inner
    }

    /// Returns the range of bytes in memory where this module's compilation
    /// image resides.
    ///
    /// The compilation image for a module contains executable code, data, debug
    /// information, etc. This is roughly the same as the `Module::serialize`
    /// but not the exact same.
    ///
    /// The range of memory reported here is exposed to allow low-level
    /// manipulation of the memory in platform-specific manners such as using
    /// `mlock` to force the contents to be paged in immediately or keep them
    /// paged in after they're loaded.
    ///
    /// It is not safe to modify the memory in this range, nor is it safe to
    /// modify the protections of memory in this range.
    pub fn image_range(&self) -> Range<usize> {
        self.compiled_module().image_range()
    }

    /// Force initialization of copy-on-write images to happen here-and-now
    /// instead of when they're requested during first instantiation.
    ///
    /// When [copy-on-write memory
    /// initialization](crate::Config::memory_init_cow) is enabled then Wasmtime
    /// will lazily create the initialization image for a module. This method
    /// can be used to explicitly dictate when this initialization happens.
    ///
    /// Note that this largely only matters on Linux when memfd is used.
    /// Otherwise the copy-on-write image typically comes from disk and in that
    /// situation the creation of the image is trivial as the image is always
    /// sourced from disk. On Linux, though, when memfd is used a memfd is
    /// created and the initialization image is written to it.
    ///
    /// Also note that this method is not required to be called, it's available
    /// as a performance optimization if required but is otherwise handled
    /// automatically.
    pub fn initialize_copy_on_write_image(&self) -> Result<()> {
        self.inner.memory_images()?;
        Ok(())
    }

    /// Get the map from `.text` section offsets to Wasm binary offsets for this
    /// module.
    ///
    /// Each entry is a (`.text` section offset, Wasm binary offset) pair.
    ///
    /// Entries are yielded in order of `.text` section offset.
    ///
    /// Some entries are missing a Wasm binary offset. This is for code that is
    /// not associated with any single location in the Wasm binary, or for when
    /// source information was optimized away.
    ///
    /// Not every module has an address map, since address map generation can be
    /// turned off on `Config`.
    ///
    /// There is not an entry for every `.text` section offset. Every offset
    /// after an entry's offset, but before the next entry's offset, is
    /// considered to map to the same Wasm binary offset as the original
    /// entry. For example, the address map will not contain the following
    /// sequnce of entries:
    ///
    /// ```ignore
    /// [
    ///     // ...
    ///     (10, Some(42)),
    ///     (11, Some(42)),
    ///     (12, Some(42)),
    ///     (13, Some(43)),
    ///     // ...
    /// ]
    /// ```
    ///
    /// Instead, it will drop the entries for offsets `11` and `12` since they
    /// are the same as the entry for offset `10`:
    ///
    /// ```ignore
    /// [
    ///     // ...
    ///     (10, Some(42)),
    ///     (13, Some(43)),
    ///     // ...
    /// ]
    /// ```
    pub fn address_map<'a>(&'a self) -> Option<impl Iterator<Item = (usize, Option<u32>)> + 'a> {
        Some(
            wasmtime_environ::iterate_address_map(
                self.code_object().code_memory().address_map_data(),
            )?
            .map(|(offset, file_pos)| (offset as usize, file_pos.file_offset())),
        )
    }

    /// Get this module's code object's `.text` section, containing its compiled
    /// executable code.
    pub fn text(&self) -> &[u8] {
        self.code_object().code_memory().text()
    }

    /// Get the locations of functions in this module's `.text` section.
    ///
    /// Each function's locartion is a (`.text` section offset, length) pair.
    pub fn function_locations<'a>(&'a self) -> impl ExactSizeIterator<Item = (usize, usize)> + 'a {
        self.compiled_module().finished_functions().map(|(f, _)| {
            let loc = self.compiled_module().func_loc(f);
            (loc.start as usize, loc.length as usize)
        })
    }
}

impl ModuleInner {
    fn memory_images(&self) -> Result<Option<&ModuleMemoryImages>> {
        let images = self
            .memory_images
            .get_or_try_init(|| memory_images(&self.engine, &self.module))?
            .as_ref();
        Ok(images)
    }
}

impl Drop for ModuleInner {
    fn drop(&mut self) {
        // When a `Module` is being dropped that means that it's no longer
        // present in any `Store` and it's additionally not longer held by any
        // embedder. Take this opportunity to purge any lingering instantiations
        // within a pooling instance allocator, if applicable.
        self.engine
            .allocator()
            .purge_module(self.module.unique_id());
    }
}

fn _assert_send_sync() {
    fn _assert<T: Send + Sync>() {}
    _assert::<Module>();
}

/// This is a helper struct used when caching to hash the state of an `Engine`
/// used for module compilation.
///
/// The hash computed for this structure is used to key the global wasmtime
/// cache and dictates whether artifacts are reused. Consequently the contents
/// of this hash dictate when artifacts are or aren't re-used.
#[cfg(compiler)]
pub(crate) struct HashedEngineCompileEnv<'a>(pub &'a Engine);

#[cfg(compiler)]
impl std::hash::Hash for HashedEngineCompileEnv<'_> {
    fn hash<H: std::hash::Hasher>(&self, hasher: &mut H) {
        // Hash the compiler's state based on its target and configuration.
        let compiler = self.0.compiler();
        compiler.triple().hash(hasher);
        compiler.flags().hash(hasher);
        compiler.isa_flags().hash(hasher);

        // Hash configuration state read for compilation
        let config = self.0.config();
        config.tunables.hash(hasher);
        config.features.hash(hasher);

        // Catch accidental bugs of reusing across crate versions.
        env!("CARGO_PKG_VERSION").hash(hasher);
    }
}

impl wasmtime_runtime::ModuleRuntimeInfo for ModuleInner {
    fn module(&self) -> &Arc<wasmtime_environ::Module> {
        self.module.module()
    }

    fn function(&self, index: DefinedFuncIndex) -> NonNull<VMWasmCallFunction> {
        let ptr = self
            .module
            .finished_function(index)
            .as_ptr()
            .cast::<VMWasmCallFunction>()
            .cast_mut();
        NonNull::new(ptr).unwrap()
    }

    fn native_to_wasm_trampoline(
        &self,
        index: DefinedFuncIndex,
    ) -> Option<NonNull<VMNativeCallFunction>> {
        let ptr = self
            .module
            .native_to_wasm_trampoline(index)?
            .as_ptr()
            .cast::<VMNativeCallFunction>()
            .cast_mut();
        Some(NonNull::new(ptr).unwrap())
    }

    fn array_to_wasm_trampoline(&self, index: DefinedFuncIndex) -> Option<VMArrayCallFunction> {
        let ptr = self.module.array_to_wasm_trampoline(index)?.as_ptr();
        Some(unsafe { mem::transmute::<*const u8, VMArrayCallFunction>(ptr) })
    }

    fn wasm_to_native_trampoline(
        &self,
        signature: VMSharedSignatureIndex,
    ) -> Option<NonNull<VMWasmCallFunction>> {
        let sig = self.code.signatures().local_signature(signature)?;
        let ptr = self
            .module
            .wasm_to_native_trampoline(sig)
            .as_ptr()
            .cast::<VMWasmCallFunction>()
            .cast_mut();
        Some(NonNull::new(ptr).unwrap())
    }

    fn memory_image(&self, memory: DefinedMemoryIndex) -> Result<Option<&Arc<MemoryImage>>> {
        let images = self.memory_images()?;
        Ok(images.and_then(|images| images.get_memory_image(memory)))
    }

    fn unique_id(&self) -> Option<CompiledModuleId> {
        Some(self.module.unique_id())
    }

    fn wasm_data(&self) -> &[u8] {
        self.module.code_memory().wasm_data()
    }

    fn signature_ids(&self) -> &[VMSharedSignatureIndex] {
        self.code.signatures().as_module_map().values().as_slice()
    }

    fn offsets(&self) -> &VMOffsets<HostPtr> {
        &self.offsets
    }
}

impl wasmtime_runtime::ModuleInfo for ModuleInner {
    fn lookup_stack_map(&self, pc: usize) -> Option<&wasmtime_environ::StackMap> {
        let text_offset = pc - self.module.text().as_ptr() as usize;
        let (index, func_offset) = self.module.func_by_text_offset(text_offset)?;
        let info = self.module.wasm_func_info(index);

        // Do a binary search to find the stack map for the given offset.
        let index = match info
            .stack_maps
            .binary_search_by_key(&func_offset, |i| i.code_offset)
        {
            // Found it.
            Ok(i) => i,

            // No stack map associated with this PC.
            //
            // Because we know we are in Wasm code, and we must be at some kind
            // of call/safepoint, then the Cranelift backend must have avoided
            // emitting a stack map for this location because no refs were live.
            Err(_) => return None,
        };

        Some(&info.stack_maps[index].stack_map)
    }
}

/// A barebones implementation of ModuleRuntimeInfo that is useful for
/// cases where a purpose-built environ::Module is used and a full
/// CompiledModule does not exist (for example, for tests or for the
/// default-callee instance).
pub(crate) struct BareModuleInfo {
    module: Arc<wasmtime_environ::Module>,
    one_signature: Option<VMSharedSignatureIndex>,
    offsets: VMOffsets<HostPtr>,
}

impl BareModuleInfo {
    pub(crate) fn empty(module: Arc<wasmtime_environ::Module>) -> Self {
        BareModuleInfo::maybe_imported_func(module, None)
    }

    pub(crate) fn maybe_imported_func(
        module: Arc<wasmtime_environ::Module>,
        one_signature: Option<VMSharedSignatureIndex>,
    ) -> Self {
        BareModuleInfo {
            offsets: VMOffsets::new(HostPtr, &module),
            module,
            one_signature,
        }
    }

    pub(crate) fn into_traitobj(self) -> Arc<dyn wasmtime_runtime::ModuleRuntimeInfo> {
        Arc::new(self)
    }
}

impl wasmtime_runtime::ModuleRuntimeInfo for BareModuleInfo {
    fn module(&self) -> &Arc<wasmtime_environ::Module> {
        &self.module
    }

    fn function(&self, _index: DefinedFuncIndex) -> NonNull<VMWasmCallFunction> {
        unreachable!()
    }

    fn array_to_wasm_trampoline(&self, _index: DefinedFuncIndex) -> Option<VMArrayCallFunction> {
        unreachable!()
    }

    fn native_to_wasm_trampoline(
        &self,
        _index: DefinedFuncIndex,
    ) -> Option<NonNull<VMNativeCallFunction>> {
        unreachable!()
    }

    fn wasm_to_native_trampoline(
        &self,
        _signature: VMSharedSignatureIndex,
    ) -> Option<NonNull<VMWasmCallFunction>> {
        unreachable!()
    }

    fn memory_image(&self, _memory: DefinedMemoryIndex) -> Result<Option<&Arc<MemoryImage>>> {
        Ok(None)
    }

    fn unique_id(&self) -> Option<CompiledModuleId> {
        None
    }

    fn wasm_data(&self) -> &[u8] {
        &[]
    }

    fn signature_ids(&self) -> &[VMSharedSignatureIndex] {
        match &self.one_signature {
            Some(id) => std::slice::from_ref(id),
            None => &[],
        }
    }

    fn offsets(&self) -> &VMOffsets<HostPtr> {
        &self.offsets
    }
}

/// Helper method to construct a `ModuleMemoryImages` for an associated
/// `CompiledModule`.
fn memory_images(engine: &Engine, module: &CompiledModule) -> Result<Option<ModuleMemoryImages>> {
    // If initialization via copy-on-write is explicitly disabled in
    // configuration then this path is skipped entirely.
    if !engine.config().memory_init_cow {
        return Ok(None);
    }

    // ... otherwise logic is delegated to the `ModuleMemoryImages::new`
    // constructor.
    let mmap = if engine.config().force_memory_init_memfd {
        None
    } else {
        Some(module.mmap())
    };
    ModuleMemoryImages::new(module.module(), module.code_memory().wasm_data(), mmap)
}
