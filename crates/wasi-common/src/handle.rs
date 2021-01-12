pub use crate::wasi::types::{
    Advice, Dircookie, Dirent, Fdflags, Fdstat, Filedelta, Filesize, Filestat, Filetype, Fstflags,
    Lookupflags, Oflags, Prestat, PrestatDir, Rights, Size, Timestamp, Whence,
};
use crate::{Error, Result};
use std::any::Any;
use std::fmt;
use std::io::{self, SeekFrom};

/// Represents rights of a `Handle`, either already held or required.
#[derive(Debug, Copy, Clone)]
pub struct HandleRights {
    pub(crate) base: Rights,
    pub(crate) inheriting: Rights,
}

impl HandleRights {
    /// Creates new `HandleRights` instance from `base` and `inheriting` rights.
    pub fn new(base: Rights, inheriting: Rights) -> Self {
        Self { base, inheriting }
    }

    /// Creates new `HandleRights` instance from `base` rights only, keeping
    /// `inheriting` set to none.
    pub fn from_base(base: Rights) -> Self {
        Self {
            base,
            inheriting: Rights::empty(),
        }
    }

    /// Creates new `HandleRights` instance with both `base` and `inheriting`
    /// rights set to none.
    pub fn empty() -> Self {
        Self {
            base: Rights::empty(),
            inheriting: Rights::empty(),
        }
    }

    /// Checks if `other` is a subset of those rights.
    pub fn contains(&self, other: Self) -> bool {
        self.base.contains(other.base) && self.inheriting.contains(other.inheriting)
    }

    /// Returns base rights.
    pub fn base(&self) -> Rights {
        self.base
    }

    /// Returns inheriting rights.
    pub fn inheriting(&self) -> Rights {
        self.inheriting
    }
}

impl fmt::Display for HandleRights {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "HandleRights {{ base: {}, inheriting: {} }}",
            self.base, self.inheriting
        )
    }
}

