use crate::component::*;
use crate::{
    EntityIndex, EntityType, ModuleEnvironment, ModuleTranslation, PrimaryMap, SignatureIndex,
    Tunables,
};
use anyhow::{bail, Result};
use indexmap::IndexMap;
use std::collections::HashMap;
use std::mem;
use wasmparser::{Chunk, Encoding, Parser, Payload, Validator};

/// Structure used to translate a component and parse it.
pub struct Translator<'a, 'data> {
    result: Translation<'data>,
    validator: &'a mut Validator,
    types: &'a mut ComponentTypesBuilder,
    tunables: &'a Tunables,
    parsers: Vec<Parser>,
    parser: Parser,
}

/// Result of translation of a component to contain all type information and
/// metadata about how to run the component.
#[derive(Default)]
pub struct Translation<'data> {
    /// Final type of the component, intended to be persisted all the way to
    /// runtime.
    pub component: Component,

    /// List of "upvars" or closed over modules that `Component` would refer
    /// to. This contains the core wasm results of translation and the indices
    /// are referred to within types in `Component`.
    pub upvars: PrimaryMap<ModuleUpvarIndex, ModuleTranslation<'data>>,

    // Index spaces which are built-up during translation but do not persist to
    // runtime. These are used to understand the structure of the component and
    // where items come from but at this time these index spaces and their
    // definitions are not required at runtime as they're effectively "erased"
    // at the moment.
    //
    /// Modules and how they're defined (either closed-over or imported)
    modules: PrimaryMap<ModuleIndex, ModuleDef>,

    /// Instances of components, either direct instantiations or "bundles of
    /// exports".
    component_instances: PrimaryMap<ComponentInstanceIndex, ComponentInstanceDef<'data>>,

    /// Instances of core wasm modules, either direct instantiations or
    /// "bundles of exports".
    module_instances: PrimaryMap<ModuleInstanceIndex, ModuleInstanceDef<'data>>,

    /// The core wasm function index space.
    funcs: PrimaryMap<FuncIndex, Func<'data>>,

    /// The component function index space.
    component_funcs: PrimaryMap<ComponentFuncIndex, ComponentFunc<'data>>,

    /// Core wasm globals, always sourced from a previously module instance.
    globals: PrimaryMap<GlobalIndex, CoreSource<'data>>,

    /// Core wasm memories, always sourced from a previously module instance.
    memories: PrimaryMap<MemoryIndex, CoreSource<'data>>,

    /// Core wasm tables, always sourced from a previously module instance.
    tables: PrimaryMap<TableIndex, CoreSource<'data>>,

    /// This is a list of pairs where the first element points to an index
    /// within `component.initializers` to an `Initializer::LowerImport` entry.
    /// After a component has finished translation and we have a
    /// `wasmparser::Types` value to lookup type information within the type of
    /// `FuncIndex`, within this component, will be used to fill in the
    /// `LowerImport::canonical_abi` field.
    ///
    /// This avoids wasmtime having to duplicate the
    /// interface-types-signature-to-core-wasm-signature lowering logic.
    signatures_to_fill: Vec<(usize, FuncIndex)>,

    /// Intern'd map of imports where `RuntimeImport` represents some
    /// (optional) projection of imports from an original import and
    /// `RuntimeImportIndex` is an array built at runtime used to instantiate
    /// this component.
    import_map: HashMap<RuntimeImport, RuntimeImportIndex>,

    /// Intern'd map of exports to the memory index they're referred to by at
    /// runtime, used when building `CanonicalOptions` to avoid storing the same
    /// memory many times within a `VMComponentContext`.
    memory_to_runtime: HashMap<CoreExport<MemoryIndex>, RuntimeMemoryIndex>,

    /// Same as `memory_to_runtime` but an intern'd map for realloc functions
    /// instead.
    realloc_to_runtime: HashMap<CoreDef, RuntimeReallocIndex>,
}

/// How a module is defined within a component.
#[derive(Debug, Clone)]
enum ModuleDef {
    /// This module is defined as an "upvar" or a closed over variable
    /// implicitly available for the component.
    ///
    /// This means that the module was either defined within the component or a
    /// module was aliased into this component which was known defined in the
    /// parent component.
    Upvar(ModuleUpvarIndex),

    /// This module is defined as an import to the current component, so
    /// nothing is known about it except for its type. The `import_index`
    /// provided here indexes into the `Component`'s import list.
    Import {
        ty: TypeModuleIndex,
        import: RuntimeImport,
    },
}

/// Forms of creation of a core wasm module instance.
#[derive(Debug, Clone)]
enum ModuleInstanceDef<'data> {
    /// A module instance created through the instantiation of a previous
    /// module.
    Instantiated {
        /// The runtime index associated with this instance.
        ///
        /// Not to be confused with `InstanceIndex` which counts "synthetic"
        /// instances as well.
        instance: RuntimeInstanceIndex,

        /// The module that was instantiated.
        module: ModuleIndex,
    },

    /// A "synthetic" module created as a bag of exports from other items
    /// already defined within this component.
    Synthetic(HashMap<&'data str, EntityIndex>),
}

