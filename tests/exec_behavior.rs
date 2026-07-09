//! Black-box coverage for `exec.rs`'s runtime: pipeline wiring, redirection
//! routing, exit-status propagation, and compound/subshell semantics —
//! against the actual compiled `rush` binary (`rush -c ...`).
//!
//! Deliberately not in-process (`parser::parse` + `run_list`/`capture_list`
//! called directly from `src/exec.rs`'s own `#[cfg(test)]` module): that
//! turned out to have real footguns. `capture_list` doesn't track `$?`
//! across jobs (it's built only for concatenating `$(...)` output, not for
//! replaying whole-script semantics) and rejects any compound command
//! outright (not just one mid-pipeline). And a builtin's redirects are wired
//! up via a process-wide `dup2` around the call (`run_builtin_foreground`),
//! which races across `cargo test`'s concurrently-running test threads since
//! fd 0/1/2 aren't per-thread. A real subprocess per test — which is what
//! `CARGO_BIN_EXE_rush` (only available to integration tests, hence `tests/`
//! rather than inline) is for — sidesteps all of that at once.

use std::process::Command;

/// Runs `rush -c src`, returning `(stdout, exit status)`.
fn rush(src: &str) -> (String, i32) {
    let output = Command::new(env!("CARGO_BIN_EXE_rush"))
        .arg("-c")
        .arg(src)
        .output()
        .expect("spawn rush");
    (String::from_utf8_lossy(&output.stdout).into_owned(), output.status.code().unwrap_or(-1))
}

#[test]
fn pipeline_wires_stdout_to_stdin_across_two_real_processes() {
    let (out, status) = rush("echo hi | tr a-z A-Z");
    assert_eq!(out, "HI\n");
    assert_eq!(status, 0);
}

#[test]
fn redirect_write_then_append_then_read_back() {
    let path = std::env::temp_dir().join(format!("rush_exec_test_redirect_{}.txt", std::process::id()));
    let path = path.to_str().unwrap();

    let (_, status) = rush(&format!("echo one > {path}; echo two >> {path}"));
    assert_eq!(status, 0);
    assert_eq!(rush(&format!("cat < {path}")).0, "one\ntwo\n");

    let _ = std::fs::remove_file(path);
}

#[test]
fn exit_status_propagation_and_short_circuit() {
    assert_eq!(rush("false; echo $?").0, "1\n");
    assert_eq!(rush("false || echo ok").0, "ok\n");
    assert_eq!(rush("true && echo ok").0, "ok\n");
    assert_eq!(rush("false && echo bad; echo tail").0, "tail\n");
}

#[test]
fn real_fd_routing_for_2_and_1_into_a_pipe() {
    // Regression lock-in for the G10 fix: `2>&1` combined with a pipe used
    // to leak stderr straight to the terminal instead of routing it through
    // the pipe (`Stdio::piped()` doesn't expose a write end that fd 2 could
    // be dup'd onto before spawn).
    let (out, _) = rush("cat /no/such/rush/test/file 2>&1 | cat");
    assert!(out.contains("No such file or directory"), "got: {out:?}");
}

#[test]
fn compound_if_and_while_status() {
    assert_eq!(rush("if true; then echo yes; else echo no; fi").0, "yes\n");
    assert_eq!(rush("if false; then echo yes; else echo no; fi").0, "no\n");
    assert_eq!(
        rush("i=0; while [ $i -lt 3 ]; do echo $i; i=$((i+1)); done").0,
        "0\n1\n2\n"
    );
}

#[test]
fn heredoc_feeds_stdin() {
    assert_eq!(rush("cat <<EOF\nhello\nEOF\n").0, "hello\n");
}

#[cfg(unix)]
#[test]
fn forked_subshell_isolates_exit_from_the_shell() {
    // Regression lock-in for the G10 fix: `exit` inside `(...)` used to exit
    // the whole shell (no real fork, just state save/restore) — if it
    // regressed, this whole test *process* would exit early instead of
    // failing a normal assertion.
    let (out, status) = rush("(exit 3); echo $?");
    assert_eq!(out, "3\n");
    assert_eq!(status, 0); // the outer script's own last command succeeded
}

#[test]
fn command_substitution_tracks_exit_status_across_its_own_jobs() {
    // Regression lock-in: capture_pipeline used to never update `$?`, so
    // this would have seen whatever `$?` was from *outside* the
    // substitution instead of `false`'s own status.
    assert_eq!(rush(r#"echo "$(false; echo mid=$?)""#).0, "mid=1\n");
}

#[test]
fn plain_assignment_still_resets_status_to_zero() {
    // A value with no command substitution shouldn't leak a stale `$?` from
    // a prior command now that capture_pipeline actively sets it elsewhere.
    assert_eq!(rush("false; x=5; echo $?").0, "0\n");
}

#[cfg(unix)]
#[test]
fn command_substitution_captures_a_sole_compound_command() {
    // Regression lock-in: capture_pipeline used to reject *any* compound
    // command outright (not just one mid-pipeline) via a hard error from
    // expand::expand, so $(if ...) / $(while ...) / $(( subshell )) simply
    // didn't work at all.
    assert_eq!(
        rush("x=$(if true; then echo yes; else echo no; fi); echo $x").0,
        "yes\n"
    );
    assert_eq!(
        rush("x=$(i=0; while [ $i -lt 3 ]; do echo $i; i=$((i+1)); done); echo \"$x\"").0,
        "0\n1\n2\n"
    );
    assert_eq!(rush("x=$( (echo a; echo b) ); echo \"$x\"").0, "a\nb\n");
}

#[cfg(unix)]
#[test]
fn command_substitution_of_a_compound_composes_with_nesting() {
    assert_eq!(
        rush(r#"echo "$(echo outer:$(if true; then echo inner; fi))""#).0,
        "outer:inner\n"
    );
}