/// Generic interface for all WASI-compatible handles. We currently group these into two groups:
/// * OS-based resources (actual, real resources): `OsFile`, `OsDir`, `OsOther`, and `Stdio`,
/// * virtual files and directories: VirtualDir`, and `InMemoryFile`.
///
/// # Constructing `Handle`s representing OS-based resources
///
/// Each type of handle can either be constructed directly (see docs entry for a specific handle
/// type such as `OsFile`), or you can let the `wasi_common` crate's machinery work it out
/// automatically for you using `std::convert::TryInto` from `std::fs::File`:
///
/// ```rust,no_run
/// use std::convert::TryInto;
/// use wasi_common::Handle;
/// use std::fs::OpenOptions;
///
/// let some_file = OpenOptions::new().read(true).open("some_file").unwrap();
/// let wasi_handle: Box<dyn Handle> = some_file.try_into().unwrap();
/// ```
pub trait Handle {
    fn as_any(&self) -> &dyn Any;
    fn try_clone(&self) -> io::Result<Box<dyn Handle>>;
    fn get_file_type(&self) -> Filetype;
    fn get_rights(&self) -> HandleRights {
        HandleRights::empty()
    }
    fn set_rights(&self, rights: HandleRights);
    fn is_directory(&self) -> bool {
        self.get_file_type() == Filetype::Directory
    }
    /// Test whether this descriptor is considered a tty within WASI.
    /// Note that since WASI itself lacks an `isatty` syscall and relies
    /// on a conservative approximation, we use the same approximation here.
    fn is_tty(&self) -> bool {
        let file_type = self.get_file_type();
        let rights = self.get_rights();
        let required_rights = HandleRights::from_base(Rights::FD_SEEK | Rights::FD_TELL);
        file_type == Filetype::CharacterDevice && rights.contains(required_rights)
    }
    // TODO perhaps should be a separate trait?
    // FdOps
    fn advise(&self, _advice: Advice, _offset: Filesize, _len: Filesize) -> Result<()> {
        Err(Error::Badf)
    }
    fn allocate(&self, _offset: Filesize, _len: Filesize) -> Result<()> {
        Err(Error::Badf)
    }
    fn datasync(&self) -> Result<()> {
        Err(Error::Inval)
    }
    fn fdstat_get(&self) -> Result<Fdflags> {
        Ok(Fdflags::empty())
    }
    fn fdstat_set_flags(&self, _fdflags: Fdflags) -> Result<()> {
        Err(Error::Badf)
    }
    fn filestat_get(&self) -> Result<Filestat> {
        Err(Error::Badf)
    }
    fn filestat_set_size(&self, _st_size: Filesize) -> Result<()> {
        Err(Error::Badf)
    }
    fn filestat_set_times(
        &self,
        _atim: Timestamp,
        _mtim: Timestamp,
        _fst_flags: Fstflags,
    ) -> Result<()> {
        Err(Error::Badf)
    }
    fn preadv(&self, _buf: &mut [io::IoSliceMut], _offset: u64) -> Result<usize> {
        Err(Error::Badf)
    }
    fn pwritev(&self, _buf: &[io::IoSlice], _offset: u64) -> Result<usize> {
        Err(Error::Badf)
    }
    fn read_vectored(&self, _iovs: &mut [io::IoSliceMut]) -> Result<usize> {
        Err(Error::Badf)
    }
    fn readdir<'a>(
        &'a self,
        _cookie: Dircookie,
    ) -> Result<Box<dyn Iterator<Item = Result<(Dirent, String)>> + 'a>> {
        Err(Error::Badf)
    }
    fn seek(&self, _offset: SeekFrom) -> Result<u64> {
        Err(Error::Badf)
    }
    fn sync(&self) -> Result<()> {
        Ok(())
    }
    fn write_vectored(&self, _iovs: &[io::IoSlice]) -> Result<usize> {
        Err(Error::Badf)
    }
    // TODO perhaps should be a separate trait?
    // PathOps
    fn create_directory(&self, _path: &str) -> Result<()> {
        Err(Error::Acces)
    }
    fn filestat_get_at(&self, _path: &str, _follow: bool) -> Result<Filestat> {
        Err(Error::Acces)
    }
    fn filestat_set_times_at(
        &self,
        _path: &str,
        _atim: Timestamp,
        _mtim: Timestamp,
        _fst_flags: Fstflags,
        _follow: bool,
    ) -> Result<()> {
        Err(Error::Acces)
    }
    fn openat(
        &self,
        _path: &str,
        _read: bool,
        _write: bool,
        _oflags: Oflags,
        _fd_flags: Fdflags,
    ) -> Result<Box<dyn Handle>> {
        Err(Error::Acces)
    }
    fn link(
        &self,
        _old_path: &str,
        _new_handle: Box<dyn Handle>,
        _new_path: &str,
        _follow: bool,
    ) -> Result<()> {
        Err(Error::Acces)
    }
    fn readlink(&self, _path: &str, _buf: &mut [u8]) -> Result<usize> {
        Err(Error::Acces)
    }
    fn readlinkat(&self, _path: &str) -> Result<String> {
        Err(Error::Acces)
    }
    fn remove_directory(&self, _path: &str) -> Result<()> {
        Err(Error::Acces)
    }
    fn rename(&self, _old_path: &str, _new_handle: Box<dyn Handle>, _new_path: &str) -> Result<()> {
        Err(Error::Acces)
    }
    fn symlink(&self, _old_path: &str, _new_path: &str) -> Result<()> {
        Err(Error::Acces)
    }
    fn unlink_file(&self, _path: &str) -> Result<()> {
        Err(Error::Acces)
    }
}

impl From<std::fs::FileType> for Filetype {
    fn from(ftype: std::fs::FileType) -> Self {
        if ftype.is_file() {
            Self::RegularFile
        } else if ftype.is_dir() {
            Self::Directory
        } else if ftype.is_symlink() {
            Self::SymbolicLink
        } else {
            Self::Unknown
        }
    }
}

pub(crate) trait AsBytes {
    fn as_bytes(&self) -> Result<Vec<u8>>;
}

