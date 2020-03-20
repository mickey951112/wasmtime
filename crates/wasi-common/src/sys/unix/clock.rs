use crate::wasi::{types, Errno, Result};
use yanix::clock::{clock_getres, clock_gettime, ClockId};

pub(crate) fn res_get(clock_id: types::Clockid) -> Result<types::Timestamp> {
    let clock_id: ClockId = clock_id.into();
    let timespec = clock_getres(clock_id)?;

    // convert to nanoseconds, returning EOVERFLOW in case of overflow;
    // this is freelancing a bit from the spec but seems like it'll
    // be an unusual situation to hit
    (timespec.tv_sec as types::Timestamp)
        .checked_mul(1_000_000_000)
        .and_then(|sec_ns| sec_ns.checked_add(timespec.tv_nsec as types::Timestamp))
        .map_or(Err(Errno::Overflow), |resolution| {
            // a supported clock can never return zero; this case will probably never get hit, but
            // make sure we follow the spec
            if resolution == 0 {
                Err(Errno::Inval)
            } else {
                Ok(resolution)
            }
        })
}

pub(crate) fn time_get(clock_id: types::Clockid) -> Result<types::Timestamp> {
    let clock_id: ClockId = clock_id.into();
    let timespec = clock_gettime(clock_id)?;

    // convert to nanoseconds, returning EOVERFLOW in case of overflow; this is freelancing a bit
    // from the spec but seems like it'll be an unusual situation to hit
    (timespec.tv_sec as types::Timestamp)
        .checked_mul(1_000_000_000)
        .and_then(|sec_ns| sec_ns.checked_add(timespec.tv_nsec as types::Timestamp))
        .map_or(Err(Errno::Overflow), Ok)
}