/// Forms of creation of a component instance.
#[derive(Debug, Clone)]
enum ComponentInstanceDef<'data> {
    /// An instance which was imported from the host.
    Import {
        /// The type of the imported instance
        ty: TypeComponentInstanceIndex,
        /// The description of where this import came from.
        import: RuntimeImport,
    },

    /// Same as `ModuleInstanceDef::Synthetic` except for component items.
    Synthetic(HashMap<&'data str, ComponentItem>),
}

/// Description of the function index space and how functions are defined.
#[derive(Clone)]
enum Func<'data> {
    /// A core wasm function that's extracted from a core wasm instance.
    Core(CoreSource<'data>),
    /// A core wasm function created by lowering an imported host function.
    ///
    /// Note that `LoweredIndex` here refers to the nth
    /// `Initializer::LowerImport`.
    Lowered(LoweredIndex),
}

/// Description of the function index space and how functions are defined.
#[derive(Clone)]
enum ComponentFunc<'data> {
    /// A component function that is imported from the host.
    Import(RuntimeImport),

    /// A component function that is lifted from core wasm function.
    Lifted {
        /// The resulting type of the lifted function
        ty: TypeFuncIndex,
        /// Which core wasm function is lifted, currently required to be an
        /// instance export as opposed to a lowered import.
        func: CoreSource<'data>,
        /// The options specified when the function was lifted.
        options: CanonicalOptions,
    },
}

/// Source of truth for where a core wasm item comes from.
#[derive(Clone)]
enum CoreSource<'data> {
    /// This item comes from an indexed entity within an instance.
    ///
    /// This is only available when the instance is statically known to be
    /// defined within the original component itself so we know the exact
    /// index.
    Index(RuntimeInstanceIndex, EntityIndex),

    /// This item comes from an named entity within an instance.
    ///
    /// This must be used for instances of imported modules because we
    /// otherwise don't know the internal structure of the module and which
    /// index is being exported.
    Export(RuntimeInstanceIndex, &'data str),
}

enum Action {
    KeepGoing,
    Skip(usize),
    Done,
}

/// Pre-intern'd representation of a `RuntimeImportIndex`.
///
/// When this is actually used within a component it will be committed into the
/// `import_map` to give it a `RuntimeImportIndex` via the
/// `runtime_import_index` function.
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct RuntimeImport {
    source: ImportIndex,
    exports: Vec<String>,
}

impl RuntimeImport {
    fn append(&self, name: &str) -> RuntimeImport {
        let mut exports = self.exports.clone();
        exports.push(name.to_string());
        RuntimeImport {
            source: self.source,
            exports,
        }
    }
}

impl<'a, 'data> Translator<'a, 'data> {
    /// Creates a new translation state ready to translate a component.
    pub fn new(
        tunables: &'a Tunables,
        validator: &'a mut Validator,
        types: &'a mut ComponentTypesBuilder,
    ) -> Self {
        Self {
            result: Translation::default(),
            tunables,
            validator,
            types,
            parser: Parser::new(0),
            parsers: Vec::new(),
        }
    }

