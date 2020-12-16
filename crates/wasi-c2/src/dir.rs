// this file is extremely wip
#![allow(dead_code, unused_variables)]
use crate::error::Error;
use crate::file::{FileCaps, OFlags, WasiFile};
use std::ops::Deref;
use std::path::{Path, PathBuf};

pub trait WasiDir {
    fn open_file(
        &self,
        symlink_follow: bool,
        path: &str,
        oflags: OFlags,
        caps: FileCaps,
    ) -> Result<Box<dyn WasiFile>, Error>;
    fn open_dir(&self, symlink_follow: bool, path: &str) -> Result<Box<dyn WasiDir>, Error>;
    fn create_dir(&self, path: &str) -> Result<(), Error>;
    fn readdir(
        &self,
        cursor: ReaddirCursor,
    ) -> Result<Box<dyn Iterator<Item = Result<(ReaddirEntity, String), Error>>>, Error>;
    fn symlink(&self, old_path: &str, new_path: &str) -> Result<(), Error>;
    fn remove_dir(&self, path: &str) -> Result<(), Error>;
    fn unlink_file(&self, path: &str) -> Result<(), Error>;
}

pub(crate) struct DirEntry {
    caps: DirCaps,
    file_caps: FileCaps,
    preopen_path: Option<PathBuf>, // precondition: PathBuf is valid unicode
    dir: Box<dyn WasiDir>,
}

impl DirEntry {
    pub fn new(
        caps: DirCaps,
        file_caps: FileCaps,
        preopen_path: Option<PathBuf>,
        dir: Box<dyn WasiDir>,
    ) -> Self {
        DirEntry {
            caps,
            file_caps,
            preopen_path,
            dir,
        }
    }
    pub fn get_cap(&self, caps: DirCaps) -> Result<&dyn WasiDir, Error> {
        if self.caps.contains(&caps) {
            Ok(self.dir.deref())
        } else {
            Err(Error::DirNotCapable {
                desired: caps,
                has: self.caps,
            })
        }
    }
    pub fn drop_caps_to(&mut self, caps: DirCaps, file_caps: FileCaps) -> Result<(), Error> {
        if self.caps.contains(&caps) && self.file_caps.contains(&file_caps) {
            self.caps = caps;
            self.file_caps = file_caps;
            Ok(())
        } else {
            Err(Error::NotCapable)
        }
    }
    pub fn child_dir_caps(&self, desired_caps: DirCaps) -> DirCaps {
        self.caps.intersection(&desired_caps)
    }
    pub fn child_file_caps(&self, desired_caps: FileCaps) -> FileCaps {
        self.file_caps.intersection(&desired_caps)
    }
    pub fn get_dirstat(&self) -> DirStat {
        DirStat {
            dir_caps: self.caps,
            file_caps: self.file_caps,
        }
    }
    pub fn preopen_path(&self) -> &Option<PathBuf> {
        &self.preopen_path
    }
}

#[derive(Debug, Clone, Copy)]
pub struct DirCaps {
    flags: u32,
}

impl DirCaps {
    pub fn empty() -> Self {
        DirCaps { flags: 0 }
    }

    /// Checks if `other` is a subset of those capabilties:
    pub fn contains(&self, other: &Self) -> bool {
        self.flags & other.flags == other.flags
    }

    /// Intersection of two sets of flags (bitwise and)
    pub fn intersection(&self, rhs: &Self) -> Self {
        DirCaps {
            flags: self.flags & rhs.flags,
        }
    }
    pub const CREATE_DIRECTORY: Self = DirCaps { flags: 1 };
    pub const CREATE_FILE: Self = DirCaps { flags: 2 };
    pub const LINK_SOURCE: Self = DirCaps { flags: 4 };
    pub const LINK_TARGET: Self = DirCaps { flags: 8 };
    pub const OPEN: Self = DirCaps { flags: 16 };
    pub const READDIR: Self = DirCaps { flags: 32 };
    pub const READLINK: Self = DirCaps { flags: 64 };
    pub const RENAME_SOURCE: Self = DirCaps { flags: 128 };
    pub const RENAME_TARGET: Self = DirCaps { flags: 256 };
    pub const SYMLINK: Self = DirCaps { flags: 512 };
    pub const REMOVE_DIRECTORY: Self = DirCaps { flags: 1024 };
    pub const UNLINK_FILE: Self = DirCaps { flags: 2048 };

    // Missing that are in wasi-common directory_base:
    // FD_FDSTAT_SET_FLAGS
    // FD_SYNC
    // FD_ADVISE
    // PATH_FILESTAT_GET
    // PATH_FILESTAT_SET_SIZE
    // PATH_FILESTAT_SET_TIMES
    // FD_FILESTAT_GET
    // FD_FILESTAT_SET_TIMES

