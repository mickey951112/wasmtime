//! WebAssembly module and function translation state.
//!
//! The `ModuleTranslationState` struct defined in this module is used to keep track of data about
//! the whole WebAssembly module, such as the decoded type signatures.
//!
//! The `FuncTranslationState` struct defined in this module is used to keep track of the WebAssembly
//! value and control stacks during the translation of a single function.

use crate::environ::{FuncEnvironment, GlobalVariable, WasmResult};
use crate::translation_utils::{FuncIndex, GlobalIndex, MemoryIndex, SignatureIndex, TableIndex};
use crate::{HashMap, Occupied, Vacant};
use cranelift_codegen::ir::{self, Ebb, Inst, Value};
use std::vec::Vec;

/// Information about the presence of an associated `else` for an `if`, or the
/// lack thereof.
#[derive(Debug)]
pub enum ElseData {
    /// The `if` does not already have an `else` block.
    ///
    /// This doesn't mean that it will never have an `else`, just that we
    /// haven't seen it yet.
    NoElse {
        /// If we discover that we need an `else` block, this is the jump
        /// instruction that needs to be fixed up to point to the new `else`
        /// block rather than the destination block after the `if...end`.
        branch_inst: Inst,
    },

    /// We have already allocated an `else` block.
    ///
    /// Usually we don't know whether we will hit an `if .. end` or an `if
    /// .. else .. end`, but sometimes we can tell based on the block's type
    /// signature that the signature is not valid if there isn't an `else`. In
    /// these cases, we pre-allocate the `else` block.
    WithElse {
        /// This is the `else` block.
        else_block: Ebb,
    },
}

/// A control stack frame can be an `if`, a `block` or a `loop`, each one having the following
/// fields:
///
/// - `destination`: reference to the `Ebb` that will hold the code after the control block;
/// - `num_return_values`: number of values returned by the control block;
/// - `original_stack_size`: size of the value stack at the beginning of the control block.
///
/// Moreover, the `if` frame has the `branch_inst` field that points to the `brz` instruction
/// separating the `true` and `false` branch. The `loop` frame has a `header` field that references
/// the `Ebb` that contains the beginning of the body of the loop.
#[derive(Debug)]
pub enum ControlStackFrame {
    If {
        destination: Ebb,
        else_data: ElseData,
        num_param_values: usize,
        num_return_values: usize,
        original_stack_size: usize,
        exit_is_branched_to: bool,
        blocktype: wasmparser::TypeOrFuncType,
        /// Was the head of the `if` reachable?
        head_is_reachable: bool,
        /// What was the reachability at the end of the consequent?
        ///
        /// This is `None` until we're finished translating the consequent, and
        /// is set to `Some` either by hitting an `else` when we will begin
        /// translating the alternative, or by hitting an `end` in which case
        /// there is no alternative.
        consequent_ends_reachable: Option<bool>,
        // Note: no need for `alternative_ends_reachable` because that is just
        // `state.reachable` when we hit the `end` in the `if .. else .. end`.
    },
    Block {
        destination: Ebb,
        num_param_values: usize,
        num_return_values: usize,
        original_stack_size: usize,
        exit_is_branched_to: bool,
    },
    Loop {
        destination: Ebb,
        header: Ebb,
        num_param_values: usize,
        num_return_values: usize,
        original_stack_size: usize,
    },
}

