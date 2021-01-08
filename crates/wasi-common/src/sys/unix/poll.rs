use crate::entry::EntryHandle;
use crate::handle::Filetype;
use crate::sched::{
    ClockEventData, Errno, Event, EventFdReadwrite, Eventrwflags, Eventtype, FdEventData,
};
use crate::sys::AsFile;
use crate::{Error, Result};
use std::io;
use std::{convert::TryInto, os::unix::prelude::AsRawFd};
use yanix::file::fionread;
use yanix::poll::{poll, PollFd, PollFlags};

pub(crate) fn oneoff(
    timeout: Option<ClockEventData>,
    fd_events: Vec<FdEventData>,
    events: &mut Vec<Event>,
) -> Result<()> {
    if fd_events.is_empty() && timeout.is_none() {
        return Ok(());
    }

    let poll_fds: Result<Vec<_>> = fd_events
        .iter()
        .map(|event| {
            let mut flags = PollFlags::empty();
            match event.r#type {
                Eventtype::FdRead => flags.insert(PollFlags::POLLIN),
                Eventtype::FdWrite => flags.insert(PollFlags::POLLOUT),
                // An event on a file descriptor can currently only be of type FD_READ or FD_WRITE
                // Nothing else has been defined in the specification, and these are also the only two
                // events we filtered before. If we get something else here, the code has a serious bug.
                _ => unreachable!(),
            };
            let file = event.handle.as_file()?;
            unsafe { Ok(PollFd::new(file.as_raw_fd(), flags)) }
        })
        .collect();
    let mut poll_fds = poll_fds?;

    let poll_timeout = timeout.map_or(-1, |timeout| {
        let delay = timeout.delay / 1_000_000; // poll syscall requires delay to expressed in milliseconds
        delay.try_into().unwrap_or(libc::c_int::max_value())
    });
    tracing::debug!(
        poll_timeout = tracing::field::debug(poll_timeout),
        "poll_oneoff"
    );

    let ready = loop {
        match poll(&mut poll_fds, poll_timeout) {
            Err(_) => {
                let last_err = io::Error::last_os_error();
                if last_err.raw_os_error().unwrap() == libc::EINTR {
                    continue;
                }
                return Err(last_err.into());
            }
            Ok(ready) => break ready,
        }
    };

    Ok(if ready == 0 {
        handle_timeout_event(timeout.expect("timeout should not be None"), events)
    } else {
        let ready_events = fd_events.into_iter().zip(poll_fds.into_iter()).take(ready);
        handle_fd_event(ready_events, events)?
    })
}

fn handle_timeout_event(timeout: ClockEventData, events: &mut Vec<Event>) {
    events.push(Event {
        userdata: timeout.userdata,
        error: Errno::Success,
        type_: Eventtype::Clock,
        fd_readwrite: EventFdReadwrite {
            flags: Eventrwflags::empty(),
            nbytes: 0,
        },
    });
}

fn handle_fd_event(
    ready_events: impl Iterator<Item = (FdEventData, yanix::poll::PollFd)>,
    events: &mut Vec<Event>,
) -> Result<()> {
    fn query_nbytes(handle: EntryHandle) -> Result<u64> {
        let file = handle.as_file()?;
        if handle.get_file_type() == Filetype::RegularFile {
            // fionread may overflow for large files, so use another way for regular files.
            use yanix::file::tell;
            let meta = file.metadata()?;
            let len = meta.len();
            let host_offset = unsafe { tell(file.as_raw_fd())? };
            return Ok(len - host_offset);
        }
        Ok(unsafe { fionread(file.as_raw_fd())?.into() })
    }

    for (fd_event, poll_fd) in ready_events {
        tracing::debug!(
            poll_fd = tracing::field::debug(poll_fd),
            poll_event = tracing::field::debug(&fd_event),
            "poll_oneoff handle_fd_event"
        );

        let revents = match poll_fd.revents() {
            Some(revents) => revents,
            None => continue,
        };

        let nbytes = if fd_event.r#type == Eventtype::FdRead {
            query_nbytes(fd_event.handle)?
        } else {
            0
        };

        let output_event = if revents.contains(PollFlags::POLLNVAL) {
            Event {
                userdata: fd_event.userdata,
                error: Error::Badf.try_into().unwrap(),
                type_: fd_event.r#type,
                fd_readwrite: EventFdReadwrite {
                    nbytes: 0,
                    flags: Eventrwflags::FD_READWRITE_HANGUP,
                },
            }
        } else if revents.contains(PollFlags::POLLERR) {
            Event {
                userdata: fd_event.userdata,
                error: Error::Io.try_into().unwrap(),
                type_: fd_event.r#type,
                fd_readwrite: EventFdReadwrite {
                    nbytes: 0,
                    flags: Eventrwflags::FD_READWRITE_HANGUP,
                },
            }
        } else if revents.contains(PollFlags::POLLHUP) {
            Event {
                userdata: fd_event.userdata,
                error: Errno::Success,
                type_: fd_event.r#type,
                fd_readwrite: EventFdReadwrite {
                    nbytes: 0,
                    flags: Eventrwflags::FD_READWRITE_HANGUP,
                },
            }
        } else if revents.contains(PollFlags::POLLIN) | revents.contains(PollFlags::POLLOUT) {
            Event {
                userdata: fd_event.userdata,
                error: Errno::Success,
                type_: fd_event.r#type,
                fd_readwrite: EventFdReadwrite {
                    nbytes: nbytes.try_into()?,
                    flags: Eventrwflags::empty(),
                },
            }
        } else {
            continue;
        };

        events.push(output_event);
    }

    Ok(())
}
