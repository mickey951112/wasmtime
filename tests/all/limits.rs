use anyhow::Result;
use std::cell::RefCell;
use std::rc::Rc;
use wasmtime::*;

#[test]
fn test_limits() -> Result<()> {
    let engine = Engine::default();
    let module = Module::new(
        &engine,
        r#"(module (memory (export "m") 0) (table (export "t") 0 anyfunc))"#,
    )?;

    let store = Store::new_with_limits(
        &engine,
        StoreLimitsBuilder::new()
            .memory_pages(10)
            .table_elements(5)
            .build(),
    );

    let instance = Instance::new(&store, &module, &[])?;

    // Test instance exports and host objects hitting the limit
    for memory in std::array::IntoIter::new([
        instance.get_memory("m").unwrap(),
        Memory::new(&store, MemoryType::new(Limits::new(0, None)))?,
    ]) {
        memory.grow(3)?;
        memory.grow(5)?;
        memory.grow(2)?;

        assert_eq!(
            memory.grow(1).map_err(|e| e.to_string()).unwrap_err(),
            "failed to grow memory by `1`"
        );
    }

    // Test instance exports and host objects hitting the limit
    for table in std::array::IntoIter::new([
        instance.get_table("t").unwrap(),
        Table::new(
            &store,
            TableType::new(ValType::FuncRef, Limits::new(0, None)),
            Val::FuncRef(None),
        )?,
    ]) {
        table.grow(2, Val::FuncRef(None))?;
        table.grow(1, Val::FuncRef(None))?;
        table.grow(2, Val::FuncRef(None))?;

        assert_eq!(
            table
                .grow(1, Val::FuncRef(None))
                .map_err(|e| e.to_string())
                .unwrap_err(),
            "failed to grow table by `1`"
        );
    }

    Ok(())
}

#[test]
fn test_limits_memory_only() -> Result<()> {
    let engine = Engine::default();
    let module = Module::new(
        &engine,
        r#"(module (memory (export "m") 0) (table (export "t") 0 anyfunc))"#,
    )?;

    let store = Store::new_with_limits(&engine, StoreLimitsBuilder::new().memory_pages(10).build());

    let instance = Instance::new(&store, &module, &[])?;

    // Test instance exports and host objects hitting the limit
    for memory in std::array::IntoIter::new([
        instance.get_memory("m").unwrap(),
        Memory::new(&store, MemoryType::new(Limits::new(0, None)))?,
    ]) {
        memory.grow(3)?;
        memory.grow(5)?;
        memory.grow(2)?;

        assert_eq!(
            memory.grow(1).map_err(|e| e.to_string()).unwrap_err(),
            "failed to grow memory by `1`"
        );
    }

    // Test instance exports and host objects *not* hitting the limit
    for table in std::array::IntoIter::new([
        instance.get_table("t").unwrap(),
        Table::new(
            &store,
            TableType::new(ValType::FuncRef, Limits::new(0, None)),
            Val::FuncRef(None),
        )?,
    ]) {
        table.grow(2, Val::FuncRef(None))?;
        table.grow(1, Val::FuncRef(None))?;
        table.grow(2, Val::FuncRef(None))?;
        table.grow(1, Val::FuncRef(None))?;
    }

    Ok(())
}

