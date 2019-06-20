mod runtime;

#[cfg(all(unix))]
#[test]
fn sched_yield() -> Result<(), String> {
    runtime::run_test("tests/misc-tests/bin/sched_yield.wasm")
}

#[cfg(all(unix))]
#[test]
fn truncation_rights() -> Result<(), String> {
    runtime::run_test("tests/misc-tests/bin/truncation_rights.wasm")
}

#[cfg(all(unix))]
#[test]
fn unlink_directory() -> Result<(), String> {
    runtime::run_test("tests/misc-tests/bin/unlink_directory.wasm")
}

#[cfg(all(unix))]
#[test]
fn remove_nonempty_directory() -> Result<(), String> {
    runtime::run_test("tests/misc-tests/bin/remove_nonempty_directory.wasm")
}

#[cfg(all(unix))]
#[test]
fn interesting_paths() -> Result<(), String> {
    runtime::run_test("tests/misc-tests/bin/interesting_paths.wasm")
}

#[cfg(all(unix))]
#[test]
fn nofollow_errors() -> Result<(), String> {
    runtime::run_test("tests/misc-tests/bin/nofollow_errors.wasm")
}

#[cfg(all(unix))]
#[test]
fn symlink_loop() -> Result<(), String> {
    runtime::run_test("tests/misc-tests/bin/symlink_loop.wasm")
}

#[cfg(all(unix))]
#[test]
fn close_preopen() -> Result<(), String> {
    runtime::run_test("tests/misc-tests/bin/close_preopen.wasm")
}

#[cfg(all(unix))]
#[test]
fn clock_time_get() -> Result<(), String> {
    runtime::run_test("tests/misc-tests/bin/clock_time_get.wasm")
}

#[cfg(all(unix))]
#[test]
fn readlink_no_buffer() -> Result<(), String> {
    runtime::run_test("tests/misc-tests/bin/readlink_no_buffer.wasm")
}

#[cfg(all(unix))]
#[test]
fn isatty() -> Result<(), String> {
    runtime::run_test("tests/misc-tests/bin/isatty.wasm")
}

#[cfg(all(unix))]
#[test]
fn directory_seek() -> Result<(), String> {
    runtime::run_test("tests/misc-tests/bin/directory_seek.wasm")
}

#[test]
fn big_random_buf() -> Result<(), String> {
    runtime::run_test("tests/misc-tests/bin/big_random_buf.wasm")
}
