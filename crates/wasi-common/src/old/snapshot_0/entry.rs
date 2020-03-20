use crate::old::snapshot_0::sys::dev_null;
use crate::old::snapshot_0::sys::entry_impl::{
    descriptor_as_oshandle, determine_type_and_access_rights, OsHandle,
};
use crate::old::snapshot_0::wasi::{self, WasiError, WasiResult};
use std::marker::PhantomData;
use std::mem::ManuallyDrop;
use std::ops::{Deref, DerefMut};
use std::path::PathBuf;
use std::{fs, io};

#[derive(Debug)]
pub(crate) enum Descriptor {
    OsHandle(OsHandle),
    Stdin,
    Stdout,
    Stderr,
}

impl Descriptor {
    /// Return a reference to the `OsHandle` treating it as an actual file/dir, and
    /// allowing operations which require an actual file and not just a stream or
    /// socket file descriptor.
    pub(crate) fn as_file(&self) -> WasiResult<&OsHandle> {
        match self {
            Self::OsHandle(file) => Ok(file),
            _ => Err(WasiError::EBADF),
        }
    }

    /// Like `as_file`, but return a mutable reference.
    pub(crate) fn as_file_mut(&mut self) -> WasiResult<&mut OsHandle> {
        match self {
            Self::OsHandle(file) => Ok(file),
            _ => Err(WasiError::EBADF),
        }
    }

    /// Return an `OsHandle`, which may be a stream or socket file descriptor.
    pub(crate) fn as_os_handle<'descriptor>(&'descriptor self) -> OsHandleRef<'descriptor> {
        descriptor_as_oshandle(self)
    }
}

/// An abstraction struct serving as a wrapper for a host `Descriptor` object which requires
/// certain base rights `rights_base` and inheriting rights `rights_inheriting` in order to be
/// accessed correctly.
///
/// Here, the `descriptor` field stores the host `Descriptor` object (such as a file descriptor, or
/// stdin handle), and accessing it can only be done via the provided `Entry::as_descriptor` and
/// `Entry::as_descriptor_mut` methods which require a set of base and inheriting rights to be
/// specified, verifying whether the stored `Descriptor` object is valid for the rights specified.
#[derive(Debug)]
pub(crate) struct Entry {
    pub(crate) file_type: wasi::__wasi_filetype_t,
    descriptor: Descriptor,
    pub(crate) rights_base: wasi::__wasi_rights_t,
    pub(crate) rights_inheriting: wasi::__wasi_rights_t,
    pub(crate) preopen_path: Option<PathBuf>,
    // TODO: directories
}

impl Entry {
    /// Create an Entry with *maximal* possible rights from a given `File`.
    /// If this is not desired, the rights of the resulting `Entry` should
    /// be manually restricted.
    pub(crate) fn from(file: fs::File) -> io::Result<Self> {
        unsafe { determine_type_and_access_rights(&file) }.map(
            |(file_type, rights_base, rights_inheriting)| Self {
                file_type,
                descriptor: Descriptor::OsHandle(OsHandle::from(file)),
                rights_base,
                rights_inheriting,
                preopen_path: None,
            },
        )
    }

    pub(crate) fn duplicate_stdin() -> io::Result<Self> {
        unsafe { determine_type_and_access_rights(&io::stdin()) }.map(
            |(file_type, rights_base, rights_inheriting)| Self {
                file_type,
                descriptor: Descriptor::Stdin,
                rights_base,
                rights_inheriting,
                preopen_path: None,
            },
        )
    }

    pub(crate) fn duplicate_stdout() -> io::Result<Self> {
        unsafe { determine_type_and_access_rights(&io::stdout()) }.map(
            |(file_type, rights_base, rights_inheriting)| Self {
                file_type,
                descriptor: Descriptor::Stdout,
                rights_base,
                rights_inheriting,
                preopen_path: None,
            },
        )
    }

    pub(crate) fn duplicate_stderr() -> io::Result<Self> {
        unsafe { determine_type_and_access_rights(&io::stderr()) }.map(
            |(file_type, rights_base, rights_inheriting)| Self {
                file_type,
                descriptor: Descriptor::Stderr,
                rights_base,
                rights_inheriting,
                preopen_path: None,
            },
        )
    }

