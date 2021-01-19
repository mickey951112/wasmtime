use crate::file::{FdFlags, FileType, Filestat, WasiFile};
use crate::Error;
use std::any::Any;
#[cfg(unix)]
use std::os::unix::io::{AsRawFd, RawFd};
use system_interface::io::ReadReady;

pub struct Stdin(std::io::Stdin);

pub fn stdin() -> Stdin {
    Stdin(std::io::stdin())
}

#[cfg(unix)]
impl AsRawFd for Stdin {
    fn as_raw_fd(&self) -> RawFd {
        self.0.as_raw_fd()
    }
}

impl ReadReady for Stdin {
    fn num_ready_bytes(&self) -> Result<u64, std::io::Error> {
        self.0.num_ready_bytes()
    }
}

impl WasiFile for Stdin {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn datasync(&self) -> Result<(), Error> {
        Ok(())
    }
    fn sync(&self) -> Result<(), Error> {
        Ok(())
    }
    fn get_filetype(&self) -> Result<FileType, Error> {
        Ok(FileType::CharacterDevice)
    }
    fn get_fdflags(&self) -> Result<FdFlags, Error> {
        todo!()
    }
    fn set_fdflags(&self, _fdflags: FdFlags) -> Result<(), Error> {
        todo!()
    }
    fn get_filestat(&self) -> Result<Filestat, Error> {
        todo!()
    }
    fn set_filestat_size(&self, _size: u64) -> Result<(), Error> {
        todo!()
    }
}

pub struct Stdout(std::io::Stdout);

pub fn stdout() -> Stdout {
    Stdout(std::io::stdout())
}

#[cfg(unix)]
impl AsRawFd for Stdout {
    fn as_raw_fd(&self) -> RawFd {
        self.0.as_raw_fd()
    }
}

impl ReadReady for Stdout {
    fn num_ready_bytes(&self) -> Result<u64, std::io::Error> {
        Ok(0)
    }
}

impl WasiFile for Stdout {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn datasync(&self) -> Result<(), Error> {
        Ok(())
    }
    fn sync(&self) -> Result<(), Error> {
        Ok(())
    }
    fn get_filetype(&self) -> Result<FileType, Error> {
        Ok(FileType::CharacterDevice)
    }
    fn get_fdflags(&self) -> Result<FdFlags, Error> {
        todo!()
    }
    fn set_fdflags(&self, _fdflags: FdFlags) -> Result<(), Error> {
        todo!()
    }
    fn get_filestat(&self) -> Result<Filestat, Error> {
        todo!()
    }
    fn set_filestat_size(&self, _size: u64) -> Result<(), Error> {
        todo!()
    }
}

pub struct Stderr(std::io::Stderr);

pub fn stderr() -> Stderr {
    Stderr(std::io::stderr())
}

#[cfg(unix)]
impl AsRawFd for Stderr {
    fn as_raw_fd(&self) -> RawFd {
        self.0.as_raw_fd()
    }
}

impl ReadReady for Stderr {
    fn num_ready_bytes(&self) -> Result<u64, std::io::Error> {
        Ok(0)
    }
}

impl WasiFile for Stderr {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn datasync(&self) -> Result<(), Error> {
        Ok(())
    }
    fn sync(&self) -> Result<(), Error> {
        Ok(())
    }
    fn get_filetype(&self) -> Result<FileType, Error> {
        Ok(FileType::CharacterDevice)
    }
    fn get_fdflags(&self) -> Result<FdFlags, Error> {
        todo!()
    }
    fn set_fdflags(&self, _fdflags: FdFlags) -> Result<(), Error> {
        todo!()
    }
    fn get_filestat(&self) -> Result<Filestat, Error> {
        todo!()
    }
    fn set_filestat_size(&self, _size: u64) -> Result<(), Error> {
        todo!()
    }
}
