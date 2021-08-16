//! Memory management for linear memories.
//!
//! `RuntimeLinearMemory` is to WebAssembly linear memories what `Table` is to WebAssembly tables.

use crate::mmap::Mmap;
use crate::vmcontext::VMMemoryDefinition;
use crate::ResourceLimiter;
use anyhow::{bail, format_err, Result};
use more_asserts::{assert_ge, assert_le};
use std::convert::TryFrom;
use wasmtime_environ::{MemoryPlan, MemoryStyle, WASM32_MAX_PAGES, WASM64_MAX_PAGES};

const WASM_PAGE_SIZE: usize = wasmtime_environ::WASM_PAGE_SIZE as usize;
const WASM_PAGE_SIZE_U64: u64 = wasmtime_environ::WASM_PAGE_SIZE as u64;

/// A memory allocator
pub trait RuntimeMemoryCreator: Send + Sync {
    /// Create new RuntimeLinearMemory
    fn new_memory(
        &self,
        plan: &MemoryPlan,
        minimum: usize,
        maximum: Option<usize>,
    ) -> Result<Box<dyn RuntimeLinearMemory>>;
}

/// A default memory allocator used by Wasmtime
pub struct DefaultMemoryCreator;

impl RuntimeMemoryCreator for DefaultMemoryCreator {
    /// Create new MmapMemory
    fn new_memory(
        &self,
        plan: &MemoryPlan,
        minimum: usize,
        maximum: Option<usize>,
    ) -> Result<Box<dyn RuntimeLinearMemory>> {
        Ok(Box::new(MmapMemory::new(plan, minimum, maximum)?))
    }
}

/// A linear memory
pub trait RuntimeLinearMemory: Send + Sync {
    /// Returns the number of allocated bytes.
    fn byte_size(&self) -> usize;

    /// Returns the maximum number of bytes the memory can grow to.
    /// Returns `None` if the memory is unbounded.
    fn maximum_byte_size(&self) -> Option<usize>;

    /// Grow memory to the specified amount of bytes.
    ///
    /// Returns `None` if memory can't be grown by the specified amount
    /// of bytes.
    fn grow_to(&mut self, size: usize) -> Option<()>;

    /// Return a `VMMemoryDefinition` for exposing the memory to compiled wasm
    /// code.
    fn vmmemory(&self) -> VMMemoryDefinition;
}

/// A linear memory instance.
#[derive(Debug)]
pub struct MmapMemory {
    // The underlying allocation.
    mmap: Mmap,

    // The number of bytes that are accessible in `mmap` and available for
    // reading and writing.
    //
    // This region starts at `pre_guard_size` offset from the base of `mmap`.
    accessible: usize,

    // The optional maximum accessible size, in bytes, for this linear memory.
    //
    // Note that this maximum does not factor in guard pages, so this isn't the
    // maximum size of the linear address space reservation for this memory.
    maximum: Option<usize>,

    // Size in bytes of extra guard pages before the start and after the end to
    // optimize loads and stores with constant offsets.
    pre_guard_size: usize,
    offset_guard_size: usize,
}

impl MmapMemory {
    /// Create a new linear memory instance with specified minimum and maximum number of wasm pages.
    pub fn new(plan: &MemoryPlan, minimum: usize, maximum: Option<usize>) -> Result<Self> {
        // It's a programmer error for these two configuration values to exceed
        // the host available address space, so panic if such a configuration is
        // found (mostly an issue for hypothetical 32-bit hosts).
        let offset_guard_bytes = usize::try_from(plan.offset_guard_size).unwrap();
        let pre_guard_bytes = usize::try_from(plan.pre_guard_size).unwrap();

        let alloc_bytes = match plan.style {
            MemoryStyle::Dynamic => minimum,
            MemoryStyle::Static { bound } => {
                assert_ge!(bound, plan.memory.minimum);
                usize::try_from(bound.checked_mul(WASM_PAGE_SIZE_U64).unwrap()).unwrap()
            }
        };
        let request_bytes = pre_guard_bytes
            .checked_add(alloc_bytes)
            .and_then(|i| i.checked_add(offset_guard_bytes))
            .ok_or_else(|| format_err!("cannot allocate {} with guard regions", minimum))?;

        let mut mmap = Mmap::accessible_reserved(0, request_bytes)?;
        if minimum > 0 {
            mmap.make_accessible(pre_guard_bytes, minimum)?;
        }

        Ok(Self {
            mmap,
            accessible: minimum,
            maximum,
            pre_guard_size: pre_guard_bytes,
            offset_guard_size: offset_guard_bytes,
        })
    }
}

