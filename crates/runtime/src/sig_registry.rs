//! Implement a registry of function signatures, for fast indirect call
//! signature checking.

use crate::vmcontext::VMSharedSignatureIndex;
use more_asserts::{assert_lt, debug_assert_lt};
use std::collections::{hash_map, HashMap};
use std::convert::TryFrom;
use std::sync::RwLock;
use wasmtime_environ::ir;

/// WebAssembly requires that the caller and callee signatures in an indirect
/// call must match. To implement this efficiently, keep a registry of all
/// signatures, shared by all instances, so that call sites can just do an
/// index comparison.
#[derive(Debug)]
pub struct SignatureRegistry {
    // This structure is stored in a `Compiler` and is intended to be shared
    // across many instances. Ideally instances can themselves be sent across
    // threads, and ideally we can compile across many threads. As a result we
    // use interior mutability here with a lock to avoid having callers to
    // externally synchronize calls to compilation.
    inner: RwLock<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    signature2index: HashMap<ir::Signature, VMSharedSignatureIndex>,
    index2signature: HashMap<VMSharedSignatureIndex, ir::Signature>,
}

impl SignatureRegistry {
    /// Create a new `SignatureRegistry`.
    pub fn new() -> Self {
        Self {
            inner: Default::default(),
        }
    }

    /// Register a signature and return its unique index.
    pub fn register(&self, sig: &ir::Signature) -> VMSharedSignatureIndex {
        let mut inner = self.inner.write().unwrap();
        let len = inner.signature2index.len();
        match inner.signature2index.entry(sig.clone()) {
            hash_map::Entry::Occupied(entry) => *entry.get(),
            hash_map::Entry::Vacant(entry) => {
                // Keep `signature_hash` len under 2**32 -- VMSharedSignatureIndex::new(std::u32::MAX)
                // is reserved for VMSharedSignatureIndex::default().
                debug_assert_lt!(
                    len,
                    std::u32::MAX as usize,
                    "Invariant check: signature_hash.len() < std::u32::MAX"
                );
                let sig_id = VMSharedSignatureIndex::new(u32::try_from(len).unwrap());
                entry.insert(sig_id);
                inner.index2signature.insert(sig_id, sig.clone());
                sig_id
            }
        }
    }

    /// Looks up a shared signature index within this registry.
    ///
    /// Note that for this operation to be semantically correct the `idx` must
    /// have previously come from a call to `register` of this same object.
    pub fn lookup(&self, idx: VMSharedSignatureIndex) -> Option<ir::Signature> {
        self.inner
            .read()
            .unwrap()
            .index2signature
            .get(&idx)
            .cloned()
    }
}
