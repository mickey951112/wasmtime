//! Low-level abstraction for allocating and managing zero-filled pages
//! of memory.

use errno;
use libc;
use region;
use std::mem;
use std::ptr;
use std::slice;
use std::string::String;

/// Round `size` up to the nearest multiple of `page_size`.
fn round_up_to_page_size(size: usize, page_size: usize) -> usize {
    (size + (page_size - 1)) & !(page_size - 1)
}

/// A simple struct consisting of a page-aligned pointer to page-aligned
/// and initially-zeroed memory and a length.
pub struct Mmap {
    ptr: *mut u8,
    len: usize,
}

impl Mmap {
    pub fn new() -> Self {
        Self {
            ptr: ptr::null_mut(),
            len: 0,
        }
    }

    /// Create a new `Mmap` pointing to at least `size` bytes of memory,
    /// suitably sized and aligned for memory protection.
    #[cfg(not(target_os = "windows"))]
    pub fn with_size(size: usize) -> Result<Self, String> {
        let page_size = region::page::size();
        let alloc_size = round_up_to_page_size(size, page_size);
        unsafe {
            let ptr = libc::mmap(
                ptr::null_mut(),
                alloc_size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANON,
                -1,
                0,
            );
            if mem::transmute::<_, isize>(ptr) != -1isize {
                Ok(Self {
                    ptr: ptr as *mut u8,
                    len: alloc_size,
                })
            } else {
                Err(errno::errno().to_string())
            }
        }
    }

    #[cfg(target_os = "windows")]
    pub fn with_size(size: usize) -> Result<Self, String> {
        use winapi::um::memoryapi::VirtualAlloc;
        use winapi::um::winnt::{MEM_COMMIT, MEM_RESERVE, PAGE_READWRITE};

        let page_size = region::page::size();

        // VirtualAlloc always rounds up to the next multiple of the page size
        let ptr = unsafe {
            VirtualAlloc(
                ptr::null_mut(),
                size,
                MEM_COMMIT | MEM_RESERVE,
                PAGE_READWRITE,
            )
        };
        if !ptr.is_null() {
            Ok(Self {
                ptr: ptr as *mut u8,
                len: round_up_to_page_size(size, page_size),
            })
        } else {
            Err(errno::errno().to_string())
        }
    }

    pub fn as_slice(&self) -> &[u8] {
        unsafe { slice::from_raw_parts(self.ptr, self.len) }
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        unsafe { slice::from_raw_parts_mut(self.ptr, self.len) }
    }

    pub fn as_ptr(&self) -> *const u8 {
        self.ptr
    }

    pub fn as_mut_ptr(&mut self) -> *mut u8 {
        self.ptr
    }

    pub fn len(&self) -> usize {
        self.len
    }
}

impl Drop for Mmap {
    #[cfg(not(target_os = "windows"))]
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            let r = unsafe { libc::munmap(self.ptr as *mut libc::c_void, self.len) };
            assert_eq!(r, 0, "munmap failed: {}", errno::errno());
        }
    }

    #[cfg(target_os = "windows")]
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            use winapi::um::memoryapi::VirtualFree;
            use winapi::um::winnt::MEM_RELEASE;
            let r = unsafe { VirtualFree(self.ptr, self.len, MEM_RELEASE) };
            assert_eq!(r, 0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_round_up_to_page_size() {
        assert_eq!(round_up_to_page_size(0, 4096), 0);
        assert_eq!(round_up_to_page_size(1, 4096), 4096);
        assert_eq!(round_up_to_page_size(4096, 4096), 4096);
        assert_eq!(round_up_to_page_size(4097, 4096), 8192);
    }
}