impl RuntimeLinearMemory for MmapMemory {
    fn byte_size(&self) -> usize {
        self.accessible
    }

    fn maximum_byte_size(&self) -> Option<usize> {
        self.maximum
    }

    fn grow_to(&mut self, new_size: usize) -> Option<()> {
        if new_size > self.mmap.len() - self.offset_guard_size - self.pre_guard_size {
            // If the new size is within the declared maximum, but needs more memory than we
            // have on hand, it's a dynamic heap and it can move.
            let request_bytes = self
                .pre_guard_size
                .checked_add(new_size)?
                .checked_add(self.offset_guard_size)?;

            let mut new_mmap = Mmap::accessible_reserved(0, request_bytes).ok()?;
            new_mmap
                .make_accessible(self.pre_guard_size, new_size)
                .ok()?;

            new_mmap.as_mut_slice()[self.pre_guard_size..][..self.accessible]
                .copy_from_slice(&self.mmap.as_slice()[self.pre_guard_size..][..self.accessible]);

            self.mmap = new_mmap;
        } else {
            assert!(new_size > self.accessible);
            // Make the newly allocated pages accessible.
            self.mmap
                .make_accessible(
                    self.pre_guard_size + self.accessible,
                    new_size - self.accessible,
                )
                .ok()?;
        }

        self.accessible = new_size;

        Some(())
    }

    fn vmmemory(&self) -> VMMemoryDefinition {
        VMMemoryDefinition {
            base: unsafe { self.mmap.as_mut_ptr().add(self.pre_guard_size) },
            current_length: self.accessible,
        }
    }
}

/// Representation of a runtime wasm linear memory.
pub enum Memory {
    /// A "static" memory where the lifetime of the backing memory is managed
    /// elsewhere. Currently used with the pooling allocator.
    Static {
        /// The memory in the host for this wasm memory. The length of this
        /// slice is the maximum size of the memory that can be grown to.
        base: &'static mut [u8],

        /// The current size, in bytes, of this memory.
        size: usize,

        /// A callback which makes portions of `base` accessible for when memory
        /// is grown. Otherwise it's expected that accesses to `base` will
        /// fault.
        make_accessible: fn(*mut u8, usize) -> Result<()>,

        /// Stores the pages in the linear memory that have faulted as guard pages when using the `uffd` feature.
        /// These pages need their protection level reset before the memory can grow.
        #[cfg(all(feature = "uffd", target_os = "linux"))]
        guard_page_faults: Vec<(usize, usize, fn(*mut u8, usize) -> Result<()>)>,
    },

    /// A "dynamic" memory whose data is managed at runtime and lifetime is tied
    /// to this instance.
    Dynamic(Box<dyn RuntimeLinearMemory>),
}

impl Memory {
    /// Create a new dynamic (movable) memory instance for the specified plan.
    pub fn new_dynamic(
        plan: &MemoryPlan,
        creator: &dyn RuntimeMemoryCreator,
        limiter: Option<&mut dyn ResourceLimiter>,
    ) -> Result<Self> {
        let (minimum, maximum) = Self::limit_new(plan, limiter)?;
        Ok(Memory::Dynamic(creator.new_memory(plan, minimum, maximum)?))
    }

    /// Create a new static (immovable) memory instance for the specified plan.
    pub fn new_static(
        plan: &MemoryPlan,
        base: &'static mut [u8],
        make_accessible: fn(*mut u8, usize) -> Result<()>,
        limiter: Option<&mut dyn ResourceLimiter>,
    ) -> Result<Self> {
        let (minimum, maximum) = Self::limit_new(plan, limiter)?;

        let base = match maximum {
            Some(max) if max < base.len() => &mut base[..max],
            _ => base,
        };

        if minimum > 0 {
            make_accessible(base.as_mut_ptr(), minimum)?;
        }

        Ok(Memory::Static {
            base,
            size: minimum,
            make_accessible,
            #[cfg(all(feature = "uffd", target_os = "linux"))]
            guard_page_faults: Vec::new(),
        })
    }