    /// Translates the binary `component`.
    ///
    /// This is the workhorse of compilation which will parse all of
    /// `component` and create type information for Wasmtime and such. The
    /// `component` does not have to be valid and it will be validated during
    /// compilation.
    pub fn translate(mut self, component: &'data [u8]) -> Result<Translation<'data>> {
        let mut remaining = component;
        loop {
            let payload = match self.parser.parse(remaining, true)? {
                Chunk::Parsed { payload, consumed } => {
                    remaining = &remaining[consumed..];
                    payload
                }
                Chunk::NeedMoreData(_) => unreachable!(),
            };

            match self.translate_payload(payload, component)? {
                Action::KeepGoing => {}
                Action::Skip(n) => remaining = &remaining[n..],
                Action::Done => break,
            }
        }
        Ok(self.result)
    }

    fn translate_payload(
        &mut self,
        payload: Payload<'data>,
        component: &'data [u8],
    ) -> Result<Action> {
        match payload {
            Payload::Version {
                num,
                encoding,
                range,
            } => {
                self.validator.version(num, encoding, &range)?;

                match encoding {
                    Encoding::Component => {}
                    Encoding::Module => {
                        bail!("attempted to parse a wasm module with a component parser");
                    }
                }

                // Push a new scope for component types so outer aliases know
                // that the 0th level is this new component.
                self.types.push_type_scope();
            }

            Payload::End(offset) => {
                let types = self.validator.end(offset)?;

                // With type information in hand fill in the canonical abi type
                // of lowered functions.
                for (idx, func) in self.result.signatures_to_fill.drain(..) {
                    let i = match &mut self.result.component.initializers[idx] {
                        Initializer::LowerImport(i) => i,
                        _ => unreachable!(),
                    };
                    assert!(i.canonical_abi.as_u32() == 0);
                    i.canonical_abi = self.types.module_types_builder().wasm_func_type(
                        types
                            .function_at(func.as_u32())
                            .expect("should be in-bounds")
                            .clone()
                            .try_into()?,
                    );
                }

                // When leaving a module be sure to pop the types scope to
                // ensure that when we go back to the previous module outer
                // type alias indices work correctly again.
                self.types.pop_type_scope();

                match self.parsers.pop() {
                    Some(p) => self.parser = p,
                    None => return Ok(Action::Done),
                }
            }

            // When we see a type section the types are validated and then
            // translated into Wasmtime's representation. Each active type
            // definition is recorded in the `ComponentTypesBuilder` tables, or
            // this component's active scope.
            //
            // Note that the push/pop of the component types scope happens above
            // in `Version` and `End` since multiple type sections can appear
            // within a component.
            Payload::ComponentTypeSection(s) => {
                self.validator.component_type_section(&s)?;
                for ty in s {
                    let ty = self.types.intern_component_type(&ty?)?;
                    self.types.push_component_typedef(ty);
                }
            }
            Payload::CoreTypeSection(s) => {
                self.validator.core_type_section(&s)?;
                for ty in s {
                    let ty = self.types.intern_core_type(&ty?)?;
                    self.types.push_core_typedef(ty);
                }
            }

            Payload::ComponentImportSection(s) => {
                self.validator.component_import_section(&s)?;
                for import in s {
                    let import = import?;
                    let ty = self.types.component_type_ref(&import.ty);
                    // Record the `ImportIndex` to be associated with this
                    // import and create the `RuntimeImport` representing the
                    // "root" where it has no extra `exports`
                    let source = self
                        .result
                        .component
                        .import_types
                        .push((import.name.to_string(), ty));
                    let import = RuntimeImport {
                        source,
                        exports: Vec::new(),
                    };
                    match ty {
                        TypeDef::Module(ty) => {
                            self.result.modules.push(ModuleDef::Import { ty, import });
                        }
                        TypeDef::ComponentInstance(ty) => {
                            self.result
                                .component_instances
                                .push(ComponentInstanceDef::Import { ty, import });
                        }
                        TypeDef::ComponentFunc(_ty) => {
                            self.result
                                .component_funcs
                                .push(ComponentFunc::Import(import));
                        }
                        TypeDef::Component(_) => {
                            unimplemented!("imports of components");
                        }
                        TypeDef::Interface(_) => {
                            unimplemented!("imports of types");
                        }

                        // not possible with a valid component
                        TypeDef::CoreFunc(_ty) => unreachable!(),
                    }
                }
            }

            Payload::ComponentCanonicalSection(s) => {
                self.validator.component_canonical_section(&s)?;
                for func in s {
                    match func? {
                        wasmparser::CanonicalFunction::Lift {
                            type_index,
                            core_func_index,
                            options,
                        } => {
                            let ty = ComponentTypeIndex::from_u32(type_index);
                            let func = FuncIndex::from_u32(core_func_index);
                            let func = self.lift_function(ty, func, &options);
                            self.result.component_funcs.push(func);
                        }
                        wasmparser::CanonicalFunction::Lower {
                            func_index,
                            options,
                        } => {
                            let func = ComponentFuncIndex::from_u32(func_index);
                            let func = self.lower_function(func, &options);
                            self.result.funcs.push(func);
                        }
                    }
                }
            }

            // Core wasm modules are translated inline directly here with the
            // `ModuleEnvironment` from core wasm compilation. This will return
            // to the caller the size of the module so it knows how many bytes
            // of the input are skipped.
            //
            // Note that this is just initial type translation of the core wasm
            // module and actual function compilation is deferred until this
            // entire process has completed.
            Payload::ModuleSection { parser, range } => {
                self.validator.module_section(&range)?;
                let translation = ModuleEnvironment::new(
                    self.tunables,
                    self.validator,
                    self.types.module_types_builder(),
                )
                .translate(parser, &component[range.start..range.end])?;
                let upvar_idx = self.result.upvars.push(translation);
                self.result.modules.push(ModuleDef::Upvar(upvar_idx));
                return Ok(Action::Skip(range.end - range.start));
            }

            Payload::ComponentSection { parser, range } => {
                self.validator.component_section(&range)?;
                let old_parser = mem::replace(&mut self.parser, parser);
                self.parsers.push(old_parser);
                unimplemented!("component section");
            }

            Payload::InstanceSection(s) => {
                self.validator.instance_section(&s)?;
                for instance in s {
                    let instance = match instance? {
                        wasmparser::Instance::Instantiate { module_index, args } => {
                            self.instantiate_module(ModuleIndex::from_u32(module_index), &args)
                        }
                        wasmparser::Instance::FromExports(exports) => {
                            self.instantiate_module_from_exports(&exports)
                        }
                    };
                    self.result.module_instances.push(instance);
                }
            }
            Payload::ComponentInstanceSection(s) => {
                self.validator.component_instance_section(&s)?;
                for instance in s {
                    let instance = match instance? {
                        wasmparser::ComponentInstance::Instantiate {
                            component_index,
                            args,
                        } => {
                            let index = ComponentIndex::from_u32(component_index);
                            drop((index, args));
                            unimplemented!("instantiating a component");
                        }
                        wasmparser::ComponentInstance::FromExports(exports) => {
                            self.instantiate_component_from_exports(&exports)
                        }
                    };
                    self.result.component_instances.push(instance);
                }
            }

            Payload::ComponentExportSection(s) => {
                self.validator.component_export_section(&s)?;
                for export in s {
                    self.export(&export?);
                }
            }

            Payload::ComponentStartSection(s) => {
                self.validator.component_start_section(&s)?;
                unimplemented!("component start section");
            }

            Payload::AliasSection(s) => {
                self.validator.alias_section(&s)?;
                for alias in s {
                    match alias? {
                        wasmparser::Alias::InstanceExport {
                            kind,
                            instance_index,
                            name,
                        } => {
                            let instance = ModuleInstanceIndex::from_u32(instance_index);
                            self.alias_module_instance_export(kind, instance, name);
                        }
                    }
                }
            }

            Payload::ComponentAliasSection(s) => {
                self.validator.component_alias_section(&s)?;
                for alias in s {
                    match alias? {
                        wasmparser::ComponentAlias::InstanceExport {
                            kind,
                            instance_index,
                            name,
                        } => {
                            let instance = ComponentInstanceIndex::from_u32(instance_index);
                            self.alias_component_instance_export(kind, instance, name);
                        }
                        wasmparser::ComponentAlias::Outer { kind, count, index } => {
                            self.alias_component_outer(kind, count, index);
                        }
                    }
                }
            }

            // All custom sections are ignored by Wasmtime at this time.
            //
            // FIXME(WebAssembly/component-model#14): probably want to specify
            // and parse a `name` section here.
            Payload::CustomSection { .. } => {}

            // Anything else is either not reachable since we never enable the
            // feature in Wasmtime or we do enable it and it's a bug we don't
            // implement it, so let validation take care of most errors here and
            // if it gets past validation provide a helpful error message to
            // debug.
            other => {
                self.validator.payload(&other)?;
                panic!("unimplemented section {other:?}");
            }
        }

        Ok(Action::KeepGoing)
    }

    fn instantiate_module(
        &mut self,
        module: ModuleIndex,
        args: &[wasmparser::InstantiationArg<'data>],
    ) -> ModuleInstanceDef<'data> {
        // Map the flat list of `args` to instead a name-to-instance index.
        let mut instance_by_name = HashMap::new();
        for arg in args {
            match arg.kind {
                wasmparser::InstantiationArgKind::Instance => {
                    let idx = ModuleInstanceIndex::from_u32(arg.index);
                    instance_by_name.insert(arg.name, idx);
                }
            }
        }

        let instantiate = match self.result.modules[module].clone() {
            // A module defined within this component is being instantiated
            // which means we statically know the structure of the module. The
            // list of imports required is ordered by the actual list of imports
            // listed on the module itself (which wasmtime later requires during
            // instantiation).
            ModuleDef::Upvar(upvar_idx) => {
                let args = self.result.upvars[upvar_idx]
                    .module
                    .imports()
                    .map(|(m, n, _)| (m.to_string(), n.to_string()))
                    .collect::<Vec<_>>()
                    .iter()
                    .map(|(module, name)| {
                        self.lookup_core_def(instance_by_name[module.as_str()], name)
                    })
                    .collect();
                InstantiateModule::Upvar(upvar_idx, args)
            }

            // For imported modules the list of arguments is built to match the
            // order of the imports listed in the declared type of the module.
            // Note that this will need to be reshuffled at runtime since the
            // actual module being instantiated may originally have required
            // imports in a different order.
            ModuleDef::Import { ty, import } => {
                let import = self.runtime_import_index(import);
                let mut args = IndexMap::new();
                let imports = self.types[ty].imports.keys().cloned().collect::<Vec<_>>();
                for (module, name) in imports {
                    let def = self.lookup_core_def(instance_by_name[module.as_str()], &name);
                    let prev = args
                        .entry(module)
                        .or_insert(IndexMap::new())
                        .insert(name, def);
                    assert!(prev.is_none());
                }
                InstantiateModule::Import(import, args)
            }
        };
        self.result
            .component
            .initializers
            .push(Initializer::InstantiateModule(instantiate));

        let instance = RuntimeInstanceIndex::from_u32(self.result.component.num_runtime_instances);
        self.result.component.num_runtime_instances += 1;
        ModuleInstanceDef::Instantiated { instance, module }
    }

    /// Calculate the `CoreDef`, a definition of a core wasm item, corresponding
    /// to the export `name` of the `instance` specified.
    ///
    /// This classifies the export of the instance as one which we
    /// statically know by index within an instantiated module (because
    /// we know the module), one that must be referred to by name since the
    /// module isn't known, or it's a synthesized lowering or adapter of a
    /// component function.
    fn lookup_core_def(&mut self, instance: ModuleInstanceIndex, name: &str) -> CoreDef {
        match &self.result.module_instances[instance] {
            ModuleInstanceDef::Instantiated { module, instance } => {
                let (src, _ty) = self.lookup_core_source_in_module(*instance, *module, name);
                src.to_core_def()
            }

            ModuleInstanceDef::Synthetic(defs) => match defs[&name] {
                EntityIndex::Function(f) => match self.result.funcs[f].clone() {
                    Func::Core(c) => c.to_core_def(),
                    Func::Lowered(i) => CoreDef::Lowered(i),
                },
                EntityIndex::Global(g) => self.result.globals[g].to_core_def(),
                EntityIndex::Table(t) => self.result.tables[t].to_core_def(),
                EntityIndex::Memory(m) => self.result.memories[m].to_core_def(),
            },
        }
    }

    /// Calculates the `CoreSource` associated with the export `name` as an
    /// instance of the instantiated `module` specified.
    ///
    /// The `instance` index here represents the runtime instance index that
    /// we're looking up within.
    fn lookup_core_source_in_module<'b>(
        &self,
        instance: RuntimeInstanceIndex,
        module: ModuleIndex,
        name: &'b str,
    ) -> (CoreSource<'b>, EntityType) {
        match self.result.modules[module] {
            // The module instantiated is one that we statically know the
            // structure of. This means that `name` points to an exact index of
            // an item within the module which we lookup here and record.
            ModuleDef::Upvar(upvar_idx) => {
                let trans = &self.result.upvars[upvar_idx];
                let idx = trans.module.exports[name];
                let src = CoreSource::Index(instance, idx);
                let ty = trans.module.type_of(idx);
                (src, ty)
            }

            // The module instantiated is imported so we don't statically know
            // its structure. This means that the export must be identified by
            // name.
            ModuleDef::Import { ty, .. } => {
                let src = CoreSource::Export(instance, name);
                let ty = self.types[ty].exports[name].clone();
                (src, ty)
            }
        }
    }