impl AsBytes for Dirent {
    fn as_bytes(&self) -> Result<Vec<u8>> {
        use std::convert::TryInto;
        use wiggle::GuestType;

        assert_eq!(
            Self::guest_size(),
            std::mem::size_of::<Self>() as _,
            "guest repr of Dirent and host repr should match"
        );

        let offset = Self::guest_size().try_into()?;
        let mut bytes: Vec<u8> = Vec::with_capacity(offset);
        bytes.resize(offset, 0);
        let ptr = bytes.as_mut_ptr() as *mut Self;
        unsafe { ptr.write_unaligned(self.clone()) };
        Ok(bytes)
    }
}

pub(crate) trait RightsExt: Sized {
    fn block_device_base() -> Self;
    fn block_device_inheriting() -> Self;
    fn character_device_base() -> Self;
    fn character_device_inheriting() -> Self;
    fn directory_base() -> Self;
    fn directory_inheriting() -> Self;
    fn regular_file_base() -> Self;
    fn regular_file_inheriting() -> Self;
    fn socket_base() -> Self;
    fn socket_inheriting() -> Self;
    fn tty_base() -> Self;
    fn tty_inheriting() -> Self;
}

impl RightsExt for Rights {
    // Block and character device interaction is outside the scope of
    // WASI. Simply allow everything.
    fn block_device_base() -> Self {
        Self::all()
    }
    fn block_device_inheriting() -> Self {
        Self::all()
    }
    fn character_device_base() -> Self {
        Self::all()
    }
    fn character_device_inheriting() -> Self {
        Self::all()
    }

    // Only allow directory operations on directories. Directories can only
    // yield file descriptors to other directories and files.
    fn directory_base() -> Self {
        Self::FD_FDSTAT_SET_FLAGS
            | Self::FD_SYNC
            | Self::FD_ADVISE
            | Self::PATH_CREATE_DIRECTORY
            | Self::PATH_CREATE_FILE
            | Self::PATH_LINK_SOURCE
            | Self::PATH_LINK_TARGET
            | Self::PATH_OPEN
            | Self::FD_READDIR
            | Self::PATH_READLINK
            | Self::PATH_RENAME_SOURCE
            | Self::PATH_RENAME_TARGET
            | Self::PATH_FILESTAT_GET
            | Self::PATH_FILESTAT_SET_SIZE
            | Self::PATH_FILESTAT_SET_TIMES
            | Self::FD_FILESTAT_GET
            | Self::FD_FILESTAT_SET_TIMES
            | Self::PATH_SYMLINK
            | Self::PATH_UNLINK_FILE
            | Self::PATH_REMOVE_DIRECTORY
            | Self::POLL_FD_READWRITE
    }
    fn directory_inheriting() -> Self {
        Self::all() ^ Self::SOCK_SHUTDOWN
    }

    // Operations that apply to regular files.
    fn regular_file_base() -> Self {
        Self::FD_DATASYNC
            | Self::FD_READ
            | Self::FD_SEEK
            | Self::FD_FDSTAT_SET_FLAGS
            | Self::FD_SYNC
            | Self::FD_TELL
            | Self::FD_WRITE
            | Self::FD_ADVISE
            | Self::FD_ALLOCATE
            | Self::FD_FILESTAT_GET
            | Self::FD_FILESTAT_SET_SIZE
            | Self::FD_FILESTAT_SET_TIMES
            | Self::POLL_FD_READWRITE
    }
    fn regular_file_inheriting() -> Self {
        Self::empty()
    }

    // Operations that apply to sockets and socket pairs.
    fn socket_base() -> Self {
        Self::FD_READ
            | Self::FD_FDSTAT_SET_FLAGS
            | Self::FD_WRITE
            | Self::FD_FILESTAT_GET
            | Self::POLL_FD_READWRITE
            | Self::SOCK_SHUTDOWN
    }
    fn socket_inheriting() -> Self {
        Self::all()
    }

    // Operations that apply to TTYs.
    fn tty_base() -> Self {
        Self::FD_READ
            | Self::FD_FDSTAT_SET_FLAGS
            | Self::FD_WRITE
            | Self::FD_FILESTAT_GET
            | Self::POLL_FD_READWRITE
    }
    fn tty_inheriting() -> Self {
        Self::empty()
    }
}
pub(crate) const DIRCOOKIE_START: Dircookie = 0;
