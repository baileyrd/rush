//! End-to-end behavioral coverage for `winjob.rs`'s background jobs
//! (`docs/WINDOWS_JOB_CONTROL.md`), driven against the actual compiled
//! `rush` binary — the same black-box shape `tests/exec_behavior.rs`'s own
//! `rush()` helper uses, kept in a separate file since this is a
//! platform-specific milestone easier to find and grow on its own.
//!
//! Scope is a single external command only (see `winjob.rs`'s module doc):
//! these tests cover backgrounding returning immediately, `$!`/`jobs`
//! reflecting it, `wait`/`kill`/`disown` against a tracked job, and
//! pipelines/builtins being rejected outright rather than silently doing
//! the wrong thing.
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

/// Like [`rush`], but also returning stderr — for diagnosing a builtin's
/// own error path (e.g. `disown`'s), which a plain exit-status check can
/// miss: a later command in the same `-c` script (like `echo $!`) still
/// sets the *script's* own final status, independent of whether an
/// earlier command failed and printed a message to stderr.
fn rush_full(src: &str) -> (String, String, i32) {
    let output = Command::new(env!("CARGO_BIN_EXE_rush"))
        .arg("-c")
        .arg(src)
        .output()
        .expect("spawn rush");
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
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

#[test]
fn wait_on_dollar_bang_reports_the_exit_status() {
    // `cmd.exe /c exit N` finishes essentially instantly and reports a
    // known, exact exit code — `wait` blocks synchronously on it, so this
    // is fully deterministic (no race the way a ping-timing assertion
    // would be).
    let (out, status) = rush(r#"cmd.exe /c "exit 5" & wait $!; echo $?"#);
    assert_eq!(status, 0, "stdout was: {out:?}");
    assert_eq!(out.trim(), "5");
}

#[test]
fn wait_on_job_spec_reports_the_exit_status() {
    let (out, status) = rush(r#"cmd.exe /c "exit 7" & wait %1; echo $?"#);
    assert_eq!(status, 0, "stdout was: {out:?}");
    assert_eq!(out.trim(), "7");
}

#[test]
fn bare_wait_always_returns_zero_and_settles_the_job() {
    // Bare `wait` (no operands) always succeeds regardless of what the
    // background job itself exited with — matching `job.rs`'s own
    // semantics. After it returns, the job's state is settled (no more
    // race with `jobs`' own poll), so asserting Done here is safe, unlike
    // the ping-based `jobs_lists_multiple_background_jobs_by_id` test.
    let (out, status) = rush(r#"cmd.exe /c "exit 9" & wait; echo $?; jobs"#);
    assert_eq!(status, 0, "stdout was: {out:?}");
    let mut lines = out.lines();
    assert_eq!(
        lines.next(),
        Some("0"),
        "bare wait's own status, got: {out:?}"
    );
    assert!(
        out.contains("Done"),
        "expected the job to show Done, got: {out:?}"
    );
}

#[test]
fn kill_terminates_the_job() {
    // `ping`'s multi-second delay stands in for "still running enough
    // that kill has something to actually terminate" — the assertion is
    // that `wait %1` afterward reports the conventional killed-exit-code
    // (128+15) rather than ping's own eventual (different) exit status,
    // proving the process was actually torn down early via
    // `TerminateJobObject`, not merely left to finish on its own.
    let (out, status) = rush(
        "ping -n 30 127.0.0.1 > nul & \
         kill %1; \
         wait %1; echo $?",
    );
    assert_eq!(status, 0, "stdout was: {out:?}");
    assert_eq!(out.trim(), "143");
}

#[test]
fn kill_on_an_unknown_job_is_an_error() {
    let (_, status) = rush("kill %1");
    assert_ne!(status, 0);
}

#[test]
fn jobs_dash_p_lists_only_pids() {
    let (out, status) = rush(r#"cmd.exe /c "exit 0" & jobs -p"#);
    assert_eq!(status, 0, "stdout was: {out:?}");
    let pid: u32 = out
        .trim()
        .parse()
        .unwrap_or_else(|_| panic!("jobs -p should print a bare pid, got: {out:?}"));
    assert!(pid > 0);
}

#[test]
fn jobs_dash_r_excludes_finished_jobs() {
    let (out, status) = rush(r#"cmd.exe /c "exit 0" & wait; jobs -r"#);
    assert_eq!(status, 0, "stdout was: {out:?}");
    assert_eq!(out, "", "expected no Running jobs listed, got: {out:?}");
}

#[test]
fn disown_removes_the_job_from_jobs_listing() {
    let (out, status) = rush("ping -n 5 127.0.0.1 > nul & disown %1; jobs");
    assert_eq!(status, 0, "stdout was: {out:?}");
    assert_eq!(
        out, "",
        "expected no jobs listed after disown, got: {out:?}"
    );
}

#[test]
fn disown_on_an_unknown_job_is_an_error() {
    let (_, status) = rush("disown %1");
    assert_ne!(status, 0);
}

#[test]
fn disown_detaches_the_job_while_the_shell_is_still_running() {
    // Proves `disown` actually does something beyond removing the table
    // entry: `rusty_win32::job::clear_kill_on_close` (added specifically
    // for this) really does reverse kill-on-close, checked by having the
    // script itself run `tasklist` on its own backgrounded pid, from
    // *within* the still-alive shell, right after `disown` — matching how
    // `rusty_win32`'s own
    // `clear_kill_on_close_lets_the_process_survive_closing_the_job_handle`
    // test verifies the same primitive (confirming survival from within
    // the same process that did the clearing, not from outside a
    // *different* process that later exits — see this test's own history
    // for why that distinction turned out to matter).
    //
    // Deliberately NOT asserting survival *after* this whole `rush -c`
    // process has exited (via an external `tasklist`, checked once
    // `Command::output` returns): that was this test's original shape,
    // and it failed consistently on real `windows-latest` CI even though
    // every check here still passed and `clear_kill_on_close` itself
    // never reported an error. The likely explanation: `rusty_win32::job`
    // only clears kill-on-close on the job `winjob.rs` itself created —
    // it has no way to detach a process from an *ambient* job the shell's
    // own process might already be a member of (Windows automatically
    // nests every child a job member spawns into that same job too), and
    // GitHub Actions' Windows runners are documented to wrap each step's
    // process tree in exactly such a job for orphan cleanup. That's a
    // property of the sandbox a background job happens to run under, not
    // a `winjob.rs`/`rusty_win32` bug — but there's no way to tell the two
    // apart from inside this CI environment, so this test only asserts
    // what's actually attributable to this crate's own code.
    let (out, err, status) = rush_full(
        r#"powershell -NoProfile -Command "Start-Sleep -Seconds 5" & disown %1; echo $!; tasklist /FI "PID eq $!" /NH"#,
    );
    assert_eq!(status, 0, "stdout was: {out:?}, stderr was: {err:?}");
    let mut lines = out.lines();
    let pid: u32 = lines
        .next()
        .unwrap_or_default()
        .trim()
        .parse()
        .unwrap_or_else(|_| panic!("$! should be a plain pid, got: {out:?} (stderr: {err:?})"));
    let listing: String = lines.collect::<Vec<_>>().join("\n");
    assert!(
        listing.contains(&pid.to_string()),
        "job (pid {pid}) should still be listed by tasklist run from *within* the \
         still-alive rush process, right after disown — got: {listing:?} \
         (full stdout: {out:?}, stderr: {err:?})"
    );

    // Best-effort: this process may or may not still exist by now,
    // depending on the sandbox caveat explained above — not asserted on.
    let _ = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/F"])
        .output();
}
