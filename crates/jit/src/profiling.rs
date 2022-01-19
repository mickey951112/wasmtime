use crate::{demangling::demangle_function_name_or_index, CompiledModule};
use wasmtime_environ::{DefinedFuncIndex, EntityRef, Module};

cfg_if::cfg_if! {
    if #[cfg(all(feature = "jitdump", target_os = "linux"))] {
        #[path = "profiling/jitdump_linux.rs"]
        mod jitdump;
    } else {
        #[path = "profiling/jitdump_disabled.rs"]
        mod jitdump;
    }
}

cfg_if::cfg_if! {
    if #[cfg(all(feature = "vtune", target_os = "linux"))] {
        #[path = "profiling/vtune_linux.rs"]
        mod vtune;
    } else {
        #[path = "profiling/vtune_disabled.rs"]
        mod vtune;
    }
}

pub use jitdump::JitDumpAgent;
pub use vtune::VTuneAgent;

/// Common interface for profiling tools.
pub trait ProfilingAgent: Send + Sync + 'static {
    /// Notify the profiler of a new module loaded into memory
    fn module_load(&self, module: &CompiledModule, dbg_image: Option<&[u8]>);

    /// Notify the profiler about a single dynamically-generated trampoline (for host function)
    /// that is being loaded now.`
    fn load_single_trampoline(&self, name: &str, addr: *const u8, size: usize, pid: u32, tid: u32);
}

/// Default agent for unsupported profiling build.
#[derive(Debug, Default, Clone, Copy)]
pub struct NullProfilerAgent;

impl ProfilingAgent for NullProfilerAgent {
    fn module_load(&self, _module: &CompiledModule, _dbg_image: Option<&[u8]>) {}
    fn load_single_trampoline(
        &self,
        _name: &str,
        _addr: *const u8,
        _size: usize,
        _pid: u32,
        _tid: u32,
    ) {
    }
}

#[allow(dead_code)]
fn debug_name(module: &Module, index: DefinedFuncIndex) -> String {
    let index = module.func_index(index);
    let mut debug_name = String::new();
    demangle_function_name_or_index(
        &mut debug_name,
        module.func_names.get(&index).map(|s| s.as_str()),
        index.index(),
    )
    .unwrap();
    debug_name
}
