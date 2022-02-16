//! Test case generators.
//!
//! Test case generators take raw, unstructured input from a fuzzer
//! (e.g. libFuzzer) and translate that into a structured test case (e.g. a
//! valid Wasm binary).
//!
//! These are generally implementations of the `Arbitrary` trait, or some
//! wrapper over an external tool, such that the wrapper implements the
//! `Arbitrary` trait for the wrapped external tool.

pub mod api;
pub mod table_ops;

use crate::oracles::{StoreLimits, Timeout};
use anyhow::Result;
use arbitrary::{Arbitrary, Unstructured};
use std::sync::Arc;
use std::time::Duration;
use wasm_smith::SwarmConfig;
use wasmtime::{Engine, LinearMemory, MemoryCreator, MemoryType, Module, Store};

#[derive(Arbitrary, Clone, Debug, PartialEq, Eq, Hash)]
enum OptLevel {
    None,
    Speed,
    SpeedAndSize,
}

impl OptLevel {
    fn to_wasmtime(&self) -> wasmtime::OptLevel {
        match self {
            OptLevel::None => wasmtime::OptLevel::None,
            OptLevel::Speed => wasmtime::OptLevel::Speed,
            OptLevel::SpeedAndSize => wasmtime::OptLevel::SpeedAndSize,
        }
    }
}

/// Configuration for `wasmtime::PoolingAllocationStrategy`.
#[derive(Arbitrary, Clone, Debug, PartialEq, Eq, Hash)]
pub enum PoolingAllocationStrategy {
    /// Use next available instance slot.
    NextAvailable,
    /// Use random instance slot.
    Random,
    /// Use an affinity-based strategy.
    ReuseAffinity,
}

impl PoolingAllocationStrategy {
    fn to_wasmtime(&self) -> wasmtime::PoolingAllocationStrategy {
        match self {
            PoolingAllocationStrategy::NextAvailable => {
                wasmtime::PoolingAllocationStrategy::NextAvailable
            }
            PoolingAllocationStrategy::Random => wasmtime::PoolingAllocationStrategy::Random,
            PoolingAllocationStrategy::ReuseAffinity => {
                wasmtime::PoolingAllocationStrategy::ReuseAffinity
            }
        }
    }
}

/// Configuration for `wasmtime::ModuleLimits`.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct ModuleLimits {
    imported_functions: u32,
    imported_tables: u32,
    imported_memories: u32,
    imported_globals: u32,
    types: u32,
    functions: u32,
    tables: u32,
    memories: u32,
    /// The maximum number of globals that can be defined in a module.
    pub globals: u32,
    table_elements: u32,
    memory_pages: u64,
}

impl ModuleLimits {
    fn to_wasmtime(&self) -> wasmtime::ModuleLimits {
        wasmtime::ModuleLimits {
            imported_functions: self.imported_functions,
            imported_tables: self.imported_tables,
            imported_memories: self.imported_memories,
            imported_globals: self.imported_globals,
            types: self.types,
            functions: self.functions,
            tables: self.tables,
            memories: self.memories,
            globals: self.globals,
            table_elements: self.table_elements,
            memory_pages: self.memory_pages,
        }
    }
}

impl<'a> Arbitrary<'a> for ModuleLimits {
    fn arbitrary(u: &mut Unstructured<'a>) -> arbitrary::Result<Self> {
        const MAX_IMPORTS: u32 = 1000;
        const MAX_TYPES: u32 = 1000;
        const MAX_FUNCTIONS: u32 = 1000;
        const MAX_TABLES: u32 = 10;
        const MAX_MEMORIES: u32 = 10;
        const MAX_GLOBALS: u32 = 1000;
        const MAX_ELEMENTS: u32 = 1000;
        const MAX_MEMORY_PAGES: u64 = 160; // 10 MiB

        Ok(Self {
            imported_functions: u.int_in_range(0..=MAX_IMPORTS)?,
            imported_tables: u.int_in_range(0..=MAX_IMPORTS)?,
            imported_memories: u.int_in_range(0..=MAX_IMPORTS)?,
            imported_globals: u.int_in_range(0..=MAX_IMPORTS)?,
            types: u.int_in_range(0..=MAX_TYPES)?,
            functions: u.int_in_range(0..=MAX_FUNCTIONS)?,
            tables: u.int_in_range(0..=MAX_TABLES)?,
            memories: u.int_in_range(0..=MAX_MEMORIES)?,
            globals: u.int_in_range(0..=MAX_GLOBALS)?,
            table_elements: u.int_in_range(0..=MAX_ELEMENTS)?,
            memory_pages: u.int_in_range(0..=MAX_MEMORY_PAGES)?,
        })
    }
}