    /// Creates a synthetic module from the list of items currently in the
    /// module and their given names.
    fn instantiate_module_from_exports(
        &mut self,
        exports: &[wasmparser::Export<'data>],
    ) -> ModuleInstanceDef<'data> {
        let mut map = HashMap::with_capacity(exports.len());
        for export in exports {
            let idx = match export.kind {
                wasmparser::ExternalKind::Func => {
                    let index = FuncIndex::from_u32(export.index);
                    EntityIndex::Function(index)
                }
                wasmparser::ExternalKind::Table => {
                    let index = TableIndex::from_u32(export.index);
                    EntityIndex::Table(index)
                }
                wasmparser::ExternalKind::Memory => {
                    let index = MemoryIndex::from_u32(export.index);
                    EntityIndex::Memory(index)
                }
                wasmparser::ExternalKind::Global => {
                    let index = GlobalIndex::from_u32(export.index);
                    EntityIndex::Global(index)
                }

                // doesn't get past validation
                wasmparser::ExternalKind::Tag => unimplemented!("wasm exceptions"),
            };
            map.insert(export.name, idx);
        }
        ModuleInstanceDef::Synthetic(map)
    }

    /// Creates a synthetic module from the list of items currently in the
    /// module and their given names.
    fn instantiate_component_from_exports(
        &mut self,
        exports: &[wasmparser::ComponentExport<'data>],
    ) -> ComponentInstanceDef<'data> {
        let mut map = HashMap::with_capacity(exports.len());
        for export in exports {
            let idx = match &export.kind {
                wasmparser::ComponentExternalKind::Func => {
                    let index = FuncIndex::from_u32(export.index);
                    ComponentItem::Func(index)
                }
                wasmparser::ComponentExternalKind::Module => {
                    let index = ModuleIndex::from_u32(export.index);
                    ComponentItem::Module(index)
                }
                wasmparser::ComponentExternalKind::Instance => {
                    let index = ComponentInstanceIndex::from_u32(export.index);
                    ComponentItem::ComponentInstance(index)
                }
                wasmparser::ComponentExternalKind::Component => {
                    let index = ComponentIndex::from_u32(export.index);
                    ComponentItem::Component(index)
                }
                wasmparser::ComponentExternalKind::Value => {
                    unimplemented!("component values");
                }
                wasmparser::ComponentExternalKind::Type => {
                    unimplemented!("component type export");
                }
            };
            map.insert(export.name, idx);
        }
        ComponentInstanceDef::Synthetic(map)
    }

