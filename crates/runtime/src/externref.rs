//! # `VMExternRef`
//!
//! `VMExternRef` is a reference-counted box for any kind of data that is
//! external and opaque to running Wasm. Sometimes it might hold a Wasmtime
//! thing, other times it might hold something from a Wasmtime embedder and is
//! opaque even to us. It is morally equivalent to `Rc<dyn Any>` in Rust, but
//! additionally always fits in a pointer-sized word. `VMExternRef` is
//! non-nullable, but `Option<VMExternRef>` is a null pointer.
//!
//! The one part of `VMExternRef` that can't ever be opaque to us is the
//! reference count. Even when we don't know what's inside an `VMExternRef`, we
//! need to be able to manipulate its reference count as we add and remove
//! references to it. And we need to do this from compiled Wasm code, so it must
//! be `repr(C)`!
//!
//! ## Memory Layout
//!
//! `VMExternRef` itself is just a pointer to an `VMExternData`, which holds the
//! opaque, boxed value, its reference count, and its vtable pointer.
//!
//! The `VMExternData` struct is *preceded* by the dynamically-sized value boxed
//! up and referenced by one or more `VMExternRef`s:
//!
//! ```ignore
//!      ,-------------------------------------------------------.
//!      |                                                       |
//!      V                                                       |
//!     +----------------------------+-----------+-----------+   |
//!     | dynamically-sized value... | ref_count | value_ptr |---'
//!     +----------------------------+-----------+-----------+
//!                                  | VMExternData          |
//!                                  +-----------------------+
//!                                   ^
//! +-------------+                   |
//! | VMExternRef |-------------------+
//! +-------------+                   |
//!                                   |
//! +-------------+                   |
//! | VMExternRef |-------------------+
//! +-------------+                   |
//!                                   |
//!   ...                            ===
//!                                   |
//! +-------------+                   |
//! | VMExternRef |-------------------'
//! +-------------+
//! ```
//!
//! The `value_ptr` member always points backwards to the start of the
//! dynamically-sized value (which is also the start of the heap allocation for
//! this value-and-`VMExternData` pair). Because it is a `dyn` pointer, it is
//! fat, and also points to the value's `Any` vtable.
//!
//! The boxed value and the `VMExternRef` footer are held a single heap
//! allocation. The layout described above is used to make satisfying the
//! value's alignment easy: we just need to ensure that the heap allocation used
//! to hold everything satisfies its alignment. It also ensures that we don't
//! need a ton of excess padding between the `VMExternData` and the value for
//! values with large alignment.
//!
//! ## Reference Counting Protocol and Wasm Functions
//!
//! Currently, `VMExternRef`s passed into compiled Wasm functions have move
//! semantics: the host code gives up ownership and does not decrement the
//! reference count. Similarly, `VMExternRef`s returned from compiled Wasm
//! functions also have move semantics: host code takes ownership and the
//! reference count is not incremented.
//!
//! This works well when a reference is passed into Wasm and then passed back
//! out again. However, if a reference is passed into Wasm, but not passed back
//! out, then the reference is leaked. This is only a temporary state, and
//! follow up work will leverage stack maps to fix this issue. Follow
//! https://github.com/bytecodealliance/wasmtime/issues/929 to keep an eye on
//! this.

use std::alloc::Layout;
use std::any::Any;
use std::cell::UnsafeCell;
use std::cmp::Ordering;
use std::hash::Hasher;
use std::mem;
use std::ops::Deref;
use std::ptr::{self, NonNull};