/// Configuration for `wasmtime::PoolingAllocationStrategy`.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct InstanceLimits {
    /// The maximum number of instances that can be instantiated in the pool at a time.
    pub count: u32,
}

impl InstanceLimits {
    fn to_wasmtime(&self) -> wasmtime::InstanceLimits {
        wasmtime::InstanceLimits { count: self.count }
    }
}

impl<'a> Arbitrary<'a> for InstanceLimits {
    fn arbitrary(u: &mut Unstructured<'a>) -> arbitrary::Result<Self> {
        const MAX_COUNT: u32 = 100;

        Ok(Self {
            count: u.int_in_range(1..=MAX_COUNT)?,
        })
    }
}

/// Configuration for `wasmtime::InstanceAllocationStrategy`.
#[derive(Arbitrary, Clone, Debug, Eq, PartialEq, Hash)]
pub enum InstanceAllocationStrategy {
    /// Use the on-demand instance allocation strategy.
    OnDemand,
    /// Use the pooling instance allocation strategy.
    Pooling {
        /// The pooling strategy to use.
        strategy: PoolingAllocationStrategy,
        /// The module limits.
        module_limits: ModuleLimits,
        /// The instance limits.
        instance_limits: InstanceLimits,
    },
}

impl InstanceAllocationStrategy {
    fn to_wasmtime(&self) -> wasmtime::InstanceAllocationStrategy {
        match self {
            InstanceAllocationStrategy::OnDemand => wasmtime::InstanceAllocationStrategy::OnDemand,
            InstanceAllocationStrategy::Pooling {
                strategy,
                module_limits,
                instance_limits,
            } => wasmtime::InstanceAllocationStrategy::Pooling {
                strategy: strategy.to_wasmtime(),
                module_limits: module_limits.to_wasmtime(),
                instance_limits: instance_limits.to_wasmtime(),
            },
        }
    }
}

/// Configuration for `wasmtime::Config` and generated modules for a session of
/// fuzzing.
///
/// This configuration guides what modules are generated, how wasmtime
/// configuration is generated, and is typically itself generated through a call
/// to `Arbitrary` which allows for a form of "swarm testing".
#[derive(Debug, Clone)]
pub struct Config {
    /// Configuration related to the `wasmtime::Config`.
    pub wasmtime: WasmtimeConfig,
    /// Configuration related to generated modules.
    pub module_config: ModuleConfig,
}

impl<'a> Arbitrary<'a> for Config {
    fn arbitrary(u: &mut Unstructured<'a>) -> arbitrary::Result<Self> {
        let mut config = Self {
            wasmtime: u.arbitrary()?,
            module_config: u.arbitrary()?,
        };

        // If using the pooling allocator, constrain the memory and module configurations
        // to the module limits.
        if let InstanceAllocationStrategy::Pooling {
            module_limits: limits,
            ..
        } = &config.wasmtime.strategy
        {
            // Force the use of a normal memory config when using the pooling allocator and
            // limit the static memory maximum to be the same as the pooling allocator's memory
            // page limit.
            config.wasmtime.memory_config = match config.wasmtime.memory_config {
                MemoryConfig::Normal(mut config) => {
                    config.static_memory_maximum_size = Some(limits.memory_pages * 0x10000);
                    MemoryConfig::Normal(config)
                }
                MemoryConfig::CustomUnaligned => {
                    let mut config: NormalMemoryConfig = u.arbitrary()?;
                    config.static_memory_maximum_size = Some(limits.memory_pages * 0x10000);
                    MemoryConfig::Normal(config)
                }
            };

            let cfg = &mut config.module_config.config;
            cfg.max_imports = limits.imported_functions.min(
                limits
                    .imported_globals
                    .min(limits.imported_memories.min(limits.imported_tables)),
            ) as usize;
            cfg.max_types = limits.types as usize;
            cfg.max_funcs = limits.functions as usize;
            cfg.max_globals = limits.globals as usize;
            cfg.max_memories = limits.memories as usize;
            cfg.max_tables = limits.tables as usize;
            cfg.max_memory_pages = limits.memory_pages;

            // Force no aliases in any generated modules as they might count against the
            // import limits above.
            cfg.max_aliases = 0;
        }

        Ok(config)
    }
}

