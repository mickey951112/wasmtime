use wasmtime_environ::{ir, wasm};

// Type Representations

// Type attributes

/// Indicator of whether a global is mutable or not
#[derive(Debug, Clone, Copy, Hash, Eq, PartialEq)]
pub enum Mutability {
    /// The global is constant and its value does not change
    Const,
    /// The value of the global can change over time
    Var,
}

/// Limits of tables/memories where the units of the limits are defined by the
/// table/memory types.
///
/// A minimum is always available but the maximum may not be present.
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub struct Limits {
    min: u32,
    max: Option<u32>,
}

impl Limits {
    /// Creates a new set of limits with the minimum and maximum both specified.
    pub fn new(min: u32, max: Option<u32>) -> Limits {
        Limits { min, max }
    }

    /// Creates a new `Limits` with the `min` specified and no maximum specified.
    pub fn at_least(min: u32) -> Limits {
        Limits::new(min, None)
    }

    /// Returns the minimum amount for these limits.
    pub fn min(&self) -> u32 {
        self.min
    }

    /// Returns the maximum amount for these limits, if specified.
    pub fn max(&self) -> Option<u32> {
        self.max
    }
}

// Value Types

/// A list of all possible value types in WebAssembly.
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub enum ValType {
    /// Signed 32 bit integer.
    I32,
    /// Signed 64 bit integer.
    I64,
    /// Floating point 32 bit integer.
    F32,
    /// Floating point 64 bit integer.
    F64,
    /// A 128 bit number.
    V128,
    /// A reference to opaque data in the Wasm instance.
    AnyRef, /* = 128 */
    /// A reference to a Wasm function.
    FuncRef,
}

impl ValType {
    /// Returns true if `ValType` matches any of the numeric types. (e.g. `I32`,
    /// `I64`, `F32`, `F64`).
    pub fn is_num(&self) -> bool {
        match self {
            ValType::I32 | ValType::I64 | ValType::F32 | ValType::F64 => true,
            _ => false,
        }
    }

    /// Returns true if `ValType` matches either of the reference types.
    pub fn is_ref(&self) -> bool {
        match self {
            ValType::AnyRef | ValType::FuncRef => true,
            _ => false,
        }
    }

    pub(crate) fn get_wasmtime_type(&self) -> Option<ir::Type> {
        match self {
            ValType::I32 => Some(ir::types::I32),
            ValType::I64 => Some(ir::types::I64),
            ValType::F32 => Some(ir::types::F32),
            ValType::F64 => Some(ir::types::F64),
            ValType::V128 => Some(ir::types::I8X16),
            _ => None,
        }
    }

    pub(crate) fn from_wasmtime_type(ty: ir::Type) -> Option<ValType> {
        match ty {
            ir::types::I32 => Some(ValType::I32),
            ir::types::I64 => Some(ValType::I64),
            ir::types::F32 => Some(ValType::F32),
            ir::types::F64 => Some(ValType::F64),
            ir::types::I8X16 => Some(ValType::V128),
            _ => None,
        }
    }
}

// External Types

/// A list of all possible types which can be externally referenced from a
/// WebAssembly module.
///
/// This list can be found in [`ImportType`] or [`ExportType`], so these types
/// can either be imported or exported.
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub enum ExternType {
    /// This external type is the type of a WebAssembly function.
    Func(FuncType),
    /// This external type is the type of a WebAssembly global.
    Global(GlobalType),
    /// This external type is the type of a WebAssembly table.
    Table(TableType),
    /// This external type is the type of a WebAssembly memory.
    Memory(MemoryType),
}

macro_rules! accessors {
    ($(($variant:ident($ty:ty) $get:ident $unwrap:ident))*) => ($(
        /// Attempt to return the underlying type of this external type,
        /// returning `None` if it is a different type.
        pub fn $get(&self) -> Option<&$ty> {
            if let ExternType::$variant(e) = self {
                Some(e)
            } else {
                None
            }
        }

        /// Returns the underlying descriptor of this [`ExternType`], panicking
        /// if it is a different type.
        ///
        /// # Panics
        ///
        /// Panics if `self` is not of the right type.
        pub fn $unwrap(&self) -> &$ty {
            self.$get().expect(concat!("expected ", stringify!($ty)))
        }
    )*)
}