/// Helper methods for the control stack objects.
impl ControlStackFrame {
    pub fn num_return_values(&self) -> usize {
        match *self {
            ControlStackFrame::If {
                num_return_values, ..
            }
            | ControlStackFrame::Block {
                num_return_values, ..
            }
            | ControlStackFrame::Loop {
                num_return_values, ..
            } => num_return_values,
        }
    }
    pub fn num_param_values(&self) -> usize {
        match *self {
            ControlStackFrame::If {
                num_param_values, ..
            }
            | ControlStackFrame::Block {
                num_param_values, ..
            }
            | ControlStackFrame::Loop {
                num_param_values, ..
            } => num_param_values,
        }
    }
    pub fn following_code(&self) -> Ebb {
        match *self {
            ControlStackFrame::If { destination, .. }
            | ControlStackFrame::Block { destination, .. }
            | ControlStackFrame::Loop { destination, .. } => destination,
        }
    }
    pub fn br_destination(&self) -> Ebb {
        match *self {
            ControlStackFrame::If { destination, .. }
            | ControlStackFrame::Block { destination, .. } => destination,
            ControlStackFrame::Loop { header, .. } => header,
        }
    }
    pub fn original_stack_size(&self) -> usize {
        match *self {
            ControlStackFrame::If {
                original_stack_size,
                ..
            }
            | ControlStackFrame::Block {
                original_stack_size,
                ..
            }
            | ControlStackFrame::Loop {
                original_stack_size,
                ..
            } => original_stack_size,
        }
    }
    pub fn is_loop(&self) -> bool {
        match *self {
            ControlStackFrame::If { .. } | ControlStackFrame::Block { .. } => false,
            ControlStackFrame::Loop { .. } => true,
        }
    }

    pub fn exit_is_branched_to(&self) -> bool {
        match *self {
            ControlStackFrame::If {
                exit_is_branched_to,
                ..
            }
            | ControlStackFrame::Block {
                exit_is_branched_to,
                ..
            } => exit_is_branched_to,
            ControlStackFrame::Loop { .. } => false,
        }
    }

    pub fn set_branched_to_exit(&mut self) {
        match *self {
            ControlStackFrame::If {
                ref mut exit_is_branched_to,
                ..
            }
            | ControlStackFrame::Block {
                ref mut exit_is_branched_to,
                ..
            } => *exit_is_branched_to = true,
            ControlStackFrame::Loop { .. } => {}
        }
    }
}

/// Contains information passed along during a function's translation and that records:
///
/// - The current value and control stacks.
/// - The depth of the two unreachable control blocks stacks, that are manipulated when translating
///   unreachable code;
pub struct FuncTranslationState {
    /// A stack of values corresponding to the active values in the input wasm function at this
    /// point.
    pub(crate) stack: Vec<Value>,
    /// A stack of active control flow operations at this point in the input wasm function.
    pub(crate) control_stack: Vec<ControlStackFrame>,
    /// Is the current translation state still reachable? This is false when translating operators
    /// like End, Return, or Unreachable.
    pub(crate) reachable: bool,

    // Map of global variables that have already been created by `FuncEnvironment::make_global`.
    globals: HashMap<GlobalIndex, GlobalVariable>,

    // Map of heaps that have been created by `FuncEnvironment::make_heap`.
    heaps: HashMap<MemoryIndex, ir::Heap>,

    // Map of tables that have been created by `FuncEnvironment::make_table`.
    tables: HashMap<TableIndex, ir::Table>,

    // Map of indirect call signatures that have been created by
    // `FuncEnvironment::make_indirect_sig()`.
    // Stores both the signature reference and the number of WebAssembly arguments
    signatures: HashMap<SignatureIndex, (ir::SigRef, usize)>,

    // Imported and local functions that have been created by
    // `FuncEnvironment::make_direct_func()`.
    // Stores both the function reference and the number of WebAssembly arguments
    functions: HashMap<FuncIndex, (ir::FuncRef, usize)>,
}

// Public methods that are exposed to non-`cranelift_wasm` API consumers.
impl FuncTranslationState {
    /// True if the current translation state expresses reachable code, false if it is unreachable.
    #[inline]
    pub fn reachable(&self) -> bool {
        self.reachable
    }
}

impl FuncTranslationState {
    /// Construct a new, empty, `FuncTranslationState`
    pub(crate) fn new() -> Self {
        Self {
            stack: Vec::new(),
            control_stack: Vec::new(),
            reachable: true,
            globals: HashMap::new(),
            heaps: HashMap::new(),
            tables: HashMap::new(),
            signatures: HashMap::new(),
            functions: HashMap::new(),
        }
    }

    fn clear(&mut self) {
        debug_assert!(self.stack.is_empty());
        debug_assert!(self.control_stack.is_empty());
        self.reachable = true;
        self.globals.clear();
        self.heaps.clear();
        self.tables.clear();
        self.signatures.clear();
        self.functions.clear();
    }

