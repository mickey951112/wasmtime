use crate::Error;
use fs_set_times::SetTimes;
use std::ops::Deref;
use system_interface::fs::FileIoExt;

pub trait WasiFile: FileIoExt + SetTimes {
    fn datasync(&self) -> Result<(), Error>;
    fn sync(&self) -> Result<(), Error>;
    fn get_filetype(&self) -> Result<FileType, Error>;
    fn get_fdflags(&self) -> Result<FdFlags, Error>;
    fn set_fdflags(&self, _flags: FdFlags) -> Result<(), Error>;
    fn get_oflags(&self) -> Result<OFlags, Error>;
    fn set_oflags(&self, _flags: OFlags) -> Result<(), Error>;
    fn get_filestat(&self) -> Result<Filestat, Error>;
    fn set_filestat_size(&self, _size: u64) -> Result<(), Error>;
}

#[derive(Debug, Copy, Clone)]
pub enum FileType {
    Directory,
    BlockDevice,
    CharacterDevice,
    RegularFile,
    SocketDgram,
    SocketStream,
    SymbolicLink,
    Unknown,
}

impl From<&cap_std::fs::FileType> for FileType {
    fn from(ft: &cap_std::fs::FileType) -> FileType {
        use cap_fs_ext::FileTypeExt;
        if ft.is_dir() {
            FileType::Directory
        } else if ft.is_symlink() {
            FileType::SymbolicLink
        } else if ft.is_socket() {
            if ft.is_block_device() {
                FileType::SocketDgram
            } else {
                FileType::SocketStream
            }
        } else if ft.is_block_device() {
            FileType::BlockDevice
        } else if ft.is_char_device() {
            FileType::CharacterDevice
        } else if ft.is_file() {
            FileType::RegularFile
        } else {
            FileType::Unknown
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct FdFlags {
    flags: u32,
}

impl FdFlags {
    /// Checks if `other` is a subset of those capabilties:
    pub fn contains(&self, other: &Self) -> bool {
        self.flags & other.flags == other.flags
    }
    pub fn empty() -> FdFlags {
        FdFlags { flags: 0 }
    }
    pub const APPEND: FdFlags = FdFlags { flags: 1 };
    pub const DSYNC: FdFlags = FdFlags { flags: 2 };
    pub const NONBLOCK: FdFlags = FdFlags { flags: 4 };
    pub const RSYNC: FdFlags = FdFlags { flags: 8 };
    pub const SYNC: FdFlags = FdFlags { flags: 16 };
    // etc
}

impl std::ops::BitOr for FdFlags {
    type Output = FdFlags;
    fn bitor(self, rhs: FdFlags) -> FdFlags {
        FdFlags {
            flags: self.flags | rhs.flags,
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct OFlags {
    flags: u32,
}

impl OFlags {
    /// Checks if `other` is a subset of those capabilties:
    pub fn contains(&self, other: &Self) -> bool {
        self.flags & other.flags == other.flags
    }
    pub fn empty() -> Self {
        OFlags { flags: 0 }
    }
    pub const CREATE: OFlags = OFlags { flags: 1 };
    pub const DIRECTORY: OFlags = OFlags { flags: 2 };
    pub const EXCLUSIVE: OFlags = OFlags { flags: 4 };
    pub const TRUNCATE: OFlags = OFlags { flags: 8 };
}
impl std::ops::BitOr for OFlags {
    type Output = OFlags;
    fn bitor(self, rhs: OFlags) -> OFlags {
        OFlags {
            flags: self.flags | rhs.flags,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Filestat {
    pub device_id: u64,
    pub inode: u64,
    pub filetype: FileType,
    pub nlink: u64,
    pub size: u64,
    pub atim: Option<std::time::SystemTime>,
    pub mtim: Option<std::time::SystemTime>,
    pub ctim: Option<std::time::SystemTime>,
}

pub(crate) struct FileEntry {
    caps: FileCaps,
    file: Box<dyn WasiFile>,
}

impl FileEntry {
    pub fn new(caps: FileCaps, file: Box<dyn WasiFile>) -> Self {
        FileEntry { caps, file }
    }

    pub fn get_cap(&self, caps: FileCaps) -> Result<&dyn WasiFile, Error> {
        if self.caps.contains(&caps) {
            Ok(self.file.deref())
        } else {
            Err(Error::FileNotCapable {
                desired: caps,
                has: self.caps,
            })
        }
    }

    pub fn drop_caps_to(&mut self, caps: FileCaps) -> Result<(), Error> {
        if self.caps.contains(&caps) {
            self.caps = caps;
            Ok(())
        } else {
            Err(Error::NotCapable)
        }
    }

    pub fn get_fdstat(&self) -> Result<FdStat, Error> {
        Ok(FdStat {
            filetype: self.file.get_filetype()?,
            caps: self.caps,
            flags: self.file.get_fdflags()?,
        })
    }
}

#[derive(Debug, Clone, Copy)]
pub struct FileCaps {
    flags: u32,
}

impl FileCaps {
    pub fn empty() -> Self {
        FileCaps { flags: 0 }
    }

    /// Checks if `other` is a subset of those capabilties:
    pub fn contains(&self, other: &Self) -> bool {
        self.flags & other.flags == other.flags
    }

    /// Intersection of two sets of flags (bitwise and)
    pub fn intersection(&self, rhs: &Self) -> Self {
        FileCaps {
            flags: self.flags & rhs.flags,
        }
    }

    pub const DATASYNC: Self = FileCaps { flags: 1 };
    pub const READ: Self = FileCaps { flags: 2 };
    pub const SEEK: Self = FileCaps { flags: 4 };
    pub const FDSTAT_SET_FLAGS: Self = FileCaps { flags: 8 };
    pub const SYNC: Self = FileCaps { flags: 16 };
    pub const TELL: Self = FileCaps { flags: 32 };
    pub const WRITE: Self = FileCaps { flags: 64 };
    pub const ADVISE: Self = FileCaps { flags: 128 };
    pub const ALLOCATE: Self = FileCaps { flags: 256 };
    pub const FILESTAT_GET: Self = FileCaps { flags: 512 };
    pub const FILESTAT_SET_SIZE: Self = FileCaps { flags: 1024 };
    pub const FILESTAT_SET_TIMES: Self = FileCaps { flags: 2048 };

    pub fn all() -> FileCaps {
        Self::DATASYNC
            | Self::READ
            | Self::SEEK
            | Self::FDSTAT_SET_FLAGS
            | Self::SYNC
            | Self::TELL
            | Self::WRITE
            | Self::ADVISE
            | Self::ALLOCATE
            | Self::FILESTAT_GET
            | Self::FILESTAT_SET_SIZE
            | Self::FILESTAT_SET_TIMES
    }
}

impl std::ops::BitOr for FileCaps {
    type Output = FileCaps;
    fn bitor(self, rhs: FileCaps) -> FileCaps {
        FileCaps {
            flags: self.flags | rhs.flags,
        }
    }
}

pub struct FdStat {
    pub filetype: FileType,
    pub caps: FileCaps,
    pub flags: FdFlags,
}

impl WasiFile for cap_std::fs::File {
    fn datasync(&self) -> Result<(), Error> {
        self.sync_data()?;
        Ok(())
    }
    fn sync(&self) -> Result<(), Error> {
        self.sync_all()?;
        Ok(())
    }
    fn get_filetype(&self) -> Result<FileType, Error> {
        let meta = self.metadata()?;
        Ok(FileType::from(&meta.file_type()))
    }
    fn get_fdflags(&self) -> Result<FdFlags, Error> {
        // XXX get_fdflags is not implemented but lets lie rather than panic:
        Ok(FdFlags::empty())
    }
    fn set_fdflags(&self, _fdflags: FdFlags) -> Result<(), Error> {
        todo!("set_fdflags is not implemented")
    }
    fn get_oflags(&self) -> Result<OFlags, Error> {
        todo!("get_oflags is not implemented");
    }
    fn set_oflags(&self, _flags: OFlags) -> Result<(), Error> {
        todo!("set_oflags is not implemented");
    }
    fn get_filestat(&self) -> Result<Filestat, Error> {
        let meta = self.metadata()?;
        use cap_fs_ext::MetadataExt;
        Ok(Filestat {
            device_id: meta.dev(),
            inode: meta.ino(),
            filetype: FileType::from(&meta.file_type()),
            nlink: meta.nlink(),
            size: meta.len(),
            atim: meta.accessed().map(|t| Some(t.into_std())).unwrap_or(None),
            mtim: meta.modified().map(|t| Some(t.into_std())).unwrap_or(None),
            ctim: meta.created().map(|t| Some(t.into_std())).unwrap_or(None),
        })
    }
    fn set_filestat_size(&self, size: u64) -> Result<(), Error> {
        self.set_len(size)?;
        Ok(())
    }
}