    /// Calls the `limiter`, if specified, to optionally prevent a memory from
    /// being allocated.
    ///
    /// Returns the minimum size and optional maximum size of the memory, in
    /// bytes.
    fn limit_new(
        plan: &MemoryPlan,
        limiter: Option<&mut dyn ResourceLimiter>,
    ) -> Result<(usize, Option<usize>)> {
        // Sanity-check what should already be true from wasm module validation.
        let absolute_max = if plan.memory.memory64 {
            WASM64_MAX_PAGES
        } else {
            WASM32_MAX_PAGES
        };
        assert_le!(plan.memory.minimum, absolute_max);
        assert!(plan.memory.maximum.is_none() || plan.memory.maximum.unwrap() <= absolute_max);

        // This is the absolute possible maximum that the module can try to
        // allocate, which is our entire address space minus a wasm page. That
        // shouldn't ever actually work in terms of an allocation because
        // presumably the kernel wants *something* for itself, but this is used
        // to pass to the `limiter` specified, if present, for a requested size
        // to approximate the scale of the request that the wasm module is
        // making. This is necessary because the limiter works on `usize` bytes
        // whereas we're working with possibly-overflowing `u64` calculations
        // here. To actually faithfully represent the byte requests of modules
        // we'd have to represent things as `u128`, but that's kinda
        // overkill for this purpose.
        let absolute_max = 0usize.wrapping_sub(WASM_PAGE_SIZE);

        // If the minimum memory size overflows the size of our own address
        // space, then we can't satisfy this request, but defer the error to
        // later so the `limiter` can be informed that an effective oom is
        // happening.
        let minimum = plan
            .memory
            .minimum
            .checked_mul(WASM_PAGE_SIZE_U64)
            .and_then(|m| usize::try_from(m).ok());

        // The plan stores the maximum size in units of wasm pages, but we
        // use units of bytes. Unlike for the `minimum` size we silently clamp
        // the effective maximum size to `absolute_max` above if the maximum is
        // too large. This should be ok since as a wasm runtime we get to
        // arbitrarily decide the actual maximum size of memory, regardless of
        // what's actually listed on the memory itself.
        let mut maximum = plan.memory.maximum.map(|max| {
            usize::try_from(max)
                .ok()
                .and_then(|m| m.checked_mul(WASM_PAGE_SIZE))
                .unwrap_or(absolute_max)
        });

        // If this is a 32-bit memory and no maximum is otherwise listed then we
        // need to still specify a maximum size of 4GB. If the host platform is
        // 32-bit then there's no need to limit the maximum this way since no
        // allocation of 4GB can succeed, but for 64-bit platforms this is
        // required to limit memories to 4GB.
        if !plan.memory.memory64 && maximum.is_none() {
            maximum = usize::try_from(1u64 << 32).ok();
        }

        // Inform the limiter what's about to happen. This will let the limiter
        // reject anything if necessary, and this also guarantees that we should
        // call the limiter for all requested memories, even if our `minimum`
        // calculation overflowed. This means that the `minimum` we're informing
        // the limiter is lossy and may not be 100% accurate, but for now the
        // expected uses of `limiter` means that's ok.
        if let Some(limiter) = limiter {
            if !limiter.memory_growing(0, minimum.unwrap_or(absolute_max), maximum) {
                bail!(
                    "memory minimum size of {} pages exceeds memory limits",
                    plan.memory.minimum
                );
            }
        }

        // At this point we need to actually handle overflows, so bail out with
        // an error if we made it this far.
        let minimum = minimum.ok_or_else(|| {
            format_err!(
                "memory minimum size of {} pages exceeds memory limits",
                plan.memory.minimum
            )
        })?;
        Ok((minimum, maximum))
    }

    /// Returns the number of allocated wasm pages.
    pub fn byte_size(&self) -> usize {
        match self {
            Memory::Static { size, .. } => *size,
            Memory::Dynamic(mem) => mem.byte_size(),
        }
    }

    /// Returns the maximum number of pages the memory can grow to at runtime.
    ///
    /// Returns `None` if the memory is unbounded.
    ///
    /// The runtime maximum may not be equal to the maximum from the linear memory's
    /// Wasm type when it is being constrained by an instance allocator.
    pub fn maximum_byte_size(&self) -> Option<usize> {
        match self {
            Memory::Static { base, .. } => Some(base.len()),
            Memory::Dynamic(mem) => mem.maximum_byte_size(),
        }
    }