#[test]
fn test_initial_memory_limits_exceeded() -> Result<()> {
    let engine = Engine::default();
    let module = Module::new(&engine, r#"(module (memory (export "m") 11))"#)?;

    let store = Store::new_with_limits(&engine, StoreLimitsBuilder::new().memory_pages(10).build());

    match Instance::new(&store, &module, &[]) {
        Ok(_) => unreachable!(),
        Err(e) => assert_eq!(
            e.to_string(),
            "Insufficient resources: memory minimum size of 11 pages exceeds memory limits"
        ),
    }

    match Memory::new(&store, MemoryType::new(Limits::new(25, None))) {
        Ok(_) => unreachable!(),
        Err(e) => assert_eq!(
            e.to_string(),
            "Insufficient resources: memory minimum size of 25 pages exceeds memory limits"
        ),
    }

    Ok(())
}

#[test]
fn test_limits_table_only() -> Result<()> {
    let engine = Engine::default();
    let module = Module::new(
        &engine,
        r#"(module (memory (export "m") 0) (table (export "t") 0 anyfunc))"#,
    )?;

    let store =
        Store::new_with_limits(&engine, StoreLimitsBuilder::new().table_elements(5).build());

    let instance = Instance::new(&store, &module, &[])?;

    // Test instance exports and host objects *not* hitting the limit
    for memory in std::array::IntoIter::new([
        instance.get_memory("m").unwrap(),
        Memory::new(&store, MemoryType::new(Limits::new(0, None)))?,
    ]) {
        memory.grow(3)?;
        memory.grow(5)?;
        memory.grow(2)?;
        memory.grow(1)?;
    }

    // Test instance exports and host objects hitting the limit
    for table in std::array::IntoIter::new([
        instance.get_table("t").unwrap(),
        Table::new(
            &store,
            TableType::new(ValType::FuncRef, Limits::new(0, None)),
            Val::FuncRef(None),
        )?,
    ]) {
        table.grow(2, Val::FuncRef(None))?;
        table.grow(1, Val::FuncRef(None))?;
        table.grow(2, Val::FuncRef(None))?;

        assert_eq!(
            table
                .grow(1, Val::FuncRef(None))
                .map_err(|e| e.to_string())
                .unwrap_err(),
            "failed to grow table by `1`"
        );
    }

    Ok(())
}

#[test]
fn test_initial_table_limits_exceeded() -> Result<()> {
    let engine = Engine::default();
    let module = Module::new(&engine, r#"(module (table (export "t") 23 anyfunc))"#)?;

    let store =
        Store::new_with_limits(&engine, StoreLimitsBuilder::new().table_elements(4).build());

    match Instance::new(&store, &module, &[]) {
        Ok(_) => unreachable!(),
        Err(e) => assert_eq!(
            e.to_string(),
            "Insufficient resources: table minimum size of 23 elements exceeds table limits"
        ),
    }

    match Table::new(
        &store,
        TableType::new(ValType::FuncRef, Limits::new(99, None)),
        Val::FuncRef(None),
    ) {
        Ok(_) => unreachable!(),
        Err(e) => assert_eq!(
            e.to_string(),
            "Insufficient resources: table minimum size of 99 elements exceeds table limits"
        ),
    }

    Ok(())
}

#[test]
fn test_pooling_allocator_initial_limits_exceeded() -> Result<()> {
    let mut config = Config::new();
    config.wasm_multi_memory(true);
    config.allocation_strategy(InstanceAllocationStrategy::Pooling {
        strategy: PoolingAllocationStrategy::NextAvailable,
        module_limits: ModuleLimits {
            memories: 2,
            ..Default::default()
        },
        instance_limits: InstanceLimits {
            count: 1,
            ..Default::default()
        },
    });

    let engine = Engine::new(&config)?;
    let module = Module::new(
        &engine,
        r#"(module (memory (export "m1") 2) (memory (export "m2") 5))"#,
    )?;

    let store = Store::new_with_limits(&engine, StoreLimitsBuilder::new().memory_pages(3).build());

    match Instance::new(&store, &module, &[]) {
        Ok(_) => unreachable!(),
        Err(e) => assert_eq!(
            e.to_string(),
            "Insufficient resources: memory minimum size of 5 pages exceeds memory limits"
        ),
    }

    // An instance should still be able to be created after the failure above
    let module = Module::new(&engine, r#"(module (memory (export "m") 2))"#)?;

    Instance::new(&store, &module, &[])?;

    Ok(())
}

struct MemoryContext {
    host_memory_used: usize,
    wasm_memory_used: usize,
    memory_limit: usize,
    limit_exceeded: bool,
    limiter_dropped: bool,
}

struct HostMemoryLimiter(Rc<RefCell<MemoryContext>>);

impl ResourceLimiter for HostMemoryLimiter {
    fn memory_growing(&self, current: u32, desired: u32, maximum: Option<u32>) -> bool {
        let mut ctx = self.0.borrow_mut();

        // Check if the desired exceeds a maximum (either from Wasm or from the host)
        if desired > maximum.unwrap_or(u32::MAX) {
            ctx.limit_exceeded = true;
            return false;
        }

        assert_eq!(current as usize * 0x10000, ctx.wasm_memory_used);
        let desired = desired as usize * 0x10000;

        if desired + ctx.host_memory_used > ctx.memory_limit {
            ctx.limit_exceeded = true;
            return false;
        }

        ctx.wasm_memory_used = desired;
        true
    }

    fn table_growing(&self, _current: u32, _desired: u32, _maximum: Option<u32>) -> bool {
        true
    }
}

impl Drop for HostMemoryLimiter {
    fn drop(&mut self) {
        self.0.borrow_mut().limiter_dropped = true;
    }
}

#[test]
fn test_custom_limiter() -> Result<()> {
    let mut config = Config::default();

    // This approximates a function that would "allocate" resources that the host tracks.
    // Here this is a simple function that increments the current host memory "used".
    config.wrap_host_func("", "alloc", |caller: Caller, size: u32| -> u32 {
        if let Some(ctx) = caller.store().get::<Rc<RefCell<MemoryContext>>>() {
            let mut ctx = ctx.borrow_mut();
            let size = size as usize;

            if size + ctx.host_memory_used + ctx.wasm_memory_used <= ctx.memory_limit {
                ctx.host_memory_used += size;
                return 1;
            }

            ctx.limit_exceeded = true;
        }

        0
    });

    let engine = Engine::new(&config)?;
    let module = Module::new(
        &engine,
        r#"(module (import "" "alloc" (func $alloc (param i32) (result i32))) (memory (export "m") 0) (func (export "f") (param i32) (result i32) local.get 0 call $alloc))"#,
    )?;

    let context = Rc::new(RefCell::new(MemoryContext {
        host_memory_used: 0,
        wasm_memory_used: 0,
        memory_limit: 1 << 20, // 16 wasm pages is the limit for both wasm + host memory
        limit_exceeded: false,
        limiter_dropped: false,
    }));

    let store = Store::new_with_limits(&engine, HostMemoryLimiter(context.clone()));

    assert!(store.set(context.clone()).is_ok());

    let linker = Linker::new(&store);
    let instance = linker.instantiate(&module)?;
    let memory = instance.get_memory("m").unwrap();

    // Grow the memory by 640 KiB
    memory.grow(3)?;
    memory.grow(5)?;
    memory.grow(2)?;

    assert!(!context.borrow().limit_exceeded);

    // Grow the host "memory" by 384 KiB
    let f = instance.get_typed_func::<u32, u32>("f")?;

    assert_eq!(f.call(1 * 0x10000).unwrap(), 1);
    assert_eq!(f.call(3 * 0x10000).unwrap(), 1);
    assert_eq!(f.call(2 * 0x10000).unwrap(), 1);

    // Memory is at the maximum, but the limit hasn't been exceeded
    assert!(!context.borrow().limit_exceeded);

    // Try to grow the memory again
    assert_eq!(
        memory.grow(1).map_err(|e| e.to_string()).unwrap_err(),
        "failed to grow memory by `1`"
    );

    assert!(context.borrow().limit_exceeded);

    // Try to grow the host "memory" again
    assert_eq!(f.call(1).unwrap(), 0);

    assert!(context.borrow().limit_exceeded);

    drop(f);
    drop(memory);
    drop(instance);
    drop(linker);
    drop(store);

    assert!(context.borrow().limiter_dropped);

    Ok(())
}
