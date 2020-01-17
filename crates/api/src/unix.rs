//! Unix-specific extension for the `wasmtime` crate.
//!
//! This module is only available on Unix targets, for example Linux and macOS.
//! It is not available on Windows, for example. Note that the import path for
//! this module is `wasmtime::unix::...`, which is intended to emphasize that it
//! is platform-specific.
//!
//! The traits contained in this module are intended to extend various types
//! throughout the `wasmtime` crate with extra functionality that's only
//! available on Unix.

use crate::Instance;

/// Extensions for the [`Instance`] type only available on Unix.
pub trait InstanceExt {
    // TODO: needs more docs?
    /// The signal handler must be
    /// [async-signal-safe](http://man7.org/linux/man-pages/man7/signal-safety.7.html).
    unsafe fn set_signal_handler<H>(&self, handler: H)
    where
        H: 'static + Fn(libc::c_int, *const libc::siginfo_t, *const libc::c_void) -> bool;
}

impl InstanceExt for Instance {
    unsafe fn set_signal_handler<H>(&self, handler: H)
    where
        H: 'static + Fn(libc::c_int, *const libc::siginfo_t, *const libc::c_void) -> bool,
    {
        self.instance_handle.clone().set_signal_handler(handler);
    }
}
