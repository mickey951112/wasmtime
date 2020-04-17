// To handle out-of-bounds reads and writes we use segfaults right now. We only
// want to catch a subset of segfaults, however, rather than all segfaults
// happening everywhere. The purpose of this test is to ensure that we *don't*
// catch segfaults if it happens in a random place in the code, but we instead
// bail out of our segfault handler early.
//
// This is sort of hard to test for but the general idea here is that we confirm
// that execution made it to our `segfault` function by printing something, and
// then we also make sure that stderr is empty to confirm that no weird panics
// happened or anything like that.

use std::env;
use std::process::{Command, ExitStatus};
use wasmtime::*;

const VAR_NAME: &str = "__TEST_TO_RUN";
const CONFIRM: &str = "well at least we ran up to the segfault\n";

fn segfault() -> ! {
    unsafe {
        print!("{}", CONFIRM);
        *(0x4 as *mut i32) = 3;
        unreachable!()
    }
}

fn main() {
    let tests: &[(&str, fn())] = &[
        ("normal segfault", || segfault()),
        ("make instance then segfault", || {
            let store = Store::default();
            let module = Module::new(&store, "(module)").unwrap();
            let _instance = Instance::new(&module, &[]).unwrap();
            segfault();
        }),
    ];
    match env::var(VAR_NAME) {
        Ok(s) => {
            let test = tests
                .iter()
                .find(|p| p.0 == s)
                .expect("failed to find test")
                .1;
            test();
        }
        Err(_) => {
            for (name, _test) in tests {
                runtest(name);
            }
        }
    }
}

fn runtest(name: &str) {
    let me = env::current_exe().unwrap();
    let mut cmd = Command::new(me);
    cmd.env(VAR_NAME, name);
    let output = cmd.output().expect("failed to spawn subprocess");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let mut desc = format!("got status: {}", output.status);
    if !stdout.trim().is_empty() {
        desc.push_str("\nstdout: ----\n");
        desc.push_str("    ");
        desc.push_str(&stdout.replace("\n", "\n    "));
    }
    if !stderr.trim().is_empty() {
        desc.push_str("\nstderr: ----\n");
        desc.push_str("    ");
        desc.push_str(&stderr.replace("\n", "\n    "));
    }
    if is_segfault(&output.status) {
        assert!(
            stdout.ends_with(CONFIRM) && stderr.is_empty(),
            "failed to find confirmation in test `{}`\n{}",
            name,
            desc
        );
    } else {
        panic!("\n\nexpected a segfault on `{}`\n{}\n\n", name, desc);
    }
}

#[cfg(unix)]
fn is_segfault(status: &ExitStatus) -> bool {
    use std::os::unix::prelude::*;

    match status.signal() {
        Some(libc::SIGSEGV) | Some(libc::SIGBUS) => true,
        _ => false,
    }
}

#[cfg(windows)]
fn is_segfault(status: &ExitStatus) -> bool {
    match status.code().map(|s| s as u32) {
        Some(0xc0000005) => true,
        _ => false,
    }
}