    /// Returns whether or not the underlying storage of the memory is "static".
    pub(crate) fn is_static(&self) -> bool {
        if let Memory::Static { .. } = self {
            true
        } else {
            false
        }
    }

    /// Grow memory by the specified amount of wasm pages.
    ///
    /// Returns `None` if memory can't be grown by the specified amount
    /// of wasm pages. Returns `Some` with the old size of memory, in bytes, on
    /// successful growth.
    ///
    /// # Safety
    ///
    /// Resizing the memory can reallocate the memory buffer for dynamic memories.
    /// An instance's `VMContext` may have pointers to the memory's base and will
    /// need to be fixed up after growing the memory.
    ///
    /// Generally, prefer using `InstanceHandle::memory_grow`, which encapsulates
    /// this unsafety.
    pub unsafe fn grow(
        &mut self,
        delta_pages: u64,
        limiter: Option<&mut dyn ResourceLimiter>,
    ) -> Option<usize> {
        let old_byte_size = self.byte_size();
        if delta_pages == 0 {
            return Some(old_byte_size);
        }

        let new_byte_size = usize::try_from(delta_pages)
            .ok()?
            .checked_mul(WASM_PAGE_SIZE)?
            .checked_add(old_byte_size)?;
        let maximum = self.maximum_byte_size();

        if let Some(max) = maximum {
            if new_byte_size > max {
                return None;
            }
        }
        if let Some(limiter) = limiter {
            if !limiter.memory_growing(old_byte_size, new_byte_size, maximum) {
                return None;
            }
        }

        #[cfg(all(feature = "uffd", target_os = "linux"))]
        {
            if self.is_static() {
                // Reset any faulted guard pages before growing the memory.
                self.reset_guard_pages().ok()?;
            }
        }

        match self {
            Memory::Static {
                base,
                size,
                make_accessible,
                ..
            } => {
                if new_byte_size > base.len() {
                    return None;
                }

                make_accessible(
                    base.as_mut_ptr().add(old_byte_size),
                    new_byte_size - old_byte_size,
                )
                .ok()?;

                *size = new_byte_size;
            }
            Memory::Dynamic(mem) => mem.grow_to(new_byte_size)?,
        }
        Some(old_byte_size)
    }

    /// Return a `VMMemoryDefinition` for exposing the memory to compiled wasm code.
    pub fn vmmemory(&self) -> VMMemoryDefinition {
        match self {
            Memory::Static { base, size, .. } => VMMemoryDefinition {
                base: base.as_ptr() as *mut _,
                current_length: *size,
            },
            Memory::Dynamic(mem) => mem.vmmemory(),
        }
    }

    /// Records a faulted guard page in a static memory.
    ///
    /// This is used to track faulted guard pages that need to be reset for the uffd feature.
    ///
    /// This function will panic if called on a dynamic memory.
    #[cfg(all(feature = "uffd", target_os = "linux"))]
    pub(crate) fn record_guard_page_fault(
        &mut self,
        page_addr: *mut u8,
        size: usize,
        reset: fn(*mut u8, usize) -> Result<()>,
    ) {
        match self {
            Memory::Static {
                guard_page_faults, ..
            } => {
                guard_page_faults.push((page_addr as usize, size, reset));
            }
            Memory::Dynamic(_) => {
                unreachable!("dynamic memories should not have guard page faults")
            }
        }
    }

    /// Resets the previously faulted guard pages of a static memory.
    ///
    /// This is used to reset the protection of any guard pages that were previously faulted.
    ///
    /// This function will panic if called on a dynamic memory.
    #[cfg(all(feature = "uffd", target_os = "linux"))]
    pub(crate) fn reset_guard_pages(&mut self) -> Result<()> {
        match self {
            Memory::Static {
                guard_page_faults, ..
            } => {
                for (addr, len, reset) in guard_page_faults.drain(..) {
                    reset(addr as *mut u8, len)?;
                }
            }
            Memory::Dynamic(_) => {
                unreachable!("dynamic memories should not have guard page faults")
            }
        }

        Ok(())
    }
}

// The default memory representation is an empty memory that cannot grow.
impl Default for Memory {
    fn default() -> Self {
        Memory::Static {
            base: &mut [],
            size: 0,
            make_accessible: |_, _| unreachable!(),
            #[cfg(all(feature = "uffd", target_os = "linux"))]
            guard_page_faults: Vec::new(),
        }
    }
}
