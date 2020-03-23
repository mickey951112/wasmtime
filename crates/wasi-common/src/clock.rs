use crate::sys;
use crate::wasi::types::{Subclockflags, SubscriptionClock};
use crate::wasi::{Errno, Result};
use std::time::SystemTime;

pub(crate) use sys::clock::*;

pub(crate) fn to_relative_ns_delay(clock: &SubscriptionClock) -> Result<u128> {
    if clock.flags != Subclockflags::SUBSCRIPTION_CLOCK_ABSTIME {
        return Ok(u128::from(clock.timeout));
    }
    let now: u128 = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_err(|_| Errno::Notcapable)?
        .as_nanos();
    let deadline = u128::from(clock.timeout);
    Ok(deadline.saturating_sub(now))
}