/// Configuration related to `wasmtime::Config` and the various settings which
/// can be tweaked from within.
#[derive(Arbitrary, Clone, Debug, Eq, Hash, PartialEq)]
pub struct WasmtimeConfig {
    opt_level: OptLevel,
    debug_info: bool,
    canonicalize_nans: bool,
    interruptable: bool,
    pub(crate) consume_fuel: bool,
    /// The Wasmtime memory configuration to use.
    pub memory_config: MemoryConfig,
    force_jump_veneers: bool,
    memfd: bool,
    use_precompiled_cwasm: bool,
    /// Configuration for the instance allocation strategy to use.
    pub strategy: InstanceAllocationStrategy,
    codegen: CodegenSettings,
}

/// Configuration for linear memories in Wasmtime.
#[derive(Arbitrary, Clone, Debug, Eq, Hash, PartialEq)]
pub enum MemoryConfig {
    /// Configuration for linear memories which correspond to normal
    /// configuration settings in `wasmtime` itself. This will tweak various
    /// parameters about static/dynamic memories.
    Normal(NormalMemoryConfig),

    /// Configuration to force use of a linear memory that's unaligned at its
    /// base address to force all wasm addresses to be unaligned at the hardware
    /// level, even if the wasm itself correctly aligns everything internally.
    CustomUnaligned,
}

/// Represents a normal memory configuration for Wasmtime with the given
/// static and dynamic memory sizes.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct NormalMemoryConfig {
    static_memory_maximum_size: Option<u64>,
    static_memory_guard_size: Option<u64>,
    dynamic_memory_guard_size: Option<u64>,
    guard_before_linear_memory: bool,
}

impl<'a> Arbitrary<'a> for NormalMemoryConfig {
    fn arbitrary(u: &mut Unstructured<'a>) -> arbitrary::Result<Self> {
        // This attempts to limit memory and guard sizes to 32-bit ranges so
        // we don't exhaust a 64-bit address space easily.
        Ok(Self {
            static_memory_maximum_size: <Option<u32> as Arbitrary>::arbitrary(u)?.map(Into::into),
            static_memory_guard_size: <Option<u32> as Arbitrary>::arbitrary(u)?.map(Into::into),
            dynamic_memory_guard_size: <Option<u32> as Arbitrary>::arbitrary(u)?.map(Into::into),
            guard_before_linear_memory: u.arbitrary()?,
        })
    }
}

impl Config {
    /// Indicates that this configuration is being used for differential
    /// execution so only a single function should be generated since that's all
    /// that's going to be exercised.
    pub fn set_differential_config(&mut self) {
        let config = &mut self.module_config.config;

        config.allow_start_export = false;
        // Make sure there's a type available for the function.
        config.min_types = 1;
        config.max_types = 1;

        // Generate one and only one function
        config.min_funcs = 1;
        config.max_funcs = 1;

        // Give the function a memory, but keep it small
        config.min_memories = 1;
        config.max_memories = 1;
        config.max_memory_pages = 1;
        config.memory_max_size_required = true;

        // Don't allow any imports
        config.max_imports = 0;

        // Try to get the function and the memory exported
        config.min_exports = 2;
        config.max_exports = 4;

        // NaN is canonicalized at the wasm level for differential fuzzing so we
        // can paper over NaN differences between engines.
        config.canonicalize_nans = true;

        // When diffing against a non-wasmtime engine then disable wasm
        // features to get selectively re-enabled against each differential
        // engine.
        config.bulk_memory_enabled = false;
        config.reference_types_enabled = false;
        config.simd_enabled = false;
        config.memory64_enabled = false;

        // If using the pooling allocator, update the module limits too
        if let InstanceAllocationStrategy::Pooling {
            module_limits: limits,
            ..
        } = &mut self.wasmtime.strategy
        {
            // No imports
            limits.imported_functions = 0;
            limits.imported_tables = 0;
            limits.imported_memories = 0;
            limits.imported_globals = 0;

            // One type, one function, and one single-page memory
            limits.types = 1;
            limits.functions = 1;
            limits.memories = 1;
            limits.memory_pages = 1;

            match &mut self.wasmtime.memory_config {
                MemoryConfig::Normal(config) => {
                    config.static_memory_maximum_size = Some(limits.memory_pages * 0x10000);
                }
                MemoryConfig::CustomUnaligned => unreachable!(), // Arbitrary impl for `Config` should have prevented this
            }
        }
    }