impl ExternType {
    accessors! {
        (Func(FuncType) func unwrap_func)
        (Global(GlobalType) global unwrap_global)
        (Table(TableType) table unwrap_table)
        (Memory(MemoryType) memory unwrap_memory)
    }
}

// Function Types
fn from_wasmtime_abiparam(param: &ir::AbiParam) -> Option<ValType> {
    assert_eq!(param.purpose, ir::ArgumentPurpose::Normal);
    ValType::from_wasmtime_type(param.value_type)
}

/// A descriptor for a function in a WebAssembly module.
///
/// WebAssembly functions can have 0 or more parameters and results.
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub struct FuncType {
    params: Box<[ValType]>,
    results: Box<[ValType]>,
}

impl FuncType {
    /// Creates a new function descriptor from the given parameters and results.
    ///
    /// The function descriptor returned will represent a function which takes
    /// `params` as arguments and returns `results` when it is finished.
    pub fn new(params: Box<[ValType]>, results: Box<[ValType]>) -> FuncType {
        FuncType { params, results }
    }

    /// Returns the list of parameter types for this function.
    pub fn params(&self) -> &[ValType] {
        &self.params
    }

    /// Returns the list of result types for this function.
    pub fn results(&self) -> &[ValType] {
        &self.results
    }

    /// Returns `Some` if this function signature was compatible with cranelift,
    /// or `None` if one of the types/results wasn't supported or compatible
    /// with cranelift.
    pub(crate) fn get_wasmtime_signature(&self, pointer_type: ir::Type) -> Option<ir::Signature> {
        use wasmtime_environ::ir::{types, AbiParam, ArgumentPurpose, Signature};
        use wasmtime_jit::native;
        let call_conv = native::call_conv();
        let mut params = self
            .params
            .iter()
            .map(|p| p.get_wasmtime_type().map(AbiParam::new))
            .collect::<Option<Vec<_>>>()?;
        let returns = self
            .results
            .iter()
            .map(|p| p.get_wasmtime_type().map(AbiParam::new))
            .collect::<Option<Vec<_>>>()?;
        params.insert(0, AbiParam::special(types::I64, ArgumentPurpose::VMContext));
        params.insert(1, AbiParam::new(pointer_type));

        Some(Signature {
            params,
            returns,
            call_conv,
        })
    }

    /// Returns `None` if any types in the signature can't be converted to the
    /// types in this crate, but that should very rarely happen and largely only
    /// indicate a bug in our cranelift integration.
    pub(crate) fn from_wasmtime_signature(signature: ir::Signature) -> Option<FuncType> {
        let params = signature
            .params
            .iter()
            .skip(2) // skip the caller/callee vmctx
            .map(|p| from_wasmtime_abiparam(p))
            .collect::<Option<Vec<_>>>()?;
        let results = signature
            .returns
            .iter()
            .map(|p| from_wasmtime_abiparam(p))
            .collect::<Option<Vec<_>>>()?;
        Some(FuncType {
            params: params.into_boxed_slice(),
            results: results.into_boxed_slice(),
        })
    }
}

// Global Types

/// A WebAssembly global descriptor.
///
/// This type describes an instance of a global in a WebAssembly module. Globals
/// are local to an [`Instance`](crate::Instance) and are either immutable or
/// mutable.
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub struct GlobalType {
    content: ValType,
    mutability: Mutability,
}

impl GlobalType {
    /// Creates a new global descriptor of the specified `content` type and
    /// whether or not it's mutable.
    pub fn new(content: ValType, mutability: Mutability) -> GlobalType {
        GlobalType {
            content,
            mutability,
        }
    }

    /// Returns the value type of this global descriptor.
    pub fn content(&self) -> &ValType {
        &self.content
    }

    /// Returns whether or not this global is mutable.
    pub fn mutability(&self) -> Mutability {
        self.mutability
    }