    fn export(&mut self, export: &wasmparser::ComponentExport<'data>) {
        let name = export.name;
        let export = match export.kind {
            wasmparser::ComponentExternalKind::Module => {
                let idx = ModuleIndex::from_u32(export.index);
                let init = match self.result.modules[idx].clone() {
                    ModuleDef::Upvar(idx) => Initializer::SaveModuleUpvar(idx),
                    ModuleDef::Import { import, .. } => {
                        Initializer::SaveModuleImport(self.runtime_import_index(import))
                    }
                };
                self.result.component.initializers.push(init);
                let runtime_index =
                    RuntimeModuleIndex::from_u32(self.result.component.num_runtime_modules);
                self.result.component.num_runtime_modules += 1;
                Export::Module(runtime_index)
            }
            wasmparser::ComponentExternalKind::Component => {
                let idx = ComponentIndex::from_u32(export.index);
                drop(idx);
                unimplemented!("exporting a component");
            }
            wasmparser::ComponentExternalKind::Instance => {
                let idx = ComponentInstanceIndex::from_u32(export.index);
                drop(idx);
                unimplemented!("exporting an instance");
            }
            wasmparser::ComponentExternalKind::Func => {
                let idx = ComponentFuncIndex::from_u32(export.index);
                match self.result.component_funcs[idx].clone() {
                    ComponentFunc::Lifted { ty, func, options } => Export::LiftedFunction {
                        ty,
                        func: func.to_core_export(|i| match i {
                            EntityIndex::Function(i) => i,
                            _ => unreachable!(),
                        }),
                        options,
                    },

                    // TODO: Not 100% clear what to do about this. Given the
                    // expected implementation of host functions there's not a
                    // great way to actually invoke a host function after it's
                    // been wrapped up in a `Func` (or similar). One of the
                    // major issues here is that the callee expects the
                    // canonical-abi format but the caller has host-rust format,
                    // and bridging that gap is expected to be nontrivial.
                    //
                    // This may be solvable with like a temporary arena to lower
                    // into which is discarded after the call finishes? Or...
                    // something like that? This may not be too important to
                    // support in terms of perf so if it's not the fastest thing
                    // in the world that's probably alright.
                    //
                    // Nevertheless this shouldn't panic, eventually when the
                    // component model implementation is finished this should do
                    // something reasonable.
                    ComponentFunc::Import { .. } => unimplemented!("exporting an import"),
                }
            }
            wasmparser::ComponentExternalKind::Value => {
                unimplemented!("exporting a value");
            }
            wasmparser::ComponentExternalKind::Type => {
                let idx = TypeIndex::from_u32(export.index);
                drop(idx);
                unimplemented!("exporting a type");
            }
        };
        self.result
            .component
            .exports
            .insert(name.to_string(), export);
    }

