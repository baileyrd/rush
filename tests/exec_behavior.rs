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
    rush_argv(src, &[])
}

/// Like [`rush`], but with `rush -c src [argv...]` — `argv[0]` becomes `$0`,
/// the rest `$1`… (and so `"$@"`).
fn rush_argv(src: &str, argv: &[&str]) -> (String, i32) {
    let output = Command::new(env!("CARGO_BIN_EXE_rush"))
        .arg("-c")
        .arg(src)
        .args(argv)
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
fn errexit_matches_bashs_positionally_last_rule() {
    // A failing command is exempt from `set -e` unless it's positionally
    // last in its &&/|| list — matching real bash, not the simpler "whichever
    // pipeline happened to run last" rule this used to use.
    assert_eq!(rush("set -e; false && true; echo survived").0, "survived\n");
    assert_eq!(rush("set -e; false || true; echo survived").0, "survived\n");
    assert_eq!(
        rush("set -e; false && true && true; echo survived").0,
        "survived\n"
    );

    // `false` IS positionally last here, so it should still fire.
    assert_eq!(rush("set -e; true && false; echo unreached").0, "");
    assert_eq!(
        rush("set -e; true && true && false; echo unreached").0,
        ""
    );
    // The simple case (a single failing command) is unaffected.
    assert_eq!(rush("set -e; false; echo unreached").0, "");

    // `$?` still reflects whatever actually happened, independent of errexit.
    assert_eq!(rush("false && true; echo status=$?").0, "status=1\n");

    // if/while conditions remain exempt regardless (a separate, pre-existing
    // exemption via exec_cond, not this fix — must not regress).
    assert_eq!(
        rush("set -e; if false; then echo yes; else echo no; fi; echo survived").0,
        "no\nsurvived\n"
    );
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

#[test]
fn assignment_takes_the_status_of_its_own_command_substitution() {
    // POSIX: a variable-assignment-only command's exit status is that of the
    // last command substitution performed while expanding it, not always 0.
    assert_eq!(rush("x=$(false); echo $?").0, "1\n");
    assert_eq!(rush("x=$(true); echo $?").0, "0\n");
    // No substitution at all: still resets to 0, not a stale prior status.
    assert_eq!(rush("false; x=5; echo $?").0, "0\n");
    // Reading $? directly (no substitution) isn't itself corrupted by the
    // mechanism that detects "did a substitution just run".
    assert_eq!(rush("false; x=$?; echo $x:$?").0, "1:0\n");
    // Several assignments on one line: the *last* substitution counts.
    assert_eq!(rush("a=$(true) b=$(false); echo $?").0, "1\n");
    // An assignment *prefixed onto a real command* is unaffected: the
    // command's own status counts, not the substitution's.
    assert_eq!(rush("FOO=$(false) true; echo $?").0, "0\n");
}

#[cfg(unix)]
#[test]
fn nested_substitution_status_reflects_its_own_last_command() {
    // The outer substitution's exit status is that of its own last command
    // ("echo inner"), not the inner assignment's ("y=$(false)").
    assert_eq!(rush("x=$(y=$(false); echo inner); echo $x:$?").0, "inner:0\n");
}

#[test]
fn for_loop_without_in_clause_iterates_positional_params() {
    // POSIX: `for name; do ...` (no `in`) iterates "$@", same as if `in "$@"`
    // had been written. `argv[0]` becomes $0, so the loop only sees a, b, c.
    let (out, _) = rush_argv("for x; do echo got:$x; done", &["dummy", "a", "b", "c"]);
    assert_eq!(out, "got:a\ngot:b\ngot:c\n");

    // No positional params at all: zero iterations, no error.
    assert_eq!(rush("for x; do echo unreached; done; echo after").0, "after\n");

    // An *explicit* `in` with no words is a real empty list, not "$@" — a
    // different, pre-existing case this fix must not disturb.
    let (out, _) = rush_argv("for x in; do echo unreached; done; echo after", &["dummy", "a", "b"]);
    assert_eq!(out, "after\n");
}

#[cfg(unix)]
#[test]
fn compound_command_as_one_stage_of_a_real_pipeline() {
    // The headline C3 case: a subshell feeding a real external command.
    assert_eq!(rush("(echo hello; echo world) | grep hel").0, "hello\n");

    // A compound as the *first* stage.
    assert_eq!(rush("if true; then echo yes; else echo no; fi | tr a-z A-Z").0, "YES\n");

    // A compound in the *middle* of a 3-stage pipeline — receiving input from
    // the previous stage and feeding the next.
    assert_eq!(rush("echo hi | (cat; echo done) | tr a-z A-Z").0, "HI\nDONE\n");

    // A loop feeding an external command.
    assert_eq!(
        rush("i=0; (while [ $i -lt 3 ]; do echo $i; i=$((i+1)); done) | wc -l").0,
        "3\n"
    );

    // The pipeline's exit status is still the *last* stage's, same as any
    // other pipeline.
    let (_, status) = rush("(echo hi) | false");
    assert_eq!(status, 1);

    // Forked-subshell isolation (G10) still holds when the subshell is a
    // pipeline stage, not just when it's the whole pipeline: `cd` inside
    // doesn't leak out to the outer shell even though it's piped to `cat`.
    assert_eq!(
        rush("cd /tmp && (cd /; pwd) | cat; pwd").0,
        "/\n/tmp\n"
    );
}
