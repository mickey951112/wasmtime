use crate::RunResult;
use std::cell::Cell;
use std::io;
use std::ptr;
use winapi::shared::minwindef::*;
use winapi::um::fibersapi::*;
use winapi::um::winbase::*;

pub struct Fiber {
    fiber: LPVOID,
    state: Box<StartState>,
}

pub struct Suspend {
    state: *const StartState,
}

struct StartState {
    parent: Cell<LPVOID>,
    initial_closure: Cell<*mut u8>,
    result_location: Cell<*const u8>,
}

const FIBER_FLAG_FLOAT_SWITCH: DWORD = 1;

extern "C" {
    fn wasmtime_fiber_get_current() -> LPVOID;
}

unsafe extern "system" fn fiber_start<F, A, B, C>(data: LPVOID)
where
    F: FnOnce(A, &super::Suspend<A, B, C>) -> C,
{
    let state = data.cast::<StartState>();
    let func = Box::from_raw((*state).initial_closure.get().cast::<F>());
    (*state).initial_closure.set(ptr::null_mut());
    let suspend = Suspend { state };
    let initial = suspend.take_resume::<A, B, C>();
    super::Suspend::<A, B, C>::execute(suspend, initial, *func);
}

impl Fiber {
    pub fn new<F, A, B, C>(stack_size: usize, func: F) -> io::Result<Fiber>
    where
        F: FnOnce(A, &super::Suspend<A, B, C>) -> C,
    {
        unsafe {
            let state = Box::new(StartState {
                initial_closure: Cell::new(Box::into_raw(Box::new(func)).cast()),
                parent: Cell::new(ptr::null_mut()),
                result_location: Cell::new(ptr::null()),
            });
            let fiber = CreateFiberEx(
                0,
                stack_size,
                FIBER_FLAG_FLOAT_SWITCH,
                Some(fiber_start::<F, A, B, C>),
                &*state as *const StartState as *mut _,
            );
            if fiber.is_null() {
                drop(Box::from_raw(state.initial_closure.get().cast::<F>()));
                Err(io::Error::last_os_error())
            } else {
                Ok(Fiber { fiber, state })
            }
        }
    }

    pub(crate) fn resume<A, B, C>(&self, result: &Cell<RunResult<A, B, C>>) {
        unsafe {
            let is_fiber = IsThreadAFiber() != 0;
            let parent_fiber = if is_fiber {
                wasmtime_fiber_get_current()
            } else {
                ConvertThreadToFiber(ptr::null_mut())
            };
            assert!(
                !parent_fiber.is_null(),
                "failed to make current thread a fiber"
            );
            self.state
                .result_location
                .set(result as *const _ as *const _);
            self.state.parent.set(parent_fiber);
            SwitchToFiber(self.fiber);
            self.state.parent.set(ptr::null_mut());
            self.state.result_location.set(ptr::null());
            if !is_fiber {
                let res = ConvertFiberToThread();
                assert!(res != 0, "failed to convert main thread back");
            }
        }
    }
}

impl Drop for Fiber {
    fn drop(&mut self) {
        unsafe {
            DeleteFiber(self.fiber);
        }
    }
}

impl Suspend {
    pub(crate) fn switch<A, B, C>(&self, result: RunResult<A, B, C>) -> A {
        unsafe {
            (*self.result_location::<A, B, C>()).set(result);
            debug_assert!(IsThreadAFiber() != 0);
            let parent = (*self.state).parent.get();
            debug_assert!(!parent.is_null());
            SwitchToFiber(parent);
            self.take_resume::<A, B, C>()
        }
    }
    unsafe fn take_resume<A, B, C>(&self) -> A {
        match (*self.result_location::<A, B, C>()).replace(RunResult::Executing) {
            RunResult::Resuming(val) => val,
            _ => panic!("not in resuming state"),
        }
    }

    unsafe fn result_location<A, B, C>(&self) -> *const Cell<RunResult<A, B, C>> {
        let ret = (*self.state)
            .result_location
            .get()
            .cast::<Cell<RunResult<A, B, C>>>();
        assert!(!ret.is_null());
        return ret;
    }
}