    fn alias_module_instance_export(
        &mut self,
        kind: wasmparser::ExternalKind,
        instance: ModuleInstanceIndex,
        name: &'data str,
    ) {
        match &self.result.module_instances[instance] {
            // The `instance` points to an instantiated module, meaning we can
            // lookup the `CoreSource` associated with it and use the type
            // information to insert it into the appropriate namespace.
            ModuleInstanceDef::Instantiated { instance, module } => {
                let (src, ty) = self.lookup_core_source_in_module(*instance, *module, name);
                match ty {
                    EntityType::Function(_) => {
                        assert_eq!(kind, wasmparser::ExternalKind::Func);
                        self.result.funcs.push(Func::Core(src));
                    }
                    EntityType::Global(_) => {
                        assert_eq!(kind, wasmparser::ExternalKind::Global);
                        self.result.globals.push(src);
                    }
                    EntityType::Memory(_) => {
                        assert_eq!(kind, wasmparser::ExternalKind::Memory);
                        self.result.memories.push(src);
                    }
                    EntityType::Table(_) => {
                        assert_eq!(kind, wasmparser::ExternalKind::Table);
                        self.result.tables.push(src);
                    }
                    EntityType::Tag(_) => unimplemented!("wasm exceptions"),
                }
            }

            // ... and like above for synthetic components aliasing exports from
            // synthetic modules is also just copying around the identifying
            // information.
            ModuleInstanceDef::Synthetic(exports) => match exports[&name] {
                EntityIndex::Function(i) => {
                    assert_eq!(kind, wasmparser::ExternalKind::Func);
                    self.result.funcs.push(self.result.funcs[i].clone());
                }
                EntityIndex::Global(i) => {
                    assert_eq!(kind, wasmparser::ExternalKind::Global);
                    self.result.globals.push(self.result.globals[i].clone());
                }
                EntityIndex::Table(i) => {
                    assert_eq!(kind, wasmparser::ExternalKind::Table);
                    self.result.tables.push(self.result.tables[i].clone());
                }
                EntityIndex::Memory(i) => {
                    assert_eq!(kind, wasmparser::ExternalKind::Memory);
                    self.result.memories.push(self.result.memories[i].clone());
                }
            },
        }
    }

