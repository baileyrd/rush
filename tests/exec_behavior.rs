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

use std::io::Write;
use std::process::{Command, Stdio};

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

/// Like [`rush`], but feeding `input` on stdin — for `read` tests.
fn rush_stdin(src: &str, input: &str) -> (String, i32) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_rush"))
        .arg("-c")
        .arg(src)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn rush");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .expect("write stdin");
    let output = child.wait_with_output().expect("wait rush");
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
fn test_builtin_logical_combinators() {
    // `-a`/`-o`, real files, matching bash's actual behavior (`-a` binds
    // tighter than `-o`, `!` negates only the next primary).
    assert_eq!(
        rush("if [ -f Cargo.toml -a -d src ]; then echo yes; else echo no; fi").0,
        "yes\n"
    );
    assert_eq!(
        rush("if [ -f Cargo.toml -a -f /no/such/file ]; then echo yes; else echo no; fi").0,
        "no\n"
    );
    assert_eq!(
        rush("if [ -f /no/such/file -o -d src ]; then echo yes; else echo no; fi").0,
        "yes\n"
    );
    assert_eq!(
        rush("if [ 1 = 2 -o 1 = 1 -a 1 = 2 ]; then echo yes; else echo no; fi").0,
        "no\n"
    );
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

#[test]
fn shift_builtin() {
    let (out, _) = rush_argv("shift; echo \"$@/$#\"", &["dummy", "a", "b", "c"]);
    assert_eq!(out, "b c/2\n");

    let (out, _) = rush_argv("shift 2; echo \"$@/$#\"", &["dummy", "a", "b", "c"]);
    assert_eq!(out, "c/1\n");

    // `shift 0` is a no-op, status 0.
    let (out, status) = rush_argv("shift 0; echo \"status=$?/$@/$#\"", &["dummy", "a", "b"]);
    assert_eq!(out, "status=0/a b/2\n");
    assert_eq!(status, 0);

    // Count greater than `$#`: fails *silently* (no shift, status 1) — bash
    // has no error message for this specific case, only for a genuinely
    // malformed (negative/non-numeric) count.
    let (out, status) = rush_argv("shift 5; echo \"status=$?/$@/$#\"", &["dummy", "a", "b"]);
    assert_eq!(out, "status=1/a b/2\n");
    assert_eq!(status, 0); // the *script's* last command (echo) still succeeds

    // The headline idiom this connects: an argument-parsing loop over
    // positional params via `case`/`shift`.
    let (out, _) = rush_argv(
        "while [ $# -gt 0 ]; do case $1 in -a) echo flag_a;; *) echo \"arg:$1\";; esac; shift; done",
        &["dummy", "-a", "x", "y"],
    );
    assert_eq!(out, "flag_a\narg:x\narg:y\n");
}

#[test]
fn command_type_and_hash_builtins() {
    // `command -v` — the standard portable existence check — is terse: an
    // alias prints its definition, a function/builtin just its bare name, a
    // real executable its resolved path; not found prints nothing and
    // fails.
    assert_eq!(rush("alias ll='ls -l'; command -v ll").0, "alias ll='ls -l'\n");
    assert_eq!(rush("f() { :; }; command -v f").0, "f\n");
    assert_eq!(rush("command -v cd").0, "cd\n");
    let (out, status) = rush("command -v ls");
    assert!(out.trim_end().ends_with("/ls"), "got: {out:?}");
    assert_eq!(status, 0);
    let (out, status) = rush("command -v rush_nonexistent_cmd_xyz");
    assert_eq!(out, "");
    assert_eq!(status, 1);

    // `command -V` and `type` share the same human-readable sentence form.
    assert_eq!(rush("alias ll='ls -l'; command -V ll").0, "ll is aliased to `ls -l'\n");
    assert_eq!(rush("f() { :; }; type f").0, "f is a function\n");
    assert_eq!(rush("type cd").0, "cd is a shell builtin\n");
    assert_eq!(rush("type if").0, "if is a shell keyword\n"); // `type` (not `command`) also covers keywords

    // `type -t` gives just the one-word classification.
    assert_eq!(
        rush("f() { :; }; type -t f; type -t cd; type -t if").0,
        "function\nbuiltin\nkeyword\n"
    );

    // The headline case: `command name` bypasses a shadowing shell function
    // of the same name, running the real builtin/executable instead.
    assert_eq!(
        rush("cd() { echo fake; }; cd; command cd; pwd").0.lines().next().unwrap(),
        "fake"
    );

    // `hash` never caches anything (rush re-searches `$PATH` on every
    // spawn), so it's a narrow stub: `-r` and a bare call are no-ops that
    // still succeed, and `hash name` at least reports whether it resolves.
    assert_eq!(rush("hash -r; echo $?").0, "0\n");
    assert_eq!(rush("hash ls; echo $?").0, "0\n");
    assert_eq!(rush("hash rush_nonexistent_cmd_xyz; echo $?").0, "1\n");
}

#[cfg(unix)]
#[test]
fn wait_builtin_and_last_bg_pid() {
    // `$!` is unset until something's been backgrounded.
    assert_eq!(rush("x=\"[$!]\"; echo \"$x\"").0, "[]\n");

    // `wait $pid` reports that specific background job's own exit status.
    assert_eq!(rush("(exit 5) & p=$!; wait $p; echo \"status=$?\"").0, "status=5\n");

    // `wait %job` reports the job's (last-stage's) exit status.
    assert_eq!(rush("{ sleep 0.1; exit 7; } & wait %1; echo \"status=$?\"").0, "status=7\n");

    // Multiple operands: waits on each in turn, reporting the *last* one's
    // status.
    assert_eq!(
        rush("(exit 3) & p1=$!; (exit 9) & p2=$!; wait $p1 $p2; echo \"status=$?\"").0,
        "status=9\n"
    );

    // `wait` with no operands always succeeds, even with nothing backgrounded.
    assert_eq!(rush("wait; echo \"status=$?\"").0, "status=0\n");
    assert_eq!(
        rush("sleep 0.1 & sleep 0.05 & wait; echo \"status=$?\"").0,
        "status=0\n"
    );

    // Waiting on the *same* already-reaped pid a second time still reports
    // its remembered status, matching a real bash quirk verified directly.
    assert_eq!(
        rush("(exit 2) & p=$!; wait $p; wait $p; echo \"status=$?\"").0,
        "status=2\n"
    );

    // Error cases: an unknown pid/job, and a malformed operand.
    let (out, status) = rush("wait 99999; echo \"status=$?\"");
    assert_eq!(out, "status=127\n");
    assert_eq!(status, 0); // the script's own last command (echo) still succeeds
    assert_eq!(rush("wait %5; echo \"status=$?\"").0, "status=127\n");
    assert_eq!(rush("wait abc; echo \"status=$?\"").0, "status=1\n");
}

#[test]
fn eval_builtin() {
    // Multiple args are joined with a single space before parsing.
    assert_eq!(rush("eval echo a echo b").0, "a echo b\n");

    // Runs in the current environment: no new scope at all.
    assert_eq!(rush("x=1; eval 'y=2; echo $x $y'; echo after:$x:$y").0, "1 2\nafter:1:2\n");

    // No arguments (or all-empty ones) is a no-op that succeeds.
    assert_eq!(rush("eval; echo status:$?").0, "status:0\n");

    // Unlike `source`, `eval` establishes no boundary at all: `return`
    // inside it unwinds the *whole enclosing function*, not just the eval.
    assert_eq!(
        rush("f() { eval 'return 5'; echo not_reached; }; f; echo status:$?").0,
        "status:5\n"
    );

    // Likewise `break`/`continue` propagate straight to the enclosing loop.
    assert_eq!(
        rush("for i in 1 2 3; do eval 'echo hi; break'; echo not_reached; done; echo after").0,
        "hi\nafter\n"
    );

    // A compound command works fine when parsed and run via eval.
    assert_eq!(rush("eval 'for i in a b c; do echo $i; done'").0, "a\nb\nc\n");

    // Exit status is that of the last command eval actually ran.
    assert_eq!(rush("eval false; echo status:$?").0, "status:1\n");

    // A parse error inside eval fails with status 2, without taking down
    // the rest of the script.
    let (_, status) = rush("eval 'if'; echo status:$?");
    assert_eq!(status, 0); // the script's own last command (echo) still succeeds
}

#[cfg(unix)]
#[test]
fn exec_builtin() {
    // With a command: replaces the process image outright — the captured
    // stdout/exit status are the executed command's own.
    assert_eq!(rush("exec echo hello world"), ("hello world\n".to_string(), 0));

    // Command not found: a non-interactive shell (which `rush -c` is)
    // exits immediately with status 127 — the rest of the script (an
    // `echo` right after) never runs at all.
    assert_eq!(
        rush("echo before; exec rush_nonexistent_cmd_xyz; echo after"),
        ("before\n".to_string(), 127)
    );

    // With no command: a no-op that always succeeds.
    assert_eq!(rush("exec; echo status:$?").0, "status:0\n");

    // With no command but a redirect: the redirect is made *permanent*
    // (unlike every other builtin's, which are scoped to just that one
    // call) — everything printed for the rest of the script goes to the
    // file instead of rush's own stdout.
    let path = std::env::temp_dir().join(format!("rush_exec_redirect_{}.txt", std::process::id()));
    let file = path.to_str().unwrap();
    let (out, status) = rush(&format!("exec > {file}; echo redirected; echo more"));
    assert_eq!(out, "");
    assert_eq!(status, 0);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "redirected\nmore\n");
    let _ = std::fs::remove_file(&path);

    // Same for redirecting the shell's own stdin: a `read` right after
    // picks up the file's contents instead of rush's real stdin.
    let in_path = std::env::temp_dir().join(format!("rush_exec_stdin_{}.txt", std::process::id()));
    let in_file = in_path.to_str().unwrap();
    std::fs::write(&in_path, "hi\n").unwrap();
    assert_eq!(
        rush(&format!("exec 0<{in_file}; read line; echo got:$line")).0,
        "got:hi\n"
    );
    let _ = std::fs::remove_file(&in_path);
}