/// An external reference to some opaque data.
///
/// `VMExternRef`s dereference to their underlying opaque data as `dyn Any`.
///
/// Unlike the `externref` in the Wasm spec, `VMExternRef`s are non-nullable,
/// and always point to a valid value. You may use `Option<VMExternRef>` to
/// represent nullable references, and `Option<VMExternRef>` is guaranteed to
/// have the same size and alignment as a raw pointer, with `None` represented
/// with the null pointer.
///
/// `VMExternRef`s are reference counted, so cloning is a cheap, shallow
/// operation. It also means they are inherently shared, so you may not get a
/// mutable, exclusive reference to their inner contents, only a shared,
/// immutable reference. You may use interior mutability with `RefCell` or
/// `Mutex` to work around this restriction, if necessary.
///
/// `VMExternRef`s have pointer-equality semantics, not structural-equality
/// semantics. Given two `VMExternRef`s `a` and `b`, `a == b` only if `a` and
/// `b` point to the same allocation. `a` and `b` are considered not equal, even
/// if `a` and `b` are two different identical copies of the same data, if they
/// are in two different allocations. The hashing and ordering implementations
/// also only operate on the pointer.
///
/// # Example
///
/// ```
/// # fn foo() -> Result<(), Box<dyn std::error::Error>> {
/// use std::cell::RefCell;
/// use wasmtime_runtime::VMExternRef;
///
/// // Open a file. Wasm doesn't know about files, but we can let Wasm instances
/// // work with files via opaque `externref` handles.
/// let file = std::fs::File::create("some/file/path")?;
///
/// // Wrap the file up as an `VMExternRef` that can be passed to Wasm.
/// let extern_ref_to_file = VMExternRef::new(RefCell::new(file));
///
/// // `VMExternRef`s dereference to `dyn Any`, so you can use `Any` methods to
/// // perform runtime type checks and downcasts.
///
/// assert!(extern_ref_to_file.is::<RefCell<std::fs::File>>());
/// assert!(!extern_ref_to_file.is::<String>());
///
/// if let Some(file) = extern_ref_to_file.downcast_ref::<RefCell<std::fs::File>>() {
///     use std::io::Write;
///     let mut file = file.borrow_mut();
///     writeln!(&mut file, "Hello, `VMExternRef`!")?;
/// }
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
#[repr(transparent)]
pub struct VMExternRef(NonNull<VMExternData>);

#[repr(C)]
struct VMExternData {
    // Implicit, dynamically-sized member that always preceded an
    // `VMExternData`.
    //
    // value: [u8],
    //
    /// The reference count for this `VMExternData` and value. When it reaches
    /// zero, we can safely destroy the value and free this heap
    /// allocation. This is an `UnsafeCell`, rather than plain `Cell`, because
    /// it can be modified by compiled Wasm code.
    ///
    /// Note: this field's offset must be kept in sync with
    /// `wasmtime_environ::VMOffsets::vm_extern_data_ref_count()` which is
    /// currently always zero.
    ref_count: UnsafeCell<usize>,

    /// Always points to the implicit, dynamically-sized `value` member that
    /// precedes this `VMExternData`.
    value_ptr: NonNull<dyn Any>,
}

impl Clone for VMExternRef {
    #[inline]
    fn clone(&self) -> VMExternRef {
        self.extern_data().increment_ref_count();
        VMExternRef(self.0)
    }
}

impl Drop for VMExternRef {
    #[inline]
    fn drop(&mut self) {
        let data = self.extern_data();
        data.decrement_ref_count();
        if data.get_ref_count() == 0 {
            // Drop our live reference to `data` before we drop it itself.
            drop(data);
            unsafe {
                VMExternData::drop_and_dealloc(self.0);
            }
        }
    }
}

impl VMExternData {
    /// Get the `Layout` for a value with the given size and alignment, and the
    /// offset within that layout where the `VMExternData` footer resides.
    ///
    /// This doesn't take a `value: &T` because `VMExternRef::new_with` hasn't
    /// constructed a `T` value yet, and it isn't generic over `T` because
    /// `VMExternData::drop_and_dealloc` doesn't know what `T` to use, and has
    /// to use `std::mem::{size,align}_of_val` instead.
    unsafe fn layout_for(value_size: usize, value_align: usize) -> (Layout, usize) {
        let extern_data_size = mem::size_of::<VMExternData>();
        let extern_data_align = mem::align_of::<VMExternData>();

        let value_and_padding_size = round_up_to_align(value_size, extern_data_align).unwrap();

        let alloc_align = std::cmp::max(value_align, extern_data_align);
        let alloc_size = value_and_padding_size + extern_data_size;

        debug_assert!(Layout::from_size_align(alloc_size, alloc_align).is_ok());
        (
            Layout::from_size_align_unchecked(alloc_size, alloc_align),
            value_and_padding_size,
        )
    }