    pub fn all() -> DirCaps {
        Self::CREATE_DIRECTORY
            | Self::CREATE_FILE
            | Self::LINK_SOURCE
            | Self::LINK_TARGET
            | Self::OPEN
            | Self::READDIR
            | Self::READLINK
            | Self::RENAME_SOURCE
            | Self::RENAME_TARGET
            | Self::SYMLINK
            | Self::REMOVE_DIRECTORY
            | Self::UNLINK_FILE
    }
}

impl std::ops::BitOr for DirCaps {
    type Output = DirCaps;
    fn bitor(self, rhs: DirCaps) -> DirCaps {
        DirCaps {
            flags: self.flags | rhs.flags,
        }
    }
}

pub struct DirStat {
    pub file_caps: FileCaps,
    pub dir_caps: DirCaps,
}

pub trait TableDirExt {
    fn is_preopen(&self, fd: u32) -> bool;
}

impl TableDirExt for crate::table::Table {
    fn is_preopen(&self, fd: u32) -> bool {
        if self.is::<DirEntry>(fd) {
            let dir_entry: std::cell::Ref<DirEntry> = self.get(fd).unwrap();
            dir_entry.preopen_path.is_some()
        } else {
            false
        }
    }
}

pub enum DirEntityType {
    File(crate::file::Filetype),
    Directory,
    SymbolicLink,
    Unknown,
}

pub struct ReaddirEntity {
    next: ReaddirCursor,
    inode: u64,
    namelen: u64,
    direnttype: DirEntityType,
}

pub struct ReaddirCursor(u64);
impl From<u64> for ReaddirCursor {
    fn from(c: u64) -> ReaddirCursor {
        ReaddirCursor(c)
    }
}
impl From<ReaddirCursor> for u64 {
    fn from(c: ReaddirCursor) -> u64 {
        c.0
    }
}

impl WasiDir for cap_std::fs::Dir {
    fn open_file(
        &self,
        symlink_follow: bool,
        path: &str,
        oflags: OFlags,
        caps: FileCaps,
    ) -> Result<Box<dyn WasiFile>, Error> {
        use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt};

        let mut opts = cap_std::fs::OpenOptions::new();

        if oflags.contains(&(OFlags::CREATE | OFlags::EXCLUSIVE)) {
            opts.create_new(true);
        } else if oflags.contains(&OFlags::CREATE) {
            opts.create(true);
        }
        if oflags.contains(&OFlags::TRUNCATE) {
            opts.truncate(true);
        }
        if caps.contains(&FileCaps::WRITE)
            || caps.contains(&FileCaps::DATASYNC)
            || caps.contains(&FileCaps::ALLOCATE)
            || caps.contains(&FileCaps::FILESTAT_SET_SIZE)
        {
            opts.write(true);
        } else {
            opts.read(true);
        }
        if symlink_follow {
            opts.follow(FollowSymlinks::Yes);
        }

        let f = self.open_with(Path::new(path), &opts)?;
        Ok(Box::new(f))
    }

    fn open_dir(&self, symlink_follow: bool, path: &str) -> Result<Box<dyn WasiDir>, Error> {
        // XXX obey symlink_follow
        let d = self.open_dir(Path::new(path))?;
        Ok(Box::new(d))
    }

    fn create_dir(&self, path: &str) -> Result<(), Error> {
        self.create_dir(Path::new(path))?;
        Ok(())
    }
    fn readdir(
        &self,
        cursor: ReaddirCursor,
    ) -> Result<Box<dyn Iterator<Item = Result<(ReaddirEntity, String), Error>>>, Error> {
        let rd = self
            .read_dir(PathBuf::new())?
            .enumerate()
            .skip(u64::from(cursor) as usize);
        Ok(Box::new(rd.map(|(ix, entry)| {
            let entry = entry?;
            let file_type = entry.file_type()?;
            let direnttype = if file_type.is_dir() {
                DirEntityType::Directory
            } else if file_type.is_file() {
                DirEntityType::File(crate::file::Filetype::RegularFile) // XXX unify this with conversion in `impl WasiFile for cap_std::fs::File { get_filetype }`
            } else if file_type.is_symlink() {
                DirEntityType::SymbolicLink
            } else {
                DirEntityType::Unknown
            };
            let name = entry.file_name().into_string().map_err(|_| {
                Error::Utf8(todo!(
                    // XXX
                    "idk how to make utf8 error out of osstring conversion"
                ))
            })?;
            let namelen = name.as_bytes().len() as u64;
            // XXX need the metadata casing to be reusable here
            let inode = todo!();
            let entity = ReaddirEntity {
                next: ReaddirCursor::from(ix as u64 + 1),
                direnttype,
                inode,
                namelen,
            };
            Ok((entity, name))
        })))
    }

    fn symlink(&self, src_path: &str, dest_path: &str) -> Result<(), Error> {
        self.symlink(Path::new(src_path), Path::new(dest_path))?;
        Ok(())
    }
    fn remove_dir(&self, path: &str) -> Result<(), Error> {
        self.remove_dir(Path::new(path))?;
        Ok(())
    }

    fn unlink_file(&self, path: &str) -> Result<(), Error> {
        self.remove_file(Path::new(path))?;
        Ok(())
    }
}
