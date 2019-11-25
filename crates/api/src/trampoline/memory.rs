use super::create_handle::create_handle;
use crate::data_structures::{wasm, PrimaryMap};
use crate::MemoryType;
use anyhow::Result;
use wasmtime_environ::Module;
use wasmtime_runtime::InstanceHandle;

#[allow(dead_code)]

pub fn create_handle_with_memory(memory: &MemoryType) -> Result<InstanceHandle> {
    let mut module = Module::new();

    let memory = wasm::Memory {
        minimum: memory.limits().min(),
        maximum: if memory.limits().max() == std::u32::MAX {
            None
        } else {
            Some(memory.limits().max())
        },
        shared: false, // TODO
    };
    let tunable = Default::default();

    let memory_plan = wasmtime_environ::MemoryPlan::for_memory(memory, &tunable);
    let memory_id = module.memory_plans.push(memory_plan);
    module.exports.insert(
        "memory".to_string(),
        wasmtime_environ::Export::Memory(memory_id),
    );

    create_handle(module, None, PrimaryMap::new(), Box::new(()))
}
