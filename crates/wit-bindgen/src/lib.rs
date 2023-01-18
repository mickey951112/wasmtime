use crate::rust::{to_rust_ident, RustGenerator, TypeMode};
use crate::types::{TypeInfo, Types};
use heck::*;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::{Read, Write};
use std::mem;
use std::process::{Command, Stdio};
use wit_parser::*;

macro_rules! uwrite {
    ($dst:expr, $($arg:tt)*) => {
        write!($dst, $($arg)*).unwrap()
    };
}

macro_rules! uwriteln {
    ($dst:expr, $($arg:tt)*) => {
        writeln!($dst, $($arg)*).unwrap()
    };
}

mod rust;
mod source;
mod types;
use source::Source;

#[derive(Default)]
struct Wasmtime {
    src: Source,
    opts: Opts,
    imports: Vec<Import>,
    exports: Exports,
    types: Types,
}

enum Import {
    Interface { snake: String },
    Function { add_to_linker: String, sig: String },
}

#[derive(Default)]
struct Exports {
    fields: BTreeMap<String, (String, String)>,
    funcs: Vec<String>,
}

#[derive(Default, Debug, Clone)]
pub struct Opts {
    /// Whether or not `rustfmt` is executed to format generated code.
    pub rustfmt: bool,

    /// Whether or not to emit `tracing` macro calls on function entry/exit.
    pub tracing: bool,

    /// Whether or not to use async rust functions and traits.
    pub async_: bool,

    /// A list of "trappable errors" which are used to replace the `E` in
    /// `result<T, E>` found in WIT.
    pub trappable_error_type: Vec<TrappableError>,
}

#[derive(Debug, Clone)]
pub struct TrappableError {
    /// The name of the error in WIT that is being mapped.
    pub wit_name: String,

    /// The owner container of the error in WIT of the error that's being
    /// mapped.
    ///
    /// This is, for example, the name of the WIT interface or the WIT world
    /// which owns the type. If this is set to `None` then any error type with
    /// `wit_name` is remapped to `rust_name`.
    pub wit_owner: Option<String>,

    /// The name, in Rust, of the error type to generate.
    pub rust_name: String,
}

impl Opts {
    pub fn generate(&self, resolve: &Resolve, world: WorldId) -> String {
        let mut r = Wasmtime::default();
        r.opts = self.clone();
        r.generate(resolve, world)
    }
}

impl Wasmtime {
    fn generate(&mut self, resolve: &Resolve, id: WorldId) -> String {
        self.types.analyze(resolve, id);
        let world = &resolve.worlds[id];
        for (name, import) in world.imports.iter() {
            self.import(resolve, name, import);
        }
        for (name, export) in world.exports.iter() {
            self.export(resolve, name, export);
        }
        self.finish(resolve, id)
    }

    fn import(&mut self, resolve: &Resolve, name: &str, item: &WorldItem) {
        let snake = name.to_snake_case();
        let mut gen = InterfaceGenerator::new(self, resolve, TypeMode::Owned);
        let import = match item {
            WorldItem::Function(func) => {
                gen.generate_function_trait_sig(TypeOwner::None, &func);
                let sig = mem::take(&mut gen.src).into();
                gen.generate_add_function_to_linker(TypeOwner::None, &func, "linker");
                let add_to_linker = gen.src.into();
                Import::Function { sig, add_to_linker }
            }
            WorldItem::Interface(id) => {
                gen.current_interface = Some(*id);
                gen.types(*id);
                gen.generate_trappable_error_types(TypeOwner::Interface(*id));
                gen.generate_add_to_linker(*id, name);

                let module = &gen.src[..];

                uwriteln!(
                    self.src,
                    "
                        #[allow(clippy::all)]
                        pub mod {snake} {{
                            #[allow(unused_imports)]
                            use wasmtime::component::__internal::anyhow;

                            {module}
                        }}
                    "
                );
                Import::Interface { snake }
            }
        };

        self.imports.push(import);
    }

