//! WebAssembly trap handling, which is built on top of the lower-level
//! signalhandling mechanisms.

use crate::instance::{InstanceHandle, SignalHandler};
use crate::trap_registry::TrapDescription;
use crate::vmcontext::VMContext;
use backtrace::Backtrace;
use std::any::Any;
use std::cell::Cell;
use std::error::Error;
use std::fmt;
use std::io;
use std::ptr;
use std::sync::Once;
use wasmtime_environ::ir;

extern "C" {
    fn RegisterSetjmp(
        jmp_buf: *mut *const u8,
        callback: extern "C" fn(*mut u8),
        payload: *mut u8,
    ) -> i32;
    fn Unwind(jmp_buf: *const u8) -> !;
}

cfg_if::cfg_if! {
    if #[cfg(unix)] {
        use std::mem::{self, MaybeUninit};

        static mut PREV_SIGSEGV: MaybeUninit<libc::sigaction> = MaybeUninit::uninit();
        static mut PREV_SIGBUS: MaybeUninit<libc::sigaction> = MaybeUninit::uninit();
        static mut PREV_SIGILL: MaybeUninit<libc::sigaction> = MaybeUninit::uninit();
        static mut PREV_SIGFPE: MaybeUninit<libc::sigaction> = MaybeUninit::uninit();

        unsafe fn platform_init() {
            let register = |slot: &mut MaybeUninit<libc::sigaction>, signal: i32| {
                let mut handler: libc::sigaction = mem::zeroed();
                // The flags here are relatively careful, and they are...
                //
                // SA_SIGINFO gives us access to information like the program
                // counter from where the fault happened.
                //
                // SA_ONSTACK allows us to handle signals on an alternate stack,
                // so that the handler can run in response to running out of
                // stack space on the main stack. Rust installs an alternate
                // stack with sigaltstack, so we rely on that.
                //
                // SA_NODEFER allows us to reenter the signal handler if we
                // crash while handling the signal, and fall through to the
                // Breakpad handler by testing handlingSegFault.
                handler.sa_flags = libc::SA_SIGINFO | libc::SA_NODEFER | libc::SA_ONSTACK;
                handler.sa_sigaction = trap_handler as usize;
                libc::sigemptyset(&mut handler.sa_mask);
                if libc::sigaction(signal, &handler, slot.as_mut_ptr()) != 0 {
                    panic!(
                        "unable to install signal handler: {}",
                        io::Error::last_os_error(),
                    );
                }
            };

            // Allow handling OOB with signals on all architectures
            register(&mut PREV_SIGSEGV, libc::SIGSEGV);

            // Handle `unreachable` instructions which execute `ud2` right now
            register(&mut PREV_SIGILL, libc::SIGILL);

            // x86 uses SIGFPE to report division by zero
            if cfg!(target_arch = "x86") || cfg!(target_arch = "x86_64") {
                register(&mut PREV_SIGFPE, libc::SIGFPE);
            }

            // On ARM, handle Unaligned Accesses.
            // On Darwin, guard page accesses are raised as SIGBUS.
            if cfg!(target_arch = "arm") || cfg!(target_os = "macos") {
                register(&mut PREV_SIGBUS, libc::SIGBUS);
            }
        }

        unsafe extern "C" fn trap_handler(
            signum: libc::c_int,
            siginfo: *mut libc::siginfo_t,
            context: *mut libc::c_void,
        ) {
            let previous = match signum {
                libc::SIGSEGV => &PREV_SIGSEGV,
                libc::SIGBUS => &PREV_SIGBUS,
                libc::SIGFPE => &PREV_SIGFPE,
                libc::SIGILL => &PREV_SIGILL,
                _ => panic!("unknown signal: {}", signum),
            };
            let handled = tls::with(|info| {
                // If no wasm code is executing, we don't handle this as a wasm
                // trap.
                let info = match info {
                    Some(info) => info,
                    None => return false,
                };

                // If we hit an exception while handling a previous trap, that's
                // quite bad, so bail out and let the system handle this
                // recursive segfault.
                //
                // Otherwise flag ourselves as handling a trap, do the trap
                // handling, and reset our trap handling flag. Then we figure
                // out what to do based on the result of the trap handling.
                let jmp_buf = info.handle_trap(
                    get_pc(context),
                    false,
                    |handler| handler(signum, siginfo, context),
                );

                // Figure out what to do based on the result of this handling of
                // the trap. Note that our sentinel value of 1 means that the
                // exception was handled by a custom exception handler, so we
                // keep executing.
                if jmp_buf.is_null() {
                    return false;
                } else if jmp_buf as usize == 1 {
                    return true;
                } else {
                    Unwind(jmp_buf)
                }
            });

            if handled {
                return;
            }

            // This signal is not for any compiled wasm code we expect, so we
            // need to forward the signal to the next handler. If there is no
            // next handler (SIG_IGN or SIG_DFL), then it's time to crash. To do
            // this, we set the signal back to its original disposition and
            // return. This will cause the faulting op to be re-executed which
            // will crash in the normal way. If there is a next handler, call
            // it. It will either crash synchronously, fix up the instruction
            // so that execution can continue and return, or trigger a crash by
            // returning the signal to it's original disposition and returning.
            let previous = &*previous.as_ptr();
            if previous.sa_flags & libc::SA_SIGINFO != 0 {
                mem::transmute::<
                    usize,
                    extern "C" fn(libc::c_int, *mut libc::siginfo_t, *mut libc::c_void),
                >(previous.sa_sigaction)(signum, siginfo, context)
            } else if previous.sa_sigaction == libc::SIG_DFL ||
                previous.sa_sigaction == libc::SIG_IGN
            {
                libc::sigaction(signum, previous, ptr::null_mut());
            } else {
                mem::transmute::<usize, extern "C" fn(libc::c_int)>(
                    previous.sa_sigaction
                )(signum)
            }
        }

        unsafe fn get_pc(cx: *mut libc::c_void) -> *const u8 {
            cfg_if::cfg_if! {
                if #[cfg(all(target_os = "linux", target_arch = "x86_64"))] {
                    let cx = &*(cx as *const libc::ucontext_t);
                    cx.uc_mcontext.gregs[libc::REG_RIP as usize] as *const u8
                } else if #[cfg(target_os = "macos")] {
                    // FIXME(rust-lang/libc#1702) - once that lands and is
                    // released we should inline the definition here
                    extern "C" {
                        fn GetPcFromUContext(cx: *mut libc::c_void) -> *const u8;
                    }
                    GetPcFromUContext(cx)
                } else {
                    compile_error!("unsupported platform");
                }
            }
        }
    } else if #[cfg(target_os = "windows")] {
        use winapi::um::errhandlingapi::*;
        use winapi::um::winnt::*;
        use winapi::um::minwinbase::*;
        use winapi::vc::excpt::*;

        unsafe fn platform_init() {
            // our trap handler needs to go first, so that we can recover from
            // wasm faults and continue execution, so pass `1` as a true value
            // here.
            if AddVectoredExceptionHandler(1, Some(exception_handler)).is_null() {
                panic!("failed to add exception handler: {}", io::Error::last_os_error());
            }
        }

        unsafe extern "system" fn exception_handler(
            exception_info: PEXCEPTION_POINTERS
        ) -> LONG {
            // Check the kind of exception, since we only handle a subset within
            // wasm code. If anything else happens we want to defer to whatever
            // the rest of the system wants to do for this exception.
            let record = &*(*exception_info).ExceptionRecord;
            if record.ExceptionCode != EXCEPTION_ACCESS_VIOLATION &&
                record.ExceptionCode != EXCEPTION_ILLEGAL_INSTRUCTION &&
                record.ExceptionCode != EXCEPTION_STACK_OVERFLOW &&
                record.ExceptionCode != EXCEPTION_INT_DIVIDE_BY_ZERO &&
                record.ExceptionCode != EXCEPTION_INT_OVERFLOW
            {
                return EXCEPTION_CONTINUE_SEARCH;
            }

            // FIXME: this is what the previous C++ did to make sure that TLS
            // works by the time we execute this trap handling code. This isn't
            // exactly super easy to call from Rust though and it's not clear we
            // necessarily need to do so. Leaving this here in case we need this
            // in the future, but for now we can probably wait until we see a
            // strange fault before figuring out how to reimplement this in
            // Rust.
            //
            // if (!NtCurrentTeb()->Reserved1[sThreadLocalArrayPointerIndex]) {
            //     return EXCEPTION_CONTINUE_SEARCH;
            // }

            // This is basically the same as the unix version above, only with a
            // few parameters tweaked here and there.
            tls::with(|info| {
                let info = match info {
                    Some(info) => info,
                    None => return EXCEPTION_CONTINUE_SEARCH,
                };
                let jmp_buf = info.handle_trap(
                    (*(*exception_info).ContextRecord).Rip as *const u8,
                    record.ExceptionCode == EXCEPTION_STACK_OVERFLOW,
                    |handler| handler(exception_info),
                );
                if jmp_buf.is_null() {
                    EXCEPTION_CONTINUE_SEARCH
                } else if jmp_buf as usize == 1 {
                    EXCEPTION_CONTINUE_EXECUTION
                } else {
                    Unwind(jmp_buf)
                }
            })
        }
    }
}

