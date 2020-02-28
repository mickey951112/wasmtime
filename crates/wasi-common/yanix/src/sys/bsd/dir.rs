use crate::{
    dir::{Dir, Entry, EntryExt, SeekLoc},
    Errno, Result,
};
use std::ops::Deref;

#[derive(Copy, Clone, Debug)]
pub(crate) struct EntryImpl {
    dirent: libc::dirent,
    loc: SeekLoc,
}

impl Deref for EntryImpl {
    type Target = libc::dirent;

    fn deref(&self) -> &Self::Target {
        &self.dirent
    }
}

pub(crate) fn iter_impl(dir: &Dir) -> Option<Result<EntryImpl>> {
    let errno = Errno::last();
    let dirent = unsafe { libc::readdir(dir.as_raw().as_ptr()) };
    if dirent.is_null() {
        if errno != Errno::last() {
            // TODO This should be verified on different BSD-flavours.
            //
            // According to 4.3BSD/POSIX.1-2001 man pages, there was an error
            // if the errno value has changed at some point during the sequence
            // of readdir calls.
            Some(Err(Errno::last().into()))
        } else {
            // Not an error. We've simply reached the end of the stream.
            None
        }
    } else {
        Some(Ok(EntryImpl {
            dirent: unsafe { *dirent },
            loc: dir.tell(),
        }))
    }
}

impl EntryExt for Entry {
    #[cfg(target_os = "freebsd")]
    fn ino(&self) -> u64 {
        self.0.d_fileno.into()
    }

    #[cfg(not(target_os = "freebsd"))]
    fn ino(&self) -> u64 {
        self.0.d_ino.into()
    }

    fn seek_loc(&self) -> Result<SeekLoc> {
        Ok(self.0.loc)
    }
}

impl SeekLoc {
    pub unsafe fn from_raw(loc: i64) -> Result<Self> {
        let loc = loc.into();
        Ok(Self(loc))
    }
}