    fn alias_component_instance_export(
        &mut self,
        kind: wasmparser::ComponentExternalKind,
        instance: ComponentInstanceIndex,
        name: &'data str,
    ) {
        match &self.result.component_instances[instance] {
            // The `instance` points to an imported component instance, meaning
            // that the item we're pushing into our index spaces is effectively
            // another form of import. The `name` is appended to the `import`
            // found here and then the appropriate namespace of an import is
            // recorded as well.
            ComponentInstanceDef::Import { import, ty } => {
                let import = import.append(name);
                match self.types[*ty].exports[name] {
                    TypeDef::Module(ty) => {
                        assert_eq!(kind, wasmparser::ComponentExternalKind::Module);
                        self.result.modules.push(ModuleDef::Import { import, ty });
                    }
                    TypeDef::ComponentInstance(ty) => {
                        assert_eq!(kind, wasmparser::ComponentExternalKind::Instance);
                        self.result
                            .component_instances
                            .push(ComponentInstanceDef::Import { import, ty });
                    }
                    TypeDef::ComponentFunc(_ty) => {
                        assert_eq!(kind, wasmparser::ComponentExternalKind::Func);
                        self.result
                            .component_funcs
                            .push(ComponentFunc::Import(import));
                    }
                    TypeDef::Interface(_) => unimplemented!("alias type export"),
                    TypeDef::Component(_) => unimplemented!("alias component export"),

                    // not possible with valid components
                    TypeDef::CoreFunc(_ty) => unreachable!(),
                }
            }

            // For synthetic component/module instances we can just copy the
            // definition of the original item into a new slot as well to record
            // that the index describes the same item.
            ComponentInstanceDef::Synthetic(exports) => match exports[&name] {
                ComponentItem::Func(i) => {
                    assert_eq!(kind, wasmparser::ComponentExternalKind::Func);
                    self.result.funcs.push(self.result.funcs[i].clone());
                }
                ComponentItem::Module(i) => {
                    assert_eq!(kind, wasmparser::ComponentExternalKind::Module);
                    self.result.modules.push(self.result.modules[i].clone());
                }
                ComponentItem::ComponentInstance(i) => {
                    assert_eq!(kind, wasmparser::ComponentExternalKind::Instance);
                    self.result
                        .component_instances
                        .push(self.result.component_instances[i].clone());
                }
                ComponentItem::Component(_) => unimplemented!("aliasing a component export"),
            },
        }
    }

    fn alias_component_outer(
        &mut self,
        kind: wasmparser::ComponentOuterAliasKind,
        count: u32,
        index: u32,
    ) {
        match kind {
            // When aliasing a type the `ComponentTypesBuilder` is used to
            // resolve the outer `count` plus the index, and then once it's
            // resolved we push the type information into our local index
            // space.
            //
            // Note that this is just copying indices around as all type
            // information is basically a pointer back into the `TypesBuilder`
            // structure (and the eventual `TypeTables` that it produces).
            wasmparser::ComponentOuterAliasKind::CoreType => {
                let index = TypeIndex::from_u32(index);
                let ty = self.types.core_outer_type(count, index);
                self.types.push_core_typedef(ty);
            }
            wasmparser::ComponentOuterAliasKind::Type => {
                let index = ComponentTypeIndex::from_u32(index);
                let ty = self.types.component_outer_type(count, index);
                self.types.push_component_typedef(ty);
            }

            wasmparser::ComponentOuterAliasKind::CoreModule => {
                unimplemented!("outer alias to module");
            }
            wasmparser::ComponentOuterAliasKind::Component => {
                unimplemented!("outer alias to component");
            }
        }
    }