    pub(crate) fn null() -> io::Result<Self> {
        Self::from(dev_null()?)
    }

    /// Convert this `Entry` into a host `Descriptor` object provided the specified
    /// `rights_base` and `rights_inheriting` rights are set on this `Entry` object.
    ///
    /// The `Entry` can only be converted into a valid `Descriptor` object if
    /// the specified set of base rights `rights_base`, and inheriting rights `rights_inheriting`
    /// is a subset of rights attached to this `Entry`. The check is performed using
    /// `Entry::validate_rights` method. If the check fails, `Error::ENOTCAPABLE` is returned.
    pub(crate) fn as_descriptor(
        &self,
        rights_base: wasi::__wasi_rights_t,
        rights_inheriting: wasi::__wasi_rights_t,
    ) -> WasiResult<&Descriptor> {
        self.validate_rights(rights_base, rights_inheriting)?;
        Ok(&self.descriptor)
    }

    /// Convert this `Entry` into a mutable host `Descriptor` object provided the specified
    /// `rights_base` and `rights_inheriting` rights are set on this `Entry` object.
    ///
    /// The `Entry` can only be converted into a valid `Descriptor` object if
    /// the specified set of base rights `rights_base`, and inheriting rights `rights_inheriting`
    /// is a subset of rights attached to this `Entry`. The check is performed using
    /// `Entry::validate_rights` method. If the check fails, `Error::ENOTCAPABLE` is returned.
    pub(crate) fn as_descriptor_mut(
        &mut self,
        rights_base: wasi::__wasi_rights_t,
        rights_inheriting: wasi::__wasi_rights_t,
    ) -> WasiResult<&mut Descriptor> {
        self.validate_rights(rights_base, rights_inheriting)?;
        Ok(&mut self.descriptor)
    }

    /// Check if this `Entry` object satisfies the specified base rights `rights_base`, and
    /// inheriting rights `rights_inheriting`; i.e., if rights attached to this `Entry` object
    /// are a superset.
    ///
    /// Upon unsuccessful check, `Error::ENOTCAPABLE` is returned.
    fn validate_rights(
        &self,
        rights_base: wasi::__wasi_rights_t,
        rights_inheriting: wasi::__wasi_rights_t,
    ) -> WasiResult<()> {
        if !self.rights_base & rights_base != 0 || !self.rights_inheriting & rights_inheriting != 0
        {
            Err(WasiError::ENOTCAPABLE)
        } else {
            Ok(())
        }
    }

    /// Test whether this descriptor is considered a tty within WASI.
    /// Note that since WASI itself lacks an `isatty` syscall and relies
    /// on a conservative approximation, we use the same approximation here.
    pub(crate) fn isatty(&self) -> bool {
        self.file_type == wasi::__WASI_FILETYPE_CHARACTER_DEVICE
            && (self.rights_base & (wasi::__WASI_RIGHTS_FD_SEEK | wasi::__WASI_RIGHTS_FD_TELL)) == 0
    }
}

/// This allows an `OsHandle` to be temporarily borrowed from a
/// `Descriptor`. The `Descriptor` continues to own the resource,
/// and `OsHandleRef`'s lifetime parameter ensures that it doesn't
/// outlive the `Descriptor`.
pub(crate) struct OsHandleRef<'descriptor> {
    handle: ManuallyDrop<OsHandle>,
    _ref: PhantomData<&'descriptor Descriptor>,
}

impl<'descriptor> OsHandleRef<'descriptor> {
    pub(crate) fn new(handle: ManuallyDrop<OsHandle>) -> Self {
        OsHandleRef {
            handle,
            _ref: PhantomData,
        }
    }
}

impl<'descriptor> Deref for OsHandleRef<'descriptor> {
    type Target = fs::File;

    fn deref(&self) -> &Self::Target {
        &self.handle
    }
}

impl<'descriptor> DerefMut for OsHandleRef<'descriptor> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.handle
    }
}
