use crate::context::Context;
use anyhow::Result;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use wasmtime_environ::{
    ir,
    settings::{self, Configurable},
};
use wasmtime_jit::{CompilationStrategy, Features};

// Runtime Environment

// Configuration

/// Global configuration options used to create an [`Engine`] and customize its
/// behavior.
///
/// This structure exposed a builder-like interface and is primarily consumed by
/// [`Engine::new()`]
#[derive(Clone)]
pub struct Config {
    pub(crate) flags: settings::Builder,
    pub(crate) features: Features,
    pub(crate) debug_info: bool,
    pub(crate) strategy: CompilationStrategy,
}

impl Config {
    /// Creates a new configuration object with the default configuration
    /// specified.
    pub fn new() -> Config {
        let mut flags = settings::builder();

        // There are two possible traps for division, and this way
        // we get the proper one if code traps.
        flags
            .enable("avoid_div_traps")
            .expect("should be valid flag");

        Config {
            debug_info: false,
            features: Default::default(),
            flags,
            strategy: CompilationStrategy::Auto,
        }
    }

    /// Configures whether DWARF debug information will be emitted during
    /// compilation.
    ///
    /// By default this option is `false`.
    pub fn debug_info(&mut self, enable: bool) -> &mut Self {
        self.debug_info = enable;
        self
    }

    /// Configures whether the WebAssembly threads proposal will be enabled for
    /// compilation.
    ///
    /// The [WebAssembly threads proposal][threads] is not currently fully
    /// standardized and is undergoing development. Additionally the support in
    /// wasmtime itself is still being worked on. Support for this feature can
    /// be enabled through this method for appropriate wasm modules.
    ///
    /// This feature gates items such as shared memories and atomic
    /// instructions.
    ///
    /// This is `false` by default.
    ///
    /// [threads]: https://github.com/webassembly/threads
    pub fn wasm_threads(&mut self, enable: bool) -> &mut Self {
        self.features.threads = enable;
        self
    }

    /// Configures whether the WebAssembly reference types proposal will be
    /// enabled for compilation.
    ///
    /// The [WebAssembly reference types proposal][proposal] is not currently
    /// fully standardized and is undergoing development. Additionally the
    /// support in wasmtime itself is still being worked on. Support for this
    /// feature can be enabled through this method for appropriate wasm
    /// modules.
    ///
    /// This feature gates items such as the `anyref` type and multiple tables
    /// being in a module.
    ///
    /// This is `false` by default.
    ///
    /// [proposal]: https://github.com/webassembly/reference-types
    pub fn wasm_reference_types(&mut self, enable: bool) -> &mut Self {
        self.features.reference_types = enable;
        self
    }

    /// Configures whether the WebAssembly SIMD proposal will be
    /// enabled for compilation.
    ///
    /// The [WebAssembly SIMD proposal][proposal] is not currently
    /// fully standardized and is undergoing development. Additionally the
    /// support in wasmtime itself is still being worked on. Support for this
    /// feature can be enabled through this method for appropriate wasm
    /// modules.
    ///
    /// This feature gates items such as the `v128` type and all of its
    /// operators being in a module.
    ///
    /// This is `false` by default.
    ///
    /// [proposal]: https://github.com/webassembly/simd
    pub fn wasm_simd(&mut self, enable: bool) -> &mut Self {
        self.features.simd = enable;
        let val = if enable { "true" } else { "false" };
        self.flags
            .set("enable_simd", val)
            .expect("should be valid flag");
        self
    }

    /// Configures whether the WebAssembly bulk memory operations proposal will
    /// be enabled for compilation.
    ///
    /// The [WebAssembly bulk memory operations proposal][proposal] is not
    /// currently fully standardized and is undergoing development.
    /// Additionally the support in wasmtime itself is still being worked on.
    /// Support for this feature can be enabled through this method for
    /// appropriate wasm modules.
    ///
    /// This feature gates items such as the `memory.copy` instruction, passive
    /// data/table segments, etc, being in a module.
    ///
    /// This is `false` by default.
    ///
    /// [proposal]: https://github.com/webassembly/bulk-memory-operations
    pub fn wasm_bulk_memory(&mut self, enable: bool) -> &mut Self {
        self.features.bulk_memory = enable;
        self
    }