    fn lift_function(
        &mut self,
        ty: ComponentTypeIndex,
        func: FuncIndex,
        options: &[wasmparser::CanonicalOption],
    ) -> ComponentFunc<'data> {
        let ty = match self.types.component_outer_type(0, ty) {
            TypeDef::ComponentFunc(ty) => ty,
            // should not be possible after validation
            _ => unreachable!(),
        };
        let func = match &self.result.funcs[func] {
            Func::Core(core) => core.clone(),

            // TODO: it's not immediately obvious how to implement this. Once
            // lowered imports are fully implemented it may be the case that
            // implementing this "just falls out" of the same implementation.
            // This technically is valid and basically just result in leaking
            // memory into core wasm (since nothing is around to call
            // deallocation/free functions).
            Func::Lowered(_) => unimplemented!("lifting a lowered function"),
        };
        let options = self.canonical_options(options);
        ComponentFunc::Lifted { ty, func, options }
    }

    fn lower_function(
        &mut self,
        func: ComponentFuncIndex,
        options: &[wasmparser::CanonicalOption],
    ) -> Func<'data> {
        let options = self.canonical_options(options);
        match self.result.component_funcs[func].clone() {
            ComponentFunc::Import(import) => {
                let import = self.runtime_import_index(import);
                let index = LoweredIndex::from_u32(self.result.component.num_lowerings);
                self.result.component.num_lowerings += 1;
                let fill_idx = self.result.component.initializers.len();
                self.result
                    .component
                    .initializers
                    .push(Initializer::LowerImport(LowerImport {
                        index,
                        import,
                        options,
                        // This is filled after the component is finished when
                        // we have wasmparser's type information available, so
                        // leave a dummy for now to get filled in.
                        canonical_abi: SignatureIndex::from_u32(0),
                    }));
                self.result
                    .signatures_to_fill
                    .push((fill_idx, self.result.funcs.next_key()));
                Func::Lowered(index)
            }

            // TODO: From reading the spec, this technically should create a
            // function that lifts the arguments and then afterwards
            // unconditionally traps. That would mean that this validates the
            // arguments within the context of `options` and then traps.
            ComponentFunc::Lifted { .. } => unimplemented!("lower a lifted function"),
        }
    }

    fn canonical_options(&mut self, opts: &[wasmparser::CanonicalOption]) -> CanonicalOptions {
        let mut ret = CanonicalOptions::default();
        for opt in opts {
            match opt {
                wasmparser::CanonicalOption::UTF8 => {
                    ret.string_encoding = StringEncoding::Utf8;
                }
                wasmparser::CanonicalOption::UTF16 => {
                    ret.string_encoding = StringEncoding::Utf16;
                }
                wasmparser::CanonicalOption::CompactUTF16 => {
                    ret.string_encoding = StringEncoding::CompactUtf16;
                }
                wasmparser::CanonicalOption::Memory(idx) => {
                    let idx = MemoryIndex::from_u32(*idx);
                    let memory = self.result.memories[idx].to_core_export(|i| match i {
                        EntityIndex::Memory(i) => i,
                        _ => unreachable!(),
                    });
                    let memory = self.runtime_memory(memory);
                    ret.memory = Some(memory);
                }
                wasmparser::CanonicalOption::Realloc(idx) => {
                    let idx = FuncIndex::from_u32(*idx);
                    let realloc = self.result.funcs[idx].to_core_def();
                    let realloc = self.runtime_realloc(realloc);
                    ret.realloc = Some(realloc);
                }
                wasmparser::CanonicalOption::PostReturn(_) => {
                    unimplemented!("post-return");
                }
            }
        }
        return ret;
    }

    fn runtime_import_index(&mut self, import: RuntimeImport) -> RuntimeImportIndex {
        if let Some(idx) = self.result.import_map.get(&import) {
            return *idx;
        }
        let idx = self
            .result
            .component
            .imports
            .push((import.source, import.exports.clone()));
        self.result.import_map.insert(import, idx);
        return idx;
    }

    fn runtime_memory(&mut self, export: CoreExport<MemoryIndex>) -> RuntimeMemoryIndex {
        if let Some(idx) = self.result.memory_to_runtime.get(&export) {
            return *idx;
        }
        let index = RuntimeMemoryIndex::from_u32(self.result.component.num_runtime_memories);
        self.result.component.num_runtime_memories += 1;
        self.result.memory_to_runtime.insert(export.clone(), index);
        self.result
            .component
            .initializers
            .push(Initializer::ExtractMemory { index, export });
        index
    }

    fn runtime_realloc(&mut self, def: CoreDef) -> RuntimeReallocIndex {
        if let Some(idx) = self.result.realloc_to_runtime.get(&def) {
            return *idx;
        }
        let index = RuntimeReallocIndex::from_u32(self.result.component.num_runtime_reallocs);
        self.result.component.num_runtime_reallocs += 1;
        self.result.realloc_to_runtime.insert(def.clone(), index);
        self.result
            .component
            .initializers
            .push(Initializer::ExtractRealloc { index, def });
        index
    }
}

impl CoreSource<'_> {
    fn to_core_export<T>(&self, get_index: impl FnOnce(EntityIndex) -> T) -> CoreExport<T> {
        match self {
            CoreSource::Index(instance, index) => CoreExport {
                instance: *instance,
                item: ExportItem::Index(get_index(*index)),
            },
            CoreSource::Export(instance, name) => CoreExport {
                instance: *instance,
                item: ExportItem::Name(name.to_string()),
            },
        }
    }

    fn to_core_def(&self) -> CoreDef {
        self.to_core_export(|i| i).into()
    }
}

impl Func<'_> {
    fn to_core_def(&self) -> CoreDef {
        match self {
            Func::Core(src) => src.to_core_def(),
            Func::Lowered(idx) => CoreDef::Lowered(*idx),
        }
    }
}
