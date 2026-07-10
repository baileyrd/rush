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
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .expect("write stdin");
    let output = child.wait_with_output().expect("wait rush");
    (String::from_utf8_lossy(&output.stdout).into_owned(), output.status.code().unwrap_or(-1))
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
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .expect("write stdin");
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

    let mut child = Command::new(env!("CARGO_BIN_EXE_rush"))
        .env("HOME", &home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn rush");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .expect("write stdin");
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
    assert_eq!(
        rush("set -o pipefail; (exit 3) | (exit 5) | true; echo $?").0,
        "5\n"
    );
    assert_eq!(
        rush("set -o pipefail; (exit 5) | (exit 3) | (exit 0); echo $?").0,
        "3\n"
    );

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