/// This function performs the low-overhead signal handler initialization that
/// we want to do eagerly to ensure a more-deterministic global process state.
///
/// This is especially relevant for signal handlers since handler ordering
/// depends on installation order: the wasm signal handler must run *before*
/// the other crash handlers and since POSIX signal handlers work LIFO, this
/// function needs to be called at the end of the startup process, after other
/// handlers have been installed. This function can thus be called multiple
/// times, having no effect after the first call.
pub fn init() {
    static INIT: Once = Once::new();
    INIT.call_once(real_init);
}

fn real_init() {
    // This is a really weird and unfortunate function call. For all the gory
    // details see #829, but the tl;dr; is that in a trap handler we have 2
    // pages of stack space on Linux, and calling into libunwind which triggers
    // the dynamic loader blows the stack.
    //
    // This is a dumb hack to work around this system-specific issue by
    // capturing a backtrace once in the lifetime of a process to ensure that
    // when we capture a backtrace in the trap handler all caches are primed,
    // aka the dynamic loader has resolved all the relevant symbols.
    drop(backtrace::Backtrace::new_unresolved());
    unsafe {
        platform_init();
    }
}

/// Raises a user-defined trap immediately.
///
/// This function performs as-if a wasm trap was just executed, only the trap
/// has a dynamic payload associated with it which is user-provided. This trap
/// payload is then returned from `catch_traps` below.
///
/// # Safety
///
/// Only safe to call when wasm code is on the stack, aka `catch_traps` must
/// have been previously called. Additionally no Rust destructors can be on the
/// stack. They will be skipped and not executed.
pub unsafe fn raise_user_trap(data: Box<dyn Error + Send + Sync>) -> ! {
    tls::with(|info| info.unwrap().unwind_with(UnwindReason::UserTrap(data)))
}

