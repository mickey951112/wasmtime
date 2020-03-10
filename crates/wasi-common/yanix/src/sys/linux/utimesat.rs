use crate::filetime::FileTime;
use crate::filetime::FileTimeExt;
use crate::from_success_code;
use std::fs;
use std::io::Result;

/// Combines `openat` with `utimes` to emulate `utimensat` on platforms where it is
/// not available. The logic for setting file times is based on [filetime::unix::set_file_handles_times].
///
/// [filetime::unix::set_file_handles_times]: https://github.com/alexcrichton/filetime/blob/master/src/unix/utimes.rs#L24
pub fn utimesat(
    dirfd: &fs::File,
    path: &str,
    atime: FileTime,
    mtime: FileTime,
    symlink_nofollow: bool,
) -> Result<()> {
    use std::ffi::CString;
    use std::os::unix::prelude::*;
    // emulate *at syscall by reading the path from a combination of
    // (fd, path)
    let p = CString::new(path.as_bytes())?;
    let mut flags = libc::O_RDWR;
    if symlink_nofollow {
        flags |= libc::O_NOFOLLOW;
    }
    let fd = unsafe { libc::openat(dirfd.as_raw_fd(), p.as_ptr(), flags) };
    let f = unsafe { fs::File::from_raw_fd(fd) };
    let (atime, mtime) = get_times(atime, mtime, || f.metadata().map_err(Into::into))?;
    let times = [to_timeval(atime)?, to_timeval(mtime)?];
    from_success_code(unsafe { libc::futimes(f.as_raw_fd(), times.as_ptr()) })
}

/// Converts `filetime::FileTime` to `libc::timeval`.
fn to_timeval(ft: filetime::FileTime) -> Result<libc::timeval> {
    Ok(libc::timeval {
        tv_sec: ft.seconds_()?,
        tv_usec: (ft.nanoseconds_() / 1000) as libc::suseconds_t,
    })
}

/// For a provided pair of access and modified `FileTime`s, converts the input to
/// `filetime::FileTime` used later in `utimensat` function. For variants `FileTime::Now`
/// and `FileTime::Omit`, this function will make two syscalls: either accessing current
/// system time, or accessing the file's metadata.
///
/// The original implementation can be found here: [filetime::unix::get_times].
///
/// [filetime::unix::get_times]: https://github.com/alexcrichton/filetime/blob/master/src/unix/utimes.rs#L42
fn get_times(
    atime: FileTime,
    mtime: FileTime,
    current: impl Fn() -> Result<fs::Metadata>,
) -> Result<(filetime::FileTime, filetime::FileTime)> {
    use std::time::SystemTime;

    let atime = match atime {
        FileTime::Now => {
            let time = SystemTime::now();
            filetime::FileTime::from_system_time(time)
        }
        FileTime::Omit => {
            let meta = current()?;
            filetime::FileTime::from_last_access_time(&meta)
        }
        FileTime::FileTime(ft) => ft,
    };

    let mtime = match mtime {
        FileTime::Now => {
            let time = SystemTime::now();
            filetime::FileTime::from_system_time(time)
        }
        FileTime::Omit => {
            let meta = current()?;
            filetime::FileTime::from_last_modification_time(&meta)
        }
        FileTime::FileTime(ft) => ft,
    };

    Ok((atime, mtime))
}