#[test]
fn source_and_dot_builtin() {
    let path = std::env::temp_dir().join(format!("rush_source_lib_{}.sh", std::process::id()));
    std::fs::write(&path, "FOO=bar\ngreet() { echo \"hi $1\"; }\n").unwrap();
    let file = path.to_str().unwrap();

    // `.` runs the file in the current environment: variables and functions
    // it defines stick around afterward, and `source` is a plain synonym.
    assert_eq!(rush(&format!(". {file}; echo $FOO; greet world")).0, "bar\nhi world\n");
    assert_eq!(rush(&format!("source {file}; echo $FOO")).0, "bar\n");

    // With no extra args, the caller's own positional params show through
    // unchanged; extra args temporarily replace them, restored afterward.
    let args_path = std::env::temp_dir().join(format!("rush_source_args_{}.sh", std::process::id()));
    std::fs::write(&args_path, "echo \"args:$#:$*\"\n").unwrap();
    let args_file = args_path.to_str().unwrap();
    assert_eq!(
        rush(&format!("f() {{ . {args_file}; echo \"after:$#:$*\"; }}; f a b c")).0,
        "args:3:a b c\nafter:3:a b c\n"
    );
    assert_eq!(
        rush(&format!("f() {{ . {args_file} x y; echo \"after:$#:$*\"; }}; f a b c")).0,
        "args:2:x y\nafter:3:a b c\n"
    );

    // `return` inside the sourced file ends only the sourcing, not the
    // caller; `break`/`continue` are NOT consumed and propagate transparently
    // to an enclosing loop back in the calling context.
    let ret_path = std::env::temp_dir().join(format!("rush_source_return_{}.sh", std::process::id()));
    std::fs::write(&ret_path, "echo before\nreturn 5\necho after\n").unwrap();
    let ret_file = ret_path.to_str().unwrap();
    let (out, status) = rush(&format!(". {ret_file}; echo \"status=$?\"; echo done"));
    assert_eq!(out, "before\nstatus=5\ndone\n");
    assert_eq!(status, 0);

    let brk_path = std::env::temp_dir().join(format!("rush_source_break_{}.sh", std::process::id()));
    std::fs::write(&brk_path, "echo in-lib\nbreak\necho unreached\n").unwrap();
    let brk_file = brk_path.to_str().unwrap();
    assert_eq!(
        rush(&format!("for i in 1 2 3; do . {brk_file}; echo unreached-loop-body; done; echo after-loop")).0,
        "in-lib\nafter-loop\n"
    );

    // Missing file: a failure status (error text goes to stderr).
    let (_, status) = rush(". /no/such/rush_source_test_file.sh");
    assert_eq!(status, 1);

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&args_path);
    let _ = std::fs::remove_file(&ret_path);
    let _ = std::fs::remove_file(&brk_path);
}

