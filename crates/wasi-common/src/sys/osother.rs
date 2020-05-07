use super::sys_impl::oshandle::RawOsHandle;
use super::{fd, AsFile};
use crate::handle::{Handle, HandleRights};
use crate::sandboxed_tty_writer::SandboxedTTYWriter;
use crate::wasi::types::{self, Filetype};
use crate::wasi::Result;
use std::any::Any;
use std::cell::Cell;
use std::fs::File;
use std::io::{self, Read, Write};
use std::ops::Deref;

pub(crate) trait OsOtherExt {
    /// Create `OsOther` as `dyn Handle` from null device.
    fn from_null() -> io::Result<Box<dyn Handle>>;
}

/// `OsOther` is something of a catch-all for everything not covered with the specific handle
/// types (`OsFile`, `OsDir`, `Stdio`). It currently encapsulates handles such as OS pipes,
/// sockets, streams, etc. As such, when redirecting stdio within `WasiCtxBuilder`, the redirected
/// pipe should be encapsulated within this instance _and not_ `OsFile` which represents a regular
/// OS file.
#[derive(Debug)]
pub(crate) struct OsOther {
    file_type: Filetype,
    rights: Cell<HandleRights>,
    handle: RawOsHandle,
}

impl OsOther {
    pub(super) fn new(file_type: Filetype, rights: HandleRights, handle: RawOsHandle) -> Self {
        let rights = Cell::new(rights);
        Self {
            file_type,
            rights,
            handle,
        }
    }
}

impl Deref for OsOther {
    type Target = RawOsHandle;

    fn deref(&self) -> &Self::Target {
        &self.handle
    }
}

impl Handle for OsOther {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn try_clone(&self) -> io::Result<Box<dyn Handle>> {
        let file_type = self.file_type;
        let handle = self.handle.try_clone()?;
        let rights = self.rights.clone();
        Ok(Box::new(Self {
            file_type,
            rights,
            handle,
        }))
    }
    fn get_file_type(&self) -> Filetype {
        self.file_type
    }
    fn get_rights(&self) -> HandleRights {
        self.rights.get()
    }
    fn set_rights(&self, new_rights: HandleRights) {
        self.rights.set(new_rights)
    }
    // FdOps
    fn fdstat_get(&self) -> Result<types::Fdflags> {
        fd::fdstat_get(&*self.as_file()?)
    }
    fn fdstat_set_flags(&self, fdflags: types::Fdflags) -> Result<()> {
        if let Some(handle) = fd::fdstat_set_flags(&*self.as_file()?, fdflags)? {
            self.handle.update_from(handle);
        }
        Ok(())
    }
    fn read_vectored(&self, iovs: &mut [io::IoSliceMut]) -> Result<usize> {
        let nread = self.as_file()?.read_vectored(iovs)?;
        Ok(nread)
    }
    fn write_vectored(&self, iovs: &[io::IoSlice]) -> Result<usize> {
        let mut fd: &File = &*self.as_file()?;
        let nwritten = if self.is_tty() {
            SandboxedTTYWriter::new(&mut fd).write_vectored(&iovs)?
        } else {
            fd.write_vectored(iovs)?
        };
        Ok(nwritten)
    }
}