    /// Initialize the state for compiling a function with the given signature.
    ///
    /// This resets the state to containing only a single block representing the whole function.
    /// The exit block is the last block in the function which will contain the return instruction.
    pub(crate) fn initialize(&mut self, sig: &ir::Signature, exit_block: Ebb) {
        self.clear();
        self.push_block(
            exit_block,
            0,
            sig.returns
                .iter()
                .filter(|arg| arg.purpose == ir::ArgumentPurpose::Normal)
                .count(),
        );
    }

    /// Push a value.
    pub(crate) fn push1(&mut self, val: Value) {
        self.stack.push(val);
    }

    /// Push multiple values.
    pub(crate) fn pushn(&mut self, vals: &[Value]) {
        self.stack.extend_from_slice(vals);
    }

    /// Pop one value.
    pub(crate) fn pop1(&mut self) -> Value {
        self.stack
            .pop()
            .expect("attempted to pop a value from an empty stack")
    }

    /// Peek at the top of the stack without popping it.
    pub(crate) fn peek1(&self) -> Value {
        *self
            .stack
            .last()
            .expect("attempted to peek at a value on an empty stack")
    }

    /// Pop two values. Return them in the order they were pushed.
    pub(crate) fn pop2(&mut self) -> (Value, Value) {
        let v2 = self.stack.pop().unwrap();
        let v1 = self.stack.pop().unwrap();
        (v1, v2)
    }

    /// Pop three values. Return them in the order they were pushed.
    pub(crate) fn pop3(&mut self) -> (Value, Value, Value) {
        let v3 = self.stack.pop().unwrap();
        let v2 = self.stack.pop().unwrap();
        let v1 = self.stack.pop().unwrap();
        (v1, v2, v3)
    }

    /// Pop the top `n` values on the stack.
    ///
    /// The popped values are not returned. Use `peekn` to look at them before popping.
    pub(crate) fn popn(&mut self, n: usize) {
        debug_assert!(
            n <= self.stack.len(),
            "popn({}) but stack only has {} values",
            n,
            self.stack.len()
        );
        let new_len = self.stack.len() - n;
        self.stack.truncate(new_len);
    }

    /// Peek at the top `n` values on the stack in the order they were pushed.
    pub(crate) fn peekn(&self, n: usize) -> &[Value] {
        debug_assert!(
            n <= self.stack.len(),
            "peekn({}) but stack only has {} values",
            n,
            self.stack.len()
        );
        &self.stack[self.stack.len() - n..]
    }

    /// Push a block on the control stack.
    pub(crate) fn push_block(
        &mut self,
        following_code: Ebb,
        num_param_types: usize,
        num_result_types: usize,
    ) {
        debug_assert!(num_param_types <= self.stack.len());
        self.control_stack.push(ControlStackFrame::Block {
            destination: following_code,
            original_stack_size: self.stack.len() - num_param_types,
            num_param_values: num_param_types,
            num_return_values: num_result_types,
            exit_is_branched_to: false,
        });
    }

    /// Push a loop on the control stack.
    pub(crate) fn push_loop(
        &mut self,
        header: Ebb,
        following_code: Ebb,
        num_param_types: usize,
        num_result_types: usize,
    ) {
        debug_assert!(num_param_types <= self.stack.len());
        self.control_stack.push(ControlStackFrame::Loop {
            header,
            destination: following_code,
            original_stack_size: self.stack.len() - num_param_types,
            num_param_values: num_param_types,
            num_return_values: num_result_types,
        });
    }

    /// Push an if on the control stack.
    pub(crate) fn push_if(
        &mut self,
        destination: Ebb,
        else_data: ElseData,
        num_param_types: usize,
        num_result_types: usize,
        blocktype: wasmparser::TypeOrFuncType,
    ) {
        debug_assert!(num_param_types <= self.stack.len());

        // Push a second copy of our `if`'s parameters on the stack. This lets
        // us avoid saving them on the side in the `ControlStackFrame` for our
        // `else` block (if it exists), which would require a second heap
        // allocation. See also the comment in `translate_operator` for
        // `Operator::Else`.
        self.stack.reserve(num_param_types);
        for i in (self.stack.len() - num_param_types)..self.stack.len() {
            let val = self.stack[i];
            self.stack.push(val);
        }

        self.control_stack.push(ControlStackFrame::If {
            destination,
            else_data,
            original_stack_size: self.stack.len() - num_param_types,
            num_param_values: num_param_types,
            num_return_values: num_result_types,
            exit_is_branched_to: false,
            head_is_reachable: self.reachable,
            consequent_ends_reachable: None,
            blocktype,
        });
    }
}

