//! WebAssembly Instance API object.

extern crate alloc;

use crate::function::Function;
use crate::memory::Memory;
use alloc::rc::Rc;
use core::cell::RefCell;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use wasmtime_environ::Export;
use wasmtime_interface_types::ModuleData;
use wasmtime_jit::{Context, InstanceHandle};
use wasmtime_runtime::Export as RuntimeExport;

#[pyclass]
pub struct Instance {
    pub context: Rc<RefCell<Context>>,
    pub instance: InstanceHandle,
    pub data: Rc<ModuleData>,
}

#[pymethods]
impl Instance {
    #[getter(exports)]
    fn get_exports(&mut self) -> PyResult<PyObject> {
        let gil = Python::acquire_gil();
        let py = gil.python();
        let exports = PyDict::new(py);
        let mut function_exports = Vec::new();
        let mut memory_exports = Vec::new();
        for (name, export) in self.instance.exports() {
            match export {
                Export::Function(_) => function_exports.push(name.to_string()),
                Export::Memory(_) => memory_exports.push(name.to_string()),
                _ => {
                    // Skip unknown export type.
                    continue;
                }
            }
        }
        for name in memory_exports {
            if let Some(RuntimeExport::Memory { .. }) = self.instance.lookup(&name) {
                let f = Py::new(
                    py,
                    Memory {
                        context: self.context.clone(),
                        instance: self.instance.clone(),
                        export_name: name.clone(),
                    },
                )?;
                exports.set_item(name, f)?;
            } else {
                panic!("memory");
            }
        }
        for name in function_exports {
            if let Some(RuntimeExport::Function { signature, .. }) = self.instance.lookup(&name) {
                let mut args_types = Vec::new();
                for index in 1..signature.params.len() {
                    let ty = signature.params[index].value_type;
                    args_types.push(ty);
                }
                let f = Py::new(
                    py,
                    Function {
                        context: self.context.clone(),
                        instance: self.instance.clone(),
                        data: self.data.clone(),
                        export_name: name.clone(),
                        args_types,
                    },
                )?;
                exports.set_item(name, f)?;
            } else {
                panic!("function");
            }
        }

        Ok(exports.to_object(py))
    }
}