/// Raises a trap from inside library code immediately.
///
/// This function performs as-if a wasm trap was just executed. This trap
/// payload is then returned from `catch_traps` below.
///
/// # Safety
///
/// Only safe to call when wasm code is on the stack, aka `catch_traps` must
/// have been previously called. Additionally no Rust destructors can be on the
/// stack. They will be skipped and not executed.
pub unsafe fn raise_lib_trap(trap: Trap) -> ! {
    tls::with(|info| info.unwrap().unwind_with(UnwindReason::LibTrap(trap)))
}

/// Carries a Rust panic across wasm code and resumes the panic on the other
/// side.
///
/// # Safety
///
/// Only safe to call when wasm code is on the stack, aka `catch_traps` must
/// have been previously called. Additionally no Rust destructors can be on the
/// stack. They will be skipped and not executed.
pub unsafe fn resume_panic(payload: Box<dyn Any + Send>) -> ! {
    tls::with(|info| info.unwrap().unwind_with(UnwindReason::Panic(payload)))
}

#[cfg(target_os = "windows")]
fn reset_guard_page() {
    extern "C" {
        fn _resetstkoflw() -> winapi::ctypes::c_int;
    }

    // We need to restore guard page under stack to handle future stack overflows properly.
    // https://docs.microsoft.com/en-us/cpp/c-runtime-library/reference/resetstkoflw?view=vs-2019
    if unsafe { _resetstkoflw() } == 0 {
        panic!("failed to restore stack guard page");
    }
}

#[cfg(not(target_os = "windows"))]
fn reset_guard_page() {}