#[cfg(unix)]
#[test]
fn source_searches_path_for_a_readable_but_not_executable_bare_name() {
    let dir = std::env::temp_dir().join(format!("rush_source_pathdir_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let lib = dir.join("rush_source_path_lib.sh");
    std::fs::write(&lib, "PATHVAR=hit\n").unwrap();

    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(&lib).unwrap().permissions();
    perms.set_mode(0o644); // readable, not executable
    std::fs::set_permissions(&lib, perms).unwrap();

    let src = format!(
        "PATH=$PATH:{}; . rush_source_path_lib.sh; echo $PATHVAR",
        dir.to_str().unwrap()
    );
    assert_eq!(rush(&src).0, "hit\n");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn getopts_builtin() {
    // Combined short flags (`-abc` = `-a -b -c`): `$OPTIND` stays put while
    // still inside the same word, advancing only once it's exhausted.
    let (out, _) = rush_argv(
        "while getopts \"abc\" opt; do echo \"opt=$opt OPTIND=$OPTIND\"; done",
        &["dummy", "-abc", "foo"],
    );
    assert_eq!(out, "opt=a OPTIND=1\nopt=b OPTIND=1\nopt=c OPTIND=2\n");

    // An option requiring an argument: attached to the same word, or taken
    // from the next word.
    let (out, _) = rush_argv(
        "while getopts \"b:\" opt; do echo \"opt=$opt arg=$OPTARG\"; done",
        &["dummy", "-bfoo"],
    );
    assert_eq!(out, "opt=b arg=foo\n");
    let (out, _) = rush_argv(
        "while getopts \"b:c:\" opt; do echo \"opt=$opt arg=$OPTARG\"; done",
        &["dummy", "-b", "foo", "-c", "bar"],
    );
    assert_eq!(out, "opt=b arg=foo\nopt=c arg=bar\n");

    // `--` ends option processing without being consumed as an option; the
    // rest (including anything looking like an option) becomes ordinary
    // positional args from here on.
    let (out, _) = rush_argv(
        "while getopts \"ab\" opt; do echo \"opt=$opt\"; done; echo \"rest=$@\"",
        &["dummy", "-a", "--", "-b"],
    );
    assert_eq!(out, "opt=a\nrest=-a -- -b\n");

    // Silent mode (`optstring` starts with `:`): unknown option and missing
    // argument each get their own distinguishable `name` value, with no
    // diagnostic printed. Uses `getopts`'s own explicit `arg...` form rather
    // than positional params, since these are single-shot (no loop).
    let (out, status) = rush("getopts \":ab\" opt -z; echo \"$?:[$opt][$OPTARG]\"");
    assert_eq!(out, "0:[?][z]\n");
    assert_eq!(status, 0);
    let (out, _) = rush("getopts \":b:\" opt -b; echo \"[$opt][$OPTARG]\"");
    assert_eq!(out, "[:][b]\n");

    // The headline idiom this connects: a real CLI-argument-parsing loop,
    // consuming recognized flags then handing the rest to the caller via
    // `shift $((OPTIND-1))`.
    let (out, _) = rush_argv(
        "verbose=0; while getopts \"vo:\" opt; do case $opt in v) verbose=1;; o) outfile=$OPTARG;; esac; done; \
         shift $((OPTIND-1)); echo \"verbose=$verbose outfile=$outfile rest=$@\"",
        &["dummy", "-v", "-o", "out.txt", "file1", "file2"],
    );
    assert_eq!(out, "verbose=1 outfile=out.txt rest=file1 file2\n");
}

#[test]
fn local_builtin_scopes_variables_to_the_function_call() {
    // The headline case: a function's own counter no longer clobbers the
    // caller's variable of the same name.
    assert_eq!(
        rush(
            "i=100; f() { local i=0; while [ $i -lt 3 ]; do i=$((i+1)); done; echo \"in f: $i\"; }; \
             f; echo \"top: $i\""
        )
        .0,
        "in f: 3\ntop: 100\n"
    );

    // A bare `local name` (no `=value`) leaves it genuinely unset within the
    // function, not merely `""` — `${x-unset}` only fires for a truly unset
    // variable.
    assert_eq!(
        rush("x=outer; f() { local x; echo \"[${x-unset}]\"; }; f").0,
        "[unset]\n"
    );

    // Nested calls: an inner function's own `local` of the same name
    // shadows further, and restores to the *enclosing* call's local value
    // (not the top-level one) when it returns.
    assert_eq!(
        rush(
            "x=top; f() { local x=in_f; echo \"f before g: $x\"; g; echo \"f after g: $x\"; }; \
             g() { local x=in_g; echo \"in g: $x\"; }; f; echo \"top: $x\""
        )
        .0,
        "f before g: in_f\nin g: in_g\nf after g: in_f\ntop: top\n"
    );

    // `local` at the top level (not inside any function) is a usage error
    // and does not fall through to setting a plain global variable.
    let (out, status) = rush("local x=1; echo \"status=$?/[$x]\"");
    assert_eq!(out, "status=1/[]\n");
    assert_eq!(status, 0); // the script's last command (echo) still succeeds
}

#[test]
fn ifs_driven_word_splitting() {
    // POSIX field splitting honors `$IFS`, not a hardcoded whitespace set.
    assert_eq!(
        rush("IFS=,; x=a,,b,c; for w in $x; do echo \"[$w]\"; done").0,
        "[a]\n[]\n[b]\n[c]\n"
    );

    // `IFS=` (explicitly empty, not unset) disables splitting entirely: the
    // whole expansion is one field.
    assert_eq!(
        rush("IFS=; x=\"a  b\"; for w in $x; do echo \"[$w]\"; done").0,
        "[a  b]\n"
    );

    // Restoring default behavior (IFS unset) still splits on whitespace.
    assert_eq!(
        rush("x=\"a  b   c\"; for w in $x; do echo \"[$w]\"; done").0,
        "[a]\n[b]\n[c]\n"
    );
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

#[test]
fn read_builtin_splits_fields_and_reports_eof() {
    // Piped stdin, default `REPLY`.
    assert_eq!(rush_stdin("read; echo \"got:$REPLY\"", "hi\n").0, "got:hi\n");

    // Named variables, default IFS; excess input goes to the *last* name
    // verbatim (original spacing intact), not re-split.
    assert_eq!(rush_stdin("read x y; echo \"[$x][$y]\"", "a b c d\n").0, "[a][b c d]\n");
    // Fewer fields than names: the rest get the empty string.
    assert_eq!(rush_stdin("read x y z; echo \"[$x][$y][$z]\"", "a  b\n").0, "[a][b][]\n");

    // Custom `$IFS`: each non-whitespace occurrence delimits its own field,
    // even empty.
    assert_eq!(
        rush_stdin("IFS=,; read a b c d; echo \"[$a][$b][$c][$d]\"", "a,b,,c\n").0,
        "[a][b][][c]\n"
    );

    // Exit status: 0 for a newline-terminated line, 1 on EOF — even when a
    // trailing unterminated line was still read and assigned.
    assert_eq!(rush_stdin("read x; echo \"$?:[$x]\"", "hello").0, "1:[hello]\n");
    assert_eq!(rush_stdin("read x; echo \"$?:[$x]\"", "").0, "1:[]\n");
}

#[test]
fn read_backslash_escaping_and_raw_mode() {
    // Default (non-`-r`): `\<newline>` continues the line (both dropped, no
    // field boundary at the join); `\<char>` drops the backslash and keeps
    // `<char>` from acting as a separator even if it's whitespace.
    assert_eq!(rush_stdin("read x y; echo \"[$x][$y]\"", "a\\\nb c\n").0, "[ab][c]\n");
    assert_eq!(rush_stdin("read x y; echo \"[$x][$y]\"", "a\\ b c\n").0, "[a b][c]\n");

    // `-r` disables all of that: the backslash is just an ordinary character.
    assert_eq!(rush_stdin("read -r x y; echo \"[$x][$y]\"", "a\\ b c\n").0, "[a\\][b c]\n");
}

#[cfg(unix)]
#[test]
fn while_read_loop_reads_from_a_redirected_file() {
    // The headline C7 case: `read` inside a `while` loop whose *compound*
    // (not per-iteration) redirect feeds it — this also exercises the fix
    // letting a redirect trail a compound command's close at all (`done <
    // file` used to be silently dropped: the file's contents were never
    // wired to fd 0, so the loop read the shell's real stdin instead).
    let path = std::env::temp_dir().join(format!("rush_read_loop_{}.txt", std::process::id()));
    std::fs::write(&path, "a\nb\nc\n").unwrap();
    let src = format!("while read line; do echo \"L:$line\"; done < {}", path.to_str().unwrap());
    assert_eq!(rush(&src).0, "L:a\nL:b\nL:c\n");
    let _ = std::fs::remove_file(&path);
}

#[cfg(unix)]
#[test]
fn redirect_trailing_a_compound_command_is_applied() {
    let path = std::env::temp_dir().join(format!("rush_compound_redir_{}.txt", std::process::id()));
    std::fs::write(&path, "one\ntwo\n").unwrap();
    let file = path.to_str().unwrap();

    // A brace group's own redirect.
    assert_eq!(rush(&format!("{{ cat; }} < {file}")).0, "one\ntwo\n");
    // A subshell's own redirect.
    assert_eq!(rush(&format!("(cat) < {file}")).0, "one\ntwo\n");
    // Still applies when the compound is also a pipeline stage.
    assert_eq!(rush(&format!("(cat) < {file} | tr a-z A-Z")).0, "ONE\nTWO\n");
    // And when the whole compound is captured via `$(...)`.
    assert_eq!(rush(&format!("x=$(cat < {file}); echo \"$x\"")).0, "one\ntwo\n");

    // Output redirect trailing a `while` loop.
    let out_path = std::env::temp_dir().join(format!("rush_compound_redir_out_{}.txt", std::process::id()));
    let out_file = out_path.to_str().unwrap();
    rush(&format!("i=0; while [ $i -lt 2 ]; do echo hi; i=$((i+1)); done > {out_file}"));
    assert_eq!(std::fs::read_to_string(&out_path).unwrap(), "hi\nhi\n");

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&out_path);
}

#[test]
fn printf_builtin() {
    // Cycling the format over extra arguments, and defaulting a missing one.
    assert_eq!(rush(r#"printf "%s-%d\n" a 1 b 2 c"#).0, "a-1\nb-2\nc-0\n");

    // Width/flags, hex/octal, `%c`, `%b`'s escape processing (vs. `%s`'s
    // lack of it), and `%%`.
    assert_eq!(rush(r#"printf "%5d|%-5d|%05d\n" 3 3 3"#).0, "    3|3    |00003\n");
    assert_eq!(rush(r#"printf "%x %o %X\n" 255 8 255"#).0, "ff 10 FF\n");
    assert_eq!(rush(r#"printf "%c\n" hello"#).0, "h\n");
    assert_eq!(rush(r#"printf "%b\n" "a\tb\nc""#).0, "a\tb\nc\n");
    assert_eq!(rush(r#"printf "%s\n" "a\tb""#).0, "a\\tb\n");
    assert_eq!(rush(r#"printf "100%%\n""#).0, "100%\n");

    // Malformed numeric input: still formats (as 0) and reports 1, not a
    // hard error that aborts output.
    let (out, status) = rush(r#"printf "%d\n" abc"#);
    assert_eq!(out, "0\n");
    assert_eq!(status, 1);
}

#[cfg(unix)]
#[test]
fn heredoc_trailing_a_compound_command_feeds_it() {
    // A here-doc attached to a compound's close, not a simple command's —
    // e.g. `while read line; do …; done <<EOF`, a common idiom for feeding
    // literal inline data into a read loop.
    assert_eq!(
        rush("while read line; do echo \"L:$line\"; done <<EOF\na\nb\nEOF").0,
        "L:a\nL:b\n"
    );
}
