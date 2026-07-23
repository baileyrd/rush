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

/// A temp path spliced into shell source must be slash-separated on every
/// platform: a bare `\` in shell text is an escape character and quote
/// removal would eat it, and Windows' file APIs accept `/` anyway.
fn shell_path(path: &std::path::Path) -> String {
    path.to_str().unwrap().replace('\\', "/")
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

/// Like [`rush`], but returning stderr instead of stdout — for `set -x`.
fn rush_stderr(src: &str) -> (String, i32) {
    let output = Command::new(env!("CARGO_BIN_EXE_rush"))
        .arg("-c")
        .arg(src)
        .output()
        .expect("spawn rush");
    (String::from_utf8_lossy(&output.stderr).into_owned(), output.status.code().unwrap_or(-1))
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
    write_stdin_concurrently(&mut child, input);
    let output = child.wait_with_output().expect("wait rush");
    (String::from_utf8_lossy(&output.stdout).into_owned(), output.status.code().unwrap_or(-1))
}

/// Feed `input` to `child`'s stdin on a background thread rather than
/// blocking the caller on `write_all` before it starts draining stdout —
/// writing synchronously then calling `wait_with_output` deadlocks the
/// instant the child fills its stdout (or stderr) pipe before finishing
/// its own read of stdin, since neither side is then making progress.
/// Never triggered on Linux's large auto-growing pipe buffer, but macOS's
/// is a small fixed 16KB, so a test comfortably under that limit elsewhere
/// can hang indefinitely there — no per-test timeout in `cargo test` means
/// this reads as a stuck CI job, not a failing test.
fn write_stdin_concurrently(child: &mut std::process::Child, input: &str) {
    let mut stdin = child.stdin.take().unwrap();
    let input = input.to_string();
    std::thread::spawn(move || {
        let _ = stdin.write_all(input.as_bytes());
    });
}

/// Pipe `input` as the command source into a *plain* `rush` (no `-c`/`-s`)
/// with a non-terminal stdin — bash's non-interactive "read commands from
/// stdin" mode. Returns stdout.
#[cfg(unix)]
fn rush_stdin_plain(input: &str) -> String {
    let mut child = Command::new(env!("CARGO_BIN_EXE_rush"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn rush");
    write_stdin_concurrently(&mut child, input);
    let output = child.wait_with_output().expect("wait rush");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Like [`rush_stdin`], but returning stderr instead of stdout — for
/// `select`'s menu/prompt, which (like real bash) goes to stderr rather
/// than stdout.
fn rush_stdin_stderr(src: &str, input: &str) -> (String, i32) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_rush"))
        .arg("-c")
        .arg(src)
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn rush");
    write_stdin_concurrently(&mut child, input);
    let output = child.wait_with_output().expect("wait rush");
    (String::from_utf8_lossy(&output.stderr).into_owned(), output.status.code().unwrap_or(-1))
}

/// Runs the compiled `rush` binary with **no** `-c`/file argument — its
/// interactive REPL — feeding `input` on stdin and returning `(stdout,
/// stderr)`. Confirmed directly that rush enters `interactive()` regardless
/// of whether stdin is a real TTY, so a plain piped-in script exercises it
/// the same way a human typing at a terminal would (prompts go to neither
/// stream, confirmed directly, so they don't pollute the assertions below).
/// Each call gets its own `$HOME` (so `~/.rush_history`/`~/.rushrc` can't
/// leak between tests or pick up a real one) — the counter keeps concurrent
/// `cargo test` runs of this from colliding on the same directory.
fn rush_interactive(input: &str) -> (String, String) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let home = std::env::temp_dir().join(format!("rush_interactive_test_{}_{n}", std::process::id()));
    std::fs::create_dir_all(&home).expect("create temp HOME");

    // `-i` forces the interactive REPL on a pipe — the only way to drive
    // interactive-only features (history expansion, PS0, IGNOREEOF, …)
    // without a real terminal.
    let mut child = Command::new(env!("CARGO_BIN_EXE_rush"))
        .arg("-i")
        .env("HOME", &home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn rush");
    write_stdin_concurrently(&mut child, input);
    let output = child.wait_with_output().expect("wait rush");
    let _ = std::fs::remove_dir_all(&home);
    (String::from_utf8_lossy(&output.stdout).into_owned(), String::from_utf8_lossy(&output.stderr).into_owned())
}

#[test]
fn pipeline_wires_stdout_to_stdin_across_two_real_processes() {
    let (out, status) = rush("echo hi | tr a-z A-Z");
    assert_eq!(out, "HI\n");
    assert_eq!(status, 0);
}

// A builtin's own redirects (`echo one > path`) are wired up around the
// call by `redirect_stdio` — `dup2` on Unix, a std-handle-slot swap on
// Windows (see docs/ARCHITECTURE.md's G11 analysis).
#[test]
fn redirect_write_then_append_then_read_back() {
    let path = std::env::temp_dir().join(format!("rush_exec_test_redirect_{}.txt", std::process::id()));
    let path = shell_path(&path);

    let (_, status) = rush(&format!("echo one > {path}; echo two >> {path}"));
    assert_eq!(status, 0);
    assert_eq!(rush(&format!("cat < {path}")).0, "one\ntwo\n");

    let _ = std::fs::remove_file(path);
}

#[test]
fn builtin_stderr_and_combined_redirects_reach_the_file() {
    let dir = std::env::temp_dir();
    let err_path = dir.join(format!("rush_builtin_stderr_{}.txt", std::process::id()));
    let err_file = shell_path(&err_path);

    // `2>`: a builtin's own error message lands in the file, not on the
    // shell's stderr — `cd` to a missing directory is the canonical case.
    let (_, status) = rush(&format!("cd rush_missing_dir_xyz 2> {err_file}"));
    assert_eq!(status, 1);
    assert!(
        std::fs::read_to_string(&err_path).unwrap().contains("rush_missing_dir_xyz"),
        "cd's error should have been redirected"
    );

    // `2>&1` after `>`: both streams into one file, bash's ordering rule.
    let both_path = dir.join(format!("rush_builtin_both_{}.txt", std::process::id()));
    let both_file = shell_path(&both_path);
    rush(&format!("echo out > {both_file} 2>&1"));
    assert_eq!(std::fs::read_to_string(&both_path).unwrap(), "out\n");

    // `&>`: the shorthand spelling.
    rush(&format!("echo amp &> {both_file}"));
    assert_eq!(std::fs::read_to_string(&both_path).unwrap(), "amp\n");

    let _ = std::fs::remove_file(&err_path);
    let _ = std::fs::remove_file(&both_path);
}

// Non-Unix only: pins the Windows `read_fd_byte` arm. (On Unix both the
// editor and `read` do raw fd-0 reads, and their interleaving over a *pipe*
// differs from the PTY case this scenario describes — the Unix behavior is
// covered interactively, not reproducible through `rush_interactive`.)
#[cfg(not(unix))]
#[test]
fn read_builtin_shares_the_interactive_editors_input_stream() {
    // In the interactive REPL, `read`'s input line arrives on the same
    // stream the line editor reads command lines from. Off Unix `read`
    // must go through the same `std::io::stdin()` buffer the editor uses
    // whenever fd 0 is *not* redirected — a raw handle read here once left
    // `read` empty-handed and handed its input line to the editor, which
    // then tried to run `hi` as a command.
    let (out, _) = rush_interactive("read x\nhi\necho \"[$x]\"\n");
    assert!(out.contains("[hi]"), "read lost its line to the editor: {out:?}");
}

#[test]
fn read_builtin_from_a_redirected_file_is_scoped_to_the_call() {
    // `read x < file` twice must re-read the file from the top both times:
    // the redirect (and any readahead) is scoped to the one call, nothing
    // buffered may leak into the shell's later stdin reads.
    let path = std::env::temp_dir().join(format!("rush_read_redir_{}.txt", std::process::id()));
    std::fs::write(&path, "first\nsecond\n").unwrap();
    let file = shell_path(&path);
    let (out, _) = rush(&format!("read a < {file}; read b < {file}; echo \"[$a][$b]\""));
    assert_eq!(out, "[first][first]\n");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn heredoc_feeds_a_builtin_and_a_bare_redirect_creates_the_file() {
    // A here-document into an in-process builtin (`read`), no fork involved.
    let (out, _) = rush("read x <<EOF\nfrom-heredoc\nEOF\necho got:$x");
    assert_eq!(out, "got:from-heredoc\n");

    // A redirection with no command words creates/truncates the target (C87).
    let path = std::env::temp_dir().join(format!("rush_bare_redir_{}.txt", std::process::id()));
    let file = shell_path(&path);
    let (_, status) = rush(&format!("> {file}"));
    assert_eq!(status, 0);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "");
    let _ = std::fs::remove_file(&path);
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
fn nounset_rejects_unset_variable_references() {
    // A bare reference to an unset variable is an error that aborts the
    // rest of the script — an `echo` right after never runs.
    let (out, status) = rush("set -u; echo before; echo $UNDEF; echo after");
    assert_eq!(out, "before\n");
    assert_eq!(status, 1);

    // Same for `${name}`, `${#name}`, and the pattern-removal operators —
    // all need the variable's actual value, unlike the family below.
    assert_eq!(rush("set -u; echo ${UNDEF}; echo after").0, "");
    assert_eq!(rush("set -u; echo ${#UNDEF}; echo after").0, "");
    assert_eq!(rush("set -u; echo ${UNDEF#prefix}; echo after").0, "");
    assert_eq!(rush("set -u; echo ${UNDEF%suffix}; echo after").0, "");

    // A previously-set-then-unset variable is treated the same as one that
    // was never set.
    assert_eq!(rush("set -u; unset y; echo $y").0, "");

    // The default/alternate family is exempt — they define their own
    // unset-variable handling, verified directly against real bash.
    assert_eq!(rush("set -u; echo ${UNDEF:-default}; echo ok").0, "default\nok\n");
    assert_eq!(rush("set -u; echo \"${UNDEF:+alt}\"; echo ok").0, "\nok\n");
    assert_eq!(
        rush("set -u; echo ${UNDEF:=created}; echo $UNDEF").0,
        "created\ncreated\n"
    );

    // `$@`/`$*`/`$#`/`$?`/`$$` are always considered set, even when the
    // positional parameters are empty — but a numbered one ($1, ${10}) is
    // still subject to the check when it doesn't exist.
    assert_eq!(
        rush("set -u; for a in \"$@\"; do echo $a; done; echo ok").0,
        "ok\n"
    );
    assert_eq!(rush("set -u; echo \"$*\"; echo \"$#\"; echo ok").0, "\n0\nok\n");
    assert_eq!(rush("set -u; echo $1; echo after").0, "");
    assert_eq!(rush("set -u; echo ${10}; echo after").0, "");

    // A set-but-empty variable is fine — the check is "unset", not "empty".
    assert_eq!(rush("set -u; x=; echo \"[$x]\"; echo ok").0, "[]\nok\n");

    // `set +u` turns it back off.
    assert_eq!(
        rush("set -u; x=1; echo $x; set +u; echo $UNDEF; echo ok").0,
        "1\n\nok\n"
    );
}

#[test]
fn pipefail_reports_the_rightmost_nonzero_stage() {
    // Without pipefail, a pipeline's status is always just its last stage's.
    assert_eq!(rush("false | true; echo $?").0, "0\n");

    // With it: the rightmost non-zero status among all stages, not "the
    // first failure" or "any failure" — specifically the one closest to
    // the end (verified directly against real bash with distinct codes at
    // each position).
    assert_eq!(rush("set -o pipefail; false | true; echo $?").0, "1\n");
    assert_eq!(rush("set -o pipefail; true | false; echo $?").0, "1\n");
    assert_eq!(rush("set -o pipefail; true | true; echo $?").0, "0\n");

    // A subshell as one stage among several needs forking that stage
    // separately from the rest of the pipeline (`job::spawn_compound_stage`)
    // — Unix-only, per docs/ARCHITECTURE.md's G11 analysis (no Windows
    // target ever sets `cfg(unix)`, so there's no fork to reach for here).
    #[cfg(unix)]
    {
        assert_eq!(
            rush("set -o pipefail; (exit 3) | (exit 5) | true; echo $?").0,
            "5\n"
        );
        assert_eq!(
            rush("set -o pipefail; (exit 5) | (exit 3) | (exit 0); echo $?").0,
            "3\n"
        );
    }

    // Applies inside command substitution too, not just a foreground pipeline.
    assert_eq!(rush("set -o pipefail; x=$(false | true); echo $?").0, "1\n");

    // `set +o pipefail` turns it back off.
    assert_eq!(
        rush("set -o pipefail; set +o pipefail; false | true; echo $?").0,
        "0\n"
    );

    // An unrecognized `-o` name is an error, not a silently-ignored no-op.
    let (_, status) = rush("set -o badname");
    assert_eq!(status, 1);
}

#[test]
fn xtrace_echoes_each_command_before_running_it() {
    // A plain command, an assignment, and a pipeline's own stages each
    // get their own traced line, prefixed with `$PS4` (default `+ `).
    assert_eq!(rush_stderr("set -x; echo hi").0, "+ echo hi\n");
    assert_eq!(rush_stderr("set -x; x=5").0, "+ x=5\n");
    assert_eq!(rush_stderr("set -x; echo a | tr a-z A-Z").0, "+ echo a\n+ tr a-z A-Z\n");

    // A word containing whitespace is re-quoted with single quotes.
    assert_eq!(rush_stderr("set -x; echo \"a b\" c").0, "+ echo 'a b' c\n");

    // A leading `NAME=value` prefix traces on its own line before the
    // command it applies to.
    assert_eq!(rush_stderr("set -x; FOO=bar echo hi").0, "+ FOO=bar\n+ echo hi\n");

    // `$PS4` is user-customizable.
    assert_eq!(rush_stderr("PS4='TRACE: '; set -x; echo hi").0, "TRACE: echo hi\n");

    // Nesting inside `$(...)` repeats `$PS4`'s first character once per level.
    assert_eq!(
        rush_stderr("set -x; x=$(echo hi)").0,
        "++ echo hi\n+ x=hi\n"
    );
    assert_eq!(
        rush_stderr("set -x; x=$(echo $(echo hi))").0,
        "+++ echo hi\n++ echo hi\n+ x=hi\n"
    );

    // `set +x` turns it back off.
    assert_eq!(rush_stderr("set -x; set +x; echo hi").0, "+ set +x\n");
}

/// Spawn `rush -c src`, send it `sig` after `delay_ms`, and return its
/// captured `(stdout, exit status)`. The script's own `sleep` durations
/// are kept short (well under a second): killed early, an interrupted
/// `sleep` is orphaned rather than reaped, and `wait_with_output` blocks on
/// the piped stdout reaching EOF — which needs *every* holder of the pipe's
/// write end, including that orphan, to close it. A long `sleep` there
/// would make the test wait out its whole natural duration despite `rush`
/// itself having already exited immediately, same as real bash would.
#[cfg(unix)]
fn rush_signaled(src: &str, sig: libc::c_int, delay_ms: u64) -> (String, i32) {
    let child = Command::new(env!("CARGO_BIN_EXE_rush"))
        .arg("-c")
        .arg(src)
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn rush");
    std::thread::sleep(std::time::Duration::from_millis(delay_ms));
    unsafe {
        libc::kill(child.id() as libc::pid_t, sig);
    }
    let output = child.wait_with_output().expect("wait rush");
    (String::from_utf8_lossy(&output.stdout).into_owned(), output.status.code().unwrap_or(-1))
}

#[cfg(unix)]
#[test]
fn term_and_hup_traps_fire_and_can_interrupt_a_blocking_wait() {
    // A trap that itself calls `exit`: the signal interrupts the blocking
    // wait for `sleep` immediately, rather than waiting for it to finish —
    // verified directly against real bash, which behaves identically.
    assert_eq!(
        rush_signaled(r#"trap "echo caught; exit 5" TERM; sleep 0.6; echo unreached"#, libc::SIGTERM, 100),
        ("caught\n".to_string(), 5)
    );

    // A trap that does *not* exit: the wait simply resumes afterward,
    // rather than the script ending early — also matching real bash.
    assert_eq!(
        rush_signaled(r#"trap "echo caught" TERM; sleep 0.3; echo after"#, libc::SIGTERM, 100),
        ("caught\nafter\n".to_string(), 0)
    );

    // Untrapped: terminates with the conventional 128+signal status, same
    // as the signal's real default disposition — but any EXIT trap still
    // runs first.
    assert_eq!(
        rush_signaled(r#"trap "echo bye" EXIT; sleep 0.6"#, libc::SIGTERM, 100),
        ("bye\n".to_string(), 143)
    );

    // `HUP` works the same way as `TERM`.
    assert_eq!(
        rush_signaled(r#"trap "echo hup-caught; exit 7" HUP; sleep 0.6"#, libc::SIGHUP, 100),
        ("hup-caught\n".to_string(), 7)
    );
}

#[test]
fn indexed_arrays_basic_literal_index_and_whole_array_reads() {
    // A literal, plain indexing, and the count.
    assert_eq!(rush(r#"arr=(a b c); echo "${arr[0]}" "${arr[2]}""#).0, "a c\n");
    assert_eq!(rush(r#"arr=(a b c); echo "count=${#arr[@]}""#).0, "count=3\n");
    // Out of range: empty, not an error.
    assert_eq!(rush(r#"arr=(a b c); echo "[${arr[10]}]"; echo status:$?"#).0, "[]\nstatus:0\n");
    // `$arr` (bare, no subscript) == `${arr[0]}`.
    assert_eq!(rush(r#"arr=(a b c); echo "$arr""#).0, "a\n");
    // A subscript is evaluated as arithmetic, bare or `$`-prefixed.
    assert_eq!(rush(r#"i=1; arr=(a b c); echo "${arr[i+1]}""#).0, "c\n");
    assert_eq!(rush(r#"i=1; arr=(a b c); echo "${arr[$i]}""#).0, "b\n");
    // A never-arrayed scalar behaves like a 1-element array at index 0.
    assert_eq!(
        rush(r#"x=hello; echo "[${x[0]}]" "[${x[1]}]" "count=${#x[@]}""#).0,
        "[hello] [] count=1\n"
    );
}

#[test]
fn indexed_arrays_at_vs_star_and_quoting() {
    // `"${arr[@]}"`: one argument per element, spaces and all — like `"$@"`.
    assert_eq!(
        rush(r#"arr=(a "b c" d); for x in "${arr[@]}"; do echo "[$x]"; done"#).0,
        "[a]\n[b c]\n[d]\n"
    );
    // `"${arr[*]}"`: always one joined string, regardless of quoting.
    assert_eq!(rush(r#"arr=(a "b c" d); echo "${arr[*]}""#).0, "a b c d\n");
    assert_eq!(rush(r#"arr=(a b); IFS=,; echo "${arr[*]}""#).0, "a,b\n");
    // Unquoted, `@` and `*` behave identically — both fully IFS-split, losing
    // the original element boundaries.
    assert_eq!(
        rush(r#"arr=("a b" "c d"); for x in ${arr[@]}; do echo "[$x]"; done"#).0,
        "[a]\n[b]\n[c]\n[d]\n"
    );
}

#[test]
fn indexed_arrays_are_sparse() {
    // `arr[5]=x` on a 2-element array doesn't create indices 2-4; the count
    // is the number of *set* indices, and `${arr[@]}`/`${!arr[@]}` skip the
    // gap entirely.
    assert_eq!(
        rush(r#"arr=(a b); arr[5]=x; echo "${!arr[@]}" "count=${#arr[@]}""#).0,
        "0 1 5 count=3\n"
    );
    assert_eq!(rush(r#"arr=(a b); arr[5]=x; echo "${arr[@]}""#).0, "a b x\n");

    // `unset 'arr[i]'` removes just that one element, a real gap — not
    // merely emptying it.
    assert_eq!(
        rush(r#"arr=(a b c); unset "arr[1]"; echo "${!arr[@]}" "count=${#arr[@]}""#).0,
        "0 2 count=2\n"
    );
    // `unset` evaluates its own subscript arithmetic independently of shell
    // quoting — `$i` resolves even single-quoted (verified directly).
    assert_eq!(
        rush("arr=(a b c); i=1; unset 'arr[$i]'; echo \"${!arr[@]}\"").0,
        "0 2\n"
    );

    // Plain `unset arr` removes the whole thing.
    assert_eq!(rush(r#"arr=(a b c); unset arr; echo "count=${#arr[@]}""#).0, "count=0\n");
}

#[test]
fn indexed_arrays_element_and_whole_array_assignment() {
    // `arr[i]=x` sets one element, auto-vivifying if `arr` didn't exist.
    assert_eq!(rush(r#"unset arr; arr[2]=x; echo "${#arr[@]}" "${arr[2]}""#).0, "1 x\n");
    // A scalar indexed at a non-zero position is promoted to an array,
    // keeping its old value at index 0.
    assert_eq!(rush(r#"x=5; x[3]=hi; echo "${x[0]}" "${x[3]}""#).0, "5 hi\n");
    // A plain `arr=x` (no brackets) on an existing array only replaces
    // element 0, leaving the rest alone.
    assert_eq!(rush(r#"arr=(a b c); arr=x; echo "${arr[@]}""#).0, "x b c\n");

    // `arr+=(...)` appends; `arr[i]+=x` appends to just that one element.
    assert_eq!(rush(r#"arr=(a b c); arr+=(d e); echo "${arr[@]}""#).0, "a b c d e\n");
    assert_eq!(rush(r#"arr=(a b c); arr[1]+=X; echo "${arr[@]}""#).0, "a bX c\n");
    // `arr+=x` (no parens) appends the *string* to element 0, not a new
    // element — a real bash quirk, verified directly.
    assert_eq!(rush(r#"arr=(a b c); arr+=X; echo "${arr[@]}""#).0, "aX b c\n");

    // Glob/command-substitution expansion inside an array literal.
    assert_eq!(rush(r#"arr=($(printf "one two\nthree\n")); echo "${arr[@]}""#).0, "one two three\n");
}

#[test]
fn local_array_scopes_to_the_function_call() {
    assert_eq!(
        rush(r#"arr=(outer); f() { local arr=(inner1 inner2); echo "in f: ${arr[@]}"; }; f; echo "after: ${arr[@]}""#).0,
        "in f: inner1 inner2\nafter: outer\n"
    );
}

#[test]
fn associative_arrays_declare_index_and_whole_array_reads() {
    // `declare -A` unlocks string-keyed subscripts; a literal's elements
    // are `[key]=value` pairs.
    assert_eq!(rush(r#"declare -A arr=([a]=1 [b]=2); echo "${arr[a]}" "${arr[b]}""#).0, "1 2\n");
    assert_eq!(rush(r#"declare -A arr=([a]=1 [b]=2); echo "count=${#arr[@]}""#).0, "count=2\n");
    assert_eq!(rush(r#"declare -A arr=([a]=1 [b]=2); echo "${#arr[a]}""#).0, "1\n");

    // Auto-vivifying element assignment on an already-`-A` name.
    assert_eq!(
        rush(r#"declare -A arr; arr[foo]=bar; arr[baz]=qux; echo "${arr[foo]} ${arr[baz]}""#).0,
        "bar qux\n"
    );

    // Without `declare -A` first, a subscript is *always* arithmetic — a
    // non-numeric one evaluates to 0, the headline distinction this whole
    // feature hinges on, verified directly against real bash.
    assert_eq!(rush(r#"arr[foo]=bar; echo "${arr[0]}""#).0, "bar\n");
}

#[test]
fn associative_arrays_at_vs_star_and_key_iteration() {
    // `"${!arr[@]}"`: one argument per key, spaces and all — the
    // associative-array analogue of `"$@"`/`"${arr[@]}"`. (The multi-word
    // key comes from a variable, not a quoted literal directly inside
    // `[...]` — `arr["b c"]=x` isn't supported, a documented gap.)
    assert_eq!(
        rush(r#"declare -A arr; arr[a]=1; k="b c"; arr[$k]=2; for k in "${!arr[@]}"; do echo "[$k]"; done"#).0,
        "[a]\n[b c]\n"
    );
    // The standard "iterate an associative array by key" idiom.
    assert_eq!(
        rush(r#"declare -A arr=([a]=1 [b]=2); for k in "${!arr[@]}"; do echo "$k=${arr[$k]}"; done"#)
            .0
            .split('\n')
            .filter(|s| !s.is_empty())
            .collect::<std::collections::BTreeSet<_>>(),
        ["a=1", "b=2"].into_iter().collect()
    );
}

#[test]
fn associative_arrays_append_merge_and_unset_key() {
    // `arr+=([k]=v ...)` merges by key (a later pair overwrites an earlier
    // one for the same key) rather than appending positionally.
    assert_eq!(
        rush(r#"declare -A arr=([a]=1 [b]=2); arr+=([c]=3 [a]=99); echo "${arr[a]} ${arr[c]}""#).0,
        "99 3\n"
    );
    // `arr[k]+=v` appends to that one key's own string.
    assert_eq!(rush(r#"declare -A arr=([a]=1); arr[a]+=X; echo "${arr[a]}""#).0, "1X\n");
    // `unset 'arr[k]'` removes just that key.
    assert_eq!(
        rush(r#"declare -A arr=([a]=1 [b]=2); unset "arr[a]"; echo "count=${#arr[@]}""#).0,
        "count=1\n"
    );
}

#[test]
fn local_assoc_array_scopes_to_the_function_call() {
    assert_eq!(
        rush(r#"f() { local -A arr=([a]=1 [b]=2); echo "${arr[a]}"; }; f; echo "outer:${#arr[@]}""#).0,
        "1\nouter:0\n"
    );
}

// `clone_or_materialize`'s real-pipe path (`make_pipe`) that this locks in
// is Unix-only — off Unix a piped fd sharing a second descriptor falls
// back to inherit (see docs/ARCHITECTURE.md's G11 analysis), so stderr
// would leak straight to the test harness instead of routing through the
// pipe.
#[cfg(unix)]
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

#[test]
fn here_string_feeds_stdin_with_a_trailing_newline_always_appended() {
    assert_eq!(rush(r#"cat <<< "hi""#).0, "hi\n");
    // Even when the content already ends in one — bash always appends
    // exactly one more, verified directly.
    assert_eq!(rush("cat <<< \"abc\n\"").0, "abc\n\n");
    // A single word, not split/globbed like an ordinary unquoted
    // expansion elsewhere would be — same rule as any other redirect
    // target.
    assert_eq!(rush(r#"x="a b"; cat <<< $x"#).0, "a b\n");
    // A later `<<<` (or `<<`) on the same command wins over an earlier
    // one, same "last redirect for this fd wins" rule as any other.
    assert_eq!(rush("cat << EOF <<< \"override\"\nheredoc body\nEOF").0, "override\n");
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
    // A resolved path's file stem is "ls" everywhere — off Unix it's
    // "ls.EXE" (or another `%PATHEXT%` suffix), so checking the raw string
    // ending for a Unix-style "/ls" doesn't hold there.
    let stem = std::path::Path::new(out.trim_end())
        .file_stem()
        .and_then(|s| s.to_str())
        .map(str::to_ascii_lowercase);
    assert_eq!(stem.as_deref(), Some("ls"), "got: {out:?}");
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
fn command_v_type_and_hash_see_a_plain_path_extension_and_spawning_honors_it_too() {
    // C36: `command -v`/`type`/`hash` used to call `std::env::var_os("PATH")`
    // directly — the *real* OS process environment — rather than the
    // shell's own `PATH` variable, so a plain (non-`export`ed)
    // `PATH=$PATH:dir` was invisible to them even though the directory's
    // contents were genuinely runnable. Root cause ran deeper than just
    // those three builtins, though: without seeding the shell's variable
    // table from the inherited environment at startup, a *bare*
    // reassignment to an already-exported variable like `PATH` created a
    // brand new, non-exported entry — so the *updated* value never made it
    // into a spawned child's environment either, even though internal
    // lookups already saw it.
    let dir = std::env::temp_dir().join(format!("rush_c36_pathdir_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let tool = dir.join("rush_c36_tool");
    std::fs::write(&tool, "#!/bin/sh\necho ran-c36-tool\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();

    let dir_str = dir.to_str().unwrap();
    let src = format!(
        "PATH=$PATH:{dir_str}; command -v rush_c36_tool; type -t rush_c36_tool; hash rush_c36_tool; echo hash=$?; rush_c36_tool"
    );
    let (out, status) = rush(&src);
    assert_eq!(
        out,
        format!("{dir_str}/rush_c36_tool\nfile\nhash=0\nran-c36-tool\n")
    );
    assert_eq!(status, 0);

    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn unset_path_actually_breaks_resolving_a_bare_command_name() {
    // C40: `unset`-ing an inherited/exported variable like `PATH` only
    // deleted rush's own internal record of it — a spawned child, and
    // rush's own resolution of *new* spawns, still fell back to the real
    // OS environment's untouched value. Real bash's child genuinely no
    // longer has it: `ls` (a bare name, needing a `$PATH` search) now
    // fails with status 127, matching bash exactly, while a direct path
    // (no search needed) keeps working.
    let (_, status) = rush("unset PATH; ls / >/dev/null 2>&1");
    assert_eq!(status, 127);

    let (out, status) = rush("unset PATH; /bin/ls / >/dev/null; echo status=$?");
    assert_eq!(out, "status=0\n");
    assert_eq!(status, 0);
}

#[cfg(unix)]
#[test]
fn bare_cd_honors_an_unexported_home_and_breaks_when_home_is_unset() {
    // The same root cause (a `std::env` fallback masking `vars`'s own,
    // possibly-`unset` value) also affected `cd`'s home-directory case, a
    // narrower but related bug found alongside C40: a bare `cd` used to
    // read `std::env::var("HOME")` directly, never checking `vars` first
    // at all — so a plain (non-`export`ed) `HOME=/some/dir` reassignment
    // was invisible to it, and `unset HOME` didn't stop it either.
    let dir = std::env::temp_dir().join(format!("rush_c40_home_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (out, _) = rush(&format!("HOME={}; cd; pwd", dir.to_str().unwrap()));
    assert_eq!(out, format!("{}\n", dir.to_str().unwrap()));
    let _ = std::fs::remove_dir_all(&dir);

    let (out, status) = rush("unset HOME; cd; echo status=$?");
    assert_eq!(out, "status=1\n");
    assert_eq!(status, 0);
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

#[test]
fn exec_builtin() {
    // Off Unix `exec CMD` is spawn-wait-exit rather than a true image
    // replacement — observably the same for everything asserted here.
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
    let file = shell_path(&path);
    let (out, status) = rush(&format!("exec > {file}; echo redirected; echo more"));
    assert_eq!(out, "");
    assert_eq!(status, 0);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "redirected\nmore\n");
    let _ = std::fs::remove_file(&path);

    // Same for redirecting the shell's own stdin: a `read` right after
    // picks up the file's contents instead of rush's real stdin.
    let in_path = std::env::temp_dir().join(format!("rush_exec_stdin_{}.txt", std::process::id()));
    let in_file = shell_path(&in_path);
    std::fs::write(&in_path, "hi\n").unwrap();
    assert_eq!(
        rush(&format!("exec 0<{in_file}; read line; echo got:$line")).0,
        "got:hi\n"
    );
    let _ = std::fs::remove_file(&in_path);
}

#[cfg(unix)]
#[test]
fn redirect_to_fd_3_actually_uses_fd_3_not_stdout() {
    // C38: a redirect to any fd other than 0/1/2 used to silently collapse
    // onto fd 1 — `cmd 3>file` redirected *stdout*, not a real fd 3. Now a
    // real `cmd 4>&3` (dup'd off it) lands in the file, and ordinary stdout
    // (unredirected) still reaches the captured output normally.
    let path = std::env::temp_dir().join(format!("rush_c38_fd3_{}.txt", std::process::id()));
    let file = path.to_str().unwrap();

    // Builtin path (`redirect_stdio`, in-process): `echo`'s own stdout is
    // dup'd through fd 3 into the file, not left on the real stdout at all.
    let (out, _) = rush(&format!("echo hi 3>{file} 1>&3"));
    assert_eq!(out, "");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "hi\n");
    let _ = std::fs::remove_file(&path);

    // External-command path (`build_stage`, a real spawned child): same
    // idea, through `cat` instead of a builtin.
    let (out, _) = rush(&format!("cat <<< external-hi 3>{file} 1>&3"));
    assert_eq!(out, "");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "external-hi\n");
    let _ = std::fs::remove_file(&path);
}

#[cfg(unix)]
#[test]
fn read_side_fd_dup_works_for_builtins_and_external_commands() {
    // The read-side counterpart of the above: `N<&target` (verified above
    // in `lexer.rs` to not even parse before this fix) actually reads
    // through the duplicated fd.
    let path = std::env::temp_dir().join(format!("rush_c38_readfd_{}.txt", std::process::id()));
    let file = path.to_str().unwrap();
    std::fs::write(&path, "fd-chain-value\n").unwrap();

    // Builtin (`read`), stdin from /dev/null so a wrong redirect would hang
    // rather than silently pass by reading the test harness's own stdin.
    let (out, _) = rush_stdin(&format!("read line 3<{file} <&3; echo got:$line"), "");
    assert_eq!(out, "got:fd-chain-value\n");

    // External command (`cat`), chaining fd 3 → fd 4 → stdin (`<&4`) to
    // also cover a multi-hop dup, not just a single one.
    let (out, _) = rush_stdin(&format!("cat 3<{file} 4<&3 <&4"), "");
    assert_eq!(out, "fd-chain-value\n");

    let _ = std::fs::remove_file(&path);
}

#[cfg(unix)]
#[test]
fn umask_builtin() {
    // No args reports the current mask; `-S` reports it symbolically.
    assert_eq!(rush("umask 022; umask").0, "0022\n");
    assert_eq!(rush("umask 027; umask -S").0, "u=rwx,g=rx,o=\n");
    assert_eq!(rush("umask 0; umask").0, "0000\n");

    // A real `libc::umask()` call: it actually changes the permissions a
    // spawned child creates a file with, not just a shell-internal value.
    let path = std::env::temp_dir().join(format!("rush_umask_test_{}.txt", std::process::id()));
    let file = path.to_str().unwrap();
    let _ = std::fs::remove_file(&path);
    rush(&format!("umask 077; touch {file}"));
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "got mode {mode:o}");
    let _ = std::fs::remove_file(&path);

    // Errors: an out-of-range or malformed mode fails with status 1,
    // without touching the mask.
    assert_eq!(rush("umask 999; echo status:$?").0, "status:1\n");
    assert_eq!(rush("umask abc; echo status:$?").0, "status:1\n");
}

#[test]
fn source_and_dot_builtin() {
    let path = std::env::temp_dir().join(format!("rush_source_lib_{}.sh", std::process::id()));
    std::fs::write(&path, "FOO=bar\ngreet() { echo \"hi $1\"; }\n").unwrap();
    let file = path.to_str().unwrap();

    // `.` runs the file in the current environment: variables and functions
    // it defines stick around afterward, and `source` is a plain synonym.
    // Single-quoted (C51-agnostic): an unquoted Windows path's `\`
    // separators would otherwise be read as escapes and stripped.
    assert_eq!(rush(&format!(". '{file}'; echo $FOO; greet world")).0, "bar\nhi world\n");
    assert_eq!(rush(&format!("source '{file}'; echo $FOO")).0, "bar\n");

    // With no extra args, the caller's own positional params show through
    // unchanged; extra args temporarily replace them, restored afterward.
    let args_path = std::env::temp_dir().join(format!("rush_source_args_{}.sh", std::process::id()));
    std::fs::write(&args_path, "echo \"args:$#:$*\"\n").unwrap();
    let args_file = args_path.to_str().unwrap();
    assert_eq!(
        rush(&format!("f() {{ . '{args_file}'; echo \"after:$#:$*\"; }}; f a b c")).0,
        "args:3:a b c\nafter:3:a b c\n"
    );
    assert_eq!(
        rush(&format!("f() {{ . '{args_file}' x y; echo \"after:$#:$*\"; }}; f a b c")).0,
        "args:2:x y\nafter:3:a b c\n"
    );

    // `return` inside the sourced file ends only the sourcing, not the
    // caller; `break`/`continue` are NOT consumed and propagate transparently
    // to an enclosing loop back in the calling context.
    let ret_path = std::env::temp_dir().join(format!("rush_source_return_{}.sh", std::process::id()));
    std::fs::write(&ret_path, "echo before\nreturn 5\necho after\n").unwrap();
    let ret_file = ret_path.to_str().unwrap();
    let (out, status) = rush(&format!(". '{ret_file}'; echo \"status=$?\"; echo done"));
    assert_eq!(out, "before\nstatus=5\ndone\n");
    assert_eq!(status, 0);

    let brk_path = std::env::temp_dir().join(format!("rush_source_break_{}.sh", std::process::id()));
    std::fs::write(&brk_path, "echo in-lib\nbreak\necho unreached\n").unwrap();
    let brk_file = brk_path.to_str().unwrap();
    assert_eq!(
        rush(&format!("for i in 1 2 3; do . '{brk_file}'; echo unreached-loop-body; done; echo after-loop")).0,
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
fn set_reassigns_positional_parameters() {
    // C39: `set -- args…` / `set args…` — the standard way to reassign
    // `$1`/`$2`/…/`$#` mid-script — used to be rejected outright ("not
    // supported") rather than actually reassigning anything.
    assert_eq!(rush("set -- a b c; echo \"$#: $1 $2 $3\"").0, "3: a b c\n");
    // No `--` needed either — a bare, non-flag first word triggers the
    // same reassignment.
    assert_eq!(rush("set a b c; echo \"$#: $1 $2 $3\"").0, "3: a b c\n");
    // `set --` alone clears the positional parameters.
    assert_eq!(rush("set --; echo \"$#:[$1]\"").0, "0:[]\n");
    // `$0` is never touched by either form.
    assert_eq!(rush_argv("set -- newarg; echo $0", &["myprog"]).0, "myprog\n");
    // After `--`, everything is positional even if it looks like a flag.
    assert_eq!(rush("set -- -x; echo \"1=[$1]\"").0, "1=[-x]\n");
    assert_eq!(rush("set -- --; echo \"1=[$1]\"").0, "1=[--]\n");
    // A flag before `--`/the positional list still applies.
    assert_eq!(
        rush("set -e -- a b c; echo \"$#: $1 $2 $3\"; false; echo unreached").0,
        "3: a b c\n"
    );
    // The textbook getopts idiom: drop the parsed flags, keep the rest.
    assert_eq!(
        rush("set -- -a foo bar; while getopts a opt; do :; done; shift $((OPTIND-1)); set -- \"$@\"; echo \"$#: $1 $2\"").0,
        "2: foo bar\n"
    );

    // An unrecognized flag is still a hard error — and, critically, must
    // *not* fall through and reassign positional parameters from whatever
    // follows it (a real bug this feature's own implementation could have
    // reintroduced: an early "unsupported flag" error path that didn't
    // stop processing would let `set -z a b` wrongly set $1/$2).
    let (out, _) = rush("set -z a b; echo \"status=$? [$1] [$2]\"");
    assert_eq!(out, "status=1 [] []\n");
    let (out, _) = rush("set -o badname a b; echo \"status=$? [$1] [$2]\"");
    assert_eq!(out, "status=1 [] []\n");
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

#[test]
fn while_read_loop_reads_from_a_redirected_file() {
    // The headline C7 case: `read` inside a `while` loop whose *compound*
    // (not per-iteration) redirect feeds it — this also exercises the fix
    // letting a redirect trail a compound command's close at all (`done <
    // file` used to be silently dropped: the file's contents were never
    // wired to fd 0, so the loop read the shell's real stdin instead).
    let path = std::env::temp_dir().join(format!("rush_read_loop_{}.txt", std::process::id()));
    std::fs::write(&path, "a\nb\nc\n").unwrap();
    let src = format!("while read line; do echo \"L:$line\"; done < {}", shell_path(&path));
    assert_eq!(rush(&src).0, "L:a\nL:b\nL:c\n");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn redirect_trailing_a_compound_command_is_applied() {
    let path = std::env::temp_dir().join(format!("rush_compound_redir_{}.txt", std::process::id()));
    std::fs::write(&path, "one\ntwo\n").unwrap();
    let file = shell_path(&path);

    // A brace group's own redirect.
    assert_eq!(rush(&format!("{{ cat; }} < {file}")).0, "one\ntwo\n");
    // A subshell's own redirect.
    assert_eq!(rush(&format!("(cat) < {file}")).0, "one\ntwo\n");
    // Still applies when the compound is also a pipeline stage. (Unix only:
    // a compound as one stage of a multi-command pipeline needs fork.)
    #[cfg(unix)]
    assert_eq!(rush(&format!("(cat) < {file} | tr a-z A-Z")).0, "ONE\nTWO\n");
    // And when the whole compound is captured via `$(...)`.
    assert_eq!(rush(&format!("x=$(cat < {file}); echo \"$x\"")).0, "one\ntwo\n");

    // Output redirect trailing a `while` loop.
    let out_path = std::env::temp_dir().join(format!("rush_compound_redir_out_{}.txt", std::process::id()));
    let out_file = shell_path(&out_path);
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

#[test]
fn case_semi_amp_falls_through_unconditionally() {
    // `;&` runs the *next* item's body too, without testing its pattern.
    assert_eq!(rush("case a in a) echo one;& b) echo two;; esac").0, "one\ntwo\n");
    // Chains through multiple `;&` in a row.
    assert_eq!(
        rush("case a in a) echo one;& b) echo two;& c) echo three;; esac").0,
        "one\ntwo\nthree\n"
    );
    // `$?` after the whole `case` is the *last* body that actually ran.
    assert_eq!(rush("case a in a) false;& b) echo two;; esac; echo status:$?").0, "two\nstatus:0\n");
}

#[test]
fn case_dsemi_amp_resumes_pattern_testing() {
    // `;;&` resumes testing *subsequent* patterns (not unconditional) —
    // runs the first one (if any) that matches, same as if the case
    // restarted right after the current item.
    assert_eq!(rush("case a in a) echo one;;& b) echo two;; a) echo three;; esac").0, "one\nthree\n");
    // No later pattern matches: nothing else runs.
    assert_eq!(rush("case a in a) echo one;;& b) echo two;; esac").0, "one\n");
    // A trailing `;;&` at the very end of the `case` (no items left to
    // resume into) just stops, like `;;` would.
    assert_eq!(rush(r#"case a in a) echo one;;& esac; echo "after:$?""#).0, "one\nafter:0\n");
}

#[test]
fn case_terminator_omitted_on_last_item_defaults_to_break() {
    assert_eq!(rush("case a in a) echo one;;esac").0, "one\n");
}

#[test]
fn select_prints_a_numbered_menu_and_prompts_on_stderr() {
    let (err, _) = rush_stdin_stderr("select x in apple banana; do echo hi; break; done", "1\n");
    assert_eq!(err, "1) apple\n2) banana\n#? ");
}

#[test]
fn select_reads_a_valid_index_or_blank_name_on_no_match() {
    // A valid 1-based index sets `NAME` to that word; `$REPLY` always
    // holds the raw line either way.
    assert_eq!(
        rush_stdin("select x in a b c; do echo \"x=[$x] reply=[$REPLY]\"; break; done", "2\n").0,
        "x=[b] reply=[2]\n"
    );
    // Out of range, non-numeric, or negative: `NAME` is empty, no error.
    assert_eq!(
        rush_stdin("select x in a b c; do echo \"x=[$x] reply=[$REPLY]\"; break; done", "foo\n").0,
        "x=[] reply=[foo]\n"
    );
    assert_eq!(
        rush_stdin("select x in a b c; do echo \"x=[$x]\"; break; done", "0\n").0,
        "x=[]\n"
    );
    // Surrounding whitespace and a leading `+`/zero are tolerated.
    assert_eq!(rush_stdin("select x in a b c; do echo \"x=$x\"; break; done", " 2 \n").0, "x=b\n");
    assert_eq!(rush_stdin("select x in a b c; do echo \"x=$x\"; break; done", "+2\n").0, "x=b\n");
}

#[test]
fn select_blank_line_redisplays_without_running_the_body() {
    // A truly empty line doesn't run the body at all — just redisplays
    // the menu and prompts again — while an all-whitespace line *does*
    // run it (with `$REPLY` holding those literal spaces, unlike ordinary
    // `read`'s own IFS-trimming).
    assert_eq!(
        rush_stdin("select x in a b; do echo \"ran reply=[$REPLY]\"; break; done", "\n2\n").0,
        "ran reply=[2]\n"
    );
    assert_eq!(
        rush_stdin("select x in a b; do echo \"ran reply=[$REPLY]\"; break; done", "   \nq\n").0,
        "ran reply=[   ]\n"
    );
}

#[test]
fn select_eof_ends_the_loop_with_status_1_overriding_the_last_body_status() {
    let (out, status) = rush_stdin("select x in a b; do echo hi; done", "q\n");
    assert_eq!(out, "hi\n");
    assert_eq!(status, 1);
    // An explicit `break` still reports the last command's own status —
    // EOF's status-1 override only applies when the loop ends *without*
    // one.
    let (out, status) = rush_stdin("select x in a b; do false; break; done", "1\n");
    assert_eq!(out, "");
    assert_eq!(status, 0);
}

#[test]
fn select_without_in_iterates_positional_params_and_empty_list_is_a_no_op() {
    // No positional params set: the list is empty, same as `for`'s own
    // `in`-omitted convention — zero iterations, status 0, no read at all
    // (nothing is ever written to its stdin, unlike the other cases here).
    let (out, status) = rush("select x; do echo \"x=$x\"; break; done");
    assert_eq!(out, "");
    assert_eq!(status, 0);
}

#[test]
fn select_ps3_prompt_defaults_and_dollar_expands() {
    let (err, _) = rush_stdin_stderr("select x in a; do break; done", "1\n");
    assert_eq!(err, "1) a\n#? ");
    let (err, _) =
        rush_stdin_stderr(r#"name=world; PS3="pick $name> "; select x in a; do break; done"#, "1\n");
    assert_eq!(err, "1) a\npick world> ");
}

#[test]
fn c_style_for_loop_basic_and_all_clauses_optional() {
    assert_eq!(rush("for ((i=0;i<3;i++)); do echo $i; done").0, "0\n1\n2\n");
    // No space needed between `for` and `((`.
    assert_eq!(rush("for((i=0;i<3;i++)); do echo $i; done").0, "0\n1\n2\n");
    // All three clauses empty: an infinite loop (an explicit `break` is
    // what actually ends it, not the missing condition).
    assert_eq!(rush("for ((;;)); do echo once; break; done").0, "once\n");
    // Only the condition clause present; init/update are ordinary
    // statements around the loop instead.
    assert_eq!(rush("i=0; for ((;i<3;)); do echo $i; i=$((i+1)); done").0, "0\n1\n2\n");
}

#[test]
fn c_style_for_continue_runs_update_but_break_does_not() {
    assert_eq!(
        rush("for ((i=0;i<5;i++)); do if [ $i -eq 2 ]; then continue; fi; echo $i; done").0,
        "0\n1\n3\n4\n"
    );
    assert_eq!(
        rush(r#"for ((i=0;i<3;i++)); do echo "i=$i"; break; done; echo "after:$i""#).0,
        "i=0\nafter:0\n"
    );
}

#[test]
fn standalone_arith_command_sets_exit_status_and_runs_side_effects() {
    // A nonzero result is status 0 (true); the assignment's side effect
    // (not its value) is what the rest of the script sees.
    assert_eq!(rush(r#"i=1; ((i = i + 5)); echo "$i status:$?""#).0, "6 status:0\n");
    assert_eq!(rush(r#"i=0; ((i)); echo "status:$?""#).0, "status:1\n");
    assert_eq!(rush(r#"i=1; ((i)); echo "status:$?""#).0, "status:0\n");
    // Empty `(( ))` evaluates as 0/status 1 rather than erroring — a real
    // bash asymmetry with `$(( ))` (which does error on empty).
    assert_eq!(rush(r#"((  )); echo "status:$?""#).0, "status:1\n");
    // Usable directly in `&&`/`||` like `test`.
    assert_eq!(rush("((1==1)) && echo yes || echo no").0, "yes\n");
}

#[test]
fn double_paren_is_always_arithmetic_never_nested_subshells() {
    // A space between the two `(` is what forces the nested-subshell
    // reading instead — matching real bash exactly, verified directly.
    assert_eq!(rush("( (echo hi) )").0, "hi\n");
    // Adjacent, no space: always arithmetic, even where that's invalid —
    // never falls back to trying nested subshells.
    let (_, status) = rush("((echo hi))");
    assert_eq!(status, 1);
}

#[test]
fn arithmetic_exponent_bitwise_and_ternary() {
    assert_eq!(rush("echo $((2**10))").0, "1024\n");
    // Unary binds tighter than `**`; `**` binds tighter than `*`.
    assert_eq!(rush("echo $((-2**2))").0, "4\n");
    assert_eq!(rush("echo $((2*3**2))").0, "18\n");
    assert_eq!(rush("echo $((5 & 3)) $((5 | 2)) $((5 ^ 1)) $((~5))").0, "1 7 4 -6\n");
    assert_eq!(rush("echo $((1 << 3)) $((16 >> 2))").0, "8 4\n");
    assert_eq!(rush("echo $((1 ? 2 : 3)) $((0 ? 2 : 3))").0, "2 3\n");
}

#[test]
fn arithmetic_assignment_and_inc_dec_in_dollar_paren() {
    assert_eq!(rush("i=5; echo $((i++)); echo $i").0, "5\n6\n"); // postfix: old value
    assert_eq!(rush("i=5; echo $((++i)); echo $i").0, "6\n6\n"); // prefix: new value
    assert_eq!(rush("i=5; echo $((i+=3)); echo $i").0, "8\n8\n");
    assert_eq!(rush(r#"i=1; j=$((i = 10)); echo "i=$i j=$j""#).0, "i=10 j=10\n");
    assert_eq!(rush("i=1; ((i++)); echo $i").0, "2\n");
}

#[test]
fn arithmetic_short_circuit_skips_assignment_side_effects() {
    assert_eq!(rush("i=1; echo $((0 && (i=5))); echo $i").0, "0\n1\n");
    assert_eq!(rush("i=1; echo $((1 || (i=5))); echo $i").0, "1\n1\n");
    assert_eq!(rush("i=1; echo $((0 ? (i=9) : (i=7))); echo $i").0, "7\n7\n");
}

#[test]
fn arithmetic_bare_array_subscript_read_assign_and_inc_dec() {
    // C170: `a[i]` had no grammar in arithmetic context at all — every
    // read/assign/compound-assign/inc-dec form errored with "unexpected
    // character `[` in arithmetic".
    assert_eq!(rush("a=(1 2 3); echo $((a[1]))").0, "2\n");
    assert_eq!(rush(r#"a=(1 2 3); (( a[1] = 99 )); echo "${a[@]}""#).0, "1 99 3\n");
    assert_eq!(rush(r#"a=(1 2 3); (( a[1]++ )); echo "${a[@]}""#).0, "1 3 3\n");
    assert_eq!(rush(r#"a=(1 2 3); (( ++a[1] )); echo "${a[@]}""#).0, "1 3 3\n");
    assert_eq!(rush(r#"a=(1 2 3); (( a[1] += 10 )); echo "${a[@]}""#).0, "1 12 3\n");
    // A computed subscript, not just a literal index.
    assert_eq!(rush("a=(1 2 3); i=1; echo $((a[i+1]))").0, "3\n");
    // An associative array's subscript is a literal key, not re-evaluated
    // as arithmetic (an arithmetic `x` would resolve the unset var to 0).
    assert_eq!(rush("declare -A m=([x]=5 [y]=10); echo $((m[x] + 1))").0, "6\n");
    assert_eq!(rush(r#"declare -A m=([x]=5 [y]=10); (( m[x] = 20 )); echo "${m[x]}""#).0, "20\n");
    // Unset element/unset array both read as 0, matching an unset scalar.
    assert_eq!(rush("a=(1 2 3); echo $((a[10]))").0, "0\n");
    // A plain scalar behaves as if it were a 1-element array at index 0.
    assert_eq!(rush("x=5; echo $((x[0])) $((x[3]))").0, "5 0\n");
}

#[test]
fn brace_expansion_comma_lists_and_cross_products() {
    // A plain comma-list turns one word into several argv words.
    assert_eq!(rush("echo {a,b,c}").0, "a b c\n");
    // Concatenated with a prefix/suffix, and two groups cross-product.
    assert_eq!(rush("echo x{a,b}y{1,2}z").0, "xay1z xay2z xby1z xby2z\n");
    // A single, comma-less `{a}` isn't a valid group — left as literal
    // text — but that doesn't block a *valid* group elsewhere in the word.
    assert_eq!(rush("echo {a}{b,c}").0, "{a}b {a}c\n");
    // Nested groups: an inner valid group's own alternatives become
    // separate top-level alternatives, not a concatenated sub-string.
    assert_eq!(rush("echo {a,{b,c},d}").0, "a b c d\n");
    // Escaped braces are never structural.
    assert_eq!(rush(r"echo \{a,b\}").0, "{a,b}\n");
    // Quoted commas/braces are inert to the scan but their content still
    // rides along in whichever alternative it lands in.
    assert_eq!(rush(r#"echo pre{"a,b",c}post"#).0, "prea,bpost precpost\n");
}

#[test]
fn brace_expansion_ranges() {
    assert_eq!(rush("echo {1..5}").0, "1 2 3 4 5\n");
    // Descending, and a letter range with a step.
    assert_eq!(rush("echo {5..1}").0, "5 4 3 2 1\n");
    assert_eq!(rush("echo {a..e..2}").0, "a c e\n");
    // A leading zero on either endpoint zero-pads every generated term to
    // that endpoint's own width — including the sign, for a negative one.
    assert_eq!(rush("echo {01..5}").0, "01 02 03 04 05\n");
    assert_eq!(rush("echo {-01..05}").0, "-01 000 001 002 003 004 005\n");
    // A malformed range (mismatched types) is left as literal text.
    assert_eq!(rush("echo {1..a}").0, "{1..a}\n");
    // For loops are the idiomatic use.
    assert_eq!(rush("for i in {1..3}; do echo n=$i; done").0, "n=1\nn=2\nn=3\n");
}

#[test]
fn brace_expansion_runs_before_dollar_expansion_and_skips_assignments() {
    // Brace expansion is purely textual and runs first: an endpoint that's
    // a variable reference at this stage (not yet a literal integer) makes
    // the group invalid — left as literal text — even though the `$n`
    // inside it still resolves normally afterwards.
    assert_eq!(rush("n=5; echo {1..$n}").0, "{1..5}\n");
    // `{$x,world}` expands the braces into two *words* first, each then
    // expanded normally — so `$x` still resolves.
    assert_eq!(rush("x=hello; echo {$x,world}").0, "hello world\n");
    // A bare assignment statement's own value is never brace-expanded,
    // matching real bash exactly (only ordinary command-argument words
    // are) — `x` keeps the literal text.
    assert_eq!(rush(r#"x={a,b}; echo "$x""#).0, "{a,b}\n");
    // An array literal's elements are ordinary argument words, though, so
    // they *do* brace-expand.
    assert_eq!(rush(r#"arr=({a,b} c); echo "${arr[@]}" "${#arr[@]}""#).0, "a b c 3\n");
}

#[cfg(unix)]
#[test]
fn process_substitution_read_side_feeds_a_readable_path() {
    assert_eq!(rush("cat <(echo hi)").0, "hi\n");
    // Each `<(...)` on one command line gets its own, independently
    // readable path.
    let (out, status) = rush("diff <(echo a) <(echo b)");
    assert!(out.contains('a') && out.contains('b'));
    assert_eq!(status, 1); // `diff`'s own convention: differing input
}

#[cfg(unix)]
#[test]
fn process_substitution_write_side_feeds_the_substituted_commands_stdin() {
    // `cmd`'s own stdout (inherited from the shell) is where the
    // substituted command's re-printed output shows up, same as real bash.
    assert_eq!(rush("echo hi > >(cat)").0, "hi\n");
}

#[cfg(unix)]
#[test]
fn process_substitution_concatenates_with_adjacent_text() {
    // Not required to be its own separate word — verified directly against
    // real bash, which glues `<(cmd)`'s expansion onto adjacent text just
    // like `$(cmd)`'s.
    let (out, _) = rush("echo pre<(echo hi)post");
    assert!(out.starts_with("pre/dev/fd/"));
    assert!(out.trim_end().ends_with("post"));
}

#[cfg(unix)]
#[test]
fn process_substitution_is_suppressed_by_quoting() {
    // Unlike `$(...)`, which *does* still expand inside double quotes,
    // `<(...)`/`>(...)` are left as literal text when quoted at all —
    // verified directly against real bash.
    assert_eq!(rush(r#"echo "<(echo hi)""#).0, "<(echo hi)\n");
    assert_eq!(rush("echo '<(echo hi)'").0, "<(echo hi)\n");
}

#[cfg(unix)]
#[test]
fn process_substitution_nests_and_composes_with_pipelines() {
    assert_eq!(rush("cat <(cat <(echo nested-inner))").0, "nested-inner\n");
    assert_eq!(rush("cat <(echo a | tr a-z A-Z)").0, "A\n");
}

#[cfg(unix)]
#[test]
fn process_substitution_works_in_assignment_rhs_and_redirect_targets() {
    // Assignment RHS *does* get process substitution — a real, deliberate
    // asymmetry with brace expansion (which doesn't), verified directly.
    assert_eq!(rush(r#"x=$(cat <(echo inner)); echo "x=$x""#).0, "x=inner\n");
    assert_eq!(rush(r#"read -r line < <(echo hello-read); echo "line=[$line]""#).0, "line=[hello-read]\n");
}

#[cfg(unix)]
#[test]
fn process_substitution_does_not_wait_and_does_not_affect_main_status() {
    // The main command's own exit status is unaffected by the substituted
    // command's — verified directly against real bash. (Deliberately the
    // read side, not `>(exit 7)`: a write-side substitution whose reader
    // exits without ever reading is a genuine, inherent write-vs-exit race
    // over the underlying pipe — confirmed to reproduce in real bash too,
    // equally, under concurrent load, not a rush-specific bug — so it's
    // not something to assert on deterministically here.)
    assert_eq!(rush("true <(exit 7); echo $?").0, "0\n");
    // `$!` reflects the substitution's own pid — real, current bash
    // behavior, verified directly.
    let (out, _) = rush(": <(echo hi); echo \"pid=[$!]\"");
    assert!(out.starts_with("pid=["));
    assert!(!out.starts_with("pid=[]"));
}

#[cfg(not(unix))]
#[test]
fn process_substitution_errors_cleanly_off_unix() {
    let (_, status) = rush("cat <(echo hi)");
    assert_ne!(status, 0);
}

#[test]
fn bang_bang_repeats_and_echoes_the_last_command() {
    let (out, err) = rush_interactive("echo one\n!!\n");
    assert_eq!(out, "one\necho one\none\n");
    assert_eq!(err, "");
}

#[test]
fn bang_n_and_bang_minus_n_recall_by_event_number() {
    // `!1` (event 1, "echo one") runs and is itself appended to history, so
    // by the time `!-2` (2nd-from-the-end) runs, that end is
    // [..., "echo two", "echo three", "echo one"] — `!-2` lands on
    // "echo three", not "echo two". Verified directly against real bash.
    let (out, _) = rush_interactive("echo one\necho two\necho three\n!1\n!-2\n");
    assert_eq!(out, "one\ntwo\nthree\necho one\none\necho three\nthree\n");
}

#[test]
fn bang_string_searches_history_backward_for_a_prefix() {
    let (out, _) = rush_interactive("echo foo\nls /tmp/does-not-exist-xyz\n!echo\n");
    assert!(out.starts_with("foo\n"));
    assert!(out.contains("echo foo\nfoo\n"));
}

#[test]
fn word_designators_reuse_pieces_of_the_previous_command() {
    // Re-typing `echo a b c` between each designator resets what "the
    // previous command" means — since (verified directly, matching real
    // bash exactly) it's the most recently *executed* line, i.e. already
    // post-expansion, not the original line as first typed.
    let (out, _) = rush_interactive(
        "echo a b c\necho !^\necho a b c\necho !$\necho a b c\necho !*\n",
    );
    assert_eq!(out, "a b c\necho a\na\na b c\necho c\nc\na b c\necho a b c\na b c\n");
}

#[test]
fn sudo_bang_bang_concatenates_mid_word() {
    let (out, _) = rush_interactive("echo hi\ntrue !!\n");
    assert_eq!(out, "hi\ntrue echo hi\n");
}

#[test]
fn single_quotes_suppress_bang_history_expansion() {
    let (out, _) = rush_interactive("echo hi\necho '!!'\n");
    assert_eq!(out, "hi\n!!\n");
}

#[test]
fn double_quotes_do_not_suppress_bang_history_expansion() {
    let (out, _) = rush_interactive("echo hi\necho \"!!\"\n");
    assert_eq!(out, "hi\necho \"echo hi\"\necho hi\n");
}

#[test]
fn unresolvable_event_reference_errors_and_runs_nothing() {
    let (out, err) = rush_interactive("!123\n");
    assert_eq!(out, "");
    assert!(err.contains("event not found"));
}

#[test]
fn bang_history_is_a_no_op_in_script_mode() {
    let (out, status) = rush("echo hi; echo !!");
    assert_eq!(status, 0);
    assert_eq!(out, "hi\n!!\n");
}

#[test]
fn backslash_escaped_dollar_in_double_quotes_stays_literal() {
    // C35: `\$` inside `"..."` must produce a literal `$` (suppressing
    // expansion of whatever follows) — same as `\"`/`\\` already do.
    // Previously rush dropped the backslash but still expanded the
    // parameter anyway (`"\$?"` printed the exit status, not `$?`).
    let (out, status) = rush(r#"echo "\$?""#);
    assert_eq!(status, 0);
    assert_eq!(out, "$?\n");

    let (out, _) = rush(r#"FOO=bar; echo "\$FOO""#);
    assert_eq!(out, "$FOO\n");
}

#[test]
fn backslash_escaped_dollar_composes_with_real_expansion_in_the_same_string() {
    let (out, _) = rush(r#"FOO=bar; echo "pre\$mid$FOO""#);
    assert_eq!(out, "pre$midbar\n");

    // A literal backslash (from `\\`) followed by a real, still-expanding
    // `$FOO` isn't mistaken for the `\$` escape.
    let (out, _) = rush(r#"FOO=bar; echo "\\$FOO""#);
    assert_eq!(out, "\\bar\n");
}

#[cfg(unix)]
#[test]
fn unknown_command_reports_127_instead_of_aborting_the_script() {
    // C37: a mistyped/nonexistent command used to print the raw OS spawn
    // error and abort the whole script right there — the `echo` right
    // after it never even ran. Now it's an ordinary failing command:
    // status 127 ("command not found"), and the rest of the script
    // continues, matching real bash exactly.
    let (out, status) = rush("echo before; totallynonexistentcmd_c37; echo after");
    assert_eq!(out, "before\nafter\n");
    assert_eq!(status, 0); // `after`'s own status, the last thing that ran

    let (out, status) = rush("totallynonexistentcmd_c37; echo status=$?");
    assert_eq!(out, "status=127\n");
    assert_eq!(status, 0);

    let (err, _) = rush_stderr("totallynonexistentcmd_c37");
    assert!(err.contains("totallynonexistentcmd_c37"), "got: {err:?}");
}

#[test]
fn command_not_found_handle_hook() {
    // The bash/zsh `command_not_found_handle` convention: when defined, a
    // failed lookup calls it with the command's own argv instead of
    // rush's usual "command not found" message, and its own status wins.
    let (out, status) = rush(
        r#"command_not_found_handle() { echo "handle: $1 $2 $3"; return 42; }; frobnicate a b"#,
    );
    assert_eq!(out, "handle: frobnicate a b\n");
    assert_eq!(status, 42);

    // rush's own "command not found" message is suppressed once a handler
    // exists — the handler owns diagnostics, matching bash.
    let (err, _) = rush_stderr(
        r#"command_not_found_handle() { echo "custom: $1"; }; totallynonexistentcmd_cnfh"#,
    );
    assert!(!err.contains("command not found"), "got: {err:?}");

    // With no handler defined, behavior is unchanged: rush's own message,
    // status 127.
    let (out, status) = rush("frobnicate_unhandled; echo status=$?");
    assert_eq!(out, "status=127\n");
    assert_eq!(status, 0);
}

#[cfg(unix)]
#[test]
fn unknown_command_still_triggers_errexit() {
    let (out, status) = rush("set -e; totallynonexistentcmd_c37; echo should_not_print");
    assert_eq!(out, "");
    assert_eq!(status, 127);
}

#[cfg(unix)]
#[test]
fn a_found_but_unexecutable_file_reports_126_not_127() {
    let path = std::env::temp_dir().join(format!("rush_c37_noexec_{}.txt", std::process::id()));
    std::fs::write(&path, "not a script\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

    let (out, _) = rush(&format!("{}; echo status=$?", path.to_str().unwrap()));
    assert_eq!(out, "status=126\n");

    let _ = std::fs::remove_file(&path);
}

#[cfg(unix)]
#[test]
fn unknown_command_in_a_command_substitution_reports_127_and_captures_nothing() {
    let (out, status) = rush("x=$(totallynonexistentcmd_c37); echo \"status=$? captured=[$x]\"");
    assert_eq!(out, "status=127 captured=[]\n");
    assert_eq!(status, 0);
}

#[cfg(unix)]
#[test]
fn backgrounding_an_unknown_command_does_not_abort_the_script_either() {
    let (out, status) = rush("totallynonexistentcmd_c37 & echo done");
    assert_eq!(out, "done\n");
    assert_eq!(status, 0);
}

#[test]
fn dollar_dollar_expands_to_the_shell_pid() {
    // C41: `echo $$` used to print the literal two-character text `$$`
    // (and `${$}` errored as a bad substitution) — the single most common
    // special-parameter idiom in real scripts (`tmpfile=/tmp/x.$$`).
    let (out, _) = rush(r#"echo $$; echo ${$}; echo "quoted=$$""#);
    let mut lines = out.lines();
    let pid = lines.next().unwrap();
    assert!(pid.parse::<u32>().is_ok(), "not a pid: {pid:?}");
    assert_eq!(lines.next().unwrap(), pid); // `${$}` — same value
    assert_eq!(lines.next().unwrap(), format!("quoted={pid}"));
}

#[cfg(unix)]
#[test]
fn ppid_expands_to_the_invoking_process() {
    // C41: `$PPID` used to expand to empty. The process spawning `rush -c`
    // here is this very test process, so the value is exactly checkable.
    let (out, _) = rush("echo $PPID");
    assert_eq!(out.trim(), std::process::id().to_string());

    // Seeded *after* the inherited environment, so a stale `PPID` exported
    // by a parent shell can't shadow the real value (bash behaves the same).
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_rush"))
        .arg("-c")
        .arg("echo $PPID")
        .env("PPID", "12345")
        .output()
        .expect("spawn rush");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        std::process::id().to_string()
    );
}

#[cfg(unix)]
#[test]
fn dollar_dash_reflects_currently_set_options() {
    // C41: `$-` used to expand to empty always. One letter per set option;
    // `set +e` removes its letter again; `${-}` is the braced spelling.
    // `h`/`B` (hashall/braceexpand) are always on and `c` marks `-c`, so a
    // bare `$-` is `hBc` here — matching `bash -c` exactly (C135).
    let (out, _) = rush("echo [$-]; set -eu; echo [$-]; set +e; echo [${-}]");
    assert_eq!(out, "[hBc]\n[ehuBc]\n[huBc]\n");
}

#[cfg(unix)]
#[test]
fn set_accepts_clustered_short_flags() {
    // Found while verifying C41's `$-`: `set -eu` / `set -euo pipefail` —
    // the near-universal script header — errored with "not supported";
    // only one flag per word ever parsed.
    let (out, status) = rush("set -euo pipefail; echo [$-]");
    assert_eq!(out, "[ehuBc]\n");
    assert_eq!(status, 0);

    // Flags before the first bare word still apply, and the word starts
    // the new positional parameters (same as real bash).
    let (out, _) = rush("set -ex a b; echo \"1=$1 2=$2\"");
    assert!(out.contains("1=a 2=b"), "got: {out:?}");
}

#[cfg(unix)]
#[test]
fn set_applies_nothing_when_any_flag_is_invalid() {
    // Real bash rolls the whole invocation back: `set -eu -z` applies
    // neither `-e` nor `-u` (verified directly) — partial application
    // would errexit-kill the shell on `set`'s own failure here.
    let (out, status) = rush("set -eu -z 2>/dev/null; echo [$-] survived");
    assert_eq!(out, "[hBc] survived\n");
    assert_eq!(status, 0);
}

#[cfg(unix)]
#[test]
fn posix_character_classes_in_case_and_pattern_removal() {
    // C42: `[[:digit:]]`-style POSIX named classes were misparsed as their
    // own literal characters — `case 5 in [[:digit:]])` silently never
    // matched. The same matcher backs `case`, filename globbing, and the
    // `${v#pat}` family, so one fix covers all three.
    let (out, _) = rush("case 5 in [[:digit:]]) echo dig;; *) echo no;; esac");
    assert_eq!(out, "dig\n");

    let (out, _) = rush("case B in [[:upper:]]) echo up;; *) echo no;; esac");
    assert_eq!(out, "up\n");

    let (out, _) = rush(r#"v=abc123; echo "${v%%[[:digit:]]*}""#);
    assert_eq!(out, "abc\n");

    // Unknown class name: matches nothing (same as bash), not an error.
    let (out, _) = rush("case b in [[:bogus:]]) echo m;; *) echo no;; esac");
    assert_eq!(out, "no\n");
}

#[cfg(unix)]
#[test]
fn posix_character_classes_in_filename_globbing() {
    // C42, the filename-expansion side: `ls [[:alpha:]]*` used to silently
    // match nothing.
    let dir = std::env::temp_dir().join(format!("rush_c42_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for f in ["a5", "ab", "aB"] {
        std::fs::write(dir.join(f), "").unwrap();
    }
    let cd = format!("cd {}; ", dir.display());
    let (out, _) = rush(&format!("{cd}echo a[[:digit:]]"));
    assert_eq!(out, "a5\n");
    let (out, _) = rush(&format!("{cd}echo a[[:upper:]]"));
    assert_eq!(out, "aB\n");
    let (out, _) = rush(&format!("{cd}echo a[![:digit:]]"));
    assert_eq!(out, "aB ab\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn declare_case_attributes_transform_assignments() {
    // C43: `declare -u/-l` used to be misparsed as bare variable names —
    // the assignment proceeded untransformed with no diagnostic.
    let (out, _) = rush("declare -u u=hello; echo $u; u=bye; echo $u");
    assert_eq!(out, "HELLO\nBYE\n");

    let (out, _) = rush("declare -l L=ABC; echo $L");
    assert_eq!(out, "abc\n");

    // Not retroactive: an existing value stays; future assignments map.
    let (out, _) = rush("x=abc; declare -u x; echo $x; x=def; echo $x");
    assert_eq!(out, "abc\nDEF\n");

    // Attributes apply per array element; `unset` drops the attribute.
    let (out, _) = rush("declare -au arr=(a b); echo ${arr[@]}");
    assert_eq!(out, "A B\n");
    let (out, _) = rush("declare -u u=x; unset u; u=abc; echo $u");
    assert_eq!(out, "abc\n");
}

#[cfg(unix)]
#[test]
fn declare_integer_attribute_routes_through_arithmetic() {
    // C43: `declare -i n; n=2+3` used to store the literal text `2+3`.
    let (out, _) = rush("declare -i n; n=2+3; echo $n");
    assert_eq!(out, "5\n");

    // Names resolve inside the expression; `+=` is arithmetic addition.
    let (out, _) = rush("m=7; declare -i k; k=m+2; echo $k; declare -i n=5; n+=3; echo $n");
    assert_eq!(out, "9\n8\n");

    // An unresolvable word is 0 (same as bash); a syntax error keeps the
    // old value and prints a diagnostic.
    let (out, _) = rush("declare -i n; n=foo; echo [$n]; n=7; n=2+ 2>/dev/null; echo [$n]");
    assert_eq!(out, "[0]\n[7]\n");
}

#[cfg(unix)]
#[test]
fn local_attributes_are_function_scoped() {
    // C43: `local -u` — the attribute applies inside the call and is gone
    // (along with the local value) after it returns.
    let (out, _) = rush("v=Mixed; f(){ local -u v=abc; echo in=$v; v=ghi; echo in=$v; }; f; echo out=$v; v=next; echo $v");
    assert_eq!(out, "in=ABC\nin=GHI\nout=Mixed\nnext\n");
}

#[cfg(unix)]
#[test]
fn trap_accepts_numeric_and_sig_prefixed_specs() {
    // C44: a trap registered under `15` or `SIGTERM` was stored verbatim
    // and silently orphaned — delivery only ever looked up `TERM`, so the
    // handler never ran and the signal took the default disposition.
    let (out, _) = rush("trap 'echo caught' 15; kill -TERM $$; sleep 0; echo after");
    assert_eq!(out, "caught\nafter\n");

    let (out, _) = rush("trap 'echo caught' SIGTERM; kill -15 $$; sleep 0; echo after");
    assert_eq!(out, "caught\nafter\n");

    // Lowercase works too (bash accepts it — verified), and `0` is EXIT.
    let (out, _) = rush("trap 'echo caught' sighup; kill -HUP $$; sleep 0; echo after");
    assert_eq!(out, "caught\nafter\n");
    let (out, _) = rush("trap 'echo bye' 0; true");
    assert_eq!(out, "bye\n");

    // `trap - 15` removes a TERM trap: default disposition kills the
    // shell (128+15) before `after` prints.
    let (out, status) = rush("trap 'echo caught' TERM; trap - 15; kill -15 $$; echo after");
    assert_eq!(out, "");
    assert_eq!(status, 143);
}

#[cfg(unix)]
#[test]
fn trap_rejects_invalid_specs_without_blocking_valid_ones() {
    // Matching real bash exactly: the invalid spec errors (status 1), the
    // valid one in the same call still registers.
    let (out, _) =
        rush("trap 'echo caught' TERM BOGUS 2>/dev/null; echo st=$?; kill -15 $$; sleep 0; echo after");
    assert_eq!(out, "st=1\ncaught\nafter\n");

    let (err, status) = rush_stderr("trap 'echo x' 99");
    assert!(err.contains("99: invalid signal specification"), "got: {err:?}");
    assert_eq!(status, 1);
}

#[cfg(unix)]
#[test]
fn trap_listing_prints_sig_prefixed_names() {
    // bash's own `trap` output format: real signals SIG-prefixed, EXIT bare.
    let (out, _) = rush("trap 'echo T' SIGTERM; trap");
    assert_eq!(out, "trap -- 'echo T' SIGTERM\n");
    let (out, _) = rush("trap 'echo E' EXIT; trap; trap - EXIT");
    assert_eq!(out, "trap -- 'echo E' EXIT\n");
}

#[cfg(unix)]
#[test]
fn readonly_builtin_assigns_and_locks() {
    // C45: `readonly` wasn't a builtin at all — `readonly x=1` was
    // "command not found" and the assignment itself was silently lost.
    let (out, _) = rush("readonly x=1; echo $x");
    assert_eq!(out, "1\n");

    // A later assignment is FATAL in a non-interactive shell (verified:
    // bash aborts the whole script there), status 1.
    let (out, status) = rush("readonly x=1; x=2; echo should_not_print");
    assert_eq!(out, "");
    assert_eq!(status, 1);
    let (err, _) = rush_stderr("readonly x=1; x=2");
    assert!(err.contains("x: readonly variable"), "got: {err:?}");

    // `+=`, element writes, and a readonly `for` variable are fatal too.
    let (out, status) = rush("readonly x=1; x+=2; echo nope");
    assert_eq!((out.as_str(), status), ("", 1));
    let (out, status) = rush("readonly arr=(a b); arr[0]=c; echo nope");
    assert_eq!((out.as_str(), status), ("", 1));
    let (out, status) = rush("readonly x=1; for x in a b; do echo loop; done");
    assert_eq!((out.as_str(), status), ("", 1));

    // `readonly z` (no value) leaves z genuinely unset but locked.
    let (out, _) = rush("readonly z; echo ${z+set}notset");
    assert_eq!(out, "notset\n");
}

#[cfg(unix)]
#[test]
fn readonly_builtin_mediated_attempts_fail_without_aborting() {
    // Unlike a bare assignment, `unset`/`export`/`local`/`readonly`
    // attempts fail with status 1 and the script continues — verified
    // against real bash for each.
    let (out, _) = rush("readonly x=1; unset x 2>/dev/null; echo \"st=$? x=$x\"");
    assert_eq!(out, "st=1 x=1\n");

    let (out, _) = rush("readonly x=1; export x=2 2>/dev/null; echo st=$?");
    assert_eq!(out, "st=1\n");

    let (out, _) = rush("readonly x=1; f(){ local x; }; f 2>/dev/null; echo st=$?");
    assert_eq!(out, "st=1\n");

    let (out, _) = rush("readonly x=1; readonly x=9 2>/dev/null; echo \"st=$? x=$x\"");
    assert_eq!(out, "st=1 x=1\n");

    // A bare `export x` on a readonly name is fine — it only adds the
    // export flag.
    let (out, _) = rush("readonly x=1; export x; echo st=$?");
    assert_eq!(out, "st=0\n");
}

#[cfg(unix)]
#[test]
fn readonly_listing_and_declare_r() {
    // `readonly`/`readonly -p` list in bash's own format. (Not piped:
    // like `declare`/`local`, the decl-path builtins aren't dispatched
    // as one stage of a multi-stage pipeline — a pre-existing, shared
    // limitation, not part of C45.)
    let (out, _) = rush("readonly x=1; readonly -p");
    assert!(out.lines().any(|l| l == "declare -r x=\"1\""), "got: {out:?}");
    let (out, _) = rush("readonly arr=(a b); readonly");
    assert!(out.lines().any(|l| l == "declare -ar arr=([0]=\"a\" [1]=\"b\")"), "got: {out:?}");

    // `declare -r` and `local -r` reach the same flag.
    let (out, status) = rush("declare -r y=5; y=6; echo nope");
    assert_eq!((out.as_str(), status), ("", 1));
    let (out, _) = rush("f(){ local -r v=5; echo v=$v; }; f; v=7; echo after=$v");
    assert_eq!(out, "v=5\nafter=7\n");
}

#[cfg(unix)]
#[test]
fn readonly_prefix_assignment_errors_but_still_runs() {
    // Verified against real bash: the diagnostic prints, the command
    // still runs, and the child does NOT see the refused new value.
    let (out, _) = rush("readonly x=1; x=2 /bin/sh -c 'echo child_x=$x' 2>/dev/null; echo after");
    assert_eq!(out, "child_x=\nafter\n");
}

#[cfg(unix)]
#[test]
fn ulimit_reads_and_sets_limits() {
    // C46: `ulimit` was "command not found" — its total absence blocked
    // the ubiquitous `ulimit -n`/`ulimit -c 0` operational-script openers.
    // With no flag the subject is -f (file size), same as bash.
    let (out, status) = rush("ulimit");
    assert_eq!(status, 0);
    assert!(!out.trim().is_empty());

    // Lowering -n applies to the process and is inherited by children —
    // observed via a real child /bin/sh reporting its own limit.
    let (out, _) = rush("ulimit -n 1024; ulimit -n; /bin/sh -c 'ulimit -n'");
    assert_eq!(out, "1024\n1024\n");

    // -S sets only the soft limit; -H still reports the original hard one.
    let (out, _) = rush("hard=$(ulimit -H -n); ulimit -S -n 512; echo \"$(ulimit -n) $([ \"$(ulimit -H -n)\" = \"$hard\" ] && echo same)\"");
    assert_eq!(out, "512 same\n");

    // -a dumps labeled lines in bash's own format.
    let (out, _) = rush("ulimit -a");
    assert!(out.lines().any(|l| l.starts_with("open files") && l.contains("-n")), "got: {out:?}");

    // Error paths: unknown flag is usage error 2, a bad number is 1.
    let (_, status) = rush("ulimit -z 2>/dev/null");
    assert_eq!(status, 2);
    let (err, status) = rush_stderr("ulimit -n abc");
    assert!(err.contains("abc: invalid number"), "got: {err:?}");
    assert_eq!(status, 1);
}

#[cfg(unix)]
#[test]
fn command_p_uses_the_default_system_path() {
    // C47: `command -p` treated `-p` as the command name itself. It now
    // executes/looks up through the fixed default system path, immune to
    // the shell's own $PATH.
    let (out, _) = rush("PATH=/nowhere; command -p ls /dev/null; echo st=$?");
    assert_eq!(out, "/dev/null\nst=0\n");

    // Lookup forms, clustered and separate, also ignore $PATH.
    let (out, _) = rush("PATH=/nowhere; command -pv ls; command -p -v ls");
    assert_eq!(out, "/bin/ls\n/bin/ls\n");

    // A builtin still wins over a default-path file, same as bash.
    let (out, _) = rush("command -p echo built");
    assert_eq!(out, "built\n");

    // Not found anywhere on the default path: ordinary 127, clean message.
    let (out, status) = rush("command -p totallynonexistent_c47 2>/dev/null; echo st=$?");
    assert_eq!(out, "st=127\n");
    assert_eq!(status, 0);
    let (err, _) = rush_stderr("command -p totallynonexistent_c47");
    assert!(err.contains("totallynonexistent_c47: command not found"), "got: {err:?}");
    assert!(!err.contains("totallynonexistent_c47/"), "synthetic slash leaked: {err:?}");
}

#[cfg(unix)]
#[test]
fn type_a_lists_every_match() {
    // C48: `type -a` used to parse `-a` as a name to look up. It now
    // lists every match — builtin first, then every $PATH hit in
    // directory order (byte-identical to bash for `type -a echo` here).
    let (out, _) = rush("PATH=/bin:/usr/bin; type -a echo");
    assert_eq!(out, "echo is a shell builtin\necho is /bin/echo\necho is /usr/bin/echo\n");

    // Clustered with -t; duplicate PATH directories deliberately not
    // deduped (bash lists ls twice for /bin:/usr/bin:/bin — verified).
    let (out, _) = rush("PATH=/bin:/usr/bin; type -at echo");
    assert_eq!(out, "builtin\nfile\nfile\n");
    let (out, _) = rush("PATH=/bin:/usr/bin:/bin; type -a ls");
    assert_eq!(out.lines().count(), 3, "got: {out:?}");

    // Alias/keyword/function still rank ahead; not-found is status 1.
    let (out, _) = rush("type -a if");
    assert_eq!(out, "if is a shell keyword\n");
    let (_, status) = rush("type -a nosuch_c48 2>/dev/null");
    assert_eq!(status, 1);
}

#[cfg(unix)]
#[test]
fn typeset_is_a_synonym_for_declare() {
    // C49: `typeset` (ksh93's only spelling; a bash/zsh synonym) wasn't
    // registered at all. Everything declare supports rides along —
    // attributes (C43), arrays, readonly (C45).
    let (out, _) = rush("typeset -u u=hello; echo $u; typeset -i n; n=2+3; echo $n");
    assert_eq!(out, "HELLO\n5\n");

    let (out, _) = rush("typeset -A m; m[k]=v; echo ${m[k]}; typeset -a arr=(x y); echo ${arr[1]}");
    assert_eq!(out, "v\ny\n");

    let (out, status) = rush("typeset -r ro=1; ro=2; echo nope");
    assert_eq!((out.as_str(), status), ("", 1));

    let (out, _) = rush("type -t typeset");
    assert_eq!(out, "builtin\n");
}

#[cfg(unix)]
#[test]
fn noclobber_refuses_overwrite_and_clobber_overrides() {
    // C50: `set -C` didn't exist and `>|` didn't lex.
    let dir = std::env::temp_dir().join(format!("rush_c50_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("f");
    let f = f.to_str().unwrap();

    // Creating a fresh file under -C is fine; a second `>` refuses and
    // the original content survives. (Rush's pre-existing behavior for
    // any failed redirect is to abort the script — bash continues with
    // status 1; that divergence is inherited here, not new to C50.)
    let (_, status) = rush(&format!("set -C; echo x > {f}; echo y > {f}"));
    assert_eq!(status, 1);
    assert_eq!(std::fs::read_to_string(f).unwrap(), "x\n");

    // `>|` overrides; `>>` and device targets are exempt.
    let (out, _) = rush(&format!("set -C; echo y >| {f}; echo st=$?; cat {f}"));
    assert_eq!(out, "st=0\ny\n");
    let (out, _) = rush(&format!("set -C; echo a >> {f}; echo z > /dev/null; echo st=$?"));
    assert_eq!(out, "st=0\n");

    // `&>` honors noclobber too (verified against bash).
    let (_, status) = rush(&format!("set -C; echo b &> {f}"));
    assert_eq!(status, 1);

    // `set +C` turns it back off; `>|` without -C is a plain write; `$-`
    // gains/loses C.
    let (out, _) = rush(&format!("set -C; set +C; echo ok > {f}; cat {f}; echo n >| {f}; cat {f}"));
    assert_eq!(out, "ok\nn\n");
    let (out, _) = rush("set -C; echo [$-]; set +C; echo [$-]");
    assert_eq!(out, "[hBCc]\n[hBc]\n");

    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn set_n_parses_but_runs_nothing() {
    // C51: `set -n` was rejected outright; `rush -n` didn't exist.
    // Mid-script, everything after `set -n` is skipped — including the
    // `set +n` that would undo it (one-way, matching bash).
    let (out, status) = rush("echo one; set -n; echo two; set +n; echo three");
    assert_eq!(out, "one\n");
    assert_eq!(status, 0);
}

#[cfg(unix)]
#[test]
fn rush_dash_n_is_syntax_check_only() {
    // The `sh -n script.sh` linting idiom: clean syntax → status 0 and no
    // execution; a syntax error → status 2.
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_rush"))
        .args(["-n", "-c", "echo should_not_run"])
        .output()
        .expect("spawn rush");
    assert_eq!(String::from_utf8_lossy(&output.stdout), "");
    assert_eq!(output.status.code(), Some(0));

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_rush"))
        .args(["-n", "-c", "echo hi("])
        .output()
        .expect("spawn rush");
    assert_eq!(output.status.code(), Some(2));

    // Script-file mode too.
    let f = std::env::temp_dir().join(format!("rush_c51_{}.sh", std::process::id()));
    std::fs::write(&f, "echo nope\n").unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_rush"))
        .arg("-n")
        .arg(&f)
        .output()
        .expect("spawn rush");
    assert_eq!(String::from_utf8_lossy(&output.stdout), "");
    assert_eq!(output.status.code(), Some(0));
    let _ = std::fs::remove_file(&f);
}

#[cfg(unix)]
#[test]
fn set_o_long_names_and_listing() {
    // C52: long spellings map to the same flags as the short forms.
    let (out, status) = rush("set -o errexit; false; echo nope");
    assert_eq!((out.as_str(), status), ("", 1));
    let (out, _) = rush("set -o nounset; echo ${UNSET_C52-fallback}; set +o nounset; echo ok$UNSET_C52");
    assert_eq!(out, "fallback\nok\n");

    // Bare `set -o`: bash's own name/on-off table (format verified
    // byte-identical to bash over the tracked options).
    let (out, _) = rush("set -o errexit; set -o");
    assert!(out.lines().any(|l| l == "errexit        \ton"), "got: {out:?}");
    assert!(out.lines().any(|l| l == "pipefail       \toff"), "got: {out:?}");

    // Bare `set +o`: directly re-runnable lines.
    let (out, _) = rush("set -o pipefail; set +o");
    assert!(out.lines().any(|l| l == "set -o pipefail"), "got: {out:?}");
    assert!(out.lines().any(|l| l == "set +o errexit"), "got: {out:?}");

    // Round-trip: `set +o` output re-runs cleanly.
    let (out, _) = rush("set -o noclobber; saved=$(set +o); set +o noclobber; eval \"$saved\"; echo [$-]");
    assert_eq!(out, "[hBCc]\n");

    let (err, status) = rush_stderr("set -o badname");
    assert!(err.contains("badname: invalid option name"), "got: {err:?}");
    assert_eq!(status, 1);
}

#[cfg(unix)]
#[test]
fn trap_err_fires_on_failing_commands() {
    // C53: `trap 'cmd' ERR` registered but never fired. It now fires on
    // exactly errexit's condition — every expectation below mirrors a
    // directly-verified bash behavior.
    let (out, _) = rush("trap 'echo E' ERR; false; false; echo end");
    assert_eq!(out, "E\nE\nend\n");

    // The handler sees the failing status as $?, and $? is restored to it
    // afterward regardless of what the handler ran.
    let (out, _) = rush("trap 'echo E:$?' ERR; false; echo end");
    assert_eq!(out, "E:1\nend\n");
    let (out, _) = rush("trap 'true' ERR; false; echo st=$?");
    assert_eq!(out, "st=1\n");

    // Not fired: if/while conditions, non-final &&/|| commands, negated
    // pipelines, or inside a function (bash's no-errtrace default).
    let (out, _) = rush("trap 'echo E' ERR; if false; then :; fi; false && echo x; ! true; f(){ false; true; }; f; echo end");
    assert_eq!(out, "end\n");

    // Fired: a failing final &&/|| command, a failing pipeline, a function
    // returning nonzero at top level, and (before the exit) under set -e.
    let (out, _) = rush("trap 'echo E' ERR; true && false; true | false; f(){ return 3; }; f; echo end");
    assert_eq!(out, "E\nE\nE\nend\n");
    let (out, status) = rush("set -e; trap 'echo E' ERR; false; echo nope");
    assert_eq!((out.as_str(), status), ("E\n", 1));
}

#[cfg(unix)]
#[test]
fn bang_negates_a_pipeline() {
    // Found while landing C53 (they interact): `! cmd` didn't parse at
    // all — "!: command not found". POSIX pipeline negation, plus its
    // errexit exemption.
    let (out, _) = rush("! false; echo st=$?; ! true; echo st=$?; ! ! true; echo st=$?");
    assert_eq!(out, "st=0\nst=1\nst=0\n");

    let (out, _) = rush("! true | false; echo st=$?");
    assert_eq!(out, "st=0\n");

    // Exempt from set -e even when the negated status is 1 (bash rule).
    let (out, status) = rush("set -e; ! true; echo survived");
    assert_eq!((out.as_str(), status), ("survived\n", 0));

    // Inside a command substitution too, including $? visibility.
    let (out, _) = rush("echo got=$(! true; echo $?)");
    assert_eq!(out, "got=1\n");
}

#[cfg(unix)]
#[test]
fn pipestatus_records_per_stage_statuses() {
    // C54: `${PIPESTATUS[@]}` always expanded empty. Every expectation
    // below mirrors directly-verified bash behavior.
    let (out, _) = rush(r#"sh -c 'exit 3' | sh -c 'exit 5' | true; echo "${PIPESTATUS[@]} ${PIPESTATUS[1]} ${#PIPESTATUS[@]}""#);
    assert_eq!(out, "3 5 0 5 3\n");

    // Every command replaces it — reading it twice shows the echo's own.
    let (out, _) = rush(r#"true | false; echo "${PIPESTATUS[@]}"; echo "${PIPESTATUS[@]}""#);
    assert_eq!(out, "0 1\n0\n");

    // Single commands (builtin, compound, assignment) get one element;
    // `! false` records the un-negated status; pipefail doesn't distort it.
    let (out, _) = rush(r#"false; echo "${PIPESTATUS[@]}"; ! false; echo "${PIPESTATUS[@]}""#);
    assert_eq!(out, "1\n1\n");
    let (out, _) = rush(r#"if true; then :; fi; echo "${PIPESTATUS[@]}"; x=5; echo "${PIPESTATUS[@]}""#);
    assert_eq!(out, "0\n0\n");
    let (out, _) = rush(r#"set -o pipefail; false | true; echo "st=$? ${PIPESTATUS[@]}""#);
    assert_eq!(out, "st=1 1 0\n");
}

#[cfg(unix)]
#[test]
fn double_bracket_extended_test() {
    // C55, the largest item in the pass: `[[ ]]` didn't exist at all —
    // `[[ foo = foo ]]` was command-not-found (127), and `<` inside one
    // was misparsed as a redirection. Every expectation below was
    // verified byte-identical against real bash first.
    let (out, _) = rush("[[ foo = foo ]] && echo yes; [[ foo = bar ]]; echo st=$?");
    assert_eq!(out, "yes\nst=1\n");

    // The whole reason `[[` exists: empty-safe, split-safe, glob-safe
    // operands — each of these is a "too many arguments" error under [ ].
    let (out, _) = rush(r#"x=; [[ $x = foo ]]; echo st=$?; x="a b"; [[ $x = "a b" ]] && echo split-safe"#);
    assert_eq!(out, "st=1\nsplit-safe\n");

    // Unquoted RHS is a glob pattern (even via $var); quoted is literal.
    let (out, _) = rush(r#"x=foo.txt; [[ $x = *.txt ]] && echo glob; [[ $x = "*.txt" ]] || echo lit; p="*.txt"; [[ $x = $p ]] && echo varpat; [[ abc = "a"* ]] && echo mixed"#);
    assert_eq!(out, "glob\nlit\nvarpat\nmixed\n");

    // `<`/`>` compare lexicographically — no redirection, no file.
    let (out, _) = rush("[[ abc < abd ]] && echo lt; [[ abd > abc ]] && echo gt; [ -e abd ] || echo nofile");
    assert_eq!(out, "lt\ngt\nnofile\n");

    // &&/||/!/( ) nest directly; -eq family is full arithmetic.
    let (out, _) = rush("[[ ( a = b || a = a ) && c = c ]] && echo grouped; [[ ! -f nosuch_c55 ]] && echo notfile; x=5; [[ x -eq 5 ]] && echo arith");
    assert_eq!(out, "grouped\nnotfile\narith\n");

    // Unary file/string ops, POSIX classes inside [[, and `if [[ … ]]`.
    let (out, _) = rush("[[ -f Cargo.toml && -d src && -n x && -z \"\" ]] && echo ops; [[ 5 = [[:digit:]] ]] && echo class; if [[ -d src ]]; then echo in-if; fi");
    assert_eq!(out, "ops\nclass\nin-if\n");

    // Multi-line [[ ]] keeps reading (Incomplete → continuation).
    let (out, _) = rush("[[ a = a\n]] && echo multiline");
    assert_eq!(out, "multiline\n");

    // A `case` class pattern still lexes as its own word, not as `[[`.
    let (out, _) = rush("case 5 in [[:digit:]]) echo case-ok;; esac");
    assert_eq!(out, "case-ok\n");

    // A malformed expression is a parse-time syntax error that aborts,
    // status 2 (same as bash: `[[ a -eq ]]` kills the script there).
    let (out, status) = rush("[[ a -eq ]]; echo nope");
    assert_eq!((out.as_str(), status), ("", 2));
    // `=~` works (C56) — full coverage in its own test below.
    let (out, _) = rush("[[ a =~ a ]]; echo st=$?");
    assert_eq!(out, "st=0\n");
}

#[cfg(unix)]
#[test]
fn regex_match_and_bash_rematch() {
    // C56: `[[ $s =~ regex ]]` — unanchored ERE search with capture
    // groups in BASH_REMATCH. Each expectation verified against bash.
    let (out, _) = rush(r#"[[ "abc123" =~ ([a-z]+)([0-9]+) ]] && echo "${BASH_REMATCH[0]}|${BASH_REMATCH[1]}|${BASH_REMATCH[2]}""#);
    assert_eq!(out, "abc123|abc|123\n");

    // Quantifiers, anchors, and the $var idiom; quoted RHS is literal.
    let (out, _) = rush(r#"[[ 2024-01-15 =~ ^([0-9]{4})-([0-9]{2}) ]] && echo "y=${BASH_REMATCH[1]} m=${BASH_REMATCH[2]}"; p="^a.c$"; [[ abc =~ $p ]] && echo var; [[ abc =~ "a.c" ]] || echo quoted-literal"#);
    assert_eq!(out, "y=2024 m=01\nvar\nquoted-literal\n");

    // An unmatched optional group is present as an empty string; a
    // failed match unsets the array (bash 5 behavior, verified).
    let (out, _) = rush(r#"[[ abc =~ (x)?(b) ]] && echo "n=${#BASH_REMATCH[@]} [${BASH_REMATCH[1]}] [${BASH_REMATCH[2]}]"; [[ abc =~ z ]]; echo "st=$? [${BASH_REMATCH[0]}]""#);
    assert_eq!(out, "n=3 [] [b]\nst=1 []\n");

    // Parens/spaces inside groups lex as part of the pattern; `\.` stays
    // a literal dot; composes with && inside the same [[.
    let (out, _) = rush(r#"[[ "a b" =~ (a b) ]] && echo group; [[ a.c =~ a\.c ]] && echo esc; [[ abc =~ a\.c ]] || echo esc2; [[ abc =~ a(b)c && x = x ]] && echo "combined ${BASH_REMATCH[1]}""#);
    assert_eq!(out, "group\nesc\nesc2\ncombined b\n");

    // An invalid (quoted, so runtime) regex piece: literal, so just no
    // match — while a syntactically-live bad pattern is a status-2 error.
    let (out, _) = rush(r#"[[ abc =~ "(" ]]; echo st=$?; p='['; [[ abc =~ $p ]] 2>/dev/null; echo st=$?; echo alive"#);
    assert_eq!(out, "st=1\nst=2\nalive\n");
}

#[cfg(unix)]
#[test]
fn extended_globs_across_surfaces() {
    // C57: `@(a|b)` used to mis-tokenize into `@` + a subshell. Always-on
    // (like ksh93 — bash gates them behind `shopt -s extglob`, and
    // *without* it these are hard syntax errors there, so always-on is
    // strictly more compatible). Verified against bash with extglob on.
    let (out, _) = rush("for s in afile bfile cfile abfile; do case $s in @(a|b)file) echo \"$s: at\";; *) :;; esac; done");
    assert_eq!(out, "afile: at\nbfile: at\n");

    // [[ ]] pattern matching and the ${v%%pat} family share the matcher.
    let (out, _) = rush("[[ aaa = +(a) ]] && echo plus; [[ cfile = !(a|b)file ]] && echo neg; v=foo.tar.gz; echo ${v%%+(.*)}");
    assert_eq!(out, "plus\nneg\nfoo\n");

    // Filename expansion, byte-identical to bash on the same fixtures.
    let dir = std::env::temp_dir().join(format!("rush_c57_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for f in ["afile", "bfile", "cfile", "abfile", "file"] {
        std::fs::write(dir.join(f), "").unwrap();
    }
    let cd = format!("cd {}; ", dir.display());
    let (out, _) = rush(&format!("{cd}echo @(a|b)file; echo !(a|b)file; echo +(a|b)file"));
    assert_eq!(out, "afile bfile\nabfile cfile file\nabfile afile bfile\n");
    let _ = std::fs::remove_dir_all(&dir);

    // No match → literal (the shared no-match rule), and a bare subshell
    // `(...)` is still a subshell.
    let (out, _) = rush("echo @(zz|yy)qq; (echo subshell)");
    assert_eq!(out, "@(zz|yy)qq\nsubshell\n");
}

#[cfg(unix)]
#[test]
fn shopt_and_glob_options() {
    // C58: shopt didn't exist and the glob engine's behavior was
    // hardcoded. Formats and statuses all verified against bash.
    let dir = std::env::temp_dir().join(format!("rush_c58_{}", std::process::id()));
    std::fs::create_dir_all(dir.join("a/b")).unwrap();
    for f in ["f1.txt", "a/f2.txt", "a/b/f3.txt", ".hidden"] {
        std::fs::write(dir.join(f), "").unwrap();
    }
    let cd = format!("cd {}; ", dir.display());

    // nullglob drops the word; failglob is a hard error; default keeps
    // the literal.
    let (out, _) = rush(&format!("{cd}shopt -s nullglob; echo x *.zzz y"));
    assert_eq!(out, "x y\n");
    let (out, status) = rush(&format!("{cd}shopt -s failglob; echo *.zzz; echo after"));
    assert_eq!((out.as_str(), status), ("", 1));
    let (out, _) = rush(&format!("{cd}echo *.zzz"));
    assert_eq!(out, "*.zzz\n");

    // dotglob lets * see dotfiles; globstar makes ** recursive (zero or
    // more levels — bash-identical output shapes).
    let (out, _) = rush(&format!("{cd}shopt -s dotglob; echo *"));
    assert_eq!(out, ".hidden a f1.txt\n");
    let (out, _) = rush(&format!("{cd}shopt -s globstar; echo **"));
    assert_eq!(out, "a a/b a/b/f3.txt a/f2.txt f1.txt\n");
    let (out, _) = rush(&format!("{cd}shopt -s globstar; echo **/*.txt"));
    assert_eq!(out, "a/b/f3.txt a/f2.txt f1.txt\n");
    let (out, _) = rush(&format!("{cd}shopt -s globstar; echo a/**"));
    assert_eq!(out, "a/ a/b a/b/f3.txt a/f2.txt\n");
    // Without globstar, ** collapses to * (the pre-existing behavior).
    let (out, _) = rush(&format!("{cd}echo **"));
    assert_eq!(out, "a f1.txt\n");

    // Query/set/quiet/print forms and statuses.
    let (out, _) = rush("shopt nullglob; echo st=$?; shopt -q nullglob; echo st=$?; shopt -s nullglob; shopt -q nullglob; echo st=$?");
    assert_eq!(out, "nullglob       \toff\nst=1\nst=1\nst=0\n");
    let (out, _) = rush("shopt -p extglob");
    assert_eq!(out, "shopt -s extglob\n"); // rush's extglob defaults ON (C57)
    let (err, status) = rush_stderr("shopt badopt");
    assert!(err.contains("invalid shell option name"), "got: {err:?}");
    assert_eq!(status, 1);

    // extglob is genuinely toggleable: off makes the pattern literal.
    let (out, _) = rush("shopt -u extglob; [[ afile = @(a|b)file ]]; echo st=$?; shopt -s extglob; [[ afile = @(a|b)file ]] && echo back-on");
    assert_eq!(out, "st=1\nback-on\n");

    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn string_transformation_operators() {
    // C59: the ${v/…}, ${v:off:len}, and ${v^^} families all fell
    // through to "bad substitution". Every expectation verified against
    // bash byte-for-byte.
    let (out, _) = rush(r#"v=hello_world; echo "${v/o/0}" "${v//o/0}" "${v/#hello/HI}" "${v/%world/EARTH}" "${v/o}" "${v//l}""#);
    assert_eq!(out, "hell0_world hell0_w0rld HI_world hello_EARTH hell_world heo_word\n");

    // Substrings: negative offset (space-disambiguated), negative
    // length, arithmetic expressions, out-of-range → empty.
    let (out, _) = rush(r#"v=abcdef; echo "${v:2}" "${v:2:3}" "${v: -3}" "${v: -4:2}" "${v:1:-2}"; n=2; echo "${v:n+1:2}"; echo "[${v:9}]" "[${v: -10}]""#);
    assert_eq!(out, "cdef cde def cd bcd\nde\n[] []\n");

    // Case conversion, with and without a pattern restriction.
    let (out, _) = rush(r#"v=hello; echo "${v^}" "${v^^}" "${v^^[a-f]}"; V=HELLO; echo "${V,}" "${V,,}""#);
    assert_eq!(out, "Hello HELLO hEllo\nhELLO hello\n");

    // Glob patterns in search/replace; escaped `/` as the pattern.
    let (out, _) = rush(r#"v=aXbXc; echo "${v/X*/Z}" "${v//X/-}" "${v/[Xb]/_}"; p=a/b/c; echo "${p/\//_}" "${p//\//.}""#);
    assert_eq!(out, "aZ a-b-c a_bXc\na_b/c a.b.c\n");

    // The `:-` default family is untouched by the substring parse.
    let (out, _) = rush(r#"v=abc; echo "${v:-default}"; u=; echo "${u:-empty-default}""#);
    assert_eq!(out, "abc\nempty-default\n");

    // A negative length landing before the offset errors like bash.
    let (err, status) = rush_stderr(r#"v=abc; echo "${v:1:-5}""#);
    assert!(err.contains("substring expression < 0"), "got: {err:?}");
    assert_eq!(status, 1);
}

#[cfg(unix)]
#[test]
fn array_wide_transformations_and_slicing() {
    // C59's array-wide forms: per-element for /, ^^, #/% — but `:` is
    // array *slicing* (elements, not characters) — verified against bash.
    let (out, _) = rush(r#"arr=(one two); echo "${arr[@]/o/0}"; echo "${arr[@]^^}"; echo "${arr[@]#o}""#);
    assert_eq!(out, "0ne tw0\nONE TWO\nne two\n");

    let (out, _) = rush(r#"arr=(a b c d); echo "${arr[@]:1:2}"; echo "${arr[@]: -1}"; echo "${arr[@]:2}""#);
    assert_eq!(out, "b c\nd\nc d\n");
}

#[cfg(unix)]
#[test]
fn indirect_expansion_and_name_listing() {
    // C60: `${!var}` double-dereference (a name or a positional number),
    // composing with trailing operators; `${!prefix@}` name listing.
    let (out, _) = rush(r#"x=hello; v=x; echo "${!v}"; set -- one two; n=2; echo "${!n}"; echo "${!v:-def}""#);
    assert_eq!(out, "hello\ntwo\nhello\n");

    let (out, _) = rush(r#"FOO_A=1; FOO_B=2; echo "${!FOO_@}""#);
    assert_eq!(out, "FOO_A FOO_B\n");

    // A referent that names an unset variable is empty; an *empty*
    // referent is a hard error (both verified against bash).
    let (out, _) = rush(r#"u=nosuchvar_c60; echo "[${!u}]""#);
    assert_eq!(out, "[]\n");
    let (_, status) = rush(r#"w=; echo "${!w}""#);
    assert_eq!(status, 1);
}

#[cfg(unix)]
#[test]
fn at_transformations() {
    // C60: ${v@Q}/@E/@a/@A. Formats verified against bash (@A's array
    // form uses the modern element-list format, documented).
    let (out, _) = rush(r#"v="it's a \"test\""; echo "${v@Q}"; w=$(printf "a\nb"); echo "${w@Q}""#);
    assert_eq!(out, "'it'\\''s a \"test\"'\n$'a\\nb'\n");

    let (out, _) = rush(r#"v="a\tb"; e="${v@E}"; printf "%s|\n" "$e""#);
    assert_eq!(out, "a\tb|\n");

    let (out, _) = rush(r#"declare -ir n=5; echo "${n@a}"; arr=(a b); echo "${arr@a}"; declare -A m; echo "${m@a}"; x=plain; echo "[${x@a}]""#);
    assert_eq!(out, "ir\na\nA\n[]\n");

    let (out, _) = rush(r#"x="hi there"; echo "${x@A}"; declare -r r=5; echo "${r@A}""#);
    assert_eq!(out, "x='hi there'\ndeclare -r r='5'\n");
}

#[cfg(unix)]
#[test]
fn mapfile_reads_lines_into_an_array() {
    // C61: mapfile/readarray were command-not-found. All verified
    // against bash.
    let f = std::env::temp_dir().join(format!("rush_c61_{}", std::process::id()));
    std::fs::write(&f, "l1\nl2\nl3\n").unwrap();
    let path = f.display();

    let (out, _) = rush(&format!("mapfile -t lines < {path}; echo \"${{lines[@]}} n=${{#lines[@]}}\"; echo \"${{lines[1]}}\""));
    assert_eq!(out, "l1 l2 l3 n=3\nl2\n");

    // Without -t each element keeps its newline; readarray is a synonym;
    // no name → MAPFILE; empty input → empty array; a trailing
    // unterminated line still becomes an element.
    let (out, _) = rush(&format!("mapfile raw < {path}; printf '[%s]' \"${{raw[0]}}\"; echo"));
    assert_eq!(out, "[l1\n]\n");
    let (out, _) = rush(&format!("readarray -t y < {path}; echo \"${{y[2]}}\""));
    assert_eq!(out, "l3\n");
    let (out, _) = rush(&format!("mapfile < {path}; echo \"${{#MAPFILE[@]}}\""));
    assert_eq!(out, "3\n");
    let (out, _) = rush("mapfile -t x </dev/null; echo n=${#x[@]}");
    assert_eq!(out, "n=0\n");
    std::fs::write(&f, "a\nb").unwrap();
    let (out, _) = rush(&format!("mapfile -t x < {path}; echo \"${{x[1]}} n=${{#x[@]}}\""));
    assert_eq!(out, "b n=2\n");

    let _ = std::fs::remove_file(&f);
}

#[cfg(unix)]
#[test]
fn nameref_variables() {
    // C62: `declare -n ref=x` used to assign the literal string "x".
    // Reads and writes now both follow the reference — all verified
    // against bash.
    let (out, _) = rush(r#"x=orig; declare -n ref=x; echo "$ref"; ref=changed; echo "$x""#);
    assert_eq!(out, "orig\nchanged\n");

    // Arrays through a ref, both directions.
    let (out, _) = rush(r#"declare -n ref=arr; arr=(a b); echo "${ref[1]}"; ref[0]=Z; echo "${arr[0]}""#);
    assert_eq!(out, "b\nZ\n");

    // The headline use: a function returning through a caller-named
    // variable (scalar and array), with the local ref frame-scoped.
    let (out, _) = rush(r#"f(){ local -n out=$1; out="from-f"; }; f result; echo "$result""#);
    assert_eq!(out, "from-f\n");
    let (out, _) = rush(r#"f(){ local -n out=$1; out=(a b c); }; f myarr; echo "${myarr[1]} ${#myarr[@]}""#);
    assert_eq!(out, "b 3\n");

    // `unset ref` unsets the target; the ref keeps referring. A bare
    // `declare -n ref` lets the next assignment pick the target.
    let (out, _) = rush(r#"x=1; declare -n r=x; unset r; echo "[${x-gone}]"; r=again; echo "$x""#);
    assert_eq!(out, "[gone]\nagain\n");
    let (out, _) = rush(r#"declare -n ref; ref=x; x=5; echo "$ref""#);
    assert_eq!(out, "5\n");

    // A circular reference stops following instead of hanging.
    let (out, _) = rush(r#"declare -n a=b; declare -n b=a; echo "[$a]"; echo alive"#);
    assert_eq!(out, "[]\nalive\n");
}

#[cfg(unix)]
#[test]
fn printf_percent_q() {
    // C63: %q was "invalid conversion specification". Output verified
    // byte-identical to bash for every case.
    let (out, _) = rush(r#"printf '%q\n' "it's" "a b" "" "plain" "a\$b" "semi;colon""#);
    assert_eq!(out, "it\\'s\na\\ b\n''\nplain\na\\$b\nsemi\\;colon\n");

    // Control characters force the $'...' form; round-trips through eval.
    let (out, _) = rush(r#"v=$(printf "x\ny"); q=$(printf '%q' "$v"); eval "w=$q"; [ "$w" = "$v" ] && echo roundtrip"#);
    assert_eq!(out, "roundtrip\n");
}

#[cfg(unix)]
#[test]
fn job_control_niceties() {
    // C64: jobs -l/-p, kill -l + the fuller signal table, wait -n, disown.
    let (out, _) = rush("kill -l TERM; kill -l 15; kill -l 9; kill -l SIGUSR1");
    assert_eq!(out, "15\nTERM\nKILL\n10\n");
    let (out, _) = rush("kill -l");
    assert!(out.contains("15) SIGTERM") && out.contains("10) SIGUSR1"), "got: {out:?}");

    // jobs -p prints just the pgid; -l includes it in the long line.
    let (out, _) = rush("sleep 2 & p=$(jobs -p); [ \"$p\" -gt 0 ] && echo p-ok; l=$(jobs -l); case $l in \"[1]  $p Running\"*) echo l-ok;; esac; kill %1");
    assert_eq!(out, "p-ok\nl-ok\n");

    // wait -n returns the next-finished child's status; 127 with none.
    let (out, _) = rush("sh -c 'exit 7' & wait -n; echo st=$?; wait -n; echo none=$?");
    assert_eq!(out, "st=7\nnone=127\n");

    // A previously-unknown signal name now works end-to-end.
    let (out, _) = rush("trap 'echo usr1' USR1; kill -USR1 $$; sleep 0; echo after");
    assert_eq!(out, "usr1\nafter\n");

    // disown removes the job from the table; it keeps running.
    let (out, _) = rush("sleep 2 & pid=$!; disown; jobs; kill %1 2>/dev/null; echo kill-st=$?; kill $pid; echo done");
    assert_eq!(out, "kill-st=1\ndone\n");
}

#[cfg(unix)]
#[test]
fn trap_debug_return_and_introspection() {
    // C65: DEBUG fires before each pipeline with $? preserved across the
    // handler (bash-verified); RETURN fires on function return and when
    // a sourced script finishes; trap -l/-p used to be silent no-ops.
    let (out, _) = rush("trap 'echo D' DEBUG; false; echo st=$?");
    assert_eq!(out, "D\nD\nst=1\n");

    let (out, _) = rush("f(){ trap 'echo R' RETURN; echo in-f; }; f; echo after");
    assert_eq!(out, "in-f\nR\nafter\n");

    let f = std::env::temp_dir().join(format!("rush_c65_{}.sh", std::process::id()));
    std::fs::write(&f, "echo sourced\n").unwrap();
    let (out, _) = rush(&format!("trap 'echo R' RETURN; . {}; echo after", f.display()));
    assert_eq!(out, "sourced\nR\nafter\n");
    let _ = std::fs::remove_file(&f);

    // trap -l: bash's numbered five-per-line table; trap -p: re-runnable,
    // filterable.
    let (out, _) = rush("trap -l");
    assert!(out.starts_with(" 1) SIGHUP\t 2) SIGINT"), "got: {out:?}");
    let (out, _) = rush("trap 'echo x' TERM EXIT; trap -p TERM; trap - EXIT");
    assert_eq!(out, "trap -- 'echo x' SIGTERM\n");
}

#[cfg(unix)]
#[test]
fn coproc_bidirectional_pipes() {
    // C66: coproc didn't exist, and neither did the `<&"${arr[N]}"`
    // fd-from-a-word redirects it needs. All verified against bash.
    let (out, _) = rush(r#"coproc cat; echo hello >&"${COPROC[1]}"; read line <&"${COPROC[0]}"; echo "got: $line"; kill $COPROC_PID 2>/dev/null; echo done"#);
    assert_eq!(out, "got: hello\ndone\n");

    // The named form takes a `{ ... }` group; NAME[0]/NAME[1]/NAME_PID.
    let (out, _) = rush(r#"coproc up { cat; }; echo hi >&"${up[1]}"; read x <&"${up[0]}"; echo "got=$x"; kill $up_PID 2>/dev/null"#);
    assert_eq!(out, "got=hi\n");

    // $! is the coprocess pid; a killed coprocess waits as 143 (TERM),
    // same as bash.
    let (out, _) = rush(r#"coproc cat; [ "$COPROC_PID" = "$!" ] && [ "$COPROC_PID" -gt 0 ] && echo pids-ok; kill $COPROC_PID; wait $COPROC_PID 2>/dev/null; echo waited-st=$?"#);
    assert_eq!(out, "pids-ok\nwaited-st=143\n");
}

#[cfg(unix)]
#[test]
fn special_variables_grab_bag() {
    // C67: RANDOM/SECONDS/EPOCH*/FUNCNAME/BASH_SOURCE/LINENO all
    // expanded empty before.
    let (out, _) = rush(r#"a=$RANDOM; [ "$a" -ge 0 ] && [ "$a" -le 32767 ] && echo range-ok; RANDOM=42; a=$RANDOM; RANDOM=42; b=$RANDOM; [ "$a" = "$b" ] && echo seeded"#);
    assert_eq!(out, "range-ok\nseeded\n");

    let (out, _) = rush(r#"SECONDS=100; echo $SECONDS; [ "${EPOCHREALTIME%.*}" = "$EPOCHSECONDS" ] && echo epoch-ok"#);
    assert_eq!(out, "100\nepoch-ok\n");

    // FUNCNAME: the call stack, innermost first; unset outside functions.
    let (out, _) = rush(r#"f(){ g; }; g(){ echo "${FUNCNAME[@]}"; }; f; echo "[${FUNCNAME[0]}]""#);
    assert_eq!(out, "g f\n[]\n");

    // BASH_SOURCE and LINENO in a real script file (LINENO values
    // byte-identical to bash for the same file).
    let f = std::env::temp_dir().join(format!("rush_c67_{}.sh", std::process::id()));
    std::fs::write(&f, "echo \"src=${BASH_SOURCE[0]}\"\necho \"line=$LINENO\"\n\necho \"line2=$LINENO\"\n").unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_rush"))
        .arg(&f)
        .output()
        .expect("spawn rush");
    let out = String::from_utf8_lossy(&output.stdout);
    assert_eq!(out, format!("src={}\nline=2\n\nline2=4\n", f.display()).replace("\n\n", "\n"));
    let _ = std::fs::remove_file(&f);

    // Under -c, BASH_SOURCE is empty (same as bash).
    let (out, _) = rush(r#"echo "[${BASH_SOURCE[0]}]""#);
    assert_eq!(out, "[]\n");
}

#[cfg(unix)]
#[test]
fn abbr_builtin_manages_the_table() {
    // C70: the abbr/unabbr builtins (the live line expansion itself is
    // interactive-only, unit-tested in completion.rs).
    let (out, _) = rush("abbr gs='git status'; abbr; abbr gs; unabbr gs; abbr gs 2>/dev/null; echo st=$?");
    assert_eq!(out, "abbr gs='git status'\nabbr gs='git status'\nst=1\n");
}

#[cfg(unix)]
#[test]
fn cd_niceties() {
    // C72: pushd/popd/dirs (byte-identical to bash), cd -N, and $CDPATH.
    let (out, _) = rush("cd /tmp; pushd /usr > /dev/null; pushd /etc >/dev/null; dirs; popd >/dev/null; dirs; pushd >/dev/null; dirs");
    assert_eq!(out, "/etc /usr /tmp\n/usr /tmp\n/tmp /usr\n");

    // $CDPATH search prints the resulting directory (POSIX).
    let (out, _) = rush("CDPATH=/usr; cd share && pwd");
    assert_eq!(out, "/usr/share\n/usr/share\n");

    // cd -N jumps into the stack (1-based, dirs order).
    let (out, _) = rush("cd /tmp; pushd /usr >/dev/null; pushd /etc >/dev/null; cd -2; pwd");
    assert_eq!(out, "/tmp\n");

    // Empty-stack errors.
    let (err, status) = rush_stderr("popd");
    assert!(err.contains("directory stack empty"), "got: {err:?}");
    assert_eq!(status, 1);

    // Spelling correction is interactive-only: a script's typo still
    // fails (unit-tested directly for the interactive path).
    let (_, status) = rush("cd /nonexistent-c72-typo 2>/dev/null");
    assert_eq!(status, 1);
}

#[cfg(unix)]
#[test]
fn set_o_vi_and_emacs_toggle_the_edit_mode_option() {
    // C73: `set -o vi` was "invalid option name". The option now tracks
    // (and the interactive loop rebuilds its editor when it changes —
    // that half is interactive-only); listings include it.
    let (out, _) = rush("set -o vi; set -o");
    assert!(out.lines().any(|l| l == "vi             \ton"), "got: {out:?}");
    let (out, _) = rush("set -o vi; set -o emacs; set +o");
    assert!(out.lines().any(|l| l == "set +o vi"), "got: {out:?}");
}

// A fork-under-load stress test: each `$(...)` command substitution forks a
// child that keeps running the interpreter, and each iteration also drives a
// here-document (a memfd on Linux). Hundreds of iterations in one process is
// what would expose a raw-fork deadlock (a child inheriting a lock held by a
// helper thread) or a here-doc regression. Meaningful in both backends; under
// `--features rusty-libc` it exercises the raw `clone(SIGCHLD)` fork directly.
#[test]
fn fork_and_heredoc_under_load() {
    let src = r#"
        total=0
        for i in $(seq 1 300); do
            n=$(cat <<EOF
$i
EOF
)
            total=$((total + n))
        done
        echo "$total"
    "#;
    let (out, code) = rush(src);
    // 1 + 2 + … + 300 = 45150. A hang would trip the harness; a wrong sum or
    // a dropped here-doc would change this number.
    assert_eq!(out.trim(), "45150", "got: {out:?}");
    assert_eq!(code, 0);
}

#[cfg(unix)]
#[test]
fn negative_array_subscripts() {
    // C85: `${a[-1]}` read empty and `a[-1]=Q` was silently dropped.
    // Negative indices count back from the maximum assigned index plus
    // one, matching bash (including on sparse arrays).
    let (out, _) = rush(r#"a=(x y z); echo "${a[-1]}" "${a[-3]}"; a[-1]=Q; echo "${a[@]}"; unset "a[-1]"; echo "${a[@]}""#);
    assert_eq!(out, "z x\nx y Q\nx y\n");

    let (out, _) = rush(r#"a=(x); a[10]=far; echo "${a[-1]}""#);
    assert_eq!(out, "far\n");

    // Out of range stays "nothing there", same as an unset index.
    let (out, _) = rush(r#"a=(x y); echo "[${a[-5]}]""#);
    assert_eq!(out, "[]\n");
}

#[cfg(unix)]
#[test]
fn positional_parameter_slicing_and_count() {
    // C86: `${@:off:len}`, `${*:off}`, `${#*}`, and `${#@}` were all hard
    // "bad substitution" errors. Offset 0 starts at `$0`, offset 1 at
    // `$1`; a negative offset counts from the end — all verified against
    // bash.
    let (out, code) = rush_argv(r#"echo "${@:2:2}"; echo "${@:3}"; echo "${#*} ${#@}"; echo "${@: -1}"; echo "${@:0:2}""#, &["zero", "a", "b", "c", "d"]);
    assert_eq!(out, "b c\nc d\n4 4\nd\nzero a\n");
    assert_eq!(code, 0);

    // `${@:-x}` must still be the default operator, not a slice.
    let (out, _) = rush(r#"echo "${@:-fallback}""#);
    assert_eq!(out, "fallback\n");
}

#[cfg(unix)]
#[test]
fn tilde_user_plus_minus() {
    // C117: `~user`, `~+`, and `~-` passed through literally. `~root` is
    // the one account whose home is stable everywhere; unknown users stay
    // literal like bash.
    let (out, _) = rush("echo ~root; echo ~nosuchuser42/x");
    assert_eq!(out, "/root\n~nosuchuser42/x\n");

    let (out, _) = rush("cd /tmp && echo ~+; cd / && echo ~-");
    assert_eq!(out, "/tmp\n/tmp\n");

    // Fixed alongside: `$PWD` itself used to go stale after `cd`.
    let (out, _) = rush(r#"cd /tmp && echo "$PWD""#);
    assert_eq!(out, "/tmp\n");
}

#[cfg(unix)]
#[test]
fn at_transform_case_key_and_prompt_forms() {
    // C118: only @Q/@E/@a/@A existed; @U/@u/@L (bash 5.1 case
    // transforms), @K/@k (round-trippable key/value pairs), and the
    // per-element array forms were "bad substitution" errors.
    let (out, _) = rush(r#"v=abc; echo "${v@U}" "${v@u}" "${v@L}"; V="A B"; echo "${V@K}""#);
    assert_eq!(out, "ABC Abc abc\n'A B'\n");

    let (out, _) = rush(r#"a=(one two); echo "${a[@]@U}"; declare -A m=([x]=1); echo "${m[@]@k}"; echo "${m[@]@K}""#);
    assert_eq!(out, "ONE TWO\nx 1\nx \"1\"\n");

    // $"..." is plain "..." (no locale translation) — the `$` used to
    // leak into the output.
    let (out, _) = rush(r#"echo $"hello world""#);
    assert_eq!(out, "hello world\n");
}

#[cfg(unix)]
#[test]
fn at_k_uses_bashs_own_double_quoted_style_not_at_qs() {
    // C171: @K's array key/value form was wrongly reusing @Q's
    // single-quote formatter; bash's real @K is double-quoted with
    // backslash escaping of `\`/`` ` ``/`$`/`"`, falling back to the same
    // `$'...'` ANSI-C form as @Q only for control characters.
    let (out, _) = rush(r#"a=('a b' 'c$d' 'e"f' 'g\h' 'i`j'); echo "${a[@]@K}""#);
    assert_eq!(out, r#"0 "a b" 1 "c\$d" 2 "e\"f" 3 "g\\h" 4 "i\`j"
"#);

    let (out, _) = rush("a=($'tab\there' plain); echo \"${a[@]@K}\"");
    assert_eq!(out, "0 $'tab\\there' 1 \"plain\"\n");
}

#[cfg(unix)]
#[test]
fn ansi_c_numeric_escapes() {
    // C119: `\xHH`, octal `\nnn`, `\uXXXX`, and `\cX` in `$'...'` came out
    // as literal backslash text.
    let (out, _) = rush(r#"echo $'\x41\x42' $'\101\102' $'A'"#);
    assert_eq!(out, "AB AB A\n");

    let (out, _) = rush(r#"[ $'\cA' = $'\x01' ] && [ $'\cj' = $'\n' ] && echo ctrl-ok"#);
    assert_eq!(out, "ctrl-ok\n");

    // Multibyte \u, and an unknown escape staying literal.
    let (out, _) = rush(r#"echo $'é' $'\q'"#);
    assert_eq!(out, "é \\q\n");
}

#[cfg(unix)]
#[test]
fn function_keyword_definition_syntax() {
    // C113: `function name { …; }` was a parse error, and
    // `function name() { …; }` silently misparsed (ran the body eagerly).
    let (out, code) = rush("function f { echo ok1; }; f; function g() { echo ok2; }; g");
    assert_eq!(out, "ok1\nok2\n");
    assert_eq!(code, 0);

    // `function` in argument position is an ordinary word.
    let (out, _) = rush("echo function");
    assert_eq!(out, "function\n");
}

#[cfg(unix)]
#[test]
fn pipe_both_shorthand() {
    // C114: `|&` — stdout+stderr both piped — was `expected a command`.
    let (out, code) = rush("{ echo out; echo err 1>&2; } |& sort");
    assert_eq!(out, "err\nout\n");
    assert_eq!(code, 0);
}

#[cfg(unix)]
#[test]
fn bare_redirection_truncates() {
    // C87: `> file` with no command errored (`empty command`) and left
    // the file untouched; it must truncate/create with status 0.
    let dir = std::env::temp_dir().join(format!("rush_bare_redir_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("t");
    let (out, code) = rush(&format!(
        "echo data > {p}; > {p}; wc -c < {p}; >> {p}; > {d}/created; echo made=$?",
        p = f.display(),
        d = dir.display()
    ));
    assert_eq!(out, "0\nmade=0\n");
    assert_eq!(code, 0);
    assert!(dir.join("created").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn return_outside_function_warns_and_continues() {
    // C88: top-level `return 5` silently exited the whole script with
    // rc 5; bash warns, sets $? to 1, and keeps going.
    let (out, code) = rush("return 5; echo alive rc=$?");
    assert_eq!(out, "alive rc=1\n");
    assert_eq!(code, 0);

    // …but inside a sourced file it really returns, with its status.
    let dir = std::env::temp_dir().join(format!("rush_return_src_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("s"), "return 3\necho unreachable\n").unwrap();
    let (out, _) = rush(&format!(". {}/s; echo st=$?", dir.display()));
    assert_eq!(out, "st=3\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn backslash_newline_continuation() {
    // C78/C79: `\<newline>` must vanish during tokenization — unquoted
    // and inside double quotes — instead of leaking a newline into the
    // arguments (or, on stdin, running the fragments as two commands).
    let (out, _) = rush("echo one \\\ntwo");
    assert_eq!(out, "one two\n");
    let (out, _) = rush("echo a\\\nb");
    assert_eq!(out, "ab\n");
    let (out, _) = rush("echo \"x\\\ny\"");
    assert_eq!(out, "xy\n");

    // An escaped backtick inside double quotes sheds its backslash.
    let (out, _) = rush(r#"printf "%s\n" "\`""#);
    assert_eq!(out, "`\n");

    // Stdin mode: the continuation joins lines instead of running `b`.
    let (out, code) = rush_stdin_script("echo a\\\nb\n");
    assert_eq!(out, "ab\n");
    assert_eq!(code, 0);
}

/// Feed a whole script on stdin with no `-c` (the piped-script path).
fn rush_stdin_script(script: &str) -> (String, i32) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_rush"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn rush");
    write_stdin_concurrently(&mut child, script);
    let output = child.wait_with_output().expect("wait rush");
    (String::from_utf8_lossy(&output.stdout).into_owned(), output.status.code().unwrap_or(-1))
}

#[cfg(unix)]
#[test]
fn arithmetic_literals_and_string_reeval() {
    // C116: base#n / hex / octal literals, the empty expression, and
    // recursive string evaluation were all hard errors.
    let (out, _) = rush("echo $((2#101)) $((16#ff)) $((8#17)) $((36#z)) $((64#_@)) $((0xff)) $((010))");
    assert_eq!(out, "5 255 15 35 4094 255 8\n");

    let (out, _) = rush("echo $(()); e=; echo $(($e))");
    assert_eq!(out, "0\n0\n");

    // A variable's string value is itself an expression, recursively.
    let (out, _) = rush("x=1+2; y=x*2; echo $((x)) $((y))");
    assert_eq!(out, "3 6\n");

    // A reference cycle errors instead of overflowing the native stack.
    let (out, code) = rush("a=b; b=a; echo $((a))");
    assert_eq!(out, "");
    assert_eq!(code, 1);

    // Bad digit for the base errors like bash.
    let (_, code) = rush("echo $((2#19))");
    assert_eq!(code, 1);
}

#[cfg(unix)]
#[test]
fn let_builtin() {
    // C91: `let` was command-not-found, so scripts proceeded with empty
    // variables.
    let (out, _) = rush(r#"let x=3+4 y=x*2; echo $x $y; let i=5; let i++; echo $i; n=1; let "n = n + 41"; echo $n"#);
    assert_eq!(out, "7 14\n6\n42\n");

    // Status: 1 when the last expression is 0 — the (( )) rule.
    let (out, _) = rush("let 0; echo st=$?; let 1; echo st=$?; let; echo st=$?");
    assert_eq!(out, "st=1\nst=0\nst=1\n");
}

#[cfg(unix)]
#[test]
fn echo_escape_flags() {
    // C90: `-e`/`-E` and clustered flags printed as literal text.
    let (out, _) = rush(r#"echo -e "a\tb"; echo -en "x\ty"; echo; echo -E "a\tb"; echo -e "one\ctwo"; echo"#);
    assert_eq!(out, "a\tb\nx\ty\na\\tb\none\n");
    // A non-flag dash word still prints literally.
    let (out, _) = rush("echo -x; echo -- foo");
    assert_eq!(out, "-x\n-- foo\n");
}

#[cfg(unix)]
#[test]
fn builtin_builtin_and_unset_f() {
    // C92: `builtin` lets a wrapper function call what it shadows.
    let (out, _) = rush(r#"cd() { builtin cd "$@" && echo wrapped; }; cd /tmp; pwd"#);
    assert_eq!(out, "wrapped\n/tmp\n");
    let (_, code) = rush("builtin nosuchbuiltin");
    assert_eq!(code, 1);

    // C97: `unset -f` removes functions; plain `unset` falls back to the
    // function when no variable exists.
    let (out, _) = rush("f(){ :; }; unset -f f; type f 2>/dev/null; echo st=$?; g(){ :; }; unset g; type g 2>/dev/null; echo st=$?");
    assert_eq!(out, "st=1\nst=1\n");
}

#[cfg(unix)]
#[test]
fn test_v_o_and_string_comparison() {
    // C95: `-v`, `-o`, and string `<`/`>` were "too many arguments".
    let (out, _) = rush(r#"x=1; test -v x; echo $?; test -v nosuch; echo $?; a=(q); test -v "a[0]"; echo $?"#);
    assert_eq!(out, "0\n1\n0\n");
    let (out, _) = rush(r#"set -e; test -o errexit; echo $?; set +e; test -o bogus; echo $?"#);
    assert_eq!(out, "0\n1\n");
    let (out, _) = rush(r#"[ abc \< abd ]; echo $?; [ b \> a ]; echo $?"#);
    assert_eq!(out, "0\n0\n");
}

#[cfg(unix)]
#[test]
fn type_path_flags_and_hash_table() {
    // C100: `type -p`/`-P` and the real `hash` table.
    let (out, code) = rush("type -p ls; type -p cd; echo st=$?; type -P ls");
    assert_eq!(out, "/usr/bin/ls\nst=0\n/usr/bin/ls\n");
    assert_eq!(code, 0);

    let (out, _) = rush("hash ls; hash -t ls; hash -d ls; hash -t ls 2>/dev/null; echo st=$?");
    assert_eq!(out, "/usr/bin/ls\nst=1\n");

    // `hash -p` really redirects future spawns of that name.
    let (out, _) = rush("hash -p /bin/echo myecho; myecho works");
    assert_eq!(out, "works\n");
}

#[cfg(unix)]
#[test]
fn assorted_flag_batch() {
    // C101: kill -s / kill -l 128+n, trap --, exec -a/-c, cd -P/-L,
    // dirs -v, popd +N.
    let (out, code) = rush("kill -l 143; kill -l 15");
    assert_eq!(out, "TERM\nTERM\n");
    assert_eq!(code, 0);

    let (_, code) = rush("kill -s TERM $$; echo alive");
    assert_eq!(code, 143);

    let (out, _) = rush(r#"trap -- "echo T" EXIT"#);
    assert_eq!(out, "T\n");

    let (out, _) = rush(r#"exec -a customname sh -c 'echo $0'"#);
    assert_eq!(out, "customname\n");

    let (out, _) = rush("FOO=bar; export FOO; exec -c env");
    assert_eq!(out, "");

    let (out, _) = rush("cd -P /tmp && pwd; cd -L / && pwd");
    assert_eq!(out, "/tmp\n/\n");

    let (out, _) = rush("cd /; pushd /tmp >/dev/null; pushd /usr >/dev/null; dirs -v; popd +1 >/dev/null; dirs");
    assert_eq!(out, " 0  /usr\n 1  /tmp\n 2  /\n/usr /\n");
}

#[cfg(unix)]
#[test]
fn declare_p_and_function_introspection() {
    // C96: declare -p/-F/-f silently printed nothing with status 0.
    let (out, _) = rush(r#"x=5; declare -p x; declare -i n=3; export e=v; declare -p n e"#);
    assert_eq!(out, "declare -- x=\"5\"\ndeclare -i n=\"3\"\ndeclare -x e=\"v\"\n");

    let (out, _) = rush(r#"a=(x "y z"); declare -p a; declare -A m; m[k]=1; declare -p m"#);
    assert_eq!(out, "declare -a a=([0]=\"x\" [1]=\"y z\")\ndeclare -A m=([k]=\"1\" )\n");

    // The round-trip that motivates the format.
    let (out, _) = rush(r#"v=abc; eval "$(declare -p v)"; echo $v"#);
    assert_eq!(out, "abc\n");

    let (out, _) = rush("declare -p nosuch 2>/dev/null; echo st=$?");
    assert_eq!(out, "st=1\n");

    let (out, _) = rush("f(){ :; }; g(){ :; }; declare -F; declare -F f; echo st=$?; declare -F nosuch; echo st=$?; declare -f f >/dev/null; echo st=$?; declare -f nosuch >/dev/null; echo st=$?");
    assert_eq!(out, "declare -f f\ndeclare -f g\nf\nst=0\nst=1\nst=0\nst=1\n");
}

#[cfg(unix)]
#[test]
fn export_n_unexports() {
    // C98: `export -n` left the variable exported.
    let (out, _) = rush(r#"export FOO=bar; export -n FOO; sh -c 'echo "x${FOO}x"'; echo "still=$FOO""#);
    assert_eq!(out, "xx\nstill=bar\n");
}

#[cfg(unix)]
#[test]
fn printf_v_time_and_char_codes() {
    // C99: printf -v treated `-v` as the format; %(fmt)T and '"c errored.
    let (out, _) = rush(r#"printf -v x "%03d" 7; echo "$x"; printf -v "a[2]" hi; echo "${a[2]}""#);
    assert_eq!(out, "007\nhi\n");

    let (out, _) = rush(r#"printf "%(%Y)T\n" 0; printf "%(%F %T)T\n" 86399"#);
    assert_eq!(out, "1970\n1970-01-01 23:59:59\n");

    let (out, _) = rush(r#"printf "%d %d\n" "'A" '"B'"#);
    assert_eq!(out, "65 66\n");
}

#[cfg(unix)]
#[test]
fn exit_trap_reset_in_subshells() {
    // C80: an inherited EXIT trap fired once per subshell/command
    // substitution — double-running cleanup.
    let (out, _) = rush(r#"trap "echo bye" EXIT; (echo sub)"#);
    assert_eq!(out, "sub\nbye\n");
    let (out, _) = rush(r#"trap "echo bye" EXIT; x=$(echo sub); echo "$x""#);
    assert_eq!(out, "sub\nbye\n");

    // …while bash's documented display rule keeps the parent's traps
    // visible to `trap` until the subshell installs its own.
    let (out, _) = rush(r#"trap "echo T" TERM; echo "$(trap)""#);
    assert_eq!(out, "trap -- 'echo T' SIGTERM\n");
}

#[cfg(unix)]
#[test]
fn errexit_suppressed_inside_functions_under_conditions() {
    // C81: `set -e` fired inside a function even when the *call* sat in a
    // suppressed context (`f || handler`, `if f`, `f && x`, `! f`).
    let (out, code) = rush("set -e; f(){ false; echo in; }; f || echo caught; echo done");
    assert_eq!(out, "in\ndone\n");
    assert_eq!(code, 0);

    let (out, _) = rush("set -e; f(){ false; echo in; }; if f; then echo yes; fi; echo done");
    assert_eq!(out, "in\nyes\ndone\n");

    let (out, _) = rush("set -e; f(){ false; }; ! f; echo done");
    assert_eq!(out, "done\n");

    // …but a bare failing call still exits, as it must.
    let (out, code) = rush("set -e; f(){ false; }; f; echo unreachable");
    assert_eq!(out, "");
    assert_eq!(code, 1);
}

#[cfg(unix)]
#[test]
fn funcnest_and_recursion_cap() {
    // C83: runaway recursion crashed the whole process with a native
    // stack overflow (SIGABRT); FUNCNEST was ignored.
    let (out, code) = rush("FUNCNEST=3; f() { echo $1; f $(($1+1)); }; f 1");
    assert_eq!(out, "1\n2\n3\n");
    assert_eq!(code, 1);

    // Unbounded recursion hits the internal cap as a shell error — the
    // process must NOT die of SIGABRT (exit 134).
    let (_, code) = rush("f() { f; }; f");
    assert_eq!(code, 1);
}

#[cfg(unix)]
#[test]
fn builtins_and_functions_as_pipeline_stages() {
    // C82: a builtin/function pipeline stage was exec'd as an external
    // command and failed with "No such file or directory".
    let (out, code) = rush("echo hi | read x; echo done$?");
    assert_eq!(out, "done0\n");
    assert_eq!(code, 0);

    let (out, _) = rush("readonly RV=1; readonly -p | grep -c RV");
    assert_eq!(out, "1\n");

    let (out, _) = rush("alias aa=1; alias | cat");
    assert_eq!(out, "alias aa='1'\n");

    let (out, _) = rush("f(){ echo fn-out; }; f | tr a-z A-Z");
    assert_eq!(out, "FN-OUT\n");

    // The variable set by a piped `read` stays in the stage's subshell —
    // bash's default (no lastpipe) semantics.
    let (out, _) = rush(r#"x=old; echo new | read x; echo "$x""#);
    assert_eq!(out, "old\n");
}

#[cfg(unix)]
#[test]
fn exec_persistent_fd_redirections() {
    // C111: exec with fd > 3, dup, close, and move all failed with
    // "Bad file descriptor" — and the failure aborted the script.
    let dir = std::env::temp_dir().join(format!("rush_exec_fds_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let log = dir.join("log");

    let (out, _) = rush(&format!(
        "exec 5>>{p}; echo entry >&5; exec 5>&-; cat {p}",
        p = log.display()
    ));
    assert_eq!(out, "entry\n");

    let (out, _) = rush("exec 4>&1; echo via4 >&4; exec 4>&-; echo after");
    assert_eq!(out, "via4\nafter\n");

    // Move: dup then close the source.
    let (out, _) = rush("exec 3>&1; exec 1>&3-; echo moved");
    assert_eq!(out, "moved\n");

    // Closing an fd that was never open is fine; per-command close too.
    let (out, _) = rush("exec 9>&-; echo ok; ls /nosuchdir 2>&-; echo rc=$?");
    assert_eq!(out, "ok\nrc=2\n");

    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn time_reserved_word() {
    // C112: `time` was command-not-found. Timing goes to stderr; the
    // pipeline's own status is preserved.
    let (err, code) = rush_stderr("time true");
    assert!(err.contains("real\t0m") && err.contains("user\t0m") && err.contains("sys\t0m"), "got: {err:?}");
    assert_eq!(code, 0);

    let (err, code) = rush_stderr("time false");
    assert!(err.contains("real\t0m"), "got: {err:?}");
    assert_eq!(code, 1);

    // POSIX -p format and TIMEFORMAT.
    let (err, _) = rush_stderr("time -p true");
    assert!(err.starts_with("real 0.0"), "got: {err:?}");
    let (err, _) = rush_stderr(r#"TIMEFORMAT="X%R"; time true"#);
    assert!(err.starts_with("X0.0"), "got: {err:?}");

    // Not a reserved word outside command position.
    let (out, _) = rush("echo time");
    assert_eq!(out, "time\n");
}

#[cfg(unix)]
#[test]
fn read_flag_coverage() {
    // C89: every read option flag printed "invalid option" and left the
    // variable empty.
    let (out, _) = rush(r#"echo abcdef | { read -n 3 x; echo "[$x]"; }"#);
    assert_eq!(out, "[abc]\n");

    let (out, _) = rush(r#"printf "xy" | { read -N 2 v; echo "[$v]"; }"#);
    assert_eq!(out, "[xy]\n");

    let (out, _) = rush(r#"printf "1:2:3" | { IFS=: read -d : x; echo "[$x]"; }"#);
    assert_eq!(out, "[1]\n");

    let (out, _) = rush(r#"echo "a b c" | { read -a arr; echo "${arr[1]}-${#arr[@]}"; }"#);
    assert_eq!(out, "b-3\n");

    let (out, _) = rush(r#"echo viafd3 | { read -u 0 v; echo "$v"; }"#);
    assert_eq!(out, "viafd3\n");

    // -t on an fd that never produces input: status 142 (128+ALRM),
    // same as bash.
    let (out, _) = rush("read -t 0.2 x < /dev/ptmx 2>/dev/null; echo st=$?");
    assert_eq!(out, "st=142\n");

    // mapfile -d.
    let (out, _) = rush(r#"printf "a:b:c" | { mapfile -t -d : m; echo "${m[2]}-${#m[@]}"; }"#);
    assert_eq!(out, "c-3\n");
}

#[cfg(unix)]
#[test]
fn standard_variables_seeded() {
    // C106: UID/EUID/HOSTNAME/OSTYPE/HOSTTYPE/MACHTYPE were all unset,
    // UID was writable, and SHLVL never incremented.
    let (out, _) = rush("echo ${UID:+u} ${EUID:+e} ${OSTYPE:+o} ${HOSTTYPE:+t} ${MACHTYPE:+m} ${RUSH_VERSION:+v}");
    assert_eq!(out, "u e o t m v\n");

    let (out, code) = rush("UID=5; echo unreachable");
    assert_eq!(out, "");
    assert_eq!(code, 1);

    let output = Command::new(env!("CARGO_BIN_EXE_rush"))
        .arg("-c")
        .arg("echo $SHLVL")
        .env("SHLVL", "5")
        .output()
        .expect("spawn rush");
    assert_eq!(String::from_utf8_lossy(&output.stdout), "6\n");
}

#[test]
fn bash_version_compat_vars() {
    // BASH_VERSION/BASH_VERSINFO (compat shim): scripts that gate
    // bash-only syntax behind `[ -n "$BASH_VERSION" ]` should see rush as
    // "bash 5.2", the version its own capability-gap tracking verifies
    // parity against. BASH_VERSINFO is readonly; BASH_VERSION isn't.
    let (out, _) = rush(r#"echo "$BASH_VERSION"; echo "${BASH_VERSINFO[0]}.${BASH_VERSINFO[1]}.${BASH_VERSINFO[2]}""#);
    assert_eq!(out, "5.2.21(1)-release\n5.2.21\n");

    let (out, code) = rush("BASH_VERSINFO=(9 9 9); echo unreachable");
    assert_eq!(out, "");
    assert_eq!(code, 1);

    let (out, _) = rush("BASH_VERSION=fake; echo $BASH_VERSION");
    assert_eq!(out, "fake\n");
}

#[test]
fn dirstack_variable_tracks_pushd_popd_and_cd() {
    let dir = std::env::temp_dir().join(format!("rush_dirstack_{}", std::process::id()));
    std::fs::create_dir_all(dir.join("a")).unwrap();
    std::fs::create_dir_all(dir.join("b")).unwrap();
    let a = dir.join("a");
    let a_display = a.display();
    let b = dir.join("b");
    let b_display = b.display();

    // DIRSTACK[0] is always the current directory, even with no pushd yet.
    let (out, _) = rush(&format!(r#"cd "{a_display}"; echo "${{DIRSTACK[0]}}""#));
    assert_eq!(out, format!("{a_display}\n"));

    // pushd prepends the old cwd behind the new one; popd removes it again.
    // No `/dev/null` here (Windows has no such path — `pushd`/`popd`'s own
    // stack-listing output is tolerated instead, via unique markers).
    let (out, _) = rush(&format!(
        r#"cd "{a_display}"; pushd "{b_display}"; echo "STACK1=${{DIRSTACK[@]}}"; popd; echo "STACK2=${{DIRSTACK[@]}}""#
    ));
    assert!(out.contains(&format!("STACK1={b_display} {a_display}\n")), "got: {out:?}");
    assert!(out.contains(&format!("STACK2={a_display}\n")), "got: {out:?}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn bash_env_startup_file() {
    // C105: $BASH_ENV was never sourced in non-interactive mode.
    let dir = std::env::temp_dir().join(format!("rush_bashenv_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("e"), "echo from-env-file\n").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_rush"))
        .arg("-c")
        .arg("echo main")
        .env("BASH_ENV", dir.join("e"))
        .output()
        .expect("spawn rush");
    assert_eq!(String::from_utf8_lossy(&output.stdout), "from-env-file\nmain\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn set_allexport_noglob_and_accepted_flags() {
    // C107: set -a and set -f now real; -E/-b/-h/etc accepted.
    let (out, _) = rush("set -a; foo_ax=1; env | grep -c ^foo_ax=");
    assert_eq!(out, "1\n");
    let (out, _) = rush("set -f; echo *.nomatchxyz; set +f");
    assert_eq!(out, "*.nomatchxyz\n");
    let (out, code) = rush("set -euEo pipefail; echo ok");
    assert_eq!(out, "ok\n");
    assert_eq!(code, 0);
}

#[cfg(unix)]
#[test]
fn shopt_xpg_echo_and_nocasematch() {
    // C108: the shopt table knew only 5 glob options.
    let (out, _) = rush(r#"shopt -s xpg_echo; echo "a\tb""#);
    assert_eq!(out, "a\tb\n");
    let (out, _) = rush("shopt -s nocasematch; [[ ABC == abc ]] && echo yes; case FOO in foo) echo case-yes;; esac");
    assert_eq!(out, "yes\ncase-yes\n");
    // Formerly-unknown options are settable without a hard error.
    let (_, code) = rush("shopt -s inherit_errexit lastpipe histappend checkwinsize");
    assert_eq!(code, 0);
}

#[cfg(unix)]
#[test]
fn ps4_expansion_and_wait_jobs_flags() {
    // C109: $PS4 wasn't expanded for xtrace.
    let (err, _) = rush_stderr(r#"PS4="+${LINENO}: "; set -x; echo hi"#);
    assert!(err.contains(": echo hi"), "got: {err:?}");

    // C110: wait -f / wait -n -p var / jobs -r.
    let (out, _) = rush("sleep 0.1 & wait -f $!; echo st=$?");
    assert_eq!(out, "st=0\n");
    let (out, _) = rush(r#"sleep 0.1 & wait -n -p who; echo "st=$? pid-set=${who:+yes}""#);
    assert_eq!(out, "st=0 pid-set=yes\n");
    let (out, _) = rush("sleep 5 & jobs -r | grep -c sleep; kill %1");
    assert_eq!(out, "1\n");
}

#[cfg(unix)]
#[test]
fn ifs_splits_only_expansion_results() {
    // C74: literal command-line text was split on a custom IFS — severe,
    // silent corruption. Only expansion output may split.
    let (out, _) = rush("IFS=x; echo axb");
    assert_eq!(out, "axb\n");
    let (out, _) = rush(r#"IFS=-; set -- a b c; echo "$*"; echo $*"#);
    assert_eq!(out, "a-b-c\na b c\n");
    // …while expansion results still split, adjacent literals unsplit.
    let (out, _) = rush(r#"IFS=,; v="1,2"; printf "[%s]" a${v}b; echo"#);
    assert_eq!(out, "[a1][2b]\n");
}

#[cfg(unix)]
#[test]
fn prefix_assignments_scope_to_builtins() {
    // C75: `IFS=: read` neither applied nor restored the assignment.
    let (out, _) = rush(r#"IFS=: read -r x y <<< "1:2:3"; echo "$x|$y"; printf '[%s]' "$IFS""#);
    assert_eq!(out, "1|2:3\n[ \t\n]");
    // The builtin's own writes persist; the prefix var restores fully
    // (removed again if it didn't exist before).
    let (out, _) = rush(r#"FOO=1 true; echo "${FOO:-none}"; f(){ echo "in=$FOO"; }; FOO=baz f; echo "after=${FOO:-none}""#);
    assert_eq!(out, "none\nin=baz\nafter=none\n");
}

#[cfg(unix)]
#[test]
fn quotes_inside_default_expansion_words() {
    // C76: quotes inside ${v:-word} leaked into the output verbatim.
    let (out, _) = rush(r#"unset v; echo "${v:-"a b"}"; echo "${v:-"a  b"}""#);
    assert_eq!(out, "a b\na  b\n");
    // Inside a double-quoted ${}, single quotes are literal characters.
    let (out, _) = rush(r#"unset v; echo "${v:-'lit'}"; echo ${v:-'one two'}"#);
    assert_eq!(out, "'lit'\none two\n");
}

#[cfg(unix)]
#[test]
fn quoted_array_expansion_with_adjacent_text() {
    // C77: "x${a[@]}y" collapsed the whole array into one word.
    let (out, _) = rush(r#"a=(1 2); printf "[%s]" "x${a[@]}y"; echo"#);
    assert_eq!(out, "[x1][2y]\n");
    let (out, _) = rush(r#"set -- p q r; printf "[%s]" "x$@y"; echo; set --; printf "[%s]" "x$@y"; echo"#);
    assert_eq!(out, "[xp][q][ry]\n[xy]\n");
    let (out, _) = rush(r#"a=(only); printf "[%s]" "x${a[@]}y"; echo; a=(1 2); printf "[%s]" "L${!a[@]}R"; echo"#);
    assert_eq!(out, "[xonlyy]\n[L0][1R]\n");
}

/// Run a whole interactive-style session (stdin piped, no -c) with HOME
/// pointed at a scratch dir so history I/O is isolated.
#[cfg(unix)]
fn rush_session(home: &std::path::Path, script: &str) -> String {
    // `-i` forces the interactive REPL on a pipe (history, HISTTIMEFORMAT,
    // IGNOREEOF, PS0 are interactive-only) — see `rush_interactive`.
    let mut child = Command::new(env!("CARGO_BIN_EXE_rush"))
        .arg("-i")
        .env("HOME", home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn rush");
    child.stdin.take().unwrap().write_all(script.as_bytes()).expect("write stdin");
    let output = child.wait_with_output().expect("wait rush");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[cfg(unix)]
#[test]
fn history_builtin_and_hist_variables() {
    // C103/C122/C123/C124.
    let home = std::env::temp_dir().join(format!("rush_hist_{}", std::process::id()));
    std::fs::create_dir_all(&home).unwrap();

    // Numbered listing; incremental append persists without a clean-exit
    // save (and without duplicating entries).
    let out = rush_session(&home, "echo one\nhistory\n");
    assert!(out.contains("    1  echo one\n"), "got: {out:?}");
    let file = std::fs::read_to_string(home.join(".rush_history")).unwrap();
    assert_eq!(file, "echo one\nhistory\n");

    // cmdhist (C124): a multi-line compound recalls as one entry.
    std::fs::remove_file(home.join(".rush_history")).unwrap();
    let out = rush_session(&home, "for i in 1 2; do\necho $i\ndone\nhistory\n");
    assert!(out.contains("1  for i in 1 2; do; echo $i; done\n"), "got: {out:?}");

    // HISTCONTROL=ignorespace (C122): the leading-space privacy idiom.
    std::fs::remove_file(home.join(".rush_history")).unwrap();
    let out = rush_session(&home, "HISTCONTROL=ignorespace\n secret-cmd 2>/dev/null\nhistory\n");
    assert!(!out.contains("secret-cmd"), "got: {out:?}");

    // history -d renumbers; -c empties.
    std::fs::remove_file(home.join(".rush_history")).unwrap();
    let out = rush_session(&home, "echo a\necho b\nhistory -d 1\nhistory\n");
    assert!(out.contains("    1  echo b\n") && !out.contains("echo a\n    "), "got: {out:?}");

    let _ = std::fs::remove_dir_all(&home);
}

#[cfg(unix)]
#[test]
fn declare_f_prints_and_roundtrips_function_source() {
    // C96 remainder: `declare -f` used to print nothing. The unparser's
    // contract is round-trip fidelity through rush's own parser.
    let (out, code) = rush("f(){ echo one; }; declare -f f");
    assert_eq!(out, "f ()\n{\n    echo one\n}\n");
    assert_eq!(code, 0);

    let (out, _) = rush(r#"f(){ echo hi; [[ x == x ]] && echo cond-ok; for i in a b; do echo $i; done; }; src=$(declare -f f); unset -f f; eval "$src"; f"#);
    assert_eq!(out, "hi\ncond-ok\na\nb\n");

    // Here-docs and case bodies survive the round trip too.
    let (out, _) = rush("g(){ cat <<EOF\nhd\nEOF\ncase $1 in a|b) echo ab;; *) echo other;; esac; }; src=$(declare -f g); unset -f g; eval \"$src\"; g a");
    assert_eq!(out, "hd\nab\n");
}

#[cfg(unix)]
#[test]
fn export_f_crosses_into_children() {
    // C98 remainder: export -f used to warn-and-fail. The BASH_FUNC
    // encoding crosses into child rush AND child bash shells.
    let (out, _) = rush(&format!(
        "f(){{ echo exported-fn $1; }}; export -f f; {} -c 'f world'",
        env!("CARGO_BIN_EXE_rush")
    ));
    assert_eq!(out, "exported-fn world\n");

    let (out, _) = rush("f(){ echo cross-shell; }; export -f f; bash -c f");
    assert_eq!(out, "cross-shell\n");

    // And bash-exported functions are imported at rush startup.
    let output = Command::new("bash")
        .arg("-c")
        .arg(format!("f(){{ echo bash-to-rush; }}; export -f f; {} -c f", env!("CARGO_BIN_EXE_rush")))
        .output()
        .expect("spawn bash");
    assert_eq!(String::from_utf8_lossy(&output.stdout), "bash-to-rush\n");
}

#[cfg(unix)]
#[test]
fn small_wins_batch() {
    // patsub_replacement `&` (bash 5.2).
    let (out, _) = rush(r#"v=aXbXc; echo "${v/X*/[&]}"; echo "${v//X/<&>}"; echo "${v/X/\&}""#);
    assert_eq!(out, "a[XbXc]\na<X>b<X>c\na&bXc\n");

    // C84: assoc keys with spaces, literal and via a variable.
    let (out, _) = rush(r#"declare -A a; a["x y"]=1; echo "${a[x y]}"; k="p q"; a["$k"]=2; echo "${a[$k]}""#);
    assert_eq!(out, "1\n2\n");

    // SHELLOPTS/BASHOPTS reflect live option state.
    let (out, _) = rush(r#"echo "[$SHELLOPTS]"; set -e; echo $SHELLOPTS | grep -qc errexit && echo reflected"#);
    assert_eq!(out, "[braceexpand:hashall:interactive-comments]\nreflected\n");
    let (out, _) = rush("shopt -s dotglob; echo $BASHOPTS | grep -qc dotglob && echo bash-reflected");
    assert_eq!(out, "bash-reflected\n");

    // C94: times/help/caller/enable.
    let (out, _) = rush("times | wc -l");
    assert_eq!(out, "2\n");
    let (out, _) = rush("help nosuchxyz 2>/dev/null; echo st=$?; caller; echo st=$?; f(){ caller; }; f");
    assert_eq!(out, "st=1\nst=1\n1 NULL\n");
    let (out, _) = rush("enable -n echo; type -t echo; enable echo; type -t echo");
    assert_eq!(out, "file\nbuiltin\n");

    // GLOBIGNORE filters matches.
    let dir = std::env::temp_dir().join(format!("rush_gi_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for f in ["a.txt", "b.log", "c.txt"] {
        std::fs::write(dir.join(f), "").unwrap();
    }
    let (out, _) = rush(&format!("cd {}; GLOBIGNORE='*.log'; echo *", dir.display()));
    assert_eq!(out, "a.txt c.txt\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn ignoreeof_and_ps0() {
    // IGNOREEOF: piped EOF still exits after the guard count.
    let out = rush_session(std::path::Path::new("/tmp"), "IGNOREEOF=2\n");
    let _ = out; // the guard message goes to stderr; surviving to exit is the test

    // PS0 prints after a command is read, before it runs.
    let out = rush_session(std::path::Path::new("/tmp"), "PS0='[ps0]'\necho hi\n");
    assert!(out.contains("[ps0]hi"), "got: {out:?}");
}

#[cfg(unix)]
#[test]
fn varfd_named_descriptors() {
    // C115: `{name}>file` allocates a fresh fd (>= 10) and stores its
    // number; the fd persists past the command, bash's rule.
    let (out, _) = rush(r#"exec {x}>/dev/null; [ "$x" -ge 10 ] && echo alloc-ok; echo hi >&$x; exec {y}>&1; echo "via-y" >&$y"#);
    assert_eq!(out, "alloc-ok\nvia-y\n");

    let (out, _) = rush(r#"{fd}>/dev/null echo hi; [ -n "$fd" ] && echo fd-set"#);
    assert_eq!(out, "hi\nfd-set\n");

    // `{` everywhere else still lexes as word text.
    let (out, _) = rush("echo {notafd}; echo {x}y");
    assert_eq!(out, "{notafd}\n{x}y\n");
}

#[cfg(unix)]
#[test]
fn fc_builtin() {
    // C102: fc -l listing (ranges, reverse, -n), -s re-execution with
    // substitution, and the edit form (FCEDIT=true = run unchanged).
    let (out, _) = rush(r#"history -s "echo a"; history -s "echo b"; history -s "echo c"; fc -l 2 3; fc -lr 2 3; fc -ln 3 3; fc -l -2 -1"#);
    assert_eq!(out, "2\t echo b\n3\t echo c\n3\t echo c\n2\t echo b\n\t echo c\n2\t echo b\n3\t echo c\n");

    let (out, _) = rush(r#"history -s "echo hello"; fc -s hello=world"#);
    assert_eq!(out, "echo world\nworld\n");

    let (out, _) = rush(r#"history -s "echo aaa"; history -s "true"; fc -s echo"#);
    assert_eq!(out, "echo aaa\naaa\n");

    let (out, _) = rush(r#"history -s "echo edited-run"; FCEDIT=true fc"#);
    assert_eq!(out, "echo edited-run\nedited-run\n");
}

#[cfg(unix)]
#[test]
fn invocation_flags() {
    // C104: every flag used to be treated as a script filename.
    // -s: stdin with positional args (the `curl | sh -s -- args` shape).
    let mut child = Command::new(env!("CARGO_BIN_EXE_rush"))
        .args(["-s", "x", "y"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn rush");
    child.stdin.take().unwrap().write_all(b"echo \"1=$1 2=$2\"\n").unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "1=x 2=y\n");

    // -r: restricted shell — cd, PATH, slash commands, output redirects.
    let run = |args: &[&str]| {
        let o = Command::new(env!("CARGO_BIN_EXE_rush")).args(args).output().unwrap();
        (String::from_utf8_lossy(&o.stderr).into_owned(), o.status.code().unwrap_or(-1))
    };
    let (err, code) = run(&["-r", "-c", "cd /tmp"]);
    assert!(err.contains("cd: restricted"), "got: {err:?}");
    assert_eq!(code, 1);
    let (err, _) = run(&["-r", "-c", "/bin/ls"]);
    assert!(err.contains("restricted: cannot specify"), "got: {err:?}");
    let (err, _) = run(&["-r", "-c", "echo hi > /tmp/rush_r_test"]);
    assert!(err.contains("restricted: cannot redirect"), "got: {err:?}");

    // -O shopt-at-startup, and clustered -lc marking login_shell.
    let o = Command::new(env!("CARGO_BIN_EXE_rush")).args(["-O", "nullglob", "-c", "shopt nullglob"]).output().unwrap();
    assert!(String::from_utf8_lossy(&o.stdout).contains("on"));
    let o = Command::new(env!("CARGO_BIN_EXE_rush")).args(["-lc", "shopt login_shell"]).output().unwrap();
    assert!(String::from_utf8_lossy(&o.stdout).contains("on"));

    // -n lint mode still composes.
    let o = Command::new(env!("CARGO_BIN_EXE_rush")).args(["-n", "-c", "echo never"]).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&o.stdout), "");
    assert_eq!(o.status.code(), Some(0));
}

#[cfg(unix)]
#[test]
fn nocaseglob_and_shopt_behaviors() {
    // C120: case-insensitive filename globbing (distinct from nocasematch).
    let dir = std::env::temp_dir().join(format!("rush_ncg_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for f in ["Foo.TXT", "bar.txt"] {
        std::fs::write(dir.join(f), "").unwrap();
    }
    let (out, _) = rush(&format!("cd {}; shopt -s nocaseglob; echo *.txt", dir.display()));
    assert_eq!(out, "Foo.TXT bar.txt\n");
    let (out, _) = rush(&format!("cd {}; echo *.txt", dir.display()));
    assert_eq!(out, "bar.txt\n");
    let _ = std::fs::remove_dir_all(&dir);

    // C108 remainders: inherit_errexit and lastpipe.
    let (out, code) = rush("set -e; shopt -s inherit_errexit; x=$(false; echo hi); echo alive");
    assert_eq!(out, "");
    assert_eq!(code, 1);
    let (out, _) = rush("shopt -s lastpipe; echo new | read x; echo \"$x\"");
    assert_eq!(out, "new\n");
}

// `/dev/tcp` itself (`net_pseudo_device`) and the fd 3+ handed to `head`
// here both need Unix-only machinery: the pseudo-device wraps a raw fd
// (`IntoRawFd`), and passing fd 3+ into an external child goes through
// `pre_exec`/`dup2` — neither has a Windows equivalent (see
// docs/ARCHITECTURE.md's G11 analysis).
#[cfg(unix)]
#[test]
fn dev_tcp_pseudo_device() {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    // C121: /dev/tcp/host/port backed by std::net.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = std::thread::spawn(move || {
        let (mut sock, _) = listener.accept().unwrap();
        let mut buf = [0u8; 5];
        sock.read_exact(&mut buf).unwrap();
        sock.write_all(b"pong:").unwrap();
        sock.write_all(&buf).unwrap();
    });
    let (out, code) = rush(&format!(
        "exec 3<>/dev/tcp/127.0.0.1/{port}; echo ping >&3; head -c 9 <&3; echo"
    ));
    assert_eq!(out, "pong:ping\n");
    assert_eq!(code, 0);
    handle.join().unwrap();
}

#[cfg(unix)]
#[test]
fn programmable_completion() {
    // C93: complete/compgen/compopt and the COMPREPLY protocol.
    // compgen -W with $-expansion and prefix filtering.
    let (out, _) = rush(r#"x=dynamic; compgen -W "static $x demo" d"#);
    assert_eq!(out, "dynamic\ndemo\n");

    // Action letters: -A function, -b builtin.
    let (out, _) = rush("f1(){ :; }; f2(){ :; }; compgen -A function f");
    assert_eq!(out, "f1\nf2\n");
    let (out, _) = rush("compgen -b echo");
    assert_eq!(out, "echo\n");

    // -P/-S decorate every candidate.
    let (out, _) = rush(r#"compgen -P "pre-" -S "-post" -W "a b" a"#);
    assert_eq!(out, "pre-a-post\n");

    // -F function protocol: the function fills COMPREPLY, compgen -F
    // reads it back.
    let (out, _) = rush("_c() { COMPREPLY=(alpha beta); }; compgen -F _c ''");
    assert_eq!(out, "alpha\nbeta\n");

    // A realistic COMP_WORDS/COMP_CWORD-driven completion.
    let (out, _) = rush(
        r#"_svc() { local cur="${COMP_WORDS[COMP_CWORD]}"; COMPREPLY=($(compgen -W "start stop restart" "$cur")); }; COMP_WORDS=(svc st); COMP_CWORD=1; _svc; printf "%s\n" "${COMPREPLY[@]}""#,
    );
    assert_eq!(out, "start\nstop\n");

    // complete registers, lists, and -r removes.
    let (out, _) = rush("complete -W x foo; complete | grep -c foo; complete -r foo; complete | grep -c foo");
    assert_eq!(out, "1\n0\n");

    // compgen with no match is status 1.
    let (_, code) = rush(r#"compgen -W "a b c" zzz"#);
    assert_eq!(code, 1);
}

#[cfg(unix)]
#[test]
fn nocasematch_regex_matching() {
    // C120 remainder: `=~` folds under nocasematch via the engine's
    // REG_ICASE mode. Verified against bash 5.2, including the corners.
    let (out, _) = rush("shopt -s nocasematch; [[ ABC =~ ^abc$ ]] && echo yes || echo no");
    assert_eq!(out, "yes\n");

    // $BASH_REMATCH keeps the ORIGINAL case (folding is comparison-only).
    let (out, _) = rush(r#"shopt -s nocasematch; [[ ABC =~ ^(a)(b) ]]; echo "${BASH_REMATCH[1]}${BASH_REMATCH[2]}""#);
    assert_eq!(out, "AB\n");

    // Named classes keep their literal meaning; ranges fold (REG_ICASE).
    let (out, _) = rush("shopt -s nocasematch; [[ ABC =~ [[:lower:]]bc ]] && echo yes || echo no");
    assert_eq!(out, "yes\n");
    // Ranges fold both cases (REG_ICASE): `xbc` matches `[X-Z]bc`, but
    // `abc` does not (`a` is in neither `X-Z` nor `x-z`) — matches bash.
    let (out, _) = rush("shopt -s nocasematch; [[ Xbc =~ [X-Z]bc ]] && echo yes || echo no");
    assert_eq!(out, "yes\n");
    let (out, _) = rush("shopt -s nocasematch; [[ abc =~ [X-Z]bc ]] && echo yes || echo no");
    assert_eq!(out, "no\n");

    // Folding is opt-in.
    let (out, _) = rush("[[ ABC =~ ^abc$ ]] && echo yes || echo no");
    assert_eq!(out, "no\n");
}

#[cfg(unix)]
#[test]
fn histtimeformat_and_native_read_s() {
    // C122 remainder: HISTTIMEFORMAT prefixes each entry with its
    // rendered timestamp, and the file carries `#<epoch>` lines.
    let home = std::env::temp_dir().join(format!("rush_htf_{}", std::process::id()));
    std::fs::create_dir_all(&home).unwrap();
    let out = rush_session(&home, "HISTTIMEFORMAT=\"T \"\necho one\nhistory\n");
    // Every listed line is prefixed with the format's literal "T ".
    assert!(out.contains("  T echo one\n"), "got: {out:?}");
    let file = std::fs::read_to_string(home.join(".rush_history")).unwrap();
    assert!(file.lines().any(|l| l.starts_with('#') && l[1..].chars().all(|c| c.is_ascii_digit())),
        "no #epoch line in: {file:?}");
    let _ = std::fs::remove_dir_all(&home);

    // C89: read -s still reads on a pipe (echo suppression is a no-op
    // off a tty); no more stty shell-out.
    let (out, _) = rush_stdin("read -s x; echo \"[$x]\"", "hidden\n");
    assert_eq!(out, "[hidden]\n");
}

#[cfg(unix)]
#[test]
fn bind_builtin_registers_without_error() {
    // C128: the bind builtin parses and queues without an editor
    // present (the REPL applies the queue); unknown functions error.
    let (out, code) = rush(r#"bind "\C-t: forward-word"; bind -x "\C-g: echo hi"; bind "set completion-ignore-case on"; echo ok"#);
    assert_eq!(out, "ok\n");
    assert_eq!(code, 0);

    let (_, code) = rush(r#"bind "\C-t: no-such-readline-function""#);
    assert_eq!(code, 1);
}

#[cfg(unix)]
#[test]
fn printf_floating_point_and_star() {
    // Untracked gap: %f/%e/%g float conversions and %*/%.* args.
    let (out, _) = rush(r#"printf "%f|%.2f|%8.3f|%-8.2f|\n" 3.14159 3.14159 2.5 1.5"#);
    assert_eq!(out, "3.141590|3.14|   2.500|1.50    |\n");
    let (out, _) = rush(r#"printf "%e|%E|%g|%G\n" 12345.678 0.00012 0.0001 1234567"#);
    assert_eq!(out, "1.234568e+04|1.200000E-04|0.0001|1.23457E+06\n");
    // Hex-int argument to a float conversion, and precision rounding.
    let (out, _) = rush(r#"printf "%f %.0f\n" 0x10 2.7"#);
    assert_eq!(out, "16.000000 3\n");
    // Sign flags on floats; * width/precision from args (negative = left).
    let (out, _) = rush(r#"printf "%+.2f|% .2f|%*d|%-*d|%.*f\n" 3.1 4.2 5 42 5 42 3 3.14159"#);
    assert_eq!(out, "+3.10| 4.20|   42|42   |3.142\n");
}

#[cfg(unix)]
#[test]
fn array_element_with_operator() {
    // Untracked gap: `${arr[i]OP}` (default/pattern/transform on one
    // element) used to be "bad substitution".
    let (out, _) = rush(r#"a=(x y); echo "${a[0]:-none}|${a[5]:-def}|${a[0]#x}|${a[1]/y/Y}|${a[0]^^}""#);
    assert_eq!(out, "x|def||Y|X\n");
    let (out, _) = rush(r#"declare -A m=([k]=v); echo "${m[k]:-none}|${m[x]:-def}|${m[k]@Q}""#);
    assert_eq!(out, "v|def|'v'\n");
    let (out, _) = rush(r#"a=(abc); echo "${a[0]%c}|${a[0]:1}""#);
    assert_eq!(out, "ab|bc\n");
}

#[cfg(unix)]
#[test]
fn deprecated_bracket_arithmetic() {
    // Untracked gap: `$[ expr ]`, bash's deprecated arithmetic expansion.
    let (out, _) = rush("echo $[ 3 + 4 ] $[2*5]; x=10; echo $[x/2]; echo \"$[ 2**8 ]\"");
    assert_eq!(out, "7 10\n5\n256\n");
}

#[cfg(unix)]
#[test]
fn arithmetic_overflow_wraps_not_panics() {
    // C132 (severe): overflow used to panic and crash the shell (rc 101).
    // Now wraps 2's-complement like bash/C.
    let (out, code) = rush("echo $((9223372036854775807+1)) $((9223372036854775807*2)) $((1<<64))");
    assert_eq!(out, "-9223372036854775808 -2 1\n");
    assert_eq!(code, 0);

    // Division/modulo by zero: bash's message wording, still an error.
    let (err, code) = rush_stderr("echo $((5%0))");
    assert!(err.contains("5%0: division by 0"), "got: {err:?}");
    assert_eq!(code, 1);
}

#[cfg(unix)]
#[test]
fn arithmetic_comma_operator() {
    // C132: the comma operator was unsupported everywhere ($(( )), let,
    // for-headers).
    let (out, _) = rush("echo $((3,4,5)); echo $((x=5, x+=3, x)); let \"a=1, b=2\"; echo $a $b");
    assert_eq!(out, "5\n8\n1 2\n");
    let (out, _) = rush("for ((i=0,j=10; i<3; i++,j--)); do echo \"$i $j\"; done");
    assert_eq!(out, "0 10\n1 9\n2 8\n");
}

#[cfg(unix)]
#[test]
fn bashpid_and_dollar_stability() {
    // C132: $BASHPID was unset; $$ wrongly changed inside a subshell.
    // $$ stays the parent's pid, $BASHPID is the live (forked) pid.
    let (out, _) = rush(r#"echo "$(( BASHPID > 0 ))"; ( echo "$(( $$ == PARENT ))" ) 2>/dev/null; outer=$$; ( [ "$$" = "$outer" ] && echo same; [ "$BASHPID" != "$outer" ] && echo differs )"#);
    assert!(out.contains("same\n") && out.contains("differs\n"), "got: {out:?}");
}

#[test]
fn bashpid_matches_shell_pid_at_top_level() {
    // The cross-platform slice of C132: at top level (no subshell fork
    // involved), `$BASHPID` is the shell's own pid — exactly `$$`. Off
    // Unix this pins the `std::process::id()` fallback arm.
    let (out, _) = rush(r#"[ "$BASHPID" = "$$" ] && echo match"#);
    assert_eq!(out, "match\n");
}

#[test]
fn recursive_function_with_command_substitution() {
    // Windows parity (G11): function dispatch used to fail off Unix with
    // "program not found". A recursive body exercises function dispatch,
    // arithmetic, and `$(...)` of a shell function (which off Unix runs
    // via the self-re-exec fallback) all at once.
    let (out, status) = rush(
        "fact() { if [ $1 -le 1 ]; then echo 1; else echo $(( $1 * $(fact $(($1-1))) )); fi; }; fact 5",
    );
    assert_eq!((out.as_str(), status), ("120\n", 0));
}

#[cfg(unix)]
#[test]
fn declare_redeclare_preserves_and_setu_arrays() {
    // C132: bare re-declare wiped an existing array; set -u ignored array
    // element access.
    let (out, _) = rush("declare -a x=(a b); declare -a x; echo \"[${x[@]}]\"; declare -A m=([k]=v); declare -A m; echo \"[${m[k]}]\"");
    assert_eq!(out, "[a b]\n[v]\n");

    let (_, code) = rush("set -u; a=(x y); echo \"${a[9]}\"; echo reached");
    assert_eq!(code, 1);
    let (out, _) = rush("set -u; declare -A m=([k]=v); echo \"${m[k]}\"; echo ok");
    assert_eq!(out, "v\nok\n");
}

#[cfg(unix)]
#[test]
fn test_builtin_file_operators_and_grouping() {
    // C132: the test/[ builtin was missing the file-type/comparison
    // operators that already worked in [[ ]], plus \( \) grouping.
    let dir = std::env::temp_dir().join(format!("rush_testops_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (f1, f2) = (dir.join("f1"), dir.join("f2"));
    std::fs::write(&f1, "").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::write(&f2, "").unwrap();
    std::os::unix::fs::symlink(&f1, dir.join("link")).unwrap();

    let (out, _) = rush(&format!(
        "[ -h {d}/link ]; echo $?; [ -L {d}/link ]; echo $?; test {f2} -nt {f1}; echo $?; test {f1} -ot {f2}; echo $?; test {f1} -ef {f1}; echo $?; [ -O {f1} ]; echo $?",
        d = dir.display(), f1 = f1.display(), f2 = f2.display()
    ));
    assert_eq!(out, "0\n0\n0\n0\n0\n0\n");

    // -t 1 is not a tty under the test harness; parenthesized grouping.
    let (out, _) = rush(r#"[ -t 1 ]; echo notty=$?; [ \( a = a \) ]; echo $?; [ \( -n x \) -a \( -z "" \) ]; echo $?"#);
    assert_eq!(out, "notty=1\n0\n0\n");

    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn kill_zero_checks_existence() {
    // C132: kill -0 always succeeded; it must probe liveness.
    let (out, _) = rush("kill -0 $$; echo self=$?; kill -0 999999 2>/dev/null; echo dead=$?");
    assert_eq!(out, "self=0\ndead=1\n");
}

#[cfg(unix)]
#[test]
fn expansion_transforms_and_export_array() {
    // C132: ${x,,} unquoted leaked a stray `$` (brace expansion wrongly
    // firing on ${...}).
    let (out, _) = rush("x=hello; echo ${x,,}; x=HELLO; echo ${x,}; a=(FOO BAR); echo ${a[@],,}");
    assert_eq!(out, "hello\nhELLO\nfoo bar\n");

    // ${@@Q}/${*@Q}/${@@U} — @-transforms on the positional params.
    let (out, _) = rush(r#"set -- ab cd; echo "${@@Q}"; echo "${*@Q}"; echo "${@@U}""#);
    assert_eq!(out, "'ab' 'cd'\n'ab' 'cd'\nAB CD\n");

    // Whole-array default/alternate family.
    let (out, _) = rush(r#"a=(x y); echo "${a[@]:-DEF}"; b=(); echo "${b[@]:-DEF}"; echo "${a[@]:+SET}"; echo "${b[@]:+SET}""#);
    assert_eq!(out, "x y\nDEF\nSET\n\n");

    // export NAME=(...) creates the array.
    let (out, _) = rush(r#"export a=(1 2 3); echo "${a[1]}"; export b=(x "y z"); echo "${b[@]}""#);
    assert_eq!(out, "2\nx y z\n");
}

#[cfg(unix)]
#[test]
fn printf_hex_unicode_and_b_terminator() {
    // C135: `\xHH` hex escapes in both the format string and a `%b`
    // argument, `\uHHHH` Unicode, `\e` escape, and `%b`'s `\c` which stops
    // the whole printf. Each verified against bash.
    assert_eq!(rush(r#"printf 'A\x42C\n'"#).0, "ABC\n");
    assert_eq!(rush(r#"printf '%b\n' 'A\x42C'"#).0, "ABC\n");
    assert_eq!(rush(r#"printf '%b' 'foo\cbar'; echo END"#).0, "fooEND\n");
    // `\c` is not special in the format string itself.
    assert_eq!(rush(r#"printf 'foo\cbar'; echo END"#).0, "foo\\cbarEND\n");
    // `%b` octal accepts an optional leading `0` prefix (`\0NNN`).
    assert_eq!(rush(r#"printf '%b\n' '\101\0101'"#).0, "AA\n");
    // A syntactically valid but oversized integer is a warning (status 0),
    // not an error (status 1); bash clamps to i64::MAX.
    let (out, st) = rush("printf '%d\\n' 99999999999999999999");
    assert_eq!(out, "9223372036854775807\n");
    assert_eq!(st, 0);
    // A non-numeric argument is a hard error (status 1).
    assert_eq!(rush("printf '%d\\n' abc").1, 1);
    // `%'d` thousands flag: accepted, no-op in the C locale.
    assert_eq!(rush("printf \"%'d\\n\" 1234567").0, "1234567\n");
}

#[cfg(unix)]
#[test]
fn mapfile_skip_count_origin_callback() {
    // C135: mapfile's -s (skip), -n (count), -O (origin, preserving the
    // rest of the array), and -c/-C (callback every quantum lines with the
    // target index and line appended). All verified against bash.
    assert_eq!(rush_stdin(r#"mapfile -t -s 1 arr; echo "${arr[@]}""#, "a\nb\nc\nd\n").0, "b c d\n");
    assert_eq!(rush_stdin(r#"mapfile -t -n 2 arr; echo "${arr[@]}""#, "a\nb\nc\nd\n").0, "a b\n");
    assert_eq!(
        rush_stdin(r#"arr=(x y z w q r); mapfile -t -O 2 arr; echo "${arr[@]}""#, "a\nb\n").0,
        "x y a b q r\n"
    );
    assert_eq!(rush_stdin(r#"mapfile -t -c 1 -C "echo cb" arr"#, "a\nb\n").0, "cb 0 a\ncb 1 b\n");
    assert_eq!(rush_stdin(r#"mapfile -c 2 -C "echo CB" -t arr"#, "a\nb\nc\nd\ne\n").0, "CB 1 b\nCB 3 d\n");
}

#[cfg(unix)]
#[test]
fn parameter_error_carries_name_prefix() {
    // C135: `${x:?msg}` / `${x?}` errors are prefixed with the parameter
    // name, and the default text distinguishes `:?` ("null or not set")
    // from `?` ("not set"), matching bash. Custom messages keep the prefix.
    assert_eq!(rush("unset x; echo ${x:?is empty}").1, 1);
    assert!(rush_stderr("unset x; echo ${x:?is empty}").0.contains("x: is empty"));
    assert!(rush_stderr("unset foo; echo ${foo?}").0.contains("foo: parameter not set"));
    assert!(rush_stderr("foo=; echo ${foo:?}").0.contains("foo: parameter null or not set"));
    assert!(rush_stderr("a=(); echo ${a[0]:?boom}").0.contains("a[0]: boom"));
}

#[cfg(unix)]
#[test]
fn return_trap_inheritance_follows_functrace() {
    // C135: a RETURN trap set *inside* a function fires on that function's
    // return regardless of functrace; an enclosing (top-level) RETURN trap
    // is only inherited by functions under `set -T`. Verified against bash.
    assert_eq!(rush("f(){ trap 'echo R' RETURN; echo in-f; }; f; echo after").0, "in-f\nR\nafter\n");
    assert_eq!(rush("trap 'echo R' RETURN; f(){ echo in-f; }; f; echo after").0, "in-f\nafter\n");
    assert_eq!(rush("set -T; trap 'echo R' RETURN; f(){ echo in-f; }; f; echo after").0, "in-f\nR\nafter\n");
    // A callee does not inherit its caller's own RETURN trap without -T.
    assert_eq!(
        rush("g(){ echo in-g; }; f(){ trap 'echo R' RETURN; g; echo in-f; }; f; echo after").0,
        "in-g\nin-f\nR\nafter\n"
    );
    // trap -p prints DEBUG/ERR/RETURN/EXIT bare, without a SIG prefix.
    assert_eq!(rush("trap 'echo e' ERR; trap -p ERR").0, "trap -- 'echo e' ERR\n");
}

#[cfg(unix)]
#[test]
fn local_redeclare_preserves_same_frame_value() {
    // C135: a bare `local x` re-declaring a name already local in the same
    // frame preserves its value (bash); a first-time `local x` from an
    // outer scope starts empty. Verified against bash.
    assert_eq!(rush("f(){ local x=hi; local x; echo \"[$x]\"; }; f").0, "[hi]\n");
    assert_eq!(rush("x=outer; f(){ local x; echo \"[$x]\"; }; f").0, "[]\n");
    // A callee's own `local x` is fresh even when the caller made x local.
    assert_eq!(rush("f(){ local x=hi; g; }; g(){ local x; echo \"[$x]\"; }; f").0, "[]\n");
    // An array re-declared bare keeps its elements.
    assert_eq!(rush("f(){ local -a a=(1 2); local a; echo \"[${a[@]}]\"; }; f").0, "[1 2]\n");
}

#[cfg(unix)]
#[test]
fn declare_export_flag_and_clustered() {
    // C135: `declare -x` marks a name exported (it was previously dropped),
    // including clustered with other flags (`-rx`) and via `local -x`.
    // Verified against bash.
    assert_eq!(rush("declare -x q=1; echo \"[${q@a}]\"").0, "[x]\n");
    assert_eq!(rush("declare -rx q=1; echo \"[${q@a}]\"").0, "[rx]\n");
    assert_eq!(rush("declare -ix n=5; echo \"[${n@a}]\"").0, "[ix]\n");
    // The export actually reaches a child's environment.
    assert_eq!(rush("declare -x FOO=bar; printenv FOO").0, "bar\n");
    assert_eq!(rush("f(){ local -x LV=1; printenv LV; }; f").0, "1\n");
    // A clustered -r still enforces readonly: reassigning aborts with
    // status 1 (a readonly-assignment error is fatal in a non-interactive
    // shell — matching bash, which also produces no further output).
    let (out, code) = rush("declare -rx r=1; r=2 2>/dev/null; echo $r");
    assert_eq!((out.as_str(), code), ("", 1));
}

#[cfg(unix)]
#[test]
fn compgen_double_dash_ends_options() {
    // C135: `compgen … -- WORD` treats WORD as the completion prefix even
    // when it looks like an option. Verified against bash.
    assert_eq!(rush("compgen -W \"apple apricot banana\" -- ap").0, "apple\napricot\n");
    assert_eq!(rush("compgen -W \"one two three\" -- \"\"").0, "one\ntwo\nthree\n");
}

#[cfg(unix)]
#[test]
fn backtick_command_substitution() {
    // Legacy backtick substitution was entirely unimplemented (treated as
    // literal text). It now behaves like `$(...)`, unquoted and inside
    // double quotes. Each verified against bash.
    assert_eq!(rush("echo `echo bare`").0, "bare\n");
    assert_eq!(rush("echo \"`echo dq`\"").0, "dq\n");
    assert_eq!(rush("x=\"`echo assign`\"; echo $x").0, "assign\n");
    assert_eq!(rush("echo \"pre `echo mid` post\"").0, "pre mid post\n");
    assert_eq!(rush("echo \"`echo a` and `echo b`\"").0, "a and b\n");
    // Backticks compose with `$(...)` and with the word around them.
    assert_eq!(rush("echo \"nested `echo x$(echo y)`\"").0, "nested xy\n");
    // A backslash-escaped backtick inside double quotes stays literal.
    assert_eq!(rush("echo \"esc \\`literal\\`\"").0, "esc `literal`\n");
    // Word-splitting of an unquoted backtick result, and multi-command bodies.
    assert_eq!(rush("for i in `seq 1 3`; do echo -n $i; done; echo").0, "123\n");
    assert_eq!(rush("echo `echo a; echo b`").0, "a b\n");
    // Nested backticks via the `\\`` escape.
    assert_eq!(rush("n=`echo \\`echo deep\\``; echo $n").0, "deep\n");
}

#[cfg(unix)]
#[test]
fn read_poll_and_delimiter_trimming() {
    // `read -t 0` polls for readiness without consuming input: exit 0 when a
    // read would not block (data or EOF present), non-zero otherwise, and the
    // variables are left untouched. Verified against bash.
    assert_eq!(rush("read -t 0 </dev/null; echo $?").0, "0\n");
    assert_eq!(rush("echo data | { read -t 0; echo $?; }").0, "0\n");
    assert_eq!(rush("read -t 0 x </dev/null; echo \"[${x-unset}]\"").0, "[unset]\n");

    // `read -d ''` (NUL delimiter) still trims trailing $IFS whitespace from
    // the absorbed remainder, like any read — so a newline-terminated body
    // doesn't keep its trailing newline.
    assert_eq!(rush_stdin(r#"read -d "" x; echo "[$x]""#, "l1\nl2\n").0, "[l1\nl2]\n");
    assert_eq!(rush_stdin(r#"read -d "" x; echo "${#x}""#, "a\nb\n\n").0, "3\n");
    // The same trailing-whitespace trim applies to an ordinary multi-field read.
    assert_eq!(rush(r#"read a b <<< "1 2 3   "; echo "[$b]""#).0, "[2 3]\n");
}

#[cfg(target_os = "linux")]
#[test]
fn ulimit_real_time_row_present() {
    // With rusty_libc exposing RLIMIT_RTTIME, `ulimit -a` now leads with the
    // real-time (`-R`) row (bash's alphabetical order), and `ulimit -R`
    // reads it. Verified against bash.
    let (out, _) = rush("ulimit -a");
    assert!(out.lines().next().unwrap().contains("(microseconds, -R)"), "got: {out:?}");
    let (r, code) = rush("ulimit -R");
    assert_eq!(code, 0);
    assert!(r.trim() == "unlimited" || r.trim().parse::<u64>().is_ok(), "got: {r:?}");
}

#[cfg(target_os = "linux")]
#[test]
fn ulimit_pipe_size_pseudo_resource() {
    // `-p` (pipe size) is a bash pseudo-resource: a fixed value, read-only.
    // Verified against bash.
    assert_eq!(rush("ulimit -p").0, "8\n");
    assert_eq!(rush("ulimit -Sp").0, "8\n");
    assert_eq!(rush("ulimit -Hp").0, "8\n");
    // It appears in `-a` between open files (-n) and POSIX message queues (-q).
    let (out, _) = rush("ulimit -a");
    assert!(out.contains("pipe size                (512 bytes, -p) 8"), "got: {out:?}");
    // Attempting to set it is rejected (status 1), same as bash.
    let (_, code) = rush("ulimit -p 16");
    assert_eq!(code, 1);
}

#[cfg(unix)]
#[test]
fn arithmetic_strips_quotes() {
    // Quotes are stripped in an arithmetic context and their content lexed
    // normally — a number stays a number, a bareword resolves like a name.
    // Verified against bash.
    assert_eq!(rush(r#"echo $(( "10" * 2 ))"#).0, "20\n");
    assert_eq!(rush(r#"x=7; echo $(( "x" + 1 ))"#).0, "8\n");
    assert_eq!(rush(r#"echo $(( "abc" ))"#).0, "0\n");
    assert_eq!(rush(r#"echo $(( 5 > 3 ? "y" : "n" ))"#).0, "0\n");
}

#[cfg(unix)]
#[test]
fn dollar_dash_matches_bash_across_modes() {
    // `$-` reports h/B (always on) plus the set-flags and the invocation
    // flag: `hBc` under `-c`, and set-flags slot in alphabetically. Each
    // verified against `bash -c`.
    assert_eq!(rush("echo $-").0, "hBc\n");
    assert_eq!(rush("set -eux; echo $-").0, "ehuxBc\n");
    assert_eq!(rush("set -f -C; echo $-").0, "fhBCc\n");
    // A pipe redirected onto stdin (no -c/-s/file) is a non-interactive
    // stdin script: `$-` gets `s`, not `i`.
    assert_eq!(rush_stdin_plain("echo $-\n"), "hBs\n");
}
