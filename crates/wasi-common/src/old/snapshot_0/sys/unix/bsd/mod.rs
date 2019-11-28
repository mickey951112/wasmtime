pub(crate) mod filetime;
pub(crate) mod hostcalls_impl;
pub(crate) mod oshandle;

pub(crate) mod fdentry_impl {
    use crate::old::snapshot_0::{sys::host_impl, Result};
    use std::os::unix::prelude::AsRawFd;

    pub(crate) unsafe fn isatty(fd: &impl AsRawFd) -> Result<bool> {
        let res = libc::isatty(fd.as_raw_fd());
        if res == 1 {
            // isatty() returns 1 if fd is an open file descriptor referring to a terminal...
            Ok(true)
        } else {
            // ... otherwise 0 is returned, and errno is set to indicate the error.
            match nix::errno::Errno::last() {
                nix::errno::Errno::ENOTTY => Ok(false),
                x => Err(host_impl::errno_from_nix(x)),
            }
        }
    }
}

pub(crate) mod host_impl {
    use crate::old::snapshot_0::{wasi, Result};
    use std::convert::TryFrom;

    pub(crate) const O_RSYNC: nix::fcntl::OFlag = nix::fcntl::OFlag::O_SYNC;

    pub(crate) fn stdev_from_nix(dev: nix::libc::dev_t) -> Result<wasi::__wasi_device_t> {
        wasi::__wasi_device_t::try_from(dev).map_err(Into::into)
    }

    pub(crate) fn stino_from_nix(ino: nix::libc::ino_t) -> Result<wasi::__wasi_inode_t> {
        wasi::__wasi_device_t::try_from(ino).map_err(Into::into)
    }
}
