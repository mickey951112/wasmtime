/// Tunable parameters for WebAssembly compilation.
#[derive(Clone)]
pub struct Tunables {
    /// For static heaps, the size in wasm pages of the heap protected by bounds checking.
    pub static_memory_bound: u32,

    /// The size in bytes of the offset guard for static heaps.
    pub static_memory_offset_guard_size: u64,

    /// The size in bytes of the offset guard for dynamic heaps.
    pub dynamic_memory_offset_guard_size: u64,
}

impl Default for Tunables {
    fn default() -> Self {
        Self {
            #[cfg(target_pointer_width = "32")]
            /// Size in wasm pages of the bound for static memories.
            static_memory_bound: 0x4000,
            #[cfg(target_pointer_width = "64")]
            /// Size in wasm pages of the bound for static memories.
            ///
            /// When we allocate 4 GiB of address space, we can avoid the
            /// need for explicit bounds checks.
            static_memory_bound: 0x1_0000,

            #[cfg(target_pointer_width = "32")]
            /// Size in bytes of the offset guard for static memories.
            static_memory_offset_guard_size: 0x1_0000,
            #[cfg(target_pointer_width = "64")]
            /// Size in bytes of the offset guard for static memories.
            ///
            /// Allocating 2 GiB of address space lets us translate wasm
            /// offsets into x86 offsets as aggressively as we can.
            static_memory_offset_guard_size: 0x8000_0000,

            /// Size in bytes of the offset guard for dynamic memories.
            ///
            /// Allocate a small guard to optimize common cases but without
            /// wasting too much memor.
            dynamic_memory_offset_guard_size: 0x1_0000,
        }
    }
}
