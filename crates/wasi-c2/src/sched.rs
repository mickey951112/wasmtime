use crate::clocks::WasiSystemClock;
use crate::file::WasiFile;
use crate::Error;
use cap_std::time::{Duration, SystemTime};
use std::cell::Ref;
pub mod subscription;

mod sync;
pub use sync::SyncSched;

use subscription::{RwSubscription, Subscription, SubscriptionResult, SystemTimerSubscription};

pub trait WasiSched {
    fn poll_oneoff(&self, poll: &Poll) -> Result<(), Error>;
    fn sched_yield(&self) -> Result<(), Error>;
}

pub struct Userdata(u64);
impl From<u64> for Userdata {
    fn from(u: u64) -> Userdata {
        Userdata(u)
    }
}

impl From<Userdata> for u64 {
    fn from(u: Userdata) -> u64 {
        u.0
    }
}

pub struct Poll<'a> {
    subs: Vec<(Subscription<'a>, Userdata)>,
}

impl<'a> Poll<'a> {
    pub fn new() -> Self {
        Self { subs: Vec::new() }
    }
    pub fn subscribe_system_timer(
        &mut self,
        clock: &'a dyn WasiSystemClock,
        deadline: SystemTime,
        precision: Duration,
        ud: Userdata,
    ) {
        self.subs.push((
            Subscription::SystemTimer(SystemTimerSubscription {
                clock,
                deadline,
                precision,
            }),
            ud,
        ));
    }
    pub fn subscribe_read(&mut self, file: Ref<'a, dyn WasiFile>, ud: Userdata) {
        self.subs
            .push((Subscription::Read(RwSubscription::new(file)), ud));
    }
    pub fn subscribe_write(&mut self, file: Ref<'a, dyn WasiFile>, ud: Userdata) {
        self.subs
            .push((Subscription::Write(RwSubscription::new(file)), ud));
    }
    pub fn results(self) -> Vec<(SubscriptionResult, Userdata)> {
        self.subs
            .into_iter()
            .filter_map(|(s, ud)| SubscriptionResult::from_subscription(s).map(|r| (r, ud)))
            .collect()
    }
    pub fn is_empty(&self) -> bool {
        self.subs.is_empty()
    }
    pub fn earliest_system_timer(&'a self) -> Option<&SystemTimerSubscription<'a>> {
        let mut subs = self
            .subs
            .iter()
            .filter_map(|(s, _ud)| match s {
                Subscription::SystemTimer(t) => Some(t),
                _ => None,
            })
            .collect::<Vec<&SystemTimerSubscription<'a>>>();
        subs.sort_by(|a, b| a.deadline.cmp(&b.deadline));
        subs.into_iter().next() // First element is earliest
    }
    pub fn rw_subscriptions(&'a self) -> impl Iterator<Item = &Subscription<'a>> {
        self.subs.iter().filter_map(|(s, _ud)| match s {
            Subscription::Read { .. } | Subscription::Write { .. } => Some(s),
            _ => None,
        })
    }
}