    /// Drop the inner value and then free this `VMExternData` heap allocation.
    unsafe fn drop_and_dealloc(mut data: NonNull<VMExternData>) {
        // Note: we introduce a block scope so that we drop the live
        // reference to the data before we free the heap allocation it
        // resides within after this block.
        let (alloc_ptr, layout) = {
            let data = data.as_mut();
            debug_assert_eq!(data.get_ref_count(), 0);

            // Same thing, but for the dropping the reference to `value` before
            // we drop it itself.
            let (layout, _) = {
                let value = data.value_ptr.as_ref();
                Self::layout_for(mem::size_of_val(value), mem::align_of_val(value))
            };

            ptr::drop_in_place(data.value_ptr.as_ptr());
            let alloc_ptr = data.value_ptr.cast::<u8>();

            (alloc_ptr, layout)
        };

        ptr::drop_in_place(data.as_ptr());
        std::alloc::dealloc(alloc_ptr.as_ptr(), layout);
    }

    #[inline]
    fn get_ref_count(&self) -> usize {
        unsafe { *self.ref_count.get() }
    }

    #[inline]
    fn increment_ref_count(&self) {
        unsafe {
            let count = self.ref_count.get();
            *count += 1;
        }
    }

    #[inline]
    fn decrement_ref_count(&self) {
        unsafe {
            let count = self.ref_count.get();
            *count -= 1;
        }
    }
}

#[inline]
fn round_up_to_align(n: usize, align: usize) -> Option<usize> {
    debug_assert!(align.is_power_of_two());
    let align_minus_one = align - 1;
    Some(n.checked_add(align_minus_one)? & !align_minus_one)
}

impl VMExternRef {
    /// Wrap the given value inside an `VMExternRef`.
    pub fn new<T>(value: T) -> VMExternRef
    where
        T: 'static + Any,
    {
        VMExternRef::new_with(|| value)
    }

    /// Construct a new `VMExternRef` in place by invoking `make_value`.
    pub fn new_with<T>(make_value: impl FnOnce() -> T) -> VMExternRef
    where
        T: 'static + Any,
    {
        unsafe {
            let (layout, footer_offset) =
                VMExternData::layout_for(mem::size_of::<T>(), mem::align_of::<T>());

            let alloc_ptr = std::alloc::alloc(layout);
            let alloc_ptr = NonNull::new(alloc_ptr).unwrap_or_else(|| {
                std::alloc::handle_alloc_error(layout);
            });

            let value_ptr = alloc_ptr.cast::<T>();
            ptr::write(value_ptr.as_ptr(), make_value());

            let value_ref: &T = value_ptr.as_ref();
            let value_ref: &dyn Any = value_ref as _;
            let value_ptr: *const dyn Any = value_ref as _;
            let value_ptr: *mut dyn Any = value_ptr as _;
            let value_ptr = NonNull::new_unchecked(value_ptr);

            let extern_data_ptr =
                alloc_ptr.cast::<u8>().as_ptr().add(footer_offset) as *mut VMExternData;
            ptr::write(
                extern_data_ptr,
                VMExternData {
                    ref_count: UnsafeCell::new(1),
                    value_ptr,
                },
            );

            VMExternRef(NonNull::new_unchecked(extern_data_ptr))
        }
    }

    /// Turn this `VMExternRef` into a raw, untyped pointer.
    ///
    /// This forgets `self` and does *not* decrement the reference count on the
    /// pointed-to data.
    ///
    /// This `VMExternRef` may be recovered with `VMExternRef::from_raw`.
    pub fn into_raw(self) -> *mut u8 {
        let ptr = self.0.cast::<u8>().as_ptr();
        mem::forget(self);
        ptr
    }