/// Stores trace message with backtrace.
#[derive(Debug)]
pub enum Trap {
    /// A user-raised trap through `raise_user_trap`.
    User(Box<dyn Error + Send + Sync>),
    /// A wasm-originating trap from wasm code itself.
    Wasm {
        /// What sort of trap happened, as well as where in the original wasm module
        /// it happened.
        desc: TrapDescription,
        /// Native stack backtrace at the time the trap occurred
        backtrace: Backtrace,
    },
}

impl fmt::Display for Trap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Trap::User(user) => user.fmt(f),
            Trap::Wasm { desc, .. } => desc.fmt(f),
        }
    }
}

impl std::error::Error for Trap {}

impl Trap {
    /// Construct a new Wasm trap with the given source location and trap code.
    ///
    /// Internally saves a backtrace when constructed.
    pub fn wasm(source_loc: ir::SourceLoc, trap_code: ir::TrapCode) -> Self {
        let desc = TrapDescription {
            source_loc,
            trap_code,
        };
        let backtrace = Backtrace::new();
        Trap::Wasm { desc, backtrace }
    }
}

/// Catches any wasm traps that happen within the execution of `closure`,
/// returning them as a `Result`.
///
/// Highly unsafe since `closure` won't have any dtors run.
pub unsafe fn catch_traps<F>(vmctx: *mut VMContext, mut closure: F) -> Result<(), Trap>
where
    F: FnMut(),
{
    return CallThreadState::new(vmctx).with(|cx| {
        RegisterSetjmp(
            cx.jmp_buf.as_ptr(),
            call_closure::<F>,
            &mut closure as *mut F as *mut u8,
        )
    });

    extern "C" fn call_closure<F>(payload: *mut u8)
    where
        F: FnMut(),
    {
        unsafe { (*(payload as *mut F))() }
    }
}

/// Temporary state stored on the stack which is registered in the `tls` module
/// below for calls into wasm.
pub struct CallThreadState {
    unwind: Cell<UnwindReason>,
    jmp_buf: Cell<*const u8>,
    reset_guard_page: Cell<bool>,
    prev: Option<*const CallThreadState>,
    vmctx: *mut VMContext,
    handling_trap: Cell<bool>,
}

enum UnwindReason {
    None,
    Panic(Box<dyn Any + Send>),
    UserTrap(Box<dyn Error + Send + Sync>),
    LibTrap(Trap),
    Trap { backtrace: Backtrace, pc: usize },
}

impl CallThreadState {
    fn new(vmctx: *mut VMContext) -> CallThreadState {
        CallThreadState {
            unwind: Cell::new(UnwindReason::None),
            vmctx,
            jmp_buf: Cell::new(ptr::null()),
            reset_guard_page: Cell::new(false),
            prev: None,
            handling_trap: Cell::new(false),
        }
    }

    fn with(mut self, closure: impl FnOnce(&CallThreadState) -> i32) -> Result<(), Trap> {
        tls::with(|prev| {
            self.prev = prev.map(|p| p as *const _);
            let ret = tls::set(&self, || closure(&self));
            match self.unwind.replace(UnwindReason::None) {
                UnwindReason::None => {
                    debug_assert_eq!(ret, 1);
                    Ok(())
                }
                UnwindReason::UserTrap(data) => {
                    debug_assert_eq!(ret, 0);
                    Err(Trap::User(data))
                }
                UnwindReason::LibTrap(trap) => Err(trap),
                UnwindReason::Trap { backtrace, pc } => {
                    debug_assert_eq!(ret, 0);
                    let instance = unsafe { InstanceHandle::from_vmctx(self.vmctx) };

                    Err(Trap::Wasm {
                        desc: instance
                            .instance()
                            .trap_registration
                            .get_trap(pc)
                            .unwrap_or_else(|| TrapDescription {
                                source_loc: ir::SourceLoc::default(),
                                trap_code: ir::TrapCode::StackOverflow,
                            }),
                        backtrace,
                    })
                }
                UnwindReason::Panic(panic) => {
                    debug_assert_eq!(ret, 0);
                    std::panic::resume_unwind(panic)
                }
            }
        })
    }

    fn any_instance(&self, func: impl Fn(&InstanceHandle) -> bool) -> bool {
        unsafe {
            if func(&InstanceHandle::from_vmctx(self.vmctx)) {
                return true;
            }
            match self.prev {
                Some(prev) => (*prev).any_instance(func),
                None => false,
            }
        }
    }