    /// Uses this configuration and the supplied source of data to generate
    /// a wasm module.
    ///
    /// If a `default_fuel` is provided, the resulting module will be configured
    /// to ensure termination; as doing so will add an additional global to the module,
    /// the pooling allocator, if configured, will also have its globals limit updated.
    pub fn generate(
        &mut self,
        input: &mut Unstructured<'_>,
        default_fuel: Option<u32>,
    ) -> arbitrary::Result<wasm_smith::Module> {
        let mut module = wasm_smith::Module::new(self.module_config.config.clone(), input)?;

        if let Some(default_fuel) = default_fuel {
            module.ensure_termination(default_fuel);

            // Bump the allowed global count by 1
            if let InstanceAllocationStrategy::Pooling { module_limits, .. } =
                &mut self.wasmtime.strategy
            {
                module_limits.globals += 1;
            }
        }

        Ok(module)
    }

    /// Indicates that this configuration should be spec-test-compliant,
    /// disabling various features the spec tests assert are disabled.
    pub fn set_spectest_compliant(&mut self) {
        let config = &mut self.module_config.config;
        config.memory64_enabled = false;
        config.simd_enabled = false;
        config.bulk_memory_enabled = true;
        config.reference_types_enabled = true;
        config.max_memories = 1;

        if let InstanceAllocationStrategy::Pooling { module_limits, .. } =
            &mut self.wasmtime.strategy
        {
            module_limits.memories = 1;
        }
    }

    /// Converts this to a `wasmtime::Config` object
    pub fn to_wasmtime(&self) -> wasmtime::Config {
        crate::init_fuzzing();

        let mut cfg = wasmtime::Config::new();
        cfg.wasm_bulk_memory(true)
            .wasm_reference_types(true)
            .wasm_module_linking(self.module_config.config.module_linking_enabled)
            .wasm_multi_memory(self.module_config.config.max_memories > 1)
            .wasm_simd(self.module_config.config.simd_enabled)
            .wasm_memory64(self.module_config.config.memory64_enabled)
            .cranelift_nan_canonicalization(self.wasmtime.canonicalize_nans)
            .cranelift_opt_level(self.wasmtime.opt_level.to_wasmtime())
            .interruptable(self.wasmtime.interruptable)
            .consume_fuel(self.wasmtime.consume_fuel)
            .memfd(self.wasmtime.memfd)
            .allocation_strategy(self.wasmtime.strategy.to_wasmtime());

        self.wasmtime.codegen.configure(&mut cfg);

        // If the wasm-smith-generated module use nan canonicalization then we
        // don't need to enable it, but if it doesn't enable it already then we
        // enable this codegen option.
        cfg.cranelift_nan_canonicalization(!self.module_config.config.canonicalize_nans);

        // Enabling the verifier will at-least-double compilation time, which
        // with a 20-30x slowdown in fuzzing can cause issues related to
        // timeouts. If generated modules can have more than a small handful of
        // functions then disable the verifier when fuzzing to try to lessen the
        // impact of timeouts.
        if self.module_config.config.max_funcs > 10 {
            cfg.cranelift_debug_verifier(false);
        }

        if self.wasmtime.force_jump_veneers {
            unsafe {
                cfg.cranelift_flag_set("wasmtime_linkopt_force_jump_veneer", "true")
                    .unwrap();
            }
        }

        match &self.wasmtime.memory_config {
            MemoryConfig::Normal(memory_config) => {
                cfg.static_memory_maximum_size(
                    memory_config.static_memory_maximum_size.unwrap_or(0),
                )
                .static_memory_guard_size(memory_config.static_memory_guard_size.unwrap_or(0))
                .dynamic_memory_guard_size(memory_config.dynamic_memory_guard_size.unwrap_or(0))
                .guard_before_linear_memory(memory_config.guard_before_linear_memory);
            }
            MemoryConfig::CustomUnaligned => {
                cfg.with_host_memory(Arc::new(UnalignedMemoryCreator))
                    .static_memory_maximum_size(0)
                    .dynamic_memory_guard_size(0)
                    .static_memory_guard_size(0)
                    .guard_before_linear_memory(false);
            }
        }

        return cfg;
    }

    /// Convenience function for generating a `Store<T>` using this
    /// configuration.
    pub fn to_store(&self) -> Store<StoreLimits> {
        let engine = Engine::new(&self.to_wasmtime()).unwrap();
        let mut store = Store::new(&engine, StoreLimits::new());
        self.configure_store(&mut store);
        store
    }

