use cranelift_entity::PrimaryMap;
use failure::Error;
use wasmtime_environ::Module;
use wasmtime_runtime::InstanceHandle;

use super::create_handle::create_handle;
use crate::MemoryType;

#[allow(dead_code)]

pub fn create_handle_with_memory(memory: &MemoryType) -> Result<InstanceHandle, Error> {
    let mut module = Module::new();

    let memory = cranelift_wasm::Memory {
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

    create_handle(module, PrimaryMap::new(), Box::new(()))
}