/// Methods for handling entity references.
impl FuncTranslationState {
    /// Get the `GlobalVariable` reference that should be used to access the global variable
    /// `index`. Create the reference if necessary.
    /// Also return the WebAssembly type of the global.
    pub(crate) fn get_global<FE: FuncEnvironment + ?Sized>(
        &mut self,
        func: &mut ir::Function,
        index: u32,
        environ: &mut FE,
    ) -> WasmResult<GlobalVariable> {
        let index = GlobalIndex::from_u32(index);
        match self.globals.entry(index) {
            Occupied(entry) => Ok(*entry.get()),
            Vacant(entry) => Ok(*entry.insert(environ.make_global(func, index)?)),
        }
    }

    /// Get the `Heap` reference that should be used to access linear memory `index`.
    /// Create the reference if necessary.
    pub(crate) fn get_heap<FE: FuncEnvironment + ?Sized>(
        &mut self,
        func: &mut ir::Function,
        index: u32,
        environ: &mut FE,
    ) -> WasmResult<ir::Heap> {
        let index = MemoryIndex::from_u32(index);
        match self.heaps.entry(index) {
            Occupied(entry) => Ok(*entry.get()),
            Vacant(entry) => Ok(*entry.insert(environ.make_heap(func, index)?)),
        }
    }

    /// Get the `Table` reference that should be used to access table `index`.
    /// Create the reference if necessary.
    pub(crate) fn get_table<FE: FuncEnvironment + ?Sized>(
        &mut self,
        func: &mut ir::Function,
        index: u32,
        environ: &mut FE,
    ) -> WasmResult<ir::Table> {
        let index = TableIndex::from_u32(index);
        match self.tables.entry(index) {
            Occupied(entry) => Ok(*entry.get()),
            Vacant(entry) => Ok(*entry.insert(environ.make_table(func, index)?)),
        }
    }

    /// Get the `SigRef` reference that should be used to make an indirect call with signature
    /// `index`. Also return the number of WebAssembly arguments in the signature.
    ///
    /// Create the signature if necessary.
    pub(crate) fn get_indirect_sig<FE: FuncEnvironment + ?Sized>(
        &mut self,
        func: &mut ir::Function,
        index: u32,
        environ: &mut FE,
    ) -> WasmResult<(ir::SigRef, usize)> {
        let index = SignatureIndex::from_u32(index);
        match self.signatures.entry(index) {
            Occupied(entry) => Ok(*entry.get()),
            Vacant(entry) => {
                let sig = environ.make_indirect_sig(func, index)?;
                Ok(*entry.insert((sig, normal_args(&func.dfg.signatures[sig]))))
            }
        }
    }

    /// Get the `FuncRef` reference that should be used to make a direct call to function
    /// `index`. Also return the number of WebAssembly arguments in the signature.
    ///
    /// Create the function reference if necessary.
    pub(crate) fn get_direct_func<FE: FuncEnvironment + ?Sized>(
        &mut self,
        func: &mut ir::Function,
        index: u32,
        environ: &mut FE,
    ) -> WasmResult<(ir::FuncRef, usize)> {
        let index = FuncIndex::from_u32(index);
        match self.functions.entry(index) {
            Occupied(entry) => Ok(*entry.get()),
            Vacant(entry) => {
                let fref = environ.make_direct_func(func, index)?;
                let sig = func.dfg.ext_funcs[fref].signature;
                Ok(*entry.insert((fref, normal_args(&func.dfg.signatures[sig]))))
            }
        }
    }
}

/// Count the number of normal parameters in a signature.
/// Exclude special-purpose parameters that represent runtime stuff and not WebAssembly arguments.
fn normal_args(sig: &ir::Signature) -> usize {
    sig.params
        .iter()
        .filter(|arg| arg.purpose == ir::ArgumentPurpose::Normal)
        .count()
}