    /// Configures whether the WebAssembly multi-value proposal will
    /// be enabled for compilation.
    ///
    /// The [WebAssembly multi-value proposal][proposal] is not
    /// currently fully standardized and is undergoing development.
    /// Additionally the support in wasmtime itself is still being worked on.
    /// Support for this feature can be enabled through this method for
    /// appropriate wasm modules.
    ///
    /// This feature gates functions and blocks returning multiple values in a
    /// module, for example.
    ///
    /// This is `false` by default.
    ///
    /// [proposal]: https://github.com/webassembly/multi-value
    pub fn wasm_multi_value(&mut self, enable: bool) -> &mut Self {
        self.features.multi_value = enable;
        self
    }

    /// Configures which compilation strategy will be used for wasm modules.
    ///
    /// This method can be used to configure which compiler is used for wasm
    /// modules, and for more documentation consult the [`Strategy`] enumeration
    /// and its documentation.
    ///
    /// The default value for this is `Strategy::Auto`.
    ///
    /// # Errors
    ///
    /// Some compilation strategies require compile-time options of `wasmtime`
    /// itself to be set, but if they're not set and the strategy is specified
    /// here then an error will be returned.
    pub fn strategy(&mut self, strategy: Strategy) -> Result<&mut Self> {
        self.strategy = match strategy {
            Strategy::Auto => CompilationStrategy::Auto,
            Strategy::Cranelift => CompilationStrategy::Cranelift,
            #[cfg(feature = "lightbeam")]
            Strategy::Lightbeam => CompilationStrategy::Lightbeam,
            #[cfg(not(feature = "lightbeam"))]
            Strategy::Lightbeam => {
                anyhow::bail!("lightbeam compilation strategy wasn't enabled at compile time");
            }
        };
        Ok(self)
    }

    /// Configures whether the debug verifier of Cranelift is enabled or not.
    ///
    /// When Cranelift is used as a code generation backend this will configure
    /// it to have the `enable_verifier` flag which will enable a number of debug
    /// checks inside of Cranelift. This is largely only useful for the
    /// developers of wasmtime itself.
    ///
    /// The default value for this is `false`
    pub fn cranelift_debug_verifier(&mut self, enable: bool) -> &mut Self {
        let val = if enable { "true" } else { "false" };
        self.flags
            .set("enable_verifier", val)
            .expect("should be valid flag");
        self
    }

    /// Configures the Cranelift code generator optimization level.
    ///
    /// When the Cranelift code generator is used you can configure the
    /// optimization level used for generated code in a few various ways. For
    /// more information see the documentation of [`OptLevel`].
    ///
    /// The default value for this is `OptLevel::None`.
    pub fn cranelift_opt_level(&mut self, level: OptLevel) -> &mut Self {
        let val = match level {
            OptLevel::None => "none",
            OptLevel::Speed => "speed",
            OptLevel::SpeedAndSize => "speed_and_size",
        };
        self.flags
            .set("opt_level", val)
            .expect("should be valid flag");
        self
    }
}

impl Default for Config {
    fn default() -> Config {
        Config::new()
    }
}

/// Possible Compilation strategies for a wasm module.
///
/// This is used as an argument to the [`Config::strategy`] method.
#[non_exhaustive]
#[derive(Clone, Debug)]
pub enum Strategy {
    /// An indicator that the compilation strategy should be automatically
    /// selected.
    ///
    /// This is generally what you want for most projects and indicates that the
    /// `wasmtime` crate itself should make the decision about what the best
    /// code generator for a wasm module is.
    ///
    /// Currently this always defaults to Cranelift, but the default value will
    /// change over time.
    Auto,

    /// Currently the default backend, Cranelift aims to be a reasonably fast
    /// code generator which generates high quality machine code.
    Cranelift,

    /// A single-pass code generator that is faster than Cranelift but doesn't
    /// produce as high-quality code.
    ///
    /// To successfully pass this argument to [`Config::strategy`] the
    /// `lightbeam` feature of this crate must be enabled.
    Lightbeam,
}

/// Possible optimization levels for the Cranelift codegen backend.
#[non_exhaustive]
#[derive(Clone, Debug)]
pub enum OptLevel {
    /// No optimizations performed, minimizes compilation time by disabling most
    /// optimizations.
    None,
    /// Generates the fastest possible code, but may take longer.
    Speed,
    /// Similar to `speed`, but also performs transformations aimed at reducing
    /// code size.
    SpeedAndSize,
}