    /// Returns `None` if the wasmtime global has a type that we can't
    /// represent, but that should only very rarely happen and indicate a bug.
    pub(crate) fn from_wasmtime_global(global: &wasm::Global) -> Option<GlobalType> {
        let ty = ValType::from_wasmtime_type(global.ty)?;
        let mutability = if global.mutability {
            Mutability::Var
        } else {
            Mutability::Const
        };
        Some(GlobalType::new(ty, mutability))
    }
}

// Table Types

/// A descriptor for a table in a WebAssembly module.
///
/// Tables are contiguous chunks of a specific element, typically a `funcref` or
/// an `anyref`. The most common use for tables is a function table through
/// which `call_indirect` can invoke other functions.
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub struct TableType {
    element: ValType,
    limits: Limits,
}

impl TableType {
    /// Creates a new table descriptor which will contain the specified
    /// `element` and have the `limits` applied to its length.
    pub fn new(element: ValType, limits: Limits) -> TableType {
        TableType { element, limits }
    }

    /// Returns the element value type of this table.
    pub fn element(&self) -> &ValType {
        &self.element
    }

    /// Returns the limits, in units of elements, of this table.
    pub fn limits(&self) -> &Limits {
        &self.limits
    }

    pub(crate) fn from_wasmtime_table(table: &wasm::Table) -> TableType {
        assert!(if let wasm::TableElementType::Func = table.ty {
            true
        } else {
            false
        });
        let ty = ValType::FuncRef;
        let limits = Limits::new(table.minimum, table.maximum);
        TableType::new(ty, limits)
    }
}

// Memory Types

/// A descriptor for a WebAssembly memory type.
///
/// Memories are described in units of pages (64KB) and represent contiguous
/// chunks of addressable memory.
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub struct MemoryType {
    limits: Limits,
}

impl MemoryType {
    /// Creates a new descriptor for a WebAssembly memory given the specified
    /// limits of the memory.
    pub fn new(limits: Limits) -> MemoryType {
        MemoryType { limits }
    }

    /// Returns the limits (in pages) that are configured for this memory.
    pub fn limits(&self) -> &Limits {
        &self.limits
    }

    pub(crate) fn from_wasmtime_memory(memory: &wasm::Memory) -> MemoryType {
        MemoryType::new(Limits::new(memory.minimum, memory.maximum))
    }
}

// Import Types

/// A descriptor for an imported value into a wasm module.
///
/// This type is primarily accessed from the
/// [`Module::imports`](crate::Module::imports) API. Each [`ImportType`]
/// describes an import into the wasm module with the module/name that it's
/// imported from as well as the type of item that's being imported.
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub struct ImportType {
    module: String,
    name: String,
    ty: ExternType,
}

impl ImportType {
    /// Creates a new import descriptor which comes from `module` and `name` and
    /// is of type `ty`.
    pub fn new(module: &str, name: &str, ty: ExternType) -> ImportType {
        ImportType {
            module: module.to_string(),
            name: name.to_string(),
            ty,
        }
    }

    /// Returns the module name that this import is expected to come from.
    pub fn module(&self) -> &str {
        &self.module
    }

    /// Returns the field name of the module that this import is expected to
    /// come from.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the expected type of this import.
    pub fn ty(&self) -> &ExternType {
        &self.ty
    }
}

// Export Types

/// A descriptor for an exported WebAssembly value.
///
/// This type is primarily accessed from the
/// [`Module::exports`](crate::Module::exports) accessor and describes what
/// names are exported from a wasm module and the type of the item that is
/// exported.
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub struct ExportType {
    name: String,
    ty: ExternType,
}

impl ExportType {
    /// Creates a new export which is exported with the given `name` and has the
    /// given `ty`.
    pub fn new(name: &str, ty: ExternType) -> ExportType {
        ExportType {
            name: name.to_string(),
            ty,
        }
    }

    /// Returns the name by which this export is known by.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the type of this export.
    pub fn ty(&self) -> &ExternType {
        &self.ty
    }
}
