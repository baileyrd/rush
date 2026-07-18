//! End-to-end behavioral coverage for `winjob.rs`'s background jobs
//! (`docs/WINDOWS_JOB_CONTROL.md`), driven against the actual compiled
//! `rush` binary — the same black-box shape `tests/exec_behavior.rs`'s own
//! `rush()` helper uses, kept in a separate file since this is a
//! platform-specific milestone easier to find and grow on its own.
//!
//! Milestone 1's scope (see `winjob.rs`'s module doc): a single external
//! command only. These tests exercise exactly that — backgrounding
//! returning immediately, `$!`/`jobs` reflecting it, and pipelines/builtins
//! being rejected outright rather than silently doing the wrong thing.
#![cfg(windows)]

use std::process::Command;

/// Runs `rush -c src`, returning `(stdout, exit status)`.
fn rush(src: &str) -> (String, i32) {
    let output = Command::new(env!("CARGO_BIN_EXE_rush"))
        .arg("-c")
        .arg(src)
        .output()
        .expect("spawn rush");
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        output.status.code().unwrap_or(-1),
    )
}

#[test]
fn background_command_returns_immediately_and_is_listed() {
    // `ping`'s built-in delay stands in for "still running" — there's no
    // `sleep` on Windows. If backgrounding actually blocked until it
    // finished, this whole `-c` invocation would take that long too
    // (several seconds); `cargo test`'s own wall-clock budget for a single
    // test is the real assertion here, not just the output text below.
    let (out, status) = rush("ping -n 5 127.0.0.1 > nul & jobs");
    assert_eq!(status, 0, "stdout was: {out:?}");
    assert!(
        out.contains("Running"),
        "expected a Running job line, got: {out:?}"
    );
}

#[test]
fn dollar_bang_is_the_backgrounded_pid() {
    let (out, status) = rush("ping -n 5 127.0.0.1 > nul & echo $!");
    assert_eq!(status, 0, "stdout was: {out:?}");
    let pid: u32 = out
        .trim()
        .parse()
        .unwrap_or_else(|_| panic!("$! should be a plain pid, got: {out:?}"));
    assert!(pid > 0);
}

#[test]
fn background_pipeline_is_rejected_not_silently_wrong() {
    // Pure builtins on both sides — no dependency on any external tool
    // being on PATH, so this stays deterministic across any Windows
    // runner. Milestone 1 explicitly narrows to a single external command;
    // this must fail loudly, not silently run only the first stage (or
    // worse, both stages un-backgrounded).
    let (_, status) = rush("echo a | echo b &");
    assert_ne!(status, 0);
}

#[test]
fn background_builtin_is_rejected_not_silently_wrong() {
    let (_, status) = rush("echo hi &");
    assert_ne!(status, 0);
}

#[test]
fn jobs_lists_multiple_background_jobs_by_id() {
    // Deliberately not asserting on Running-vs-Done for either job: `-c`
    // scripts never reach the interactive prompt loop that prunes
    // finished jobs (winjob::reap_background), and `jobs`' own state
    // refresh races a fast loopback ping's own completion time closely
    // enough that asserting a specific state would be flaky. What's
    // guaranteed regardless of that race is that both jobs got their own
    // distinct id and are still listed.
    let (out, status) = rush(
        "ping -n 2 127.0.0.1 > nul & \
         ping -n 3 127.0.0.1 > nul & \
         jobs",
    );
    assert_eq!(status, 0, "stdout was: {out:?}");
    assert!(out.contains("[1]"), "expected job [1] listed, got: {out:?}");
    assert!(out.contains("[2]"), "expected job [2] listed, got: {out:?}");
}