    fn unwind_with(&self, reason: UnwindReason) -> ! {
        self.unwind.replace(reason);
        unsafe {
            Unwind(self.jmp_buf.get());
        }
    }

    /// Trap handler using our thread-local state.
    ///
    /// * `pc` - the program counter the trap happened at
    /// * `reset_guard_page` - whether or not to reset the guard page,
    ///   currently Windows specific
    /// * `call_handler` - a closure used to invoke the platform-specific
    ///   signal handler for each instance, if available.
    ///
    /// Attempts to handle the trap if it's a wasm trap. Returns a few
    /// different things:
    ///
    /// * null - the trap didn't look like a wasm trap and should continue as a
    ///   trap
    /// * 1 as a pointer - the trap was handled by a custom trap handler on an
    ///   instance, and the trap handler should quickly return.
    /// * a different pointer - a jmp_buf buffer to longjmp to, meaning that
    ///   the wasm trap was succesfully handled.
    fn handle_trap(
        &self,
        pc: *const u8,
        reset_guard_page: bool,
        call_handler: impl Fn(&SignalHandler) -> bool,
    ) -> *const u8 {
        // If we hit a fault while handling a previous trap, that's quite bad,
        // so bail out and let the system handle this recursive segfault.
        //
        // Otherwise flag ourselves as handling a trap, do the trap handling,
        // and reset our trap handling flag.
        if self.handling_trap.replace(true) {
            return ptr::null();
        }

        // First up see if any instance registered has a custom trap handler,
        // in which case run them all. If anything handles the trap then we
        // return that the trap was handled.
        if self.any_instance(|i| {
            let handler = match i.instance().signal_handler.replace(None) {
                Some(handler) => handler,
                None => return false,
            };
            let result = call_handler(&handler);
            i.instance().signal_handler.set(Some(handler));
            return result;
        }) {
            self.handling_trap.set(false);
            return 1 as *const _;
        }

        // TODO: stack overflow can happen at any random time (i.e. in malloc()
        // in memory.grow) and it's really hard to determine if the cause was
        // stack overflow and if it happened in WebAssembly module.
        //
        // So, let's assume that any untrusted code called from WebAssembly
        // doesn't trap. Then, if we have called some WebAssembly code, it
        // means the trap is stack overflow.
        if self.jmp_buf.get().is_null() {
            self.handling_trap.set(false);
            return ptr::null();
        }
        let backtrace = Backtrace::new_unresolved();
        self.reset_guard_page.set(reset_guard_page);
        self.unwind.replace(UnwindReason::Trap {
            backtrace,
            pc: pc as usize,
        });
        self.handling_trap.set(false);
        self.jmp_buf.get()
    }
}

impl Drop for CallThreadState {
    fn drop(&mut self) {
        if self.reset_guard_page.get() {
            reset_guard_page();
        }
    }
}

// A private inner module for managing the TLS state that we require across
// calls in wasm. The WebAssembly code is called from C++ and then a trap may
// happen which requires us to read some contextual state to figure out what to
// do with the trap. This `tls` module is used to persist that information from
// the caller to the trap site.
mod tls {
    use super::CallThreadState;
    use std::cell::Cell;
    use std::ptr;

    thread_local!(static PTR: Cell<*const CallThreadState> = Cell::new(ptr::null()));

    /// Configures thread local state such that for the duration of the
    /// execution of `closure` any call to `with` will yield `ptr`, unless this
    /// is recursively called again.
    pub fn set<R>(ptr: &CallThreadState, closure: impl FnOnce() -> R) -> R {
        struct Reset<'a, T: Copy>(&'a Cell<T>, T);

        impl<T: Copy> Drop for Reset<'_, T> {
            fn drop(&mut self) {
                self.0.set(self.1);
            }
        }

        PTR.with(|p| {
            let _r = Reset(p, p.replace(ptr));
            closure()
        })
    }

    /// Returns the last pointer configured with `set` above. Panics if `set`
    /// has not been previously called.
    pub fn with<R>(closure: impl FnOnce(Option<&CallThreadState>) -> R) -> R {
        PTR.with(|ptr| {
            let p = ptr.get();
            unsafe { closure(if p.is_null() { None } else { Some(&*p) }) }
        })
    }
}