    fn export(&mut self, resolve: &Resolve, name: &str, item: &WorldItem) {
        let snake = name.to_snake_case();
        let mut gen = InterfaceGenerator::new(self, resolve, TypeMode::AllBorrowed("'a"));
        let (ty, getter) = match item {
            WorldItem::Function(func) => {
                gen.define_rust_guest_export(None, func);
                let body = mem::take(&mut gen.src).into();
                let (_name, getter) = gen.extract_typed_function(func);
                assert!(gen.src.is_empty());
                self.exports.funcs.push(body);
                (format!("wasmtime::component::Func"), getter)
            }
            WorldItem::Interface(id) => {
                gen.current_interface = Some(*id);
                gen.types(*id);
                gen.generate_trappable_error_types(TypeOwner::Interface(*id));
                let iface = &resolve.interfaces[*id];

                let camel = name.to_upper_camel_case();
                uwriteln!(gen.src, "pub struct {camel} {{");
                for (_, func) in iface.functions.iter() {
                    uwriteln!(
                        gen.src,
                        "{}: wasmtime::component::Func,",
                        func.name.to_snake_case()
                    );
                }
                uwriteln!(gen.src, "}}");

                uwriteln!(gen.src, "impl {camel} {{");
                uwrite!(
                    gen.src,
                    "
                        pub fn new(
                            __exports: &mut wasmtime::component::ExportInstance<'_, '_>,
                        ) -> anyhow::Result<{camel}> {{
                    "
                );
                let mut fields = Vec::new();
                for (_, func) in iface.functions.iter() {
                    let (name, getter) = gen.extract_typed_function(func);
                    uwriteln!(gen.src, "let {name} = {getter};");
                    fields.push(name);
                }
                uwriteln!(gen.src, "Ok({camel} {{");
                for name in fields {
                    uwriteln!(gen.src, "{name},");
                }
                uwriteln!(gen.src, "}})");
                uwriteln!(gen.src, "}}");
                for (_, func) in iface.functions.iter() {
                    gen.define_rust_guest_export(Some(name), func);
                }
                uwriteln!(gen.src, "}}");

                let module = &gen.src[..];

                uwriteln!(
                    self.src,
                    "
                        #[allow(clippy::all)]
                        pub mod {snake} {{
                            #[allow(unused_imports)]
                            use wasmtime::component::__internal::anyhow;

                            {module}
                        }}
                    "
                );

                let getter = format!(
                    "\
                        {snake}::{camel}::new(
                            &mut __exports.instance(\"{name}\")
                                .ok_or_else(|| anyhow::anyhow!(\"exported instance `{name}` not present\"))?
                        )?\
                    "
                );
                self.exports.funcs.push(format!(
                    "
                        pub fn {snake}(&self) -> &{snake}::{camel} {{
                            &self.{snake}
                        }}
                    "
                ));
                (format!("{snake}::{camel}"), getter)
            }
        };
        let prev = self.exports.fields.insert(snake.clone(), (ty, getter));
        assert!(prev.is_none());
    }

    fn finish(&mut self, resolve: &Resolve, world: WorldId) -> String {
        let camel = resolve.worlds[world].name.to_upper_camel_case();
        uwriteln!(self.src, "pub struct {camel} {{");
        for (name, (ty, _)) in self.exports.fields.iter() {
            uwriteln!(self.src, "{name}: {ty},");
        }
        self.src.push_str("}\n");

        let (async_, async__, send, await_) = if self.opts.async_ {
            ("async", "_async", ":Send", ".await")
        } else {
            ("", "", "", "")
        };

        self.toplevel_import_trait(resolve, world);

        uwriteln!(self.src, "const _: () = {{");
        uwriteln!(self.src, "use wasmtime::component::__internal::anyhow;");

        uwriteln!(self.src, "impl {camel} {{");
        self.toplevel_add_to_linker(resolve, world);
        uwriteln!(
            self.src,
            "
                /// Instantiates the provided `module` using the specified
                /// parameters, wrapping up the result in a structure that
                /// translates between wasm and the host.
                pub {async_} fn instantiate{async__}<T {send}>(
                    mut store: impl wasmtime::AsContextMut<Data = T>,
                    component: &wasmtime::component::Component,
                    linker: &wasmtime::component::Linker<T>,
                ) -> anyhow::Result<(Self, wasmtime::component::Instance)> {{
                    let instance = linker.instantiate{async__}(&mut store, component){await_}?;
                    Ok((Self::new(store, &instance)?, instance))
                }}

                /// Low-level creation wrapper for wrapping up the exports
                /// of the `instance` provided in this structure of wasm
                /// exports.
                ///
                /// This function will extract exports from the `instance`
                /// defined within `store` and wrap them all up in the
                /// returned structure which can be used to interact with
                /// the wasm module.
                pub fn new(
                    mut store: impl wasmtime::AsContextMut,
                    instance: &wasmtime::component::Instance,
                ) -> anyhow::Result<Self> {{
                    let mut store = store.as_context_mut();
                    let mut exports = instance.exports(&mut store);
                    let mut __exports = exports.root();
            ",
        );
        for (name, (_, get)) in self.exports.fields.iter() {
            uwriteln!(self.src, "let {name} = {get};");
        }
        uwriteln!(self.src, "Ok({camel} {{");
        for (name, _) in self.exports.fields.iter() {
            uwriteln!(self.src, "{name},");
        }
        uwriteln!(self.src, "}})");
        uwriteln!(self.src, "}}"); // close `fn new`

        for func in self.exports.funcs.iter() {
            self.src.push_str(func);
        }

        uwriteln!(self.src, "}}"); // close `impl {camel}`

        uwriteln!(self.src, "}};"); // close `const _: () = ...

        let mut src = mem::take(&mut self.src);
        if self.opts.rustfmt {
            let mut child = Command::new("rustfmt")
                .arg("--edition=2018")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .expect("failed to spawn `rustfmt`");
            child
                .stdin
                .take()
                .unwrap()
                .write_all(src.as_bytes())
                .unwrap();
            src.as_mut_string().truncate(0);
            child
                .stdout
                .take()
                .unwrap()
                .read_to_string(src.as_mut_string())
                .unwrap();
            let status = child.wait().unwrap();
            assert!(status.success());
        }

        src.into()
    }
}

impl Wasmtime {
    fn toplevel_import_trait(&mut self, resolve: &Resolve, world: WorldId) {
        let mut functions = Vec::new();
        for import in self.imports.iter() {
            match import {
                Import::Interface { .. } => continue,
                Import::Function {
                    sig,
                    add_to_linker: _,
                } => functions.push(sig),
            }
        }
        if functions.is_empty() {
            return;
        }

        let world_camel = resolve.worlds[world].name.to_upper_camel_case();
        uwriteln!(self.src, "pub trait {world_camel}Imports {{");
        for sig in functions {
            self.src.push_str(sig);
            self.src.push_str("\n");
        }
        uwriteln!(self.src, "}}");
    }

    fn toplevel_add_to_linker(&mut self, resolve: &Resolve, world: WorldId) {
        if self.imports.is_empty() {
            return;
        }
        let mut functions = Vec::new();
        let mut interfaces = Vec::new();
        for import in self.imports.iter() {
            match import {
                Import::Interface { snake } => interfaces.push(snake),
                Import::Function {
                    add_to_linker,
                    sig: _,
                } => functions.push(add_to_linker),
            }
        }

        uwrite!(
            self.src,
            "
                pub fn add_to_linker<T, U>(
                    linker: &mut wasmtime::component::Linker<T>,
                    get: impl Fn(&mut T) -> &mut U + Send + Sync + Copy + 'static,
                ) -> anyhow::Result<()>
                    where U: \
            "
        );
        let world_camel = resolve.worlds[world].name.to_upper_camel_case();
        let world_trait = format!("{world_camel}Imports");
        for (i, name) in interfaces
            .iter()
            .map(|n| format!("{n}::{}", n.to_upper_camel_case()))
            .chain(if functions.is_empty() {
                None
            } else {
                Some(world_trait.clone())
            })
            .enumerate()
        {
            if i > 0 {
                self.src.push_str(" + ");
            }
            self.src.push_str(&name);
        }
        let maybe_send = if self.opts.async_ {
            " + Send, T: Send"
        } else {
            ""
        };
        self.src.push_str(maybe_send);
        self.src.push_str(",\n{\n");
        for name in interfaces.iter() {
            uwriteln!(self.src, "{name}::add_to_linker(linker, get)?;");
        }
        if !functions.is_empty() {
            uwriteln!(self.src, "Self::add_root_to_linker(linker, get)?;");
        }
        uwriteln!(self.src, "Ok(())\n}}");
        if functions.is_empty() {
            return;
        }

        uwrite!(
            self.src,
            "
                pub fn add_root_to_linker<T, U>(
                    linker: &mut wasmtime::component::Linker<T>,
                    get: impl Fn(&mut T) -> &mut U + Send + Sync + Copy + 'static,
                ) -> anyhow::Result<()>
                    where U: {world_trait}{maybe_send}
                {{
                    let mut linker = linker.root();
            ",
        );
        for add_to_linker in functions {
            self.src.push_str(add_to_linker);
            self.src.push_str("\n");
        }
        uwriteln!(self.src, "Ok(())\n}}");
    }
}

struct InterfaceGenerator<'a> {
    src: Source,
    gen: &'a mut Wasmtime,
    resolve: &'a Resolve,
    default_param_mode: TypeMode,
    current_interface: Option<InterfaceId>,
}

impl<'a> InterfaceGenerator<'a> {
    fn new(
        gen: &'a mut Wasmtime,
        resolve: &'a Resolve,
        default_param_mode: TypeMode,
    ) -> InterfaceGenerator<'a> {
        InterfaceGenerator {
            src: Source::default(),
            gen,
            resolve,
            default_param_mode,
            current_interface: None,
        }
    }

    fn types(&mut self, id: InterfaceId) {
        for (name, id) in self.resolve.interfaces[id].types.iter() {
            let id = *id;
            let ty = &self.resolve.types[id];
            match &ty.kind {
                TypeDefKind::Record(record) => self.type_record(id, name, record, &ty.docs),
                TypeDefKind::Flags(flags) => self.type_flags(id, name, flags, &ty.docs),
                TypeDefKind::Tuple(tuple) => self.type_tuple(id, name, tuple, &ty.docs),
                TypeDefKind::Enum(enum_) => self.type_enum(id, name, enum_, &ty.docs),
                TypeDefKind::Variant(variant) => self.type_variant(id, name, variant, &ty.docs),
                TypeDefKind::Option(t) => self.type_option(id, name, t, &ty.docs),
                TypeDefKind::Result(r) => self.type_result(id, name, r, &ty.docs),
                TypeDefKind::Union(u) => self.type_union(id, name, u, &ty.docs),
                TypeDefKind::List(t) => self.type_list(id, name, t, &ty.docs),
                TypeDefKind::Type(t) => self.type_alias(id, name, t, &ty.docs),
                TypeDefKind::Future(_) => todo!("generate for future"),
                TypeDefKind::Stream(_) => todo!("generate for stream"),
                TypeDefKind::Unknown => unreachable!(),
            }
        }
    }

    fn type_record(&mut self, id: TypeId, _name: &str, record: &Record, docs: &Docs) {
        let info = self.info(id);
        for (name, mode) in self.modes_of(id) {
            let lt = self.lifetime_for(&info, mode);
            self.rustdoc(docs);

            self.push_str("#[derive(wasmtime::component::ComponentType)]\n");
            if lt.is_none() {
                self.push_str("#[derive(wasmtime::component::Lift)]\n");
            }
            self.push_str("#[derive(wasmtime::component::Lower)]\n");
            self.push_str("#[component(record)]\n");

            if !info.has_list {
                self.push_str("#[derive(Copy, Clone)]\n");
            } else {
                self.push_str("#[derive(Clone)]\n");
            }
            self.push_str(&format!("pub struct {}", name));
            self.print_generics(lt);
            self.push_str(" {\n");
            for field in record.fields.iter() {
                self.rustdoc(&field.docs);
                self.push_str(&format!("#[component(name = \"{}\")]\n", field.name));
                self.push_str("pub ");
                self.push_str(&to_rust_ident(&field.name));
                self.push_str(": ");
                self.print_ty(&field.ty, mode);
                self.push_str(",\n");
            }
            self.push_str("}\n");

            self.push_str("impl");
            self.print_generics(lt);
            self.push_str(" core::fmt::Debug for ");
            self.push_str(&name);
            self.print_generics(lt);
            self.push_str(" {\n");
            self.push_str(
                "fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {\n",
            );
            self.push_str(&format!("f.debug_struct(\"{}\")", name));
            for field in record.fields.iter() {
                self.push_str(&format!(
                    ".field(\"{}\", &self.{})",
                    field.name,
                    to_rust_ident(&field.name)
                ));
            }
            self.push_str(".finish()\n");
            self.push_str("}\n");
            self.push_str("}\n");

            if info.error {
                self.push_str("impl");
                self.print_generics(lt);
                self.push_str(" core::fmt::Display for ");
                self.push_str(&name);
                self.print_generics(lt);
                self.push_str(" {\n");
                self.push_str(
                    "fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {\n",
                );
                self.push_str("write!(f, \"{:?}\", self)\n");
                self.push_str("}\n");
                self.push_str("}\n");
                self.push_str("impl std::error::Error for ");
                self.push_str(&name);
                self.push_str("{}\n");
            }
        }
    }

    fn type_tuple(&mut self, id: TypeId, _name: &str, tuple: &Tuple, docs: &Docs) {
        let info = self.info(id);
        for (name, mode) in self.modes_of(id) {
            let lt = self.lifetime_for(&info, mode);
            self.rustdoc(docs);
            self.push_str(&format!("pub type {}", name));
            self.print_generics(lt);
            self.push_str(" = (");
            for ty in tuple.types.iter() {
                self.print_ty(ty, mode);
                self.push_str(",");
            }
            self.push_str(");\n");
        }
    }

    fn type_flags(&mut self, _id: TypeId, name: &str, flags: &Flags, docs: &Docs) {
        self.rustdoc(docs);
        self.src.push_str("wasmtime::component::flags!(\n");
        self.src
            .push_str(&format!("{} {{\n", name.to_upper_camel_case()));
        for flag in flags.flags.iter() {
            // TODO wasmtime-component-macro doesnt support docs for flags rn
            uwrite!(
                self.src,
                "#[component(name=\"{}\")] const {};\n",
                flag.name,
                flag.name.to_shouty_snake_case()
            );
        }
        self.src.push_str("}\n");
        self.src.push_str(");\n\n");
    }

    fn type_variant(&mut self, id: TypeId, _name: &str, variant: &Variant, docs: &Docs) {
        self.print_rust_enum(
            id,
            variant.cases.iter().map(|c| {
                (
                    c.name.to_upper_camel_case(),
                    Some(c.name.clone()),
                    &c.docs,
                    c.ty.as_ref(),
                )
            }),
            docs,
            "variant",
        );
    }

    fn type_union(&mut self, id: TypeId, _name: &str, union: &Union, docs: &Docs) {
        self.print_rust_enum(
            id,
            std::iter::zip(self.union_case_names(union), &union.cases)
                .map(|(name, case)| (name, None, &case.docs, Some(&case.ty))),
            docs,
            "union",
        );
    }

    fn type_option(&mut self, id: TypeId, _name: &str, payload: &Type, docs: &Docs) {
        let info = self.info(id);

        for (name, mode) in self.modes_of(id) {
            self.rustdoc(docs);
            let lt = self.lifetime_for(&info, mode);
            self.push_str(&format!("pub type {}", name));
            self.print_generics(lt);
            self.push_str("= Option<");
            self.print_ty(payload, mode);
            self.push_str(">;\n");
        }
    }

    fn print_rust_enum<'b>(
        &mut self,
        id: TypeId,
        cases: impl IntoIterator<Item = (String, Option<String>, &'b Docs, Option<&'b Type>)> + Clone,
        docs: &Docs,
        derive_component: &str,
    ) where
        Self: Sized,
    {
        let info = self.info(id);

        for (name, mode) in self.modes_of(id) {
            let name = name.to_upper_camel_case();

            self.rustdoc(docs);
            let lt = self.lifetime_for(&info, mode);
            self.push_str("#[derive(wasmtime::component::ComponentType)]\n");
            if lt.is_none() {
                self.push_str("#[derive(wasmtime::component::Lift)]\n");
            }
            self.push_str("#[derive(wasmtime::component::Lower)]\n");
            self.push_str(&format!("#[component({})]\n", derive_component));
            if !info.has_list {
                self.push_str("#[derive(Clone, Copy)]\n");
            } else {
                self.push_str("#[derive(Clone)]\n");
            }
            self.push_str(&format!("pub enum {name}"));
            self.print_generics(lt);
            self.push_str("{\n");
            for (case_name, component_name, docs, payload) in cases.clone() {
                self.rustdoc(docs);
                if let Some(n) = component_name {
                    self.push_str(&format!("#[component(name = \"{}\")] ", n));
                }
                self.push_str(&case_name);
                if let Some(ty) = payload {
                    self.push_str("(");
                    self.print_ty(ty, mode);
                    self.push_str(")")
                }
                self.push_str(",\n");
            }
            self.push_str("}\n");

            self.print_rust_enum_debug(
                id,
                mode,
                &name,
                cases
                    .clone()
                    .into_iter()
                    .map(|(name, _attr, _docs, ty)| (name, ty)),
            );

            if info.error {
                self.push_str("impl");
                self.print_generics(lt);
                self.push_str(" core::fmt::Display for ");
                self.push_str(&name);
                self.print_generics(lt);
                self.push_str(" {\n");
                self.push_str(
                    "fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {\n",
                );
                self.push_str("write!(f, \"{:?}\", self)");
                self.push_str("}\n");
                self.push_str("}\n");
                self.push_str("\n");

                self.push_str("impl");
                self.print_generics(lt);
                self.push_str(" std::error::Error for ");
                self.push_str(&name);
                self.print_generics(lt);
                self.push_str(" {}\n");
            }
        }
    }

    fn print_rust_enum_debug<'b>(
        &mut self,
        id: TypeId,
        mode: TypeMode,
        name: &str,
        cases: impl IntoIterator<Item = (String, Option<&'b Type>)>,
    ) where
        Self: Sized,
    {
        let info = self.info(id);
        let lt = self.lifetime_for(&info, mode);
        self.push_str("impl");
        self.print_generics(lt);
        self.push_str(" core::fmt::Debug for ");
        self.push_str(name);
        self.print_generics(lt);
        self.push_str(" {\n");
        self.push_str("fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {\n");
        self.push_str("match self {\n");
        for (case_name, payload) in cases {
            self.push_str(name);
            self.push_str("::");
            self.push_str(&case_name);
            if payload.is_some() {
                self.push_str("(e)");
            }
            self.push_str(" => {\n");
            self.push_str(&format!("f.debug_tuple(\"{}::{}\")", name, case_name));
            if payload.is_some() {
                self.push_str(".field(e)");
            }
            self.push_str(".finish()\n");
            self.push_str("}\n");
        }
        self.push_str("}\n");
        self.push_str("}\n");
        self.push_str("}\n");
    }

    fn type_result(&mut self, id: TypeId, _name: &str, result: &Result_, docs: &Docs) {
        let info = self.info(id);

        for (name, mode) in self.modes_of(id) {
            self.rustdoc(docs);
            let lt = self.lifetime_for(&info, mode);
            self.push_str(&format!("pub type {}", name));
            self.print_generics(lt);
            self.push_str("= Result<");
            self.print_optional_ty(result.ok.as_ref(), mode);
            self.push_str(",");
            self.print_optional_ty(result.err.as_ref(), mode);
            self.push_str(">;\n");
        }
    }

    fn type_enum(&mut self, id: TypeId, name: &str, enum_: &Enum, docs: &Docs) {
        let info = self.info(id);

        let name = name.to_upper_camel_case();
        self.rustdoc(docs);
        self.push_str("#[derive(wasmtime::component::ComponentType)]\n");
        self.push_str("#[derive(wasmtime::component::Lift)]\n");
        self.push_str("#[derive(wasmtime::component::Lower)]\n");
        self.push_str("#[component(enum)]\n");
        self.push_str("#[derive(Clone, Copy, PartialEq, Eq)]\n");
        self.push_str(&format!("pub enum {} {{\n", name.to_upper_camel_case()));
        for case in enum_.cases.iter() {
            self.rustdoc(&case.docs);
            self.push_str(&format!("#[component(name = \"{}\")]", case.name));
            self.push_str(&case.name.to_upper_camel_case());
            self.push_str(",\n");
        }
        self.push_str("}\n");

        // Auto-synthesize an implementation of the standard `Error` trait for
        // error-looking types based on their name.
        if info.error {
            self.push_str("impl ");
            self.push_str(&name);
            self.push_str("{\n");

            self.push_str("pub fn name(&self) -> &'static str {\n");
            self.push_str("match self {\n");
            for case in enum_.cases.iter() {
                self.push_str(&name);
                self.push_str("::");
                self.push_str(&case.name.to_upper_camel_case());
                self.push_str(" => \"");
                self.push_str(case.name.as_str());
                self.push_str("\",\n");
            }
            self.push_str("}\n");
            self.push_str("}\n");

            self.push_str("pub fn message(&self) -> &'static str {\n");
            self.push_str("match self {\n");
            for case in enum_.cases.iter() {
                self.push_str(&name);
                self.push_str("::");
                self.push_str(&case.name.to_upper_camel_case());
                self.push_str(" => \"");
                if let Some(contents) = &case.docs.contents {
                    self.push_str(contents.trim());
                }
                self.push_str("\",\n");
            }
            self.push_str("}\n");
            self.push_str("}\n");

            self.push_str("}\n");

            self.push_str("impl core::fmt::Debug for ");
            self.push_str(&name);
            self.push_str(
                "{\nfn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {\n",
            );
            self.push_str("f.debug_struct(\"");
            self.push_str(&name);
            self.push_str("\")\n");
            self.push_str(".field(\"code\", &(*self as i32))\n");
            self.push_str(".field(\"name\", &self.name())\n");
            self.push_str(".field(\"message\", &self.message())\n");
            self.push_str(".finish()\n");
            self.push_str("}\n");
            self.push_str("}\n");

            self.push_str("impl core::fmt::Display for ");
            self.push_str(&name);
            self.push_str(
                "{\nfn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {\n",
            );
            self.push_str("write!(f, \"{} (error {})\", self.name(), *self as i32)");
            self.push_str("}\n");
            self.push_str("}\n");
            self.push_str("\n");
            self.push_str("impl std::error::Error for ");
            self.push_str(&name);
            self.push_str("{}\n");
        } else {
            self.print_rust_enum_debug(
                id,
                TypeMode::Owned,
                &name,
                enum_
                    .cases
                    .iter()
                    .map(|c| (c.name.to_upper_camel_case(), None)),
            )
        }
    }

    fn type_alias(&mut self, id: TypeId, _name: &str, ty: &Type, docs: &Docs) {
        let info = self.info(id);
        for (name, mode) in self.modes_of(id) {
            self.rustdoc(docs);
            self.push_str(&format!("pub type {}", name));
            let lt = self.lifetime_for(&info, mode);
            self.print_generics(lt);
            self.push_str(" = ");
            self.print_ty(ty, mode);
            self.push_str(";\n");
        }
    }

    fn type_list(&mut self, id: TypeId, _name: &str, ty: &Type, docs: &Docs) {
        let info = self.info(id);
        for (name, mode) in self.modes_of(id) {
            let lt = self.lifetime_for(&info, mode);
            self.rustdoc(docs);
            self.push_str(&format!("pub type {}", name));
            self.print_generics(lt);
            self.push_str(" = ");
            self.print_list(ty, mode);
            self.push_str(";\n");
        }
    }

    fn print_result_ty(&mut self, results: &Results, mode: TypeMode) {
        match results {
            Results::Named(rs) => match rs.len() {
                0 => self.push_str("()"),
                1 => self.print_ty(&rs[0].1, mode),
                _ => {
                    self.push_str("(");
                    for (i, (_, ty)) in rs.iter().enumerate() {
                        if i > 0 {
                            self.push_str(", ")
                        }
                        self.print_ty(ty, mode)
                    }
                    self.push_str(")");
                }
            },
            Results::Anon(ty) => self.print_ty(ty, mode),
        }
    }

    fn special_case_trappable_error(
        &self,
        owner: TypeOwner,
        results: &Results,
    ) -> Option<(&'a Result_, String)> {
        // We fillin a special trappable error type in the case when a function has just one
        // result, which is itself a `result<a, e>`, and the `e` is *not* a primitive
        // (i.e. defined in std) type, and matches the typename given by the user.
        let mut i = results.iter_types();
        let id = match i.next()? {
            Type::Id(id) => id,
            _ => return None,
        };
        if i.next().is_some() {
            return None;
        }
        let result = match &self.resolve.types[*id].kind {
            TypeDefKind::Result(r) => r,
            _ => return None,
        };
        let error_typeid = match result.err? {
            Type::Id(id) => id,
            _ => return None,
        };
        self.trappable_error_types(owner)
            .find(|(wit_error_typeid, _)| error_typeid == *wit_error_typeid)
            .map(|(_, rust_errortype)| (result, rust_errortype))
    }

    fn generate_add_to_linker(&mut self, id: InterfaceId, name: &str) {
        let iface = &self.resolve.interfaces[id];
        let camel = name.to_upper_camel_case();
        let owner = TypeOwner::Interface(id);

        if self.gen.opts.async_ {
            uwriteln!(self.src, "#[wasmtime::component::__internal::async_trait]")
        }
        // Generate the `pub trait` which represents the host functionality for
        // this import.
        uwriteln!(self.src, "pub trait {camel}: Sized {{");
        for (_, func) in iface.functions.iter() {
            self.generate_function_trait_sig(owner, func);
        }
        uwriteln!(self.src, "}}");

        let where_clause = if self.gen.opts.async_ {
            format!("T: Send, U: {camel} + Send")
        } else {
            format!("U: {camel}")
        };
        uwriteln!(
            self.src,
            "
                pub fn add_to_linker<T, U>(
                    linker: &mut wasmtime::component::Linker<T>,
                    get: impl Fn(&mut T) -> &mut U + Send + Sync + Copy + 'static,
                ) -> anyhow::Result<()>
                    where {where_clause},
                {{
            "
        );
        uwriteln!(self.src, "let mut inst = linker.instance(\"{name}\")?;");
        for (_, func) in iface.functions.iter() {
            self.generate_add_function_to_linker(owner, func, "inst");
        }
        uwriteln!(self.src, "Ok(())");
        uwriteln!(self.src, "}}");
    }

    fn generate_add_function_to_linker(&mut self, owner: TypeOwner, func: &Function, linker: &str) {
        uwrite!(
            self.src,
            "{linker}.{}(\"{}\", ",
            if self.gen.opts.async_ {
                "func_wrap_async"
            } else {
                "func_wrap"
            },
            func.name
        );
        self.generate_guest_import_closure(owner, func);
        uwriteln!(self.src, ")?;")
    }

    fn generate_guest_import_closure(&mut self, owner: TypeOwner, func: &Function) {
        // Generate the closure that's passed to a `Linker`, the final piece of
        // codegen here.
        self.src
            .push_str("move |mut caller: wasmtime::StoreContextMut<'_, T>, (");
        for (i, _param) in func.params.iter().enumerate() {
            uwrite!(self.src, "arg{},", i);
        }
        self.src.push_str(") : (");
        for param in func.params.iter() {
            // Lift is required to be impled for this type, so we can't use
            // a borrowed type:
            self.print_ty(&param.1, TypeMode::Owned);
            self.src.push_str(", ");
        }
        self.src.push_str(") |");
        if self.gen.opts.async_ {
            self.src.push_str(" Box::new(async move { \n");
        } else {
            self.src.push_str(" { \n");
        }

        if self.gen.opts.tracing {
            self.src.push_str(&format!(
                "
                   let span = tracing::span!(
                       tracing::Level::TRACE,
                       \"wit-bindgen guest import\",
                       module = \"{}\",
                       function = \"{}\",
                   );
                   let _enter = span.enter();
               ",
                match owner {
                    TypeOwner::Interface(id) => self.resolve.interfaces[id]
                        .name
                        .as_deref()
                        .unwrap_or("<no module>"),
                    TypeOwner::World(id) => &self.resolve.worlds[id].name,
                    TypeOwner::None => "<no owner>",
                },
                func.name,
            ));
        }

        self.src.push_str("let host = get(caller.data_mut());\n");

        uwrite!(self.src, "let r = host.{}(", func.name.to_snake_case());
        for (i, _) in func.params.iter().enumerate() {
            uwrite!(self.src, "arg{},", i);
        }
        if self.gen.opts.async_ {
            uwrite!(self.src, ").await;\n");
        } else {
            uwrite!(self.src, ");\n");
        }

        if self
            .special_case_trappable_error(owner, &func.results)
            .is_some()
        {
            uwrite!(
                self.src,
                "match r {{
                    Ok(a) => Ok((Ok(a),)),
                    Err(e) => match e.downcast() {{
                        Ok(api_error) => Ok((Err(api_error),)),
                        Err(anyhow_error) => Err(anyhow_error),
                    }}
                }}"
            );
        } else if func.results.iter_types().len() == 1 {
            uwrite!(self.src, "Ok((r?,))\n");
        } else {
            uwrite!(self.src, "r\n");
        }

        if self.gen.opts.async_ {
            // Need to close Box::new and async block
            self.src.push_str("})");
        } else {
            self.src.push_str("}");
        }
    }

    fn generate_function_trait_sig(&mut self, owner: TypeOwner, func: &Function) {
        self.rustdoc(&func.docs);

        if self.gen.opts.async_ {
            self.push_str("async ");
        }
        self.push_str("fn ");
        self.push_str(&to_rust_ident(&func.name));
        self.push_str("(&mut self, ");
        for (name, param) in func.params.iter() {
            let name = to_rust_ident(name);
            self.push_str(&name);
            self.push_str(": ");
            self.print_ty(param, TypeMode::Owned);
            self.push_str(",");
        }
        self.push_str(")");
        self.push_str(" -> ");

        if let Some((r, error_typename)) = self.special_case_trappable_error(owner, &func.results) {
            // Functions which have a single result `result<ok,err>` get special
            // cased to use the host_wasmtime_rust::Error<err>, making it possible
            // for them to trap or use `?` to propogate their errors
            self.push_str("Result<");
            if let Some(ok) = r.ok {
                self.print_ty(&ok, TypeMode::Owned);
            } else {
                self.push_str("()");
            }
            self.push_str(",");
            self.push_str(&error_typename);
            self.push_str(">");
        } else {
            // All other functions get their return values wrapped in an anyhow::Result.
            // Returning the anyhow::Error case can be used to trap.
            self.push_str("anyhow::Result<");
            self.print_result_ty(&func.results, TypeMode::Owned);
            self.push_str(">");
        }

        self.push_str(";\n");
    }

    fn extract_typed_function(&mut self, func: &Function) -> (String, String) {
        let prev = mem::take(&mut self.src);
        let snake = func.name.to_snake_case();
        uwrite!(self.src, "*__exports.typed_func::<(");
        for (_, ty) in func.params.iter() {
            self.print_ty(ty, TypeMode::AllBorrowed("'_"));
            self.push_str(", ");
        }
        self.src.push_str("), (");
        for ty in func.results.iter_types() {
            self.print_ty(ty, TypeMode::Owned);
            self.push_str(", ");
        }
        self.src.push_str(")>(\"");
        self.src.push_str(&func.name);
        self.src.push_str("\")?.func()");

        let ret = (snake, mem::take(&mut self.src).to_string());
        self.src = prev;
        return ret;
    }

    fn define_rust_guest_export(&mut self, ns: Option<&str>, func: &Function) {
        let (async_, async__, await_) = if self.gen.opts.async_ {
            ("async", "_async", ".await")
        } else {
            ("", "", "")
        };

        self.rustdoc(&func.docs);
        uwrite!(
            self.src,
            "pub {async_} fn {}<S: wasmtime::AsContextMut>(&self, mut store: S, ",
            func.name.to_snake_case(),
        );
        for (i, param) in func.params.iter().enumerate() {
            uwrite!(self.src, "arg{}: ", i);
            self.print_ty(&param.1, TypeMode::AllBorrowed("'_"));
            self.push_str(",");
        }
        self.src.push_str(") -> anyhow::Result<");
        self.print_result_ty(&func.results, TypeMode::Owned);

        if self.gen.opts.async_ {
            self.src
                .push_str("> where <S as wasmtime::AsContext>::Data: Send {\n");
        } else {
            self.src.push_str("> {\n");
        }

        if self.gen.opts.tracing {
            self.src.push_str(&format!(
                "
                   let span = tracing::span!(
                       tracing::Level::TRACE,
                       \"wit-bindgen guest export\",
                       module = \"{}\",
                       function = \"{}\",
                   );
                   let _enter = span.enter();
               ",
                ns.unwrap_or("default"),
                func.name,
            ));
        }

        self.src.push_str("let callee = unsafe {\n");
        self.src.push_str("wasmtime::component::TypedFunc::<(");
        for (_, ty) in func.params.iter() {
            self.print_ty(ty, TypeMode::AllBorrowed("'_"));
            self.push_str(", ");
        }
        self.src.push_str("), (");
        for ty in func.results.iter_types() {
            self.print_ty(ty, TypeMode::Owned);
            self.push_str(", ");
        }
        uwriteln!(
            self.src,
            ")>::new_unchecked(self.{})",
            func.name.to_snake_case()
        );
        self.src.push_str("};\n");
        self.src.push_str("let (");
        for (i, _) in func.results.iter_types().enumerate() {
            uwrite!(self.src, "ret{},", i);
        }
        uwrite!(
            self.src,
            ") = callee.call{async__}(store.as_context_mut(), ("
        );
        for (i, _) in func.params.iter().enumerate() {
            uwrite!(self.src, "arg{}, ", i);
        }
        uwriteln!(self.src, ")){await_}?;");

        uwriteln!(
            self.src,
            "callee.post_return{async__}(store.as_context_mut()){await_}?;"
        );

        self.src.push_str("Ok(");
        if func.results.iter_types().len() == 1 {
            self.src.push_str("ret0");
        } else {
            self.src.push_str("(");
            for (i, _) in func.results.iter_types().enumerate() {
                uwrite!(self.src, "ret{},", i);
            }
            self.src.push_str(")");
        }
        self.src.push_str(")\n");

        // End function body
        self.src.push_str("}\n");
    }

    fn trappable_error_types(
        &self,
        owner: TypeOwner,
    ) -> impl Iterator<Item = (TypeId, String)> + '_ {
        let resolve = self.resolve;
        self.gen
            .opts
            .trappable_error_type
            .iter()
            .filter_map(move |trappable| {
                if let Some(name) = &trappable.wit_owner {
                    let owner_name = match owner {
                        TypeOwner::Interface(id) => resolve.interfaces[id].name.as_deref()?,
                        TypeOwner::World(id) => &resolve.worlds[id].name,
                        TypeOwner::None => return None,
                    };
                    if owner_name != name {
                        return None;
                    }
                }
                let id = match owner {
                    TypeOwner::Interface(id) => {
                        *resolve.interfaces[id].types.get(&trappable.wit_name)?
                    }
                    // TODO: right now worlds can't have types defined within
                    // them but that's just a temporary limitation of
                    // `wit-parser`. Once that's filled in this should be
                    // replaced with a type-lookup in the world.
                    TypeOwner::World(_id) => unimplemented!(),
                    TypeOwner::None => return None,
                };

                Some((id, trappable.rust_name.clone()))
            })
    }

    fn generate_trappable_error_types(&mut self, owner: TypeOwner) {
        for (wit_type, trappable_type) in self.trappable_error_types(owner).collect::<Vec<_>>() {
            let info = self.info(wit_type);
            if self.lifetime_for(&info, TypeMode::Owned).is_some() {
                panic!("wit error for {trappable_type} is not 'static")
            }
            let abi_type = self.param_name(wit_type);

            uwriteln!(
                self.src,
                "
                #[derive(Debug)]
                pub struct {trappable_type} {{
                    inner: anyhow::Error,
                }}
                impl std::fmt::Display for {trappable_type} {{
                    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {{
                        write!(f, \"{{}}\", self.inner)
                    }}
                }}
                impl std::error::Error for {trappable_type} {{
                    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {{
                        self.inner.source()
                    }}
                }}
                impl {trappable_type} {{
                    pub fn trap(inner: anyhow::Error) -> Self {{
                        Self {{ inner }}
                    }}
                    pub fn downcast(self) -> Result<{abi_type}, anyhow::Error> {{
                        self.inner.downcast()
                    }}
                    pub fn downcast_ref(&self) -> Option<&{abi_type}> {{
                        self.inner.downcast_ref()
                    }}
                    pub fn context(self, s: impl Into<String>) -> Self {{
                        Self {{ inner: self.inner.context(s.into()) }}
                    }}
                }}
                impl From<{abi_type}> for {trappable_type} {{
                    fn from(abi: {abi_type}) -> {trappable_type} {{
                        {trappable_type} {{ inner: anyhow::Error::from(abi) }}
                    }}
                }}
           "
            );
        }
    }

    fn rustdoc(&mut self, docs: &Docs) {
        let docs = match &docs.contents {
            Some(docs) => docs,
            None => return,
        };
        for line in docs.trim().lines() {
            self.push_str("/// ");
            self.push_str(line);
            self.push_str("\n");
        }
    }
}

impl<'a> RustGenerator<'a> for InterfaceGenerator<'a> {
    fn resolve(&self) -> &'a Resolve {
        self.resolve
    }

    fn current_interface(&self) -> Option<InterfaceId> {
        self.current_interface
    }

    fn default_param_mode(&self) -> TypeMode {
        self.default_param_mode
    }

    fn push_str(&mut self, s: &str) {
        self.src.push_str(s);
    }

    fn info(&self, ty: TypeId) -> TypeInfo {
        self.gen.types.get(ty)
    }
}
