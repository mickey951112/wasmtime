use crate::entry::{Descriptor, Entry};
use crate::fdpool::FdPool;
use crate::sys::entry::OsHandle;
use crate::virtfs::{VirtualDir, VirtualDirEntry};
use crate::wasi::types;
use crate::wasi::{Errno, Result};
use std::borrow::Borrow;
use std::cell::{Ref, RefCell, RefMut};
use std::collections::HashMap;
use std::ffi::{self, CString, OsString};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::{env, io, string};

/// Possible errors when `WasiCtxBuilder` fails building
/// `WasiCtx`.
#[derive(Debug, thiserror::Error)]
pub enum WasiCtxBuilderError {
    /// General I/O error was encountered.
    #[error("general I/O error encountered: {0}")]
    Io(#[from] io::Error),
    /// Provided sequence of bytes was not a valid UTF-8.
    #[error("provided sequence is not valid UTF-8: {0}")]
    InvalidUtf8(#[from] string::FromUtf8Error),
    /// Provided sequence of bytes was not a valid UTF-16.
    ///
    /// This error is expected to only occur on Windows hosts.
    #[error("provided sequence is not valid UTF-16: {0}")]
    InvalidUtf16(#[from] string::FromUtf16Error),
    /// Provided sequence of bytes contained an unexpected NUL byte.
    #[error("provided sequence contained an unexpected NUL byte")]
    UnexpectedNul(#[from] ffi::NulError),
    /// Provided `File` is not a directory.
    #[error("preopened directory path {} is not a directory", .0.display())]
    NotADirectory(PathBuf),
    /// `WasiCtx` has too many opened files.
    #[error("context object has too many opened files")]
    TooManyFilesOpen,
}

type WasiCtxBuilderResult<T> = std::result::Result<T, WasiCtxBuilderError>;

enum PendingEntry {
    Thunk(fn() -> io::Result<Entry>),
    File(File),
}

impl std::fmt::Debug for PendingEntry {
    fn fmt(&self, fmt: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Thunk(f) => write!(
                fmt,
                "PendingEntry::Thunk({:p})",
                f as *const fn() -> io::Result<Entry>
            ),
            Self::File(f) => write!(fmt, "PendingEntry::File({:?})", f),
        }
    }
}

#[derive(Debug, Eq, Hash, PartialEq)]
enum PendingCString {
    Bytes(Vec<u8>),
    OsString(OsString),
}

impl From<Vec<u8>> for PendingCString {
    fn from(bytes: Vec<u8>) -> Self {
        Self::Bytes(bytes)
    }
}

impl From<OsString> for PendingCString {
    fn from(s: OsString) -> Self {
        Self::OsString(s)
    }
}

impl PendingCString {
    fn into_string(self) -> WasiCtxBuilderResult<String> {
        let res = match self {
            Self::Bytes(v) => String::from_utf8(v)?,
            #[cfg(unix)]
            Self::OsString(s) => {
                use std::os::unix::ffi::OsStringExt;
                String::from_utf8(s.into_vec())?
            }
            #[cfg(windows)]
            Self::OsString(s) => {
                use std::os::windows::ffi::OsStrExt;
                let bytes: Vec<u16> = s.encode_wide().collect();
                String::from_utf16(&bytes)?
            }
        };
        Ok(res)
    }

    /// Create a `CString` containing valid UTF-8.
    fn into_utf8_cstring(self) -> WasiCtxBuilderResult<CString> {
        let s = self.into_string()?;
        let s = CString::new(s)?;
        Ok(s)
    }
}

/// A builder allowing customizable construction of `WasiCtx` instances.
pub struct WasiCtxBuilder {
    stdin: Option<PendingEntry>,
    stdout: Option<PendingEntry>,
    stderr: Option<PendingEntry>,
    preopens: Option<Vec<(PathBuf, Descriptor)>>,
    args: Option<Vec<PendingCString>>,
    env: Option<HashMap<PendingCString, PendingCString>>,
}

impl WasiCtxBuilder {
    /// Builder for a new `WasiCtx`.
    pub fn new() -> Self {
        let stdin = Some(PendingEntry::Thunk(Entry::null));
        let stdout = Some(PendingEntry::Thunk(Entry::null));
        let stderr = Some(PendingEntry::Thunk(Entry::null));

        Self {
            stdin,
            stdout,
            stderr,
            preopens: Some(Vec::new()),
            args: Some(Vec::new()),
            env: Some(HashMap::new()),
        }
    }

    /// Add arguments to the command-line arguments list.
    ///
    /// Arguments must be valid UTF-8 with no NUL bytes, or else `WasiCtxBuilder::build()` will fail.
    pub fn args<S: AsRef<[u8]>>(&mut self, args: impl IntoIterator<Item = S>) -> &mut Self {
        self.args
            .as_mut()
            .unwrap()
            .extend(args.into_iter().map(|a| a.as_ref().to_vec().into()));
        self
    }

    /// Add an argument to the command-line arguments list.
    ///
    /// Arguments must be valid UTF-8 with no NUL bytes, or else `WasiCtxBuilder::build()` will fail.
    pub fn arg<S: AsRef<[u8]>>(&mut self, arg: S) -> &mut Self {
        self.args
            .as_mut()
            .unwrap()
            .push(arg.as_ref().to_vec().into());
        self
    }

    /// Inherit the command-line arguments from the host process.
    ///
    /// If any arguments from the host process contain invalid UTF-8, `WasiCtxBuilder::build()` will
    /// fail.
    pub fn inherit_args(&mut self) -> &mut Self {
        let args = self.args.as_mut().unwrap();
        args.clear();
        args.extend(env::args_os().map(PendingCString::OsString));
        self
    }

    /// Inherit stdin from the host process.
    pub fn inherit_stdin(&mut self) -> &mut Self {
        self.stdin = Some(PendingEntry::Thunk(Entry::duplicate_stdin));
        self
    }

    /// Inherit stdout from the host process.
    pub fn inherit_stdout(&mut self) -> &mut Self {
        self.stdout = Some(PendingEntry::Thunk(Entry::duplicate_stdout));
        self
    }

    /// Inherit stdout from the host process.
    pub fn inherit_stderr(&mut self) -> &mut Self {
        self.stderr = Some(PendingEntry::Thunk(Entry::duplicate_stderr));
        self
    }

    /// Inherit the stdin, stdout, and stderr streams from the host process.
    pub fn inherit_stdio(&mut self) -> &mut Self {
        self.stdin = Some(PendingEntry::Thunk(Entry::duplicate_stdin));
        self.stdout = Some(PendingEntry::Thunk(Entry::duplicate_stdout));
        self.stderr = Some(PendingEntry::Thunk(Entry::duplicate_stderr));
        self
    }

    /// Inherit the environment variables from the host process.
    ///
    /// If any environment variables from the host process contain invalid Unicode (UTF-16 for
    /// Windows, UTF-8 for other platforms), `WasiCtxBuilder::build()` will fail.
    pub fn inherit_env(&mut self) -> &mut Self {
        let env = self.env.as_mut().unwrap();
        env.clear();
        env.extend(std::env::vars_os().map(|(k, v)| (k.into(), v.into())));
        self
    }

    /// Add an entry to the environment.
    ///
    /// Environment variable keys and values must be valid UTF-8 with no NUL bytes, or else
    /// `WasiCtxBuilder::build()` will fail.
    pub fn env<S: AsRef<[u8]>>(&mut self, k: S, v: S) -> &mut Self {
        self.env
            .as_mut()
            .unwrap()
            .insert(k.as_ref().to_vec().into(), v.as_ref().to_vec().into());
        self
    }

    /// Add entries to the environment.
    ///
    /// Environment variable keys and values must be valid UTF-8 with no NUL bytes, or else
    /// `WasiCtxBuilder::build()` will fail.
    pub fn envs<S: AsRef<[u8]>, T: Borrow<(S, S)>>(
        &mut self,
        envs: impl IntoIterator<Item = T>,
    ) -> &mut Self {
        self.env.as_mut().unwrap().extend(envs.into_iter().map(|t| {
            let (k, v) = t.borrow();
            (k.as_ref().to_vec().into(), v.as_ref().to_vec().into())
        }));
        self
    }

    /// Provide a File to use as stdin
    pub fn stdin(&mut self, file: File) -> &mut Self {
        self.stdin = Some(PendingEntry::File(file));
        self
    }

    /// Provide a File to use as stdout
    pub fn stdout(&mut self, file: File) -> &mut Self {
        self.stdout = Some(PendingEntry::File(file));
        self
    }

    /// Provide a File to use as stderr
    pub fn stderr(&mut self, file: File) -> &mut Self {
        self.stderr = Some(PendingEntry::File(file));
        self
    }

    /// Add a preopened directory.
    pub fn preopened_dir<P: AsRef<Path>>(&mut self, dir: File, guest_path: P) -> &mut Self {
        self.preopens.as_mut().unwrap().push((
            guest_path.as_ref().to_owned(),
            Descriptor::OsHandle(OsHandle::from(dir)),
        ));
        self
    }

    /// Add a preopened virtual directory.
    pub fn preopened_virt<P: AsRef<Path>>(
        &mut self,
        dir: VirtualDirEntry,
        guest_path: P,
    ) -> &mut Self {
        fn populate_directory(virtentry: HashMap<String, VirtualDirEntry>, dir: &mut VirtualDir) {
            for (path, entry) in virtentry.into_iter() {
                match entry {
                    VirtualDirEntry::Directory(dir_entries) => {
                        let mut subdir = VirtualDir::new(true);
                        populate_directory(dir_entries, &mut subdir);
                        dir.add_dir(subdir, path);
                    }
                    VirtualDirEntry::File(content) => {
                        dir.add_file(content, path);
                    }
                }
            }
        }

        let dir = if let VirtualDirEntry::Directory(entries) = dir {
            let mut dir = VirtualDir::new(true);
            populate_directory(entries, &mut dir);
            Box::new(dir)
        } else {
            panic!("the root of a VirtualDirEntry tree must be a VirtualDirEntry::Directory");
        };

        self.preopens
            .as_mut()
            .unwrap()
            .push((guest_path.as_ref().to_owned(), Descriptor::VirtualFile(dir)));
        self
    }

    /// Build a `WasiCtx`, consuming this `WasiCtxBuilder`.
    ///
    /// If any of the arguments or environment variables in this builder cannot be converted into
    /// `CString`s, either due to NUL bytes or Unicode conversions, this will fail.
    pub fn build(&mut self) -> WasiCtxBuilderResult<WasiCtx> {
        // Process arguments and environment variables into `CString`s, failing quickly if they
        // contain any NUL bytes, or if conversion from `OsString` fails.
        let args = self
            .args
            .take()
            .unwrap()
            .into_iter()
            .map(|arg| arg.into_utf8_cstring())
            .collect::<WasiCtxBuilderResult<Vec<CString>>>()?;

        let env = self
            .env
            .take()
            .unwrap()
            .into_iter()
            .map(|(k, v)| {
                k.into_string().and_then(|mut pair| {
                    v.into_string().and_then(|v| {
                        pair.push('=');
                        pair.push_str(v.as_str());
                        // We have valid UTF-8, but the keys and values have not yet been checked
                        // for NULs, so we do a final check here.
                        let s = CString::new(pair)?;
                        Ok(s)
                    })
                })
            })
            .collect::<WasiCtxBuilderResult<Vec<CString>>>()?;

        let mut entries = EntryTable::new();
        // Populate the non-preopen entries.
        for pending in vec![
            self.stdin.take().unwrap(),
            self.stdout.take().unwrap(),
            self.stderr.take().unwrap(),
        ] {
            log::debug!("WasiCtx inserting entry {:?}", pending);
            let fd = match pending {
                PendingEntry::Thunk(f) => entries
                    .insert(f()?)
                    .ok_or(WasiCtxBuilderError::TooManyFilesOpen)?,
                PendingEntry::File(f) => entries
                    .insert(Entry::from(Descriptor::OsHandle(OsHandle::from(f)))?)
                    .ok_or(WasiCtxBuilderError::TooManyFilesOpen)?,
            };
            log::debug!("WasiCtx inserted at {:?}", fd);
        }
        // Then add the preopen entries.
        for (guest_path, dir) in self.preopens.take().unwrap() {
            match &dir {
                Descriptor::OsHandle(handle) => {
                    if !handle.metadata()?.is_dir() {
                        return Err(WasiCtxBuilderError::NotADirectory(guest_path));
                    }
                }
                Descriptor::VirtualFile(virt) => {
                    if !virt.is_directory() {
                        return Err(WasiCtxBuilderError::NotADirectory(guest_path));
                    }
                }
                Descriptor::Stdin | Descriptor::Stdout | Descriptor::Stderr => {
                    panic!("implementation error, stdin/stdout/stderr shouldn't be in the list of preopens");
                }
            }

            let mut entry = Entry::from(dir)?;
            entry.preopen_path = Some(guest_path);
            log::debug!("WasiCtx inserting {:?}", entry);
            let fd = entries
                .insert(entry)
                .ok_or(WasiCtxBuilderError::TooManyFilesOpen)?;
            log::debug!("WasiCtx inserted at {:?}", fd);
            log::debug!("WasiCtx entries = {:?}", entries);
        }

        Ok(WasiCtx {
            args,
            env,
            entries: RefCell::new(entries),
        })
    }
}

#[derive(Debug)]
struct EntryTable {
    fd_pool: FdPool,
    entries: HashMap<types::Fd, Entry>,
}

impl EntryTable {
    fn new() -> Self {
        Self {
            fd_pool: FdPool::new(),
            entries: HashMap::new(),
        }
    }

    fn contains(&self, fd: &types::Fd) -> bool {
        self.entries.contains_key(fd)
    }

    fn insert(&mut self, entry: Entry) -> Option<types::Fd> {
        let fd = self.fd_pool.allocate()?;
        self.entries.insert(fd, entry);
        Some(fd)
    }

    fn insert_at(&mut self, fd: &types::Fd, entry: Entry) {
        self.entries.insert(*fd, entry);
    }

    fn get(&self, fd: &types::Fd) -> Option<&Entry> {
        self.entries.get(fd)
    }

    fn get_mut(&mut self, fd: &types::Fd) -> Option<&mut Entry> {
        self.entries.get_mut(fd)
    }

    fn remove(&mut self, fd: types::Fd) -> Option<Entry> {
        let entry = self.entries.remove(&fd)?;
        self.fd_pool.deallocate(fd);
        Some(entry)
    }
}

#[derive(Debug)]
pub struct WasiCtx {
    entries: RefCell<EntryTable>,
    pub(crate) args: Vec<CString>,
    pub(crate) env: Vec<CString>,
}

impl WasiCtx {
    /// Make a new `WasiCtx` with some default settings.
    ///
    /// - File descriptors 0, 1, and 2 inherit stdin, stdout, and stderr from the host process.
    ///
    /// - Environment variables are inherited from the host process.
    ///
    /// To override these behaviors, use `WasiCtxBuilder`.
    pub fn new<S: AsRef<[u8]>>(args: impl IntoIterator<Item = S>) -> WasiCtxBuilderResult<Self> {
        WasiCtxBuilder::new()
            .args(args)
            .inherit_stdio()
            .inherit_env()
            .build()
    }

    /// Check if `WasiCtx` contains the specified raw WASI `fd`.
    pub(crate) fn contains_entry(&self, fd: types::Fd) -> bool {
        self.entries.borrow().contains(&fd)
    }

    /// Get an immutable `Entry` corresponding to the specified raw WASI `fd`.
    pub(crate) fn get_entry(&self, fd: types::Fd) -> Result<Ref<Entry>> {
        if !self.contains_entry(fd) {
            return Err(Errno::Badf);
        }
        Ok(Ref::map(self.entries.borrow(), |entries| {
            entries.get(&fd).unwrap()
        }))
    }

    /// Get a mutable `Entry` corresponding to the specified raw WASI `fd`.
    // TODO This runs the risk of a potential difficult-to-predict panic down-the-line.
    pub(crate) fn get_entry_mut(&self, fd: types::Fd) -> Result<RefMut<Entry>> {
        if !self.contains_entry(fd) {
            return Err(Errno::Badf);
        }
        Ok(RefMut::map(self.entries.borrow_mut(), |entries| {
            entries.get_mut(&fd).unwrap()
        }))
    }

    /// Insert the specified `Entry` into the `WasiCtx` object.
    ///
    /// The `Entry` will automatically get another free raw WASI `fd` assigned. Note that
    /// the two subsequent free raw WASI `fd`s do not have to be stored contiguously.
    pub(crate) fn insert_entry(&self, entry: Entry) -> Result<types::Fd> {
        self.entries.borrow_mut().insert(entry).ok_or(Errno::Mfile)
    }

    /// Insert the specified `Entry` with the specified raw WASI `fd` key into the `WasiCtx`
    /// object.
    pub(crate) fn insert_entry_at(&self, fd: types::Fd, entry: Entry) {
        self.entries.borrow_mut().insert_at(&fd, entry)
    }

    /// Remove `Entry` corresponding to the specified raw WASI `fd` from the `WasiCtx` object.
    pub(crate) fn remove_entry(&self, fd: types::Fd) -> Result<Entry> {
        self.entries.borrow_mut().remove(fd).ok_or(Errno::Badf)
    }
}
