//! Evaluate an exported Wasm function using the wasmi interpreter.

use crate::generators::{Config, DiffValue, DiffValueType};
use crate::oracles::engine::{DiffEngine, DiffInstance};
use anyhow::{Context, Error, Result};
use wasmtime::Trap;

/// A wrapper for `wasmi` as a [`DiffEngine`].
pub struct WasmiEngine {
    engine: wasmi::Engine,
}

impl WasmiEngine {
    pub(crate) fn new(config: &mut Config) -> Self {
        let config = &mut config.module_config.config;
        config.reference_types_enabled = false;
        config.simd_enabled = false;
        config.memory64_enabled = false;
        config.bulk_memory_enabled = false;
        config.threads_enabled = false;
        config.max_memories = config.max_memories.min(1);
        config.min_memories = config.min_memories.min(1);
        config.max_tables = config.max_tables.min(1);
        config.min_tables = config.min_tables.min(1);

        Self {
            engine: wasmi::Engine::default(),
        }
    }
}

impl DiffEngine for WasmiEngine {
    fn name(&self) -> &'static str {
        "wasmi"
    }

    fn instantiate(&mut self, wasm: &[u8]) -> Result<Box<dyn DiffInstance>> {
        let module =
            wasmi::Module::new(&self.engine, wasm).context("unable to validate Wasm module")?;
        let mut store = wasmi::Store::new(&self.engine, ());
        let instance = wasmi::Linker::<()>::new()
            .instantiate(&mut store, &module)
            .and_then(|i| i.start(&mut store))
            .context("unable to instantiate module in wasmi")?;
        Ok(Box::new(WasmiInstance { store, instance }))
    }

    fn assert_error_match(&self, trap: &Trap, err: &Error) {
        // Acquire a `wasmi::Trap` from the wasmi error which we'll use to
        // assert that it has the same kind of trap as the wasmtime-based trap.
        let wasmi = match err.downcast_ref::<wasmi::Error>() {
            Some(wasmi::Error::Trap(trap)) => trap,

            // Out-of-bounds data segments turn into this category which
            // Wasmtime reports as a `MemoryOutOfBounds`.
            Some(wasmi::Error::Memory(msg)) => {
                assert_eq!(
                    *trap,
                    Trap::MemoryOutOfBounds,
                    "wasmtime error did not match wasmi: {msg}"
                );
                return;
            }

            // Ignore this for now, looks like "elements segment does not fit"
            // falls into this category and to avoid doing string matching this
            // is just ignored.
            Some(wasmi::Error::Instantiation(msg)) => {
                log::debug!("ignoring wasmi instantiation error: {msg}");
                return;
            }

            Some(other) => panic!("unexpected wasmi error: {}", other),

            None => err
                .downcast_ref::<wasmi::core::Trap>()
                .expect(&format!("not a trap: {:?}", err)),
        };
        assert!(wasmi.as_code().is_some());
        assert_eq!(wasmi_to_wasmtime_trap_code(wasmi.as_code().unwrap()), *trap);
    }

    fn is_stack_overflow(&self, err: &Error) -> bool {
        let trap = match err.downcast_ref::<wasmi::Error>() {
            Some(wasmi::Error::Trap(trap)) => trap,
            Some(_) => return false,
            None => match err.downcast_ref::<wasmi::core::Trap>() {
                Some(trap) => trap,
                None => return false,
            },
        };
        matches!(trap.as_code(), Some(wasmi::core::TrapCode::StackOverflow))
    }
}

/// Converts `wasmi` trap code to `wasmtime` trap code.
fn wasmi_to_wasmtime_trap_code(trap: wasmi::core::TrapCode) -> Trap {
    use wasmi::core::TrapCode;
    match trap {
        TrapCode::Unreachable => Trap::UnreachableCodeReached,
        TrapCode::MemoryAccessOutOfBounds => Trap::MemoryOutOfBounds,
        TrapCode::TableAccessOutOfBounds => Trap::TableOutOfBounds,
        TrapCode::ElemUninitialized => Trap::IndirectCallToNull,
        TrapCode::DivisionByZero => Trap::IntegerDivisionByZero,
        TrapCode::IntegerOverflow => Trap::IntegerOverflow,
        TrapCode::InvalidConversionToInt => Trap::BadConversionToInteger,
        TrapCode::StackOverflow => Trap::StackOverflow,
        TrapCode::UnexpectedSignature => Trap::BadSignature,
    }
}

/// A wrapper for `wasmi` Wasm instances.
struct WasmiInstance {
    store: wasmi::Store<()>,
    instance: wasmi::Instance,
}

impl DiffInstance for WasmiInstance {
    fn name(&self) -> &'static str {
        "wasmi"
    }

    fn evaluate(
        &mut self,
        function_name: &str,
        arguments: &[DiffValue],
        result_tys: &[DiffValueType],
    ) -> Result<Option<Vec<DiffValue>>> {
        let function = self
            .instance
            .get_export(&self.store, function_name)
            .and_then(wasmi::Extern::into_func)
            .unwrap();
        let arguments: Vec<_> = arguments.iter().map(|x| x.into()).collect();
        let mut results = vec![wasmi::core::Value::I32(0); result_tys.len()];
        function
            .call(&mut self.store, &arguments, &mut results)
            .context("wasmi function trap")?;
        Ok(Some(results.into_iter().map(Into::into).collect()))
    }

    fn get_global(&mut self, name: &str, _ty: DiffValueType) -> Option<DiffValue> {
        Some(
            self.instance
                .get_export(&self.store, name)
                .unwrap()
                .into_global()
                .unwrap()
                .get(&self.store)
                .into(),
        )
    }

    fn get_memory(&mut self, name: &str, shared: bool) -> Option<Vec<u8>> {
        assert!(!shared);
        Some(
            self.instance
                .get_export(&self.store, name)
                .unwrap()
                .into_memory()
                .unwrap()
                .data(&self.store)
                .to_vec(),
        )
    }
}

impl From<&DiffValue> for wasmi::core::Value {
    fn from(v: &DiffValue) -> Self {
        use wasmi::core::Value::*;
        match *v {
            DiffValue::I32(n) => I32(n),
            DiffValue::I64(n) => I64(n),
            DiffValue::F32(n) => F32(wasmi::core::F32::from_bits(n)),
            DiffValue::F64(n) => F64(wasmi::core::F64::from_bits(n)),
            DiffValue::V128(_) | DiffValue::FuncRef { .. } | DiffValue::ExternRef { .. } => {
                unimplemented!()
            }
        }
    }
}

impl From<wasmi::core::Value> for DiffValue {
    fn from(value: wasmi::core::Value) -> Self {
        use wasmi::core::Value as WasmiValue;
        match value {
            WasmiValue::I32(n) => DiffValue::I32(n),
            WasmiValue::I64(n) => DiffValue::I64(n),
            WasmiValue::F32(n) => DiffValue::F32(n.to_bits()),
            WasmiValue::F64(n) => DiffValue::F64(n.to_bits()),
        }
    }
}

#[test]
fn smoke() {
    crate::oracles::engine::smoke_test_engine(|_, config| Ok(WasmiEngine::new(config)))
}