    /// Create a `VMExternRef` from a pointer returned from a previous call to
    /// `VMExternRef::into_raw`.
    ///
    /// # Safety
    ///
    /// Wildly unsafe to use with anything other than the result of a previous
    /// `into_raw` call!
    ///
    /// This method does *not* increment the reference count on the pointed-to
    /// data, so `from_raw` must be called at most *once* on the result of a
    /// previous `into_raw` call. (Ideally, every `into_raw` is later followed
    /// by a `from_raw`, but it is technically memory safe to never call
    /// `from_raw` after `into_raw`: it will leak the pointed-to value, which is
    /// memory safe).
    pub unsafe fn from_raw(ptr: *mut u8) -> Self {
        debug_assert!(!ptr.is_null());
        VMExternRef(NonNull::new_unchecked(ptr).cast())
    }

    #[inline]
    fn extern_data(&self) -> &VMExternData {
        unsafe { self.0.as_ref() }
    }
}

/// Methods that would normally be trait implementations, but aren't to avoid
/// potential footguns around `VMExternRef`'s pointer-equality semantics.
///
/// Note that none of these methods are on `&self`, they all require a
/// fully-qualified `VMExternRef::foo(my_ref)` invocation.
impl VMExternRef {
    /// Check whether two `VMExternRef`s point to the same inner allocation.
    ///
    /// Note that this uses pointer-equality semantics, not structural-equality
    /// semantics, and so only pointers are compared, and doesn't use any `Eq`
    /// or `PartialEq` implementation of the pointed-to values.
    #[inline]
    pub fn eq(a: &Self, b: &Self) -> bool {
        ptr::eq(a.0.as_ptr() as *const _, b.0.as_ptr() as *const _)
    }

    /// Hash a given `VMExternRef`.
    ///
    /// Note that this just hashes the pointer to the inner value, it does *not*
    /// use the inner value's `Hash` implementation (if any).
    #[inline]
    pub fn hash<H>(externref: &Self, hasher: &mut H)
    where
        H: Hasher,
    {
        ptr::hash(externref.0.as_ptr() as *const _, hasher);
    }

    /// Compare two `VMExternRef`s.
    ///
    /// Note that this uses pointer-equality semantics, not structural-equality
    /// semantics, and so only pointers are compared, and doesn't use any `Cmp`
    /// or `PartialCmp` implementation of the pointed-to values.
    #[inline]
    pub fn cmp(a: &Self, b: &Self) -> Ordering {
        let a = a.0.as_ptr() as usize;
        let b = b.0.as_ptr() as usize;
        a.cmp(&b)
    }
}

impl Deref for VMExternRef {
    type Target = dyn Any;

    fn deref(&self) -> &dyn Any {
        unsafe { self.extern_data().value_ptr.as_ref() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::convert::TryInto;

    #[test]
    fn extern_ref_is_pointer_sized_and_aligned() {
        assert_eq!(mem::size_of::<VMExternRef>(), mem::size_of::<*mut ()>());
        assert_eq!(mem::align_of::<VMExternRef>(), mem::align_of::<*mut ()>());
        assert_eq!(
            mem::size_of::<Option<VMExternRef>>(),
            mem::size_of::<*mut ()>()
        );
        assert_eq!(
            mem::align_of::<Option<VMExternRef>>(),
            mem::align_of::<*mut ()>()
        );
    }

    #[test]
    fn ref_count_is_at_correct_offset() {
        let s = "hi";
        let s: &dyn Any = &s as _;
        let s: *const dyn Any = s as _;
        let s: *mut dyn Any = s as _;

        let extern_data = VMExternData {
            ref_count: UnsafeCell::new(0),
            value_ptr: NonNull::new(s).unwrap(),
        };

        let extern_data_ptr = &extern_data as *const _;
        let ref_count_ptr = &extern_data.ref_count as *const _;

        let actual_offset = (ref_count_ptr as usize) - (extern_data_ptr as usize);

        assert_eq!(
            wasmtime_environ::VMOffsets::vm_extern_data_ref_count(),
            actual_offset.try_into().unwrap(),
        );
    }
}