    /// Configures a store based on this configuration.
    pub fn configure_store(&self, store: &mut Store<StoreLimits>) {
        store.limiter(|s| s as &mut dyn wasmtime::ResourceLimiter);
        if self.wasmtime.consume_fuel {
            store.add_fuel(u64::max_value()).unwrap();
        }
    }

    /// Generates an arbitrary method of timing out an instance, ensuring that
    /// this configuration supports the returned timeout.
    pub fn generate_timeout(&mut self, u: &mut Unstructured<'_>) -> arbitrary::Result<Timeout> {
        if u.arbitrary()? {
            self.wasmtime.interruptable = true;
            Ok(Timeout::Time(Duration::from_secs(20)))
        } else {
            self.wasmtime.consume_fuel = true;
            Ok(Timeout::Fuel(100_000))
        }
    }

    /// Compiles the `wasm` within the `engine` provided.
    ///
    /// This notably will use `Module::{serialize,deserialize_file}` to
    /// round-trip if configured in the fuzzer.
    pub fn compile(&self, engine: &Engine, wasm: &[u8]) -> Result<Module> {
        // Propagate this error in case the caller wants to handle
        // valid-vs-invalid wasm.
        let module = Module::new(engine, wasm)?;
        if !self.wasmtime.use_precompiled_cwasm {
            return Ok(module);
        }

        // Don't propagate these errors to prevent them from accidentally being
        // interpreted as invalid wasm, these should never fail on a
        // well-behaved host system.
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), module.serialize().unwrap()).unwrap();
        unsafe { Ok(Module::deserialize_file(engine, file.path()).unwrap()) }
    }
}

struct UnalignedMemoryCreator;

unsafe impl MemoryCreator for UnalignedMemoryCreator {
    fn new_memory(
        &self,
        _ty: MemoryType,
        minimum: usize,
        maximum: Option<usize>,
        reserved_size_in_bytes: Option<usize>,
        guard_size_in_bytes: usize,
    ) -> Result<Box<dyn LinearMemory>, String> {
        assert_eq!(guard_size_in_bytes, 0);
        assert!(reserved_size_in_bytes.is_none() || reserved_size_in_bytes == Some(0));
        Ok(Box::new(UnalignedMemory {
            src: vec![0; minimum + 1],
            maximum,
        }))
    }
}

/// A custom "linear memory allocator" for wasm which only works with the
/// "dynamic" mode of configuration where wasm always does explicit bounds
/// checks.
///
/// This memory attempts to always use unaligned host addresses for the base
/// address of linear memory with wasm. This means that all jit loads/stores
/// should be unaligned, which is a "big hammer way" of testing that all our JIT
/// code works with unaligned addresses since alignment is not required for
/// correctness in wasm itself.
struct UnalignedMemory {
    /// This memory is always one byte larger than the actual size of linear
    /// memory.
    src: Vec<u8>,
    maximum: Option<usize>,
}

unsafe impl LinearMemory for UnalignedMemory {
    fn byte_size(&self) -> usize {
        // Chop off the extra byte reserved for the true byte size of this
        // linear memory.
        self.src.len() - 1
    }

    fn maximum_byte_size(&self) -> Option<usize> {
        self.maximum
    }

    fn grow_to(&mut self, new_size: usize) -> Result<()> {
        // Make sure to allocate an extra byte for our "unalignment"
        self.src.resize(new_size + 1, 0);
        Ok(())
    }

    fn as_ptr(&self) -> *mut u8 {
        // Return our allocated memory, offset by one, so that the base address
        // of memory is always unaligned.
        self.src[1..].as_ptr() as *mut _
    }
}

include!(concat!(env!("OUT_DIR"), "/spectests.rs"));

/// A spec test from the upstream wast testsuite, arbitrarily chosen from the
/// list of known spec tests.
#[derive(Debug)]
pub struct SpecTest {
    /// The filename of the spec test
    pub file: &'static str,
    /// The `*.wast` contents of the spec test
    pub contents: &'static str,
}

impl<'a> Arbitrary<'a> for SpecTest {
    fn arbitrary(u: &mut Unstructured<'a>) -> arbitrary::Result<Self> {
        // NB: this does get a uniform value in the provided range.
        let i = u.int_in_range(0..=FILES.len() - 1)?;
        let (file, contents) = FILES[i];
        Ok(SpecTest { file, contents })
    }

    fn size_hint(_depth: usize) -> (usize, Option<usize>) {
        (1, Some(std::mem::size_of::<usize>()))
    }
}

