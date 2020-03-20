use crate::entry::Descriptor;
use crate::sys;
use crate::wasi::types;
use std::cell::Ref;

pub(crate) use sys::poll::*;

#[derive(Debug, Copy, Clone)]
pub(crate) struct ClockEventData {
    pub(crate) delay: u128, // delay is expressed in nanoseconds
    pub(crate) userdata: types::Userdata,
}

#[derive(Debug)]
pub(crate) struct FdEventData<'a> {
    pub(crate) descriptor: Ref<'a, Descriptor>,
    pub(crate) r#type: types::Eventtype,
    pub(crate) userdata: types::Userdata,
}