// Engine

/// An `Engine` which is a global context for compilation and management of wasm
/// modules.
///
/// An engine can be safely shared across threads and is a cheap cloneable
/// handle to the actual engine. The engine itself will be deallocate once all
/// references to it have gone away.
///
/// Engines store global configuration preferences such as compilation settings,
/// enabled features, etc. You'll likely only need at most one of these for a
/// program.
///
/// ## Engines and `Clone`
///
/// Using `clone` on an `Engine` is a cheap operation. It will not create an
/// entirely new engine, but rather just a new reference to the existing engine.
/// In other words it's a shallow copy, not a deep copy.
///
/// ## Engines and `Default`
///
/// You can create an engine with default configuration settings using
/// `Engine::default()`. Be sure to consult the documentation of [`Config`] for
/// default settings.
#[derive(Default, Clone)]
pub struct Engine {
    pub(crate) config: Arc<Config>,
}

impl Engine {
    /// Creates a new [`Engine`] with the specified compilation and
    /// configuration settings.
    pub fn new(config: &Config) -> Engine {
        Engine {
            config: Arc::new(config.clone()),
        }
    }
}

// Store

/// A `Store` is a shared cache of information between WebAssembly modules.
///
/// Each `Module` is compiled into a `Store` and a `Store` is associated with an
/// [`Engine`]. You'll use a `Store` to attach to a number of global items in
/// the production of various items for wasm modules.
///
/// # Stores and `Clone`
///
/// Using `clone` on a `Store` is a cheap operation. It will not create an
/// entirely new store, but rather just a new reference to the existing object.
/// In other words it's a shallow copy, not a deep copy.
///
/// ## Stores and `Default`
///
/// You can create a store with default configuration settings using
/// `Store::default()`. This will create a brand new [`Engine`] with default
/// ocnfiguration (see [`Config`] for more information).
#[derive(Clone)]
pub struct Store {
    // FIXME(#777) should be `Arc` and this type should be thread-safe
    inner: Rc<StoreInner>,
}

struct StoreInner {
    engine: Engine,
    context: Context,
    global_exports: Rc<RefCell<HashMap<String, Option<wasmtime_runtime::Export>>>>,
    signature_cache: RefCell<HashMap<wasmtime_runtime::VMSharedSignatureIndex, ir::Signature>>,
}

impl Store {
    /// Creates a new store to be associated with the given [`Engine`].
    pub fn new(engine: &Engine) -> Store {
        Store {
            inner: Rc::new(StoreInner {
                engine: engine.clone(),
                context: Context::new(&engine.config),
                global_exports: Rc::new(RefCell::new(HashMap::new())),
                signature_cache: RefCell::new(HashMap::new()),
            }),
        }
    }

    /// Returns the [`Engine`] that this store is associated with.
    pub fn engine(&self) -> &Engine {
        &self.inner.engine
    }

    pub(crate) fn context(&self) -> &Context {
        &self.inner.context
    }

    // Specific to wasmtime: hack to pass memory around to wasi
    #[doc(hidden)]
    pub fn global_exports(
        &self,
    ) -> &Rc<RefCell<HashMap<String, Option<wasmtime_runtime::Export>>>> {
        &self.inner.global_exports
    }

    pub(crate) fn register_wasmtime_signature(
        &self,
        signature: &ir::Signature,
    ) -> wasmtime_runtime::VMSharedSignatureIndex {
        use std::collections::hash_map::Entry;
        let index = self.context().compiler().signatures().register(signature);
        match self.inner.signature_cache.borrow_mut().entry(index) {
            Entry::Vacant(v) => {
                v.insert(signature.clone());
            }
            Entry::Occupied(_) => (),
        }
        index
    }

    pub(crate) fn lookup_wasmtime_signature(
        &self,
        type_index: wasmtime_runtime::VMSharedSignatureIndex,
    ) -> Option<ir::Signature> {
        self.inner
            .signature_cache
            .borrow()
            .get(&type_index)
            .cloned()
    }
}

impl Default for Store {
    fn default() -> Store {
        Store::new(&Engine::default())
    }
}

fn _assert_send_sync() {
    fn _assert<T: Send + Sync>() {}
    _assert::<Engine>();
    _assert::<Config>();
}