/// Default module-level configuration for fuzzing Wasmtime.
///
/// Internally this uses `wasm-smith`'s own `SwarmConfig` but we further refine
/// the defaults here as well.
#[derive(Debug, Clone)]
pub struct ModuleConfig {
    #[allow(missing_docs)]
    pub config: SwarmConfig,
}

impl<'a> Arbitrary<'a> for ModuleConfig {
    fn arbitrary(u: &mut Unstructured<'a>) -> arbitrary::Result<ModuleConfig> {
        let mut config = SwarmConfig::arbitrary(u)?;

        // Allow multi-memory by default.
        config.max_memories = config.max_memories.max(2);

        // Allow multi-table by default.
        config.max_tables = config.max_tables.max(4);

        // Allow enabling some various wasm proposals by default.
        config.bulk_memory_enabled = u.arbitrary()?;
        config.reference_types_enabled = u.arbitrary()?;
        config.simd_enabled = u.arbitrary()?;
        config.memory64_enabled = u.arbitrary()?;

        Ok(ModuleConfig { config })
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum CodegenSettings {
    Native,
    #[allow(dead_code)]
    Target {
        target: String,
        flags: Vec<(String, String)>,
    },
}

impl CodegenSettings {
    fn configure(&self, config: &mut wasmtime::Config) {
        match self {
            CodegenSettings::Native => {}
            CodegenSettings::Target { target, flags } => {
                config.target(target).unwrap();
                for (key, value) in flags {
                    unsafe {
                        config.cranelift_flag_set(key, value).unwrap();
                    }
                }
            }
        }
    }
}

impl<'a> Arbitrary<'a> for CodegenSettings {
    #[allow(unused_macros, unused_variables)]
    fn arbitrary(u: &mut Unstructured<'a>) -> arbitrary::Result<Self> {
        // Helper macro to enable clif features based on what the native host
        // supports. If the input says to enable a feature and the host doesn't
        // support it then that test case is rejected with a warning.
        macro_rules! target_features {
            (
                test:$test:ident,
                $(std: $std:tt => clif: $clif:tt $(ratio: $a:tt in $b:tt)?,)*
            ) => ({
                let mut flags = Vec::new();
                $(
                    let (low, hi) = (1, 2);
                    $(let (low, hi) = ($a, $b);)?
                    let enable = u.ratio(low, hi)?;
                    if enable && !std::$test!($std) {
                        log::error!("want to enable clif `{}` but host doesn't support it",
                            $clif);
                        return Err(arbitrary::Error::EmptyChoose)
                    }
                    flags.push((
                        $clif.to_string(),
                        enable.to_string(),
                    ));
                )*
                flags
            })
        }
        #[cfg(target_arch = "x86_64")]
        {
            if u.ratio(1, 10)? {
                let flags = target_features! {
                    test: is_x86_feature_detected,

                    // These features are considered to be baseline required by
                    // Wasmtime. Currently some SIMD code generation will
                    // fail if these features are disabled, so unconditionally
                    // enable them as we're not interested in fuzzing without
                    // them.
                    std:"sse3" => clif:"has_sse3" ratio: 1 in 1,
                    std:"ssse3" => clif:"has_ssse3" ratio: 1 in 1,
                    std:"sse4.1" => clif:"has_sse41" ratio: 1 in 1,

                    std:"sse4.2" => clif:"has_sse42",
                    std:"popcnt" => clif:"has_popcnt",
                    std:"avx" => clif:"has_avx",
                    std:"avx2" => clif:"has_avx2",
                    std:"bmi1" => clif:"has_bmi1",
                    std:"bmi2" => clif:"has_bmi2",
                    std:"lzcnt" => clif:"has_lzcnt",

                    // not a lot of of cpus support avx512 so these are weighted
                    // to get enabled much less frequently.
                    std:"avx512bitalg" => clif:"has_avx512bitalg" ratio:1 in 1000,
                    std:"avx512dq" => clif:"has_avx512dq" ratio: 1 in 1000,
                    std:"avx512f" => clif:"has_avx512f" ratio: 1 in 1000,
                    std:"avx512vl" => clif:"has_avx512vl" ratio: 1 in 1000,
                    std:"avx512vbmi" => clif:"has_avx512vbmi" ratio: 1 in 1000,
                };
                return Ok(CodegenSettings::Target {
                    target: target_lexicon::Triple::host().to_string(),
                    flags,
                });
            }
        }
        Ok(CodegenSettings::Native)
    }
}
