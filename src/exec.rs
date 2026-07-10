//! Execute a parsed command list.
//!
//! A [`CommandList`] is a sequence of jobs separated by `;`/`&`. Each job is an
//! and-or chain of pipelines (`&&`/`||`); a job marked `background` runs without
//! blocking the shell. Every pipeline is expanded (variables, globs, …) *just
//! before it runs*, left to right, so a `cd` takes effect for later pipelines.
//!
//! On Unix, foreground and background pipelines go through [`crate::job`], which
//! adds process groups, terminal control, and stop/`fg`/`bg` handling. On other
//! platforms there is no job control: foreground pipelines run with a plain
//! spawn-and-wait, and `&` is rejected.
//!
//! Within a pipeline, builtins only run in-process when the pipeline is a
//! single command — a builtin in the middle of a pipe (`echo hi | cd`) is a
//! rare case we punt on for now.

use std::fs::{File, OpenOptions};
use std::io::Read;
use std::process::{Child, Command as OsCommand, Stdio};

use crate::builtins;
use crate::parser::{AndOrList, CommandList, Compound, Connector, Job, RawCommand, RawCompound, RawPipeline};

#[derive(Debug, Clone)]
pub struct Command {
    pub argv: Vec<String>,
    pub redirects: Vec<Redirect>,
    /// Leading `NAME=value`/`NAME=(a b c)` assignments. With no `argv` they
    /// set shell variables (any kind — scalar or array, see
    /// `crate::vars::assign`); otherwise only a *scalar* one applies to this
    /// command's own environment (see `build_stage`) — an array can't be
    /// represented in a child's environment at all, so it's simply skipped
    /// there rather than set anywhere.
    pub assignments: Vec<(String, crate::vars::AssignOp)>,
    /// A here-document body (already expanded) to feed on stdin, if any.
    pub heredoc: Option<String>,
    /// Only populated when `argv == ["local"]`: each declared name with its
    /// optional assignment (`None` for a bare `local name`) — a separate
    /// field (rather than reusing `assignments`, or making `local`'s own
    /// builtin re-parse `argv` strings) specifically so `local arr=(a b c)`
    /// can carry a real array literal, which a plain `Vec<String>` argv
    /// can't represent at all. See `builtins::local_cmd`.
    pub local_decls: Vec<(String, Option<crate::vars::AssignOp>)>,
    /// Attributes declared by this `local`/`declare` invocation's flags
    /// (`-u`/`-l`/`-i`, C43), applying to every name in `local_decls`.
    pub decl_attrs: crate::vars::Attrs,
}

#[derive(Debug, Clone)]
pub enum Redirect {
    /// `[fd]< file` / `[fd]> file` / `[fd]>> file`.
    File { fd: u32, file: String, mode: RedirMode },
    /// `&> file` / `&>> file`.
    Both { file: String, append: bool },
    /// `fd>&target` (e.g. `2>&1`).
    Dup { fd: u32, target: u32 },
}

pub use crate::parser::RedirMode;

/// One stage of a pipeline: an external/builtin command, or a compound
/// (`if`/`while`/`(...)`/…). A compound stage only runs by forking (Unix
/// only, in `job::spawn_pipeline`) — it never goes through `build_stage`.
#[derive(Debug, Clone)]
pub enum Stage {
    Simple(Command),
    Compound(CompoundStage),
}

/// A compound command plus any redirects trailing its close (`done < file`,
/// `{ …; } > log`), already expanded — mirrors `Command`'s own
/// `redirects`/`heredoc` split (a here-doc feeds stdin rather than naming a
/// target file, so it isn't itself a `Redirect`).
#[derive(Debug, Clone)]
pub struct CompoundStage {
    pub compound: Box<Compound>,
    pub redirects: Vec<Redirect>,
    pub heredoc: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Pipeline {
    pub commands: Vec<Stage>,
}

/// Run a whole command line, returning the exit status of the last foreground
/// job that ran. A `break`/`continue` that escapes all loops is discarded here.
pub fn run_list(list: &CommandList) -> Result<i32, String> {
    let status = exec_list(list)?;
    // Any break/continue/return that escaped to the top level is discarded.
    crate::vars::set_loop_ctl(None);
    crate::vars::set_returning(None);
    Ok(status)
}

/// Run a list, stopping early if `break`/`continue`/`return` becomes pending —
/// used for the top level and most compound-command bodies. Checks `errexit`
/// (`set -e`) after each job.
fn exec_list(list: &CommandList) -> Result<i32, String> {
    exec_list_impl(list, true)
}

/// Like `exec_list`, but never triggers `errexit` — for `if`/`while`/`until`
/// conditions, which bash explicitly exempts (a failing condition is the
/// normal, expected way to end a loop or skip a branch, not a script error).
fn exec_cond(list: &CommandList) -> Result<i32, String> {
    exec_list_impl(list, false)
}

/// Matches bash's actual `errexit` rule: a failing pipeline is exempt unless
/// it's positionally last in its `&&`/`||` list — `set -e; false && true`
/// does *not* exit (`false` isn't last), but `set -e; true && false` does
/// (`false` is). `run_job`/`run_andor` report whether the textually-last
/// pipeline in a job's and-or chain actually ran, alongside its status, so
/// this only fires when a *reached* final command fails — not merely
/// whichever pipeline happened to run last (which, under short-circuiting,
/// can be an earlier one in the source).
fn exec_list_impl(list: &CommandList, check_errexit: bool) -> Result<i32, String> {
    let mut status = 0;
    for job in &list.jobs {
        let (job_status, last_ran) = run_job(job)?;
        status = job_status;
        // A TERM/HUP that arrived while that job was running (and wasn't
        // already caught mid-wait — see `job::wait_pgid`) gets handled here,
        // at the next command boundary — same idea as `set -e`'s own check
        // just below.
        #[cfg(unix)]
        crate::trap::check_pending();
        if crate::vars::flow_pending() {
            break;
        }
        if check_errexit && status != 0 && last_ran {
            // `trap 'cmd' ERR` (C53) fires on exactly the condition
            // errexit checks — a reached, non-negated final command
            // failing outside an `if`/`while` condition — whether or not
            // `set -e` is on, and *before* the errexit exit when it is
            // (order verified against bash). Not fired inside a function
            // call: bash's ERR trap isn't inherited by functions unless
            // `set -o errtrace`, which rush doesn't implement (documented).
            if crate::vars::function_depth() == 0 {
                crate::trap::fire_err(status);
            }
            if crate::vars::errexit() {
                crate::trap::exit_shell(status);
            }
        }
    }
    Ok(status)
}

/// Returns `(status, last_ran)`: `last_ran` is whether the textually-last
/// pipeline in the job's `&&`/`||` chain actually ran (as opposed to being
/// skipped by short-circuiting) — see `exec_list_impl`.
fn run_job(job: &Job) -> Result<(i32, bool), String> {
    if job.background {
        // Backgrounding an `&&`/`||` list would need a subshell; we support the
        // common case of a single (possibly piped) command.
        if !job.list.rest.is_empty() {
            return Err("background '&&'/'||' lists are not supported".into());
        }
        let pipeline = crate::expand::expand(&job.list.first)?;
        run_background(&pipeline)?;
        #[cfg(unix)]
        close_pending_proc_subs();
        crate::vars::set_last_status(0);
        crate::vars::set_pipestatus(&[0]); // `cmd &` → PIPESTATUS=(0), same as bash
        Ok((0, true))
    } else {
        run_andor(&job.list)
    }
}

fn run_andor(list: &AndOrList) -> Result<(i32, bool), String> {
    // `set -n` (noexec, C51): everything still parses, nothing runs. The
    // check sits here — the choke point every top-level and compound-body
    // command funnels through — so a mid-script `set -n` stops the rest of
    // the script (including the `set +n` that would undo it, matching
    // bash's own one-way behavior, verified directly).
    if crate::vars::noexec() {
        return Ok((0, false));
    }
    // `$LINENO` (C67): the source line this pipeline started on.
    crate::vars::set_current_line(list.first.line);
    // `trap 'cmd' DEBUG` (C65) fires before each pipeline here — bash
    // fires per *simple command*, so one `a | b` stage-pair is a single
    // firing in rush where bash may fire per stage; a documented
    // approximation. `$?` is preserved across the handler.
    crate::trap::fire_preserving("DEBUG");
    // Update `$?` after every pipeline, so a later one in the same line can read
    // it (e.g. `false || echo $?`).
    let mut status = run_pipeline_node(&list.first)?;
    crate::vars::set_last_status(status);
    // If there's no `rest`, `first` *is* the last pipeline, and it just ran.
    // A `!`-negated pipeline reports `last_ran = false` even when it did
    // run: the only consumer is the errexit/ERR check, and a negated
    // pipeline is exempt from both (verified against bash: `set -e; ! true`
    // survives, and `true && ! true` fires no ERR trap).
    let mut last_ran = list.rest.is_empty() && !list.first.negated;
    if crate::vars::flow_pending() {
        return Ok((status, last_ran));
    }
    let final_idx = list.rest.len().wrapping_sub(1);
    for (i, (connector, raw)) in list.rest.iter().enumerate() {
        if should_run(*connector, status) {
            crate::vars::set_current_line(raw.line);
            crate::trap::fire_preserving("DEBUG");
            status = run_pipeline_node(raw)?;
            crate::vars::set_last_status(status);
            last_ran = i == final_idx && !raw.negated;
            if crate::vars::flow_pending() {
                break;
            }
        } else {
            // Short-circuited: this pipeline (whether or not it's the final
            // one) didn't run.
            last_ran = false;
        }
    }
    Ok((status, last_ran))
}

/// A pipeline that is a single compound command (`if`/`while`/`for`) is run
/// directly; everything else goes through the simple-command path.
fn run_pipeline_node(raw: &RawPipeline) -> Result<i32, String> {
    let status = if let [RawCommand::Compound(rc)] = raw.commands.as_slice() {
        run_compound_with_redirects(rc)?
    } else {
        run_foreground(raw)?
    };
    // `${PIPESTATUS[@]}` (C54), single-stage case — builtins, functions,
    // compounds, assignments, one external command alike get a
    // one-element array. Recorded *before* negation: `! false` leaves
    // `PIPESTATUS=(1)` in bash (verified). The multi-stage vector is
    // recorded where the stages are actually reaped (`job::wait_pgid`).
    if raw.commands.len() == 1 {
        crate::vars::set_pipestatus(&[status]);
    }
    Ok(negate_if(raw.negated, status))
}

/// `! pipeline` — logical negation of an exit status (0 ↔ 1); any nonzero
/// becomes 0, matching bash.
fn negate_if(negated: bool, status: i32) -> i32 {
    if !negated {
        status
    } else if status == 0 {
        1
    } else {
        0
    }
}

/// Run a sole compound command, applying any redirects trailing its close
/// (`while …; done < file`, `{ …; } > log`) for the duration — the same idea
/// as `run_builtin_foreground`'s `redirect_stdio`, just wrapping the whole
/// compound instead of one builtin call. This covers the common case (no
/// pipe involved); a compound as one stage *of* a real pipeline goes through
/// `job::spawn_compound_stage` instead, which applies its own redirects the
/// same way in the forked child.
fn run_compound_with_redirects(rc: &RawCompound) -> Result<i32, String> {
    if rc.redirects.is_empty() {
        return run_compound(&rc.compound);
    }
    let (redirects, heredoc) = crate::expand::expand_redirects(&rc.redirects)?;
    #[cfg(unix)]
    {
        // Unlike a builtin (which writes straight to the process's own fds),
        // a compound's body can itself spawn real children (an external
        // command, a piped stage, a subshell) that inherit fd 0/1/2 by the
        // usual rules — dup2'ing the *shell's* fds for the duration covers
        // both that and any builtins/`println!` output inside it uniformly.
        let _guard = redirect_stdio(&redirects, heredoc.as_deref())?;
        run_compound(&rc.compound)
    }
    #[cfg(not(unix))]
    {
        let _ = (redirects, heredoc);
        Err("redirects on a compound command are not supported on this platform".into())
    }
}

pub(crate) fn run_compound(compound: &Compound) -> Result<i32, String> {
    match compound {
        // `[[ expr ]]` (C55): 0 when true, 1 when false, 2 on an
        // evaluation error (bad operator, unfinished `=~`) — bash's own
        // status convention — without aborting the script.
        // `coproc [NAME] command` (C66): fork the command with a
        // bidirectional pipe, publishing `NAME[0]` (read from its
        // stdout) / `NAME[1]` (write to its stdin) and `NAME_PID`.
        Compound::Coproc { name, cmd } => run_coproc(name, cmd),
        Compound::Cond(ast) => match eval_cond(ast) {
            Ok(true) => Ok(0),
            Ok(false) => Ok(1),
            Err(e) => {
                eprintln!("rush: [[: {e}");
                Ok(2)
            }
        },
        Compound::If { branches, else_body } => {
            for (cond, body) in branches {
                if exec_cond(cond)? == 0 {
                    return exec_list(body);
                }
            }
            match else_body {
                Some(body) => exec_list(body),
                None => Ok(0),
            }
        }
        Compound::Loop { until, cond, body } => {
            let mut status = 0;
            loop {
                let met = exec_cond(cond)? == 0;
                if met == *until {
                    break; // while: stop when not met; until: stop when met
                }
                status = exec_list(body)?;
                if loop_step()? {
                    break;
                }
            }
            Ok(status)
        }
        Compound::For { var, words, has_in, body } => {
            // POSIX: omitting `in` iterates the positional parameters ("$@"),
            // as if `in "$@"` had been written; an explicit `in` with no
            // words (`for x in; do ...`) is a real empty list instead.
            let values = if *has_in { crate::expand::expand_words(words)? } else { crate::vars::args() };
            // A readonly loop variable is fatal, same as a bare assignment
            // (verified: bash aborts before the first iteration).
            if !values.is_empty() && crate::vars::is_readonly(var) {
                return Err(format!("{var}: readonly variable"));
            }
            let mut status = 0;
            for value in values {
                crate::vars::set(var, &value);
                status = exec_list(body)?;
                if loop_step()? {
                    break;
                }
            }
            Ok(status)
        }
        // `for ((init; cond; update)); do BODY; done` — C-style. `cond`
        // empty means always-true (`for ((;;))` is a real infinite loop,
        // verified directly); `init`/`update` empty are no-ops.
        Compound::CFor { init, cond, update, body } => {
            if let Some(e) = init {
                eval_arith_stmt(e)?;
            }
            let mut status = 0;
            loop {
                let keep_going = match cond {
                    Some(e) => eval_arith_stmt(e)? != 0,
                    None => true,
                };
                if !keep_going {
                    break;
                }
                status = exec_list(body)?;
                // This loop's own `continue` still runs `update` before
                // re-testing `cond` — real C `for` semantics, verified
                // directly; `break` (here, or propagating from an outer
                // loop via `break N`/`continue N`) does not.
                let ran_to_completion =
                    matches!(crate::vars::loop_ctl(), None | Some(crate::vars::LoopCtl::Continue(1)));
                if ran_to_completion && let Some(e) = update {
                    eval_arith_stmt(e)?;
                }
                if loop_step()? {
                    break;
                }
            }
            Ok(status)
        }
        // `((expr))` — a standalone arithmetic command, for its side
        // effects (assignment, `++`/`--`) rather than its value. Exit
        // status mirrors `test`'s convention: `0` if `expr` is nonzero,
        // `1` if zero. An empty `expr` evaluates as `0` (status `1`)
        // rather than erroring — real bash's own asymmetry with `$(( ))`,
        // which does error on empty — verified directly.
        Compound::Arith(expr) => {
            if expr.trim().is_empty() {
                return Ok(1);
            }
            Ok(if eval_arith_stmt(expr)? != 0 { 0 } else { 1 })
        }
        // `select NAME [in WORDS]; do BODY; done`: prints `WORDS` as a
        // numbered menu to stderr, then repeatedly prompts (`$PS3`, default
        // `#? `) and reads a line, setting `$REPLY` to it *raw* — no
        // `$IFS` splitting/trimming, unlike ordinary `read` (verified
        // directly: three bare spaces as the whole line come back as three
        // spaces in `$REPLY`). A blank line (zero-length, not merely
        // all-whitespace) redisplays the menu and prompts again, without
        // running `BODY`. Otherwise `NAME` becomes the word at that
        // 1-based index if the line parses as one in range, or `""`
        // otherwise (`$REPLY` is set either way); `BODY` runs once, same
        // `break`/status semantics as `for`/`while`. EOF on read ends the
        // whole construct with status 1, overriding whatever `BODY`'s
        // last run returned — bash's own documented quirk, verified
        // directly (unlike `while read line; do …; done`, whose status
        // after its own final failing `read` stays whatever the loop
        // body's last iteration returned).
        Compound::Select { var, words, has_in, body } => {
            let values = if *has_in { crate::expand::expand_words(words)? } else { crate::vars::args() };
            if values.is_empty() {
                return Ok(0);
            }
            // `vars::get` alone — no `std::env` fallback (C36/C40).
            let ps3 = match crate::vars::get("PS3") {
                Some(ps3) => crate::expand::expand_dollars(&ps3)?,
                None => "#? ".to_string(),
            };
            print_select_menu(&values);
            let status = loop {
                eprint!("{ps3}");
                let (line, hit_eof) = crate::builtins::read_reply_line();
                crate::vars::set("REPLY", &line);
                if hit_eof {
                    // A tidy-cursor touch bash's own `select` has too:
                    // move off the unanswered prompt's line before the
                    // whole construct ends, verified directly.
                    eprintln!();
                    break 1;
                }
                if line.is_empty() {
                    print_select_menu(&values);
                    continue;
                }
                let chosen = line
                    .trim()
                    .parse::<i64>()
                    .ok()
                    .filter(|&n| n >= 1 && (n as usize) <= values.len())
                    .map(|n| values[(n - 1) as usize].clone())
                    .unwrap_or_default();
                crate::vars::set(var, &chosen);
                let status = exec_list(body)?;
                if loop_step()? {
                    break status;
                }
            };
            Ok(status)
        }
        Compound::Case { word, items } => {
            let subject = crate::expand::expand_to_string(word)?;
            let mut idx = None;
            for (i, (patterns, _, _)) in items.iter().enumerate() {
                if case_patterns_match(patterns, &subject)? {
                    idx = Some(i);
                    break;
                }
            }
            let Some(mut idx) = idx else { return Ok(0) };
            let status = loop {
                let (_, body, term) = &items[idx];
                let status = exec_list(body)?;
                idx = match term {
                    crate::parser::CaseTerm::Break => break status,
                    // Unconditionally run the next item's body too, no
                    // pattern test — falls off the end of `items` the same
                    // way `;;` would if there's no next item.
                    crate::parser::CaseTerm::FallThrough => {
                        if idx + 1 >= items.len() {
                            break status;
                        }
                        idx + 1
                    }
                    // Resume pattern testing at the next item, same as if
                    // the whole `case` restarted from there.
                    crate::parser::CaseTerm::Continue => {
                        let mut next = idx + 1;
                        let mut found = None;
                        while next < items.len() {
                            if case_patterns_match(&items[next].0, &subject)? {
                                found = Some(next);
                                break;
                            }
                            next += 1;
                        }
                        match found {
                            Some(n) => n,
                            None => break status,
                        }
                    }
                };
            };
            Ok(status)
        }
        Compound::Group(list) => exec_list(list),
        Compound::Subshell(list) => {
            #[cfg(unix)]
            {
                run_subshell_forked(list)
            }
            #[cfg(not(unix))]
            {
                // No `fork` on this platform: approximate isolation by saving
                // and restoring the state commands usually mutate — the
                // working directory and variables — so `(cd x; …)` and
                // `(VAR=…; …)` don't leak out. `exit` inside still exits the
                // whole shell (see docs/ARCHITECTURE.md's Windows note).
                let saved_cwd = std::env::current_dir().ok();
                let saved_vars = crate::vars::snapshot();

                let result = exec_list(list);

                if let Some(dir) = saved_cwd {
                    let _ = std::env::set_current_dir(dir);
                }
                crate::vars::restore(saved_vars);
                result
            }
        }
        Compound::FuncDef { name, body } => {
            crate::func::define(name, body.clone());
            Ok(0)
        }
    }
}

/// Evaluate a raw arithmetic clause from `((expr))` or a C-style `for`
/// header: `$`-references are resolved first (same two-step pipeline
/// `$((...))` itself uses — a bare `i` and a `$`-prefixed `$i` both work),
/// then the result is evaluated for its value *and* side effects
/// (assignment, `++`/`--`).
fn eval_arith_stmt(expr: &str) -> Result<i64, String> {
    let expanded = crate::expand::expand_dollars(expr)?;
    crate::arith::eval(&expanded)
}

/// `select`'s numbered menu, one entry per line: `N) word`. Real bash lays
/// this out in columns sized to `$COLUMNS`; rush always uses a single
/// column instead — an accepted, cosmetic scope narrowing (the functional
/// behavior — numbering, `$REPLY`, `break`, exit status — is unaffected,
/// and every real script reads `$REPLY`/`NAME`, not the menu's own
/// layout).
fn print_select_menu(values: &[String]) {
    for (i, value) in values.iter().enumerate() {
        eprintln!("{}) {value}", i + 1);
    }
}

/// Whether any of a `case` item's patterns match `subject` — shared by the
/// initial left-to-right scan and `;;&`'s own resumed scan starting partway
/// through `items`.
fn case_patterns_match(patterns: &[crate::lexer::Word], subject: &str) -> Result<bool, String> {
    for pat in patterns {
        if crate::glob::match_component(&crate::expand::expand_pattern(pat)?, subject) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Run a subshell's body in a forked child: real isolation for cwd,
/// variables, and even `exit` — none of it can leak back to the parent,
/// because the child is a genuinely separate process. The parent just waits
/// for it and adopts its exit status as `$?`.
#[cfg(unix)]
fn run_subshell_forked(list: &CommandList) -> Result<i32, String> {
    match unsafe { libc::fork() } {
        -1 => Err(std::io::Error::last_os_error().to_string()),
        0 => {
            // Child: run the body, then exit with its status — firing its own
            // (forked, independent) copy of any EXIT trap. The `exit`
            // builtin's `trap::exit_shell` now only ends this child.
            let status = exec_list(list).unwrap_or(1);
            crate::trap::exit_shell(status);
        }
        pid => {
            let mut status: libc::c_int = 0;
            loop {
                if unsafe { libc::waitpid(pid, &mut status, 0) } != -1 {
                    return Ok(crate::job::exit_code(status));
                }
                // Interrupted by a signal (e.g. a background job's SIGCHLD); retry.
                if std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
                    return Err(std::io::Error::last_os_error().to_string());
                }
            }
        }
    }
}

/// Run a defined function: swap in the call's arguments as `$1`…, push a
/// fresh `local` frame (C10), run the body (a `return` ends it), then
/// restore the previous positional parameters and pop the `local` frame —
/// restoring whatever any `local name` in the body shadowed back to the
/// caller's own value (or removing it, if it didn't have one).
fn call_function(argv: &[String]) -> Result<i32, String> {
    let body = crate::func::get(&argv[0]).expect("function is defined");

    let name0 = crate::vars::arg(0).unwrap_or_else(|| "rush".to_string());
    let saved = crate::vars::args();
    crate::vars::set_args(name0.clone(), argv[1..].to_vec());
    crate::vars::push_local_frame();
    crate::vars::push_function(&argv[0]); // `${FUNCNAME[@]}` (C67)

    let result = exec_list(&body);

    crate::vars::pop_function();
    let returned = crate::vars::returning();
    crate::vars::set_returning(None);
    crate::vars::set_args(name0, saved);
    crate::vars::pop_local_frame();

    // `trap 'cmd' RETURN` (C65) fires as the function returns, with the
    // function's own status preserved for the caller.
    crate::trap::fire_preserving("RETURN");

    Ok(returned.unwrap_or(result?))
}

/// `. name [args...]` / `source name [args...]` — run `name`'s commands in
/// the *current* shell (no fork, no new variable scope): assignments,
/// `cd`, function definitions, etc. all persist in the caller. If `args`
/// are given, they become the positional parameters for the duration,
/// restored after (like a function call's own `$1`…) — with none, the
/// sourced file just sees the caller's own positional parameters
/// unchanged (verified against real bash directly, alongside everything
/// else here). A `return` inside it ends just the sourcing (consumed here,
/// the same way `call_function` consumes its own); `break`/`continue` are
/// *not* consumed — they propagate to an enclosing loop in the *calling*
/// context if the sourced file doesn't have a loop of its own to catch
/// them first. A bare filename (no `/`) is searched on `$PATH`, same as an
/// ordinary command — but for a *readable* file, not an executable one,
/// since its content is parsed and run directly, never exec'd (unlike a
/// plain command, sourcing doesn't need the execute bit set).
pub fn source_file(name: &str, args: &[String]) -> Result<i32, String> {
    let path = resolve_source_path(name).ok_or_else(|| "No such file or directory".to_string())?;
    let src = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let list = crate::parser::parse(&src).map_err(|e| e.to_string())?;

    let name0 = crate::vars::arg(0).unwrap_or_else(|| "rush".to_string());
    let saved_args = (!args.is_empty()).then(crate::vars::args);
    if !args.is_empty() {
        crate::vars::set_args(name0.clone(), args.to_vec());
    }

    crate::vars::push_source(&path.to_string_lossy()); // `${BASH_SOURCE[@]}` (C67)
    let result = exec_list(&list);
    crate::vars::pop_source();

    let returned = crate::vars::returning();
    crate::vars::set_returning(None);
    if let Some(saved) = saved_args {
        crate::vars::set_args(name0, saved);
    }

    Ok(returned.unwrap_or(result?))
}

/// Resolve a `source`/`.` filename: a bare name (no `/`) is searched on
/// `$PATH` for a readable file; anything else is used as a literal path.
fn resolve_source_path(name: &str) -> Option<std::path::PathBuf> {
    if name.contains('/') {
        let p = std::path::Path::new(name);
        return p.is_file().then(|| p.to_path_buf());
    }
    // `vars::get` alone — no `std::env` fallback (C36/C40).
    let path = crate::vars::get("PATH")?;
    std::env::split_paths(&path).map(|dir| dir.join(name)).find(|c| c.is_file())
}

/// `eval arg...` — join `args` with a single space, parse the result, and run
/// it in the *current* shell, exactly as if it had been typed inline (no
/// scope of any kind: unlike `source_file`, there's no filename/PATH search,
/// no positional-parameter swap, and a `return`/`break`/`continue` inside is
/// *not* consumed — it propagates straight to the enclosing function/loop,
/// verified directly against real bash). Empty input (no args, or all-empty
/// args) is a no-op that succeeds.
pub fn eval_cmd(args: &[String]) -> Result<i32, String> {
    let src = args.join(" ");
    if src.trim().is_empty() {
        return Ok(0);
    }
    let list = crate::parser::parse(&src).map_err(|e| e.to_string())?;
    exec_list(&list)
}

/// `exec [cmd [args...]]` (Unix only). With a command: replaces the current
/// process image via `execvp` — no fork, so on success this never returns.
/// It inherits whatever fds 0/1/2 the caller's redirects already left them
/// as (`run_builtin_foreground` applies those before calling into `try_run`,
/// same as for any other builtin) and the shell's exported environment,
/// exactly like an ordinary spawned child (`build_stage`). On failure (e.g.
/// command not found) — verified directly against real bash — a
/// non-interactive shell exits immediately with status 127 (the same as
/// bash: the *whole script* stops there, not just this command), while an
/// interactive one just reports 127 and keeps running, its redirects
/// restored as normal since the `run_builtin_foreground` guard never got
/// disarmed.
///
/// With no command (bare `exec`, or `exec` followed only by redirects) it's
/// a no-op that always succeeds — the redirects were already applied by the
/// caller, which makes them permanent by disarming its own guard rather
/// than restoring it, exactly the way `exec > file`/`exec 3<&-` are meant
/// to work.
///
/// Caveat shared with the rest of rush's redirect machinery: a target `fd`
/// other than 0/1/2 (`exec 3>file`) isn't actually honored as fd 3 — see
/// `redirect_stdio`'s own `target_fd` collapse, a pre-existing limitation
/// not specific to `exec`.
#[cfg(unix)]
pub fn exec_cmd(argv: &[String]) -> i32 {
    use std::os::unix::process::CommandExt;

    let Some(program) = argv.get(1) else {
        return 0;
    };

    let mut command = std::process::Command::new(resolve_program(program));
    command.args(&argv[2..]);
    // `env_clear` first: `Command` otherwise inherits the *real* OS
    // environment by default regardless of what's fed to `.envs()`, which
    // only adds/overrides — never removes — entries on top of it. Since
    // `main.rs` seeds `vars`'s own table from that same inherited
    // environment at startup (C36), `vars::exported()` is a complete,
    // accurate picture of what this process's environment should be, so
    // rebuilding it from scratch here (rather than layering onto the
    // default inheritance) is exactly what's needed for `unset` of an
    // inherited/exported name to actually take effect in the replaced
    // process (C40) — `exec_cmd` replaces the whole process image, so
    // there's nothing else in it that could depend on the untouched
    // default inheritance.
    command.env_clear();
    command.envs(crate::vars::exported());

    let err = command.exec();
    eprintln!("{}: {program}: {err}", argv[0]);
    if !crate::job::job_control_enabled() {
        std::process::exit(127);
    }
    127
}

#[cfg(not(unix))]
pub fn exec_cmd(argv: &[String]) -> i32 {
    eprintln!("{}: process replacement is not supported on this platform", argv[0]);
    1
}

/// After running a loop body, consume one level of any pending `break`/
/// `continue`. Returns `true` if this loop should stop iterating.
fn loop_step() -> Result<bool, String> {
    use crate::vars::LoopCtl;
    // A pending `return` unwinds straight through the loop (left for the
    // enclosing function call to consume).
    if crate::vars::returning().is_some() {
        return Ok(true);
    }
    match crate::vars::loop_ctl() {
        None => Ok(false),
        Some(LoopCtl::Continue(1)) => {
            crate::vars::set_loop_ctl(None);
            Ok(false) // keep looping
        }
        Some(LoopCtl::Break(1)) => {
            crate::vars::set_loop_ctl(None);
            Ok(true)
        }
        // `break N` / `continue N` for an outer loop: drop a level and stop this
        // one, leaving the request pending for the enclosing loop to handle.
        Some(LoopCtl::Break(n)) => {
            crate::vars::set_loop_ctl(Some(LoopCtl::Break(n - 1)));
            Ok(true)
        }
        Some(LoopCtl::Continue(n)) => {
            crate::vars::set_loop_ctl(Some(LoopCtl::Continue(n - 1)));
            Ok(true)
        }
    }
}

fn should_run(connector: Connector, prev_status: i32) -> bool {
    match connector {
        Connector::And => prev_status == 0,
        Connector::Or => prev_status != 0,
    }
}

/// `set -x`: print each simple stage of `pipeline` to stderr before it
/// runs, matching real bash's format — `$PS4` (default `+ `), each leading
/// `NAME=value` assignment on its own line, then the command word and its
/// arguments, each re-quoted with single quotes if it contains whitespace
/// or a shell-special character (verified directly against real bash). A
/// no-op unless `set -x` is on.
fn trace_pipeline(pipeline: &Pipeline) {
    if !crate::vars::xtrace() {
        return;
    }
    let prefix = trace_prefix();
    for stage in &pipeline.commands {
        if let Stage::Simple(cmd) = stage {
            for (name, op) in &cmd.assignments {
                eprintln!("{prefix}{}", trace_assignment(name, op));
            }
            if !cmd.argv.is_empty() {
                let words: Vec<String> = cmd.argv.iter().map(|w| trace_quote(w)).collect();
                eprintln!("{prefix}{}", words.join(" "));
            }
        }
    }
}

/// `$PS4` (default `+ `) with its first character repeated once per level
/// of `$(...)` command substitution currently being expanded — matching
/// real bash's own nesting-depth indicator, verified directly.
fn trace_prefix() -> String {
    // `vars::get` alone — no `std::env` fallback (C36/C40).
    let ps4 = crate::vars::get("PS4").unwrap_or_else(|| "+ ".to_string());
    let mut chars = ps4.chars();
    match chars.next() {
        Some(c) => {
            let rest: String = chars.collect();
            format!("{}{rest}", c.to_string().repeat(crate::vars::trace_depth() as usize + 1))
        }
        None => String::new(),
    }
}

/// Re-quote a word for `set -x` display: wrapped in single quotes if it
/// contains whitespace or a shell-special character, else printed as-is.
fn trace_quote(word: &str) -> String {
    let needs_quote = word.is_empty()
        || word.chars().any(|c| c.is_whitespace() || "'\"$&|;<>()`\\*?[]{}~#".contains(c));
    if needs_quote {
        format!("'{}'", word.replace('\'', r"'\''"))
    } else {
        word.to_string()
    }
}

/// Render one `set -x`-traced assignment: `name=value`, `name+=value`,
/// `name=(a b c)`, `name+=(a b c)`, `name=([k]=v ...)`, or `name[k]=v` —
/// matching real bash's own format for each (verified directly), modulo
/// one small, accepted difference: bash re-quotes an array/assoc element
/// containing whitespace with *double* quotes there specifically (and
/// inconsistently quotes an associative-array *key* too, only in some of
/// these forms), where this reuses `trace_quote`'s single-quote convention
/// (the same one already used for a plain command's own argv) uniformly.
fn trace_assignment(name: &str, op: &crate::vars::AssignOp) -> String {
    use crate::vars::{AssignOp, AssignValue};
    match op {
        AssignOp::Set(AssignValue::Scalar(v)) => format!("{name}={v}"),
        AssignOp::Append(AssignValue::Scalar(v)) => format!("{name}+={v}"),
        AssignOp::Set(AssignValue::Array(vs)) => {
            format!("{name}=({})", vs.iter().map(|v| trace_quote(v)).collect::<Vec<_>>().join(" "))
        }
        AssignOp::Append(AssignValue::Array(vs)) => {
            format!("{name}+=({})", vs.iter().map(|v| trace_quote(v)).collect::<Vec<_>>().join(" "))
        }
        AssignOp::Set(AssignValue::Assoc(pairs)) => {
            format!("{name}=({})", trace_assoc_pairs(pairs))
        }
        AssignOp::Append(AssignValue::Assoc(pairs)) => {
            format!("{name}+=({})", trace_assoc_pairs(pairs))
        }
        AssignOp::SetKey(k, v) => format!("{name}[{k}]={v}"),
        AssignOp::AppendKey(k, v) => format!("{name}[{k}]+={v}"),
    }
}

fn trace_assoc_pairs(pairs: &[(String, String)]) -> String {
    pairs.iter().map(|(k, v)| format!("[{k}]={}", trace_quote(v))).collect::<Vec<_>>().join(" ")
}

/// `coproc` (C66), Unix only: two real pipes, a fork, and two shell
/// variables. The child gets the parent→child pipe on stdin and the
/// child→parent pipe on stdout, then runs `cmd` and exits with its
/// status. The parent publishes `NAME=(read_fd write_fd)` and
/// `NAME_PID`, marks both fds close-on-exec (matching bash — ordinary
/// spawned children don't inherit them; an explicit `>&$fd` redirect
/// still works, since `dup2` clears the flag on the copy), and records
/// the pid as `$!`. Documented narrowing: the coprocess isn't entered
/// in the interactive job table (bash lists it under `jobs`), but
/// `wait $COPROC_PID` works through the ordinary pid path.
#[cfg(unix)]
fn run_coproc(name: &str, cmd: &crate::parser::RawCommand) -> Result<i32, String> {
    use std::os::unix::io::{AsRawFd, IntoRawFd};

    let (to_child_read, to_child_write) = make_pipe()?; // parent writes → child stdin
    let (from_child_read, from_child_write) = make_pipe()?; // child stdout → parent reads
    // The coprocess must die on a plain `kill $NAME_PID` — but the child
    // inherits the parent's TERM/HUP record-and-defer handlers across
    // fork, and a kill racing in *before* the child could reset them was
    // silently swallowed (caught as an intermittent test hang). Setting
    // the default dispositions in the parent before forking closes the
    // race; the parent reinstalls its own handlers immediately after.
    unsafe {
        libc::signal(libc::SIGTERM, libc::SIG_DFL);
        libc::signal(libc::SIGHUP, libc::SIG_DFL);
    }
    match unsafe { libc::fork() } {
        -1 => Err(std::io::Error::last_os_error().to_string()),
        0 => {
            unsafe {
                libc::dup2(to_child_read.as_raw_fd(), 0);
                libc::dup2(from_child_write.as_raw_fd(), 1);
            }
            drop(to_child_read);
            drop(to_child_write);
            drop(from_child_read);
            drop(from_child_write);
            let pipeline = crate::parser::RawPipeline { commands: vec![cmd.clone()], negated: false, line: 0 };
            let status = run_foreground(&pipeline).unwrap_or(1);
            crate::trap::exit_shell(status);
        }
        pid => {
            crate::trap::install_signal_handlers(); // restore the deferring handlers
            drop(to_child_read);
            drop(from_child_write);
            let read_fd = from_child_read.into_raw_fd();
            let write_fd = to_child_write.into_raw_fd();
            unsafe {
                libc::fcntl(read_fd, libc::F_SETFD, libc::FD_CLOEXEC);
                libc::fcntl(write_fd, libc::F_SETFD, libc::FD_CLOEXEC);
            }
            crate::vars::set_array(name, vec![read_fd.to_string(), write_fd.to_string()]);
            crate::vars::set(&format!("{name}_PID"), &pid.to_string());
            crate::vars::set_last_bg_pid(pid);
            Ok(0)
        }
    }
}

#[cfg(not(unix))]
fn run_coproc(_name: &str, _cmd: &crate::parser::RawCommand) -> Result<i32, String> {
    Err("coproc is not supported on this platform".into())
}

/// Evaluate a parsed `[[ ... ]]` expression (C55). Operands expand with
/// `$`/`$(...)`/quote handling but — the whole point of `[[` — no
/// word-splitting and no filename globbing, so `x=; [[ $x = foo ]]` and
/// `x="a b"; [[ $x = "a b" ]]` both behave (each verified against bash,
/// where the `[ ]` spellings of the same tests are "too many arguments"
/// errors).
fn eval_cond(ast: &crate::parser::CondAst) -> Result<bool, String> {
    use crate::parser::CondAst;
    match ast {
        CondAst::Or(a, b) => Ok(eval_cond(a)? || eval_cond(b)?),
        CondAst::And(a, b) => Ok(eval_cond(a)? && eval_cond(b)?),
        CondAst::Not(x) => Ok(!eval_cond(x)?),
        CondAst::Str(w) => Ok(!crate::expand::expand_word(w)?.is_empty()),
        CondAst::Unary(op, w) => {
            let s = crate::expand::expand_word(w)?;
            crate::builtins::cond_unary(op, &s)
        }
        CondAst::Binary(lhs, op, rhs) => {
            let l = crate::expand::expand_word(lhs)?;
            match op.as_str() {
                // `=`/`==`/`!=`: the RHS is a glob pattern where (and only
                // where) it's unquoted — `[[ $x = "a"* ]]` matches anything
                // starting with a literal `a` (verified against bash);
                // `expand_cond_pattern` backslash-escapes the quoted parts.
                "=" | "==" => Ok(crate::glob::match_component(&crate::expand::expand_cond_pattern(rhs)?, &l)),
                "!=" => Ok(!crate::glob::match_component(&crate::expand::expand_cond_pattern(rhs)?, &l)),
                // Lexicographic string comparison — `<`/`>` never
                // redirect inside `[[` (that misparse was C55's own
                // headline repro).
                "<" => Ok(l < crate::expand::expand_word(rhs)?),
                ">" => Ok(l > crate::expand::expand_word(rhs)?),
                // The arithmetic comparisons evaluate both sides as full
                // arithmetic expressions (variable names resolve, unset
                // names are 0) — `[[ x -eq 5 ]]` with `x=5` is true in
                // bash, unlike `[ ]`'s integer-literal-only rule.
                "-eq" | "-ne" | "-lt" | "-le" | "-gt" | "-ge" => {
                    let r = crate::expand::expand_word(rhs)?;
                    let (a, b) = (crate::arith::eval(&l)?, crate::arith::eval(&r)?);
                    Ok(match op.as_str() {
                        "-eq" => a == b,
                        "-ne" => a != b,
                        "-lt" => a < b,
                        "-le" => a <= b,
                        "-gt" => a > b,
                        _ => a >= b,
                    })
                }
                // File-timestamp/identity comparisons.
                "-nt" | "-ot" | "-ef" => {
                    let r = crate::expand::expand_word(rhs)?;
                    let (ma, mb) = (std::fs::metadata(&l), std::fs::metadata(&r));
                    Ok(match op.as_str() {
                        "-ef" => {
                            #[cfg(unix)]
                            {
                                use std::os::unix::fs::MetadataExt;
                                matches!((&ma, &mb), (Ok(a), Ok(b)) if a.dev() == b.dev() && a.ino() == b.ino())
                            }
                            #[cfg(not(unix))]
                            {
                                false
                            }
                        }
                        "-nt" => matches!((ma.and_then(|m| m.modified()), mb.and_then(|m| m.modified())), (Ok(a), Ok(b)) if a > b),
                        _ => matches!((ma.and_then(|m| m.modified()), mb.and_then(|m| m.modified())), (Ok(a), Ok(b)) if a < b),
                    })
                }
                // `=~` (C56): an unanchored ERE search. On a match,
                // `BASH_REMATCH[0]` is the whole match and `[n]` the
                // capture groups (an unmatched optional group is present
                // as an empty string); on a failed match the array is
                // *unset* — both verified against bash. An invalid regex
                // is an evaluation error (status 2, script continues).
                "=~" => {
                    let pattern = crate::expand::expand_cond_regex(rhs)?;
                    let re = rusty_regx::Regex::new(&pattern).map_err(|e| format!("invalid regex: {e}"))?;
                    match re.captures(&l) {
                        Some(caps) => {
                            let groups: Vec<String> = (0..caps.len())
                                .map(|i| caps.get(i).unwrap_or_default().to_string())
                                .collect();
                            crate::vars::set_array("BASH_REMATCH", groups);
                            Ok(true)
                        }
                        None => {
                            crate::vars::unset("BASH_REMATCH");
                            Ok(false)
                        }
                    }
                }
                other => Err(format!("unknown operator `{other}`")),
            }
        }
    }
}

/// A single command that is only `NAME=value` assignments (no program word):
/// `FOO=bar`. These set shell variables rather than spawning anything.
fn assignment_only(pipeline: &Pipeline) -> bool {
    matches!(
        pipeline.commands.as_slice(),
        [Stage::Simple(cmd)] if cmd.argv.is_empty() && !cmd.assignments.is_empty()
    )
}

/// A bare-assignment statement targeting a readonly name is *fatal* in a
/// non-interactive shell — real bash aborts the whole script there
/// (verified: `readonly x=1; x=2; echo after` prints nothing after the
/// error), unlike the builtin-mediated attempts (`unset`/`export`/
/// `local`), which just fail with status 1 and continue. The `Err` here
/// rides the same channel as an expansion error, which has exactly that
/// abort behavior.
fn apply_assignments(pipeline: &Pipeline) -> Result<(), String> {
    if let [Stage::Simple(cmd)] = pipeline.commands.as_slice() {
        for (name, op) in &cmd.assignments {
            if crate::vars::is_readonly(name) {
                return Err(format!("{name}: readonly variable"));
            }
            crate::vars::assign(name, op);
        }
    }
    Ok(())
}

/// If `cmd` is `command name [args...]` — the *execution* form, not
/// `command -v`/`-V`, which are pure lookups the `command` builtin handles
/// entirely on its own — returns the inner command with the leading
/// `command` word stripped, ready to run bypassing function lookup.
fn command_bypass(cmd: &Command) -> Option<Command> {
    if cmd.argv.first().map(String::as_str) != Some("command") {
        return None;
    }
    // Skip over leading flag words to find what follows: a lookup flag
    // (`-v`/`-V`, alone or clustered as in `-pv`) means this is the pure
    // lookup form, handled entirely by the `command` builtin — not a
    // bypass. `-p` (C47) without `-v`/`-V` is the default-`$PATH`
    // *execution* form: strip the flags and pin argv[0] to its
    // default-path resolution now (an absolute path), so the ordinary
    // spawn below can't be swayed by the shell's own `$PATH`.
    let mut idx = 1;
    let mut default_path = false;
    while let Some(word) = cmd.argv.get(idx) {
        let Some(flags) = word.strip_prefix('-').filter(|f| !f.is_empty()) else {
            break;
        };
        if !flags.chars().all(|c| matches!(c, 'p' | 'v' | 'V')) {
            break;
        }
        if flags.contains(['v', 'V']) {
            return None;
        }
        default_path = true;
        idx += 1;
    }
    if idx >= cmd.argv.len() {
        return None; // bare `command`, or `command -p` with nothing to run
    }
    let mut inner = cmd.clone();
    inner.argv.drain(..idx);
    if default_path {
        match crate::builtins::resolve_in_default_path(&inner.argv[0]) {
            // A builtin still wins over a default-path file, same as bash
            // (`command -p echo` runs the builtin) — leave it alone.
            _ if builtins::is_builtin(&inner.argv[0]) => {}
            Some(path) => inner.argv[0] = path.display().to_string(),
            // Leave the name untouched: with no `/` and no default-path
            // hit, the spawn fails with the ordinary "command not found"
            // (127) path.
            None if !inner.argv[0].contains('/') => {
                inner.argv[0] = format!("{}/", inner.argv[0]); // guaranteed NotFound, skips PATH search
            }
            None => {}
        }
    }
    Some(inner)
}

/// Expand and run a single pipeline in the foreground.
fn run_foreground(raw: &RawPipeline) -> Result<i32, String> {
    let result = run_foreground_dispatch(raw);
    #[cfg(unix)]
    close_pending_proc_subs();
    result
}

/// The actual dispatch `run_foreground` wraps — pulled out so every one of
/// its several return points (builtin, function call, `command` bypass,
/// job-control/plain-runner fallback) is covered by a *single*
/// `close_pending_proc_subs` call in the wrapper above, rather than one
/// per branch.
fn run_foreground_dispatch(raw: &RawPipeline) -> Result<i32, String> {
    crate::vars::reset_last_subst_status();
    let pipeline = crate::expand::expand(raw)?;
    trace_pipeline(&pipeline);

    if assignment_only(&pipeline) {
        apply_assignments(&pipeline)?;
        // POSIX: a variable-assignment-only command takes the exit status of
        // the last command substitution performed while expanding it, rather
        // than always 0 (`run_andor` sets `$?` from whatever we return here).
        return Ok(crate::vars::take_last_subst_status().unwrap_or(0));
    }

    // The sole-compound case (`run_pipeline_node`) is intercepted before this
    // function is ever called, so a single-stage pipeline reaching here is
    // always `Stage::Simple`.
    if let [Stage::Simple(cmd)] = pipeline.commands.as_slice() {
        if let Some(inner) = command_bypass(cmd) {
            // `command name [args...]`: run bypassing function lookup — the
            // whole point of `command` in this form (C12) — otherwise
            // proceeding exactly as a plain simple command would.
            if inner.argv.first().is_some_and(|name| builtins::is_builtin(name)) {
                return run_builtin_foreground(&inner);
            }
            let pipeline = Pipeline { commands: vec![Stage::Simple(inner)] };
            #[cfg(unix)]
            {
                return crate::job::run_foreground(&pipeline);
            }
            #[cfg(not(unix))]
            {
                return run(&pipeline, false).map(|(status, _)| status);
            }
        }
        // A defined function shadows external commands (but not builtins).
        if cmd.argv.first().is_some_and(|name| crate::func::exists(name)) {
            return call_function(&cmd.argv);
        }
        if cmd.argv.first().is_some_and(|name| builtins::is_builtin(name)) {
            return run_builtin_foreground(cmd);
        }
    }

    #[cfg(unix)]
    {
        crate::job::run_foreground(&pipeline)
    }
    #[cfg(not(unix))]
    {
        run(&pipeline, false).map(|(status, _)| status)
    }
}

/// Run a builtin as the shell's sole foreground command, honoring any
/// redirects attached to it (`echo hi > f`, `pwd 2>e`, `cd < f`, …). Builtins
/// write via `println!`/`eprintln!` straight to the process's real stdio, so
/// unlike an external command (whose redirects `build_stage` wires into a
/// *child's* fds) a builtin's redirects have to be applied to the shell's own
/// fds — temporarily, for the duration of the call.
fn run_builtin_foreground(cmd: &Command) -> Result<i32, String> {
    #[cfg(unix)]
    {
        let mut guard = redirect_stdio(&cmd.redirects, cmd.heredoc.as_deref())?;
        let status = dispatch_builtin(cmd);
        // The no-command form of `exec` (`exec > file`, `exec 3<&-`, bare
        // `exec`) exists specifically to make its redirects permanent — the
        // opposite of every other builtin, whose redirects are always
        // scoped to just that one call. Disarming the guard here (instead
        // of letting it restore on drop, as usual) is what makes that happen.
        if cmd.argv.len() == 1 && cmd.argv.first().map(String::as_str) == Some("exec") {
            guard.disarm();
        }
        Ok(status)
    }
    #[cfg(not(unix))]
    {
        // No raw `dup2` equivalent in play here, so a builtin's redirects are
        // silently ignored on this platform (see docs/ARCHITECTURE.md).
        Ok(dispatch_builtin(cmd))
    }
}

/// Run a builtin from its expanded `Command` — every builtin but `local`/
/// `declare` just runs on `cmd.argv` (plain strings) as always; those two
/// are the exception, since an array or associative-array literal
/// (`local arr=(a b c)`, `declare -A arr=([k]=v ...)`) can't survive being
/// flattened into `Vec<String>` argv at all (see `Command::local_decls`'s
/// own doc comment and `expand::expand_simple`, which builds it).
fn dispatch_builtin(cmd: &Command) -> i32 {
    match cmd.argv.first().map(String::as_str) {
        Some("local") => builtins::local_from_decls(&cmd.local_decls, cmd.decl_attrs),
        // `typeset` is ksh/zsh's own spelling of `declare` (C49) — ksh93
        // has *only* typeset; bash and zsh accept both as synonyms.
        Some("declare") | Some("typeset") => builtins::declare_from_decls(&cmd.local_decls, cmd.decl_attrs),
        Some("readonly") => builtins::readonly_from_decls(&cmd.local_decls, cmd.decl_attrs),
        _ => builtins::try_run(&cmd.argv).unwrap_or(1),
    }
}

/// Temporarily redirect the shell's own fd 0/1/2 to match `redirects` (plus
/// `heredoc`, if any, which always wins for fd 0 — same ordering
/// `build_stage` uses), restoring the originals when the returned guard
/// drops. Used both for a lone builtin (`run_builtin_foreground`) and for a
/// whole compound command run in-process (`run_compound_with_redirects`) —
/// forked pipeline stages instead use this same logic but discard the guard,
/// since a forked child never needs to restore anything (see
/// `job::spawn_compound_stage`). Unix only: needs a real `dup`/`dup2` to save
/// and restore descriptors that outlive this call.
#[cfg(unix)]
pub(crate) fn redirect_stdio(redirects: &[Redirect], heredoc: Option<&str>) -> Result<StdioGuard, String> {
    use std::os::unix::io::AsRawFd;

    let mut guard = StdioGuard { saved: Vec::new() };

    let redirect_to = |guard: &mut StdioGuard, target: i32, source: File| -> Result<(), String> {
        if !guard.saved.iter().any(|(fd, _)| *fd == target) {
            let saved = unsafe { libc::dup(target) };
            if saved == -1 {
                return Err(std::io::Error::last_os_error().to_string());
            }
            guard.saved.push((target, saved));
        }
        if unsafe { libc::dup2(source.as_raw_fd(), target) } == -1 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        // A freshly opened file's own fd is often *exactly* `target` (its
        // lowest-available-fd allocation landing on the very number we're
        // redirecting to) — overwhelmingly likely for fd 3+ specifically,
        // since 0/1/2 are essentially always already open in a real
        // process but 3+ usually isn't. `dup2` on identical fds is a
        // defined no-op (POSIX: neither closes nor duplicates anything),
        // so in that case `source` *is* the live redirect now — letting it
        // drop normally would close the very fd this call just set up.
        // Forget it instead; ownership has effectively passed to the fd
        // table entry itself.
        if source.as_raw_fd() == target {
            std::mem::forget(source);
        }
        Ok(())
    };

    for r in redirects {
        match r {
            Redirect::File { fd, file, mode } => {
                let f = match mode {
                    RedirMode::Read => File::open(file).map_err(|e| format!("{file}: {e}"))?,
                    RedirMode::Write | RedirMode::Clobber | RedirMode::Append => open_write(file, *mode)?,
                };
                // Any fd, not just 0/1/2 — `StdioGuard.saved` is keyed by
                // plain `i32`, so no fd is special-cased here (see C38).
                redirect_to(&mut guard, *fd as i32, f)?;
            }
            Redirect::Both { file, append } => {
                let f = open_write(file, if *append { crate::parser::RedirMode::Append } else { crate::parser::RedirMode::Write })?;
                let g = f.try_clone().map_err(|e| e.to_string())?;
                redirect_to(&mut guard, 1, f)?;
                redirect_to(&mut guard, 2, g)?;
            }
            Redirect::Dup { fd, target } => {
                // `target` is already live on its own fd (possibly redirected
                // by an earlier entry in this same list) — dup straight from
                // it, whatever fd it actually is. No freshly opened `File`
                // involved here (unlike the `File` arm above), so no
                // self-dup/forget concern: `target` is a plain existing fd
                // number, not something we'd otherwise drop.
                let dst = *fd as i32;
                let src = *target as i32;
                if !guard.saved.iter().any(|(fd, _)| *fd == dst) {
                    let saved = unsafe { libc::dup(dst) };
                    if saved == -1 {
                        return Err(std::io::Error::last_os_error().to_string());
                    }
                    guard.saved.push((dst, saved));
                }
                if unsafe { libc::dup2(src, dst) } == -1 {
                    return Err(std::io::Error::last_os_error().to_string());
                }
            }
        }
    }

    // A here-document always wins for fd 0, same ordering `build_stage` uses.
    // We aren't forking here, so there's no `Child::stdin` to write into
    // after spawn (`feed_heredoc`'s approach) — instead materialize a real
    // pipe and feed it from a background thread (so a body bigger than the
    // pipe buffer can't deadlock), then dup2 its read end onto fd 0 through
    // the same tracked `redirect_to`, so it's restored like any other.
    //
    // Both ends get `CLOEXEC`: if the compound's body spawns a real child
    // (an external command) before the writer thread finishes, that child
    // would otherwise inherit its own copy of the write end (fork/exec
    // inherits open fds by default) and keep it open past the thread's own
    // close — the reader would then never see EOF. `dup2` onto fd 0 always
    // clears `CLOEXEC` on the *new* descriptor regardless, so this doesn't
    // stop the child from reading its inherited fd 0 normally.
    if let Some(body) = heredoc {
        let (read, write) = make_pipe()?;
        set_cloexec(&read)?;
        set_cloexec(&write)?;
        let body = body.to_string();
        std::thread::spawn(move || {
            use std::io::Write;
            let mut write = write;
            let _ = write.write_all(body.as_bytes());
        });
        redirect_to(&mut guard, 0, read)?;
    }

    Ok(guard)
}

/// Mark a descriptor close-on-exec, so a forked child that goes on to `exec`
/// doesn't inherit it.
#[cfg(unix)]
fn set_cloexec(f: &File) -> Result<(), String> {
    use std::os::unix::io::AsRawFd;
    let fd = f.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags == -1 || unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } == -1 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok(())
}

/// Restores the shell's original fd 0/1/2 (saved by `redirect_stdio`) when
/// dropped — including on an early return via `?`, so a redirect that fails
/// partway through never leaves the shell talking to the wrong fd.
#[cfg(unix)]
pub(crate) struct StdioGuard {
    saved: Vec<(i32, i32)>,
}

#[cfg(unix)]
impl StdioGuard {
    /// Make the current redirects permanent: just close the saved originals
    /// instead of restoring them on drop. Used by the no-command form of
    /// `exec` (`exec > file`, bare `exec`), the one case where a builtin's
    /// redirects are meant to outlive the call.
    fn disarm(&mut self) {
        for (_, saved) in self.saved.drain(..) {
            unsafe {
                libc::close(saved);
            }
        }
    }
}

#[cfg(unix)]
impl Drop for StdioGuard {
    fn drop(&mut self) {
        for (fd, saved) in self.saved.drain(..) {
            unsafe {
                libc::dup2(saved, fd);
                libc::close(saved);
            }
        }
    }
}

/// Run an already-expanded pipeline in the background. Unix only.
#[cfg(unix)]
fn run_background(pipeline: &Pipeline) -> Result<(), String> {
    crate::job::run_background(pipeline)
}
#[cfg(not(unix))]
fn run_background(_pipeline: &Pipeline) -> Result<(), String> {
    Err("background jobs are not supported on this platform".into())
}

/// Run a command list and return its stdout — the engine behind `$(...)`.
/// Substitutions are synchronous: every job runs in the foreground with a plain
/// spawn-and-wait (no job control), and the `&` background marker is ignored.
pub fn capture_list(list: &CommandList) -> Result<String, String> {
    let mut out = String::new();
    let mut status = 0;
    for job in &list.jobs {
        status = capture_andor(&job.list, &mut out)?;
    }
    // POSIX: a variable-assignment-only command (`x=$(...)`) takes the exit
    // status of the last command substitution performed while expanding it,
    // rather than always 0 — this is how a caller (an enclosing
    // `capture_pipeline`/`run_foreground`) finds out this substitution ran,
    // and what its own last job's status was.
    crate::vars::set_last_subst_status(status);
    Ok(out)
}

fn capture_andor(list: &AndOrList, out: &mut String) -> Result<i32, String> {
    let mut status = capture_pipeline(&list.first, out)?;
    for (connector, raw) in &list.rest {
        if should_run(*connector, status) {
            status = capture_pipeline(raw, out)?;
        }
    }
    Ok(status)
}

/// Updates `$?` after every pipeline (like `run_andor` does for the main
/// runtime), so e.g. `$(false; echo $?)` sees the right value *within* the
/// substitution — this was missing before, leaving `$?` at whatever it was
/// from outside the substitution instead of tracking its own jobs.
fn capture_pipeline(raw: &RawPipeline, out: &mut String) -> Result<i32, String> {
    if let [RawCommand::Compound(rc)] = raw.commands.as_slice() {
        let (status, captured) = capture_compound(rc)?;
        let status = negate_if(raw.negated, status);
        out.push_str(&captured);
        crate::vars::set_last_status(status);
        return Ok(status);
    }

    let result = capture_pipeline_expanded(raw, out).map(|s| {
        let s = negate_if(raw.negated, s);
        if raw.negated {
            // `capture_pipeline_expanded` set `$?` before the negation —
            // put the negated value there too (`$(! true; echo $?)` → 1).
            crate::vars::set_last_status(s);
        }
        s
    });
    #[cfg(unix)]
    close_pending_proc_subs();
    result
}

/// The expanded-pipeline half of `capture_pipeline`, wrapped by it so its
/// two return points both get a single `close_pending_proc_subs` call.
fn capture_pipeline_expanded(raw: &RawPipeline, out: &mut String) -> Result<i32, String> {
    crate::vars::reset_last_subst_status();
    let pipeline = crate::expand::expand(raw)?;
    trace_pipeline(&pipeline);
    if assignment_only(&pipeline) {
        apply_assignments(&pipeline)?;
        let status = crate::vars::take_last_subst_status().unwrap_or(0);
        crate::vars::set_last_status(status);
        return Ok(status);
    }
    // A sole builtin or shell function: `run` below spawns externals only,
    // which used to mean `$(umask)`, `$(type x)`, `$(ulimit -n)`, and
    // `$(myfunc)` all failed with "command not found" unless an external
    // twin happened to exist on PATH (found while landing C46, whose
    // `$(ulimit -n)` is exactly this shape). Capture these in-process via
    // the same fork-with-fd1-on-a-pipe scheme `capture_compound` uses —
    // a real subshell, which is also bash's own semantics for `$(...)`
    // (its side effects, `$(cd /tmp)` included, don't escape).
    #[cfg(unix)]
    if let [Stage::Simple(cmd)] = pipeline.commands.as_slice()
        && cmd.argv.first().is_some_and(|n| crate::func::exists(n) || builtins::is_builtin(n))
    {
        let (status, captured) = capture_shell_command(cmd)?;
        out.push_str(&captured);
        crate::vars::set_last_status(status);
        return Ok(status);
    }
    let (status, captured) = run(&pipeline, true)?;
    out.push_str(&captured);
    crate::vars::set_last_status(status);
    Ok(status)
}

/// Capture a sole compound command's (`if`/`while`/`(...)`/…) output and exit
/// status — e.g. `$(if true; then echo yes; fi)`. A compound never goes
/// through `build_stage`/`Stdio`; it runs in-process via `run_compound`,
/// recursing into ordinary builtins/external spawns as it goes. To capture
/// *all* of that (including builtins, which write straight to the process's
/// real stdout), fork (Unix only, mirroring `run_subshell_forked`) and
/// redirect the *child's* fd 1 to a pipe we own before running the compound
/// there — everything the child writes, in-process or via a further spawn
/// that inherits its stdout, ends up in that pipe. Any redirects trailing the
/// compound's own close (`$(while …; done < file)`) are applied *after* that
/// baseline, so — same precedence as an ordinary command inside `$(...)` —
/// an explicit one targeting fd 1 overrides the capture pipe rather than
/// being captured. This only handles a pipeline that *is* a single compound;
/// one as one stage among several in a larger pipeline remains the
/// documented, separate limitation.
#[cfg(unix)]
fn capture_compound(rc: &RawCompound) -> Result<(i32, String), String> {
    use std::os::unix::io::AsRawFd;

    let (redirects, heredoc) = crate::expand::expand_redirects(&rc.redirects)?;
    let (read, write) = make_pipe()?;
    match unsafe { libc::fork() } {
        -1 => Err(std::io::Error::last_os_error().to_string()),
        0 => {
            // Child: point fd 1 at the pipe's write end; neither original fd
            // is needed once that's done.
            unsafe {
                libc::dup2(write.as_raw_fd(), 1);
            }
            drop(write);
            drop(read);
            match redirect_stdio(&redirects, heredoc.as_deref()) {
                // Never restore — this child exits right after running the
                // compound, so there's nothing to give the fds back to.
                Ok(guard) => std::mem::forget(guard),
                Err(e) => {
                    eprintln!("rush: {e}");
                    crate::trap::exit_shell(1);
                }
            }
            let status = run_compound(&rc.compound).unwrap_or(1);
            crate::trap::exit_shell(status);
        }
        pid => {
            // Parent: only reads. Drop our copy of the write end *before*
            // reading, or read_to_string blocks forever waiting for an EOF
            // that can't come while a write end is still open here too (the
            // same deadlock `build_stage`'s doc comment warns about).
            drop(write);
            let mut captured = String::new();
            let mut read = read;
            read.read_to_string(&mut captured).map_err(|e| e.to_string())?;
            loop {
                let mut status: libc::c_int = 0;
                if unsafe { libc::waitpid(pid, &mut status, 0) } != -1 {
                    return Ok((crate::job::exit_code(status), captured));
                }
                if std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
                    return Err(std::io::Error::last_os_error().to_string());
                }
            }
        }
    }
}

/// No `fork` on this platform (see docs/ARCHITECTURE.md's Windows note) — a
/// compound can't be captured, same as it already couldn't be part of a
/// pipeline here.
#[cfg(not(unix))]
fn capture_compound(_rc: &RawCompound) -> Result<(i32, String), String> {
    Err("compound commands cannot be captured on this platform".into())
}

/// Capture a sole builtin's or shell function's output for `$(...)` — the
/// in-process analogue of `capture_compound`, sharing its exact
/// fork/pipe/waitpid scheme (see that function's doc comment for the
/// mechanics, including why the parent must drop its write end before
/// reading). The child runs the builtin via the ordinary
/// `run_builtin_foreground` path (so the builtin's own redirects apply as
/// usual, after fd 1 already points at the capture pipe) or the function
/// via `call_function`, then exits with its status.
#[cfg(unix)]
fn capture_shell_command(cmd: &Command) -> Result<(i32, String), String> {
    use std::os::unix::io::AsRawFd;

    let (read, write) = make_pipe()?;
    match unsafe { libc::fork() } {
        -1 => Err(std::io::Error::last_os_error().to_string()),
        0 => {
            unsafe {
                libc::dup2(write.as_raw_fd(), 1);
            }
            drop(write);
            drop(read);
            let status = if cmd.argv.first().is_some_and(|n| crate::func::exists(n)) {
                call_function(&cmd.argv).unwrap_or(1)
            } else {
                run_builtin_foreground(cmd).unwrap_or(1)
            };
            crate::trap::exit_shell(status);
        }
        pid => {
            drop(write);
            let mut captured = String::new();
            let mut read = read;
            read.read_to_string(&mut captured).map_err(|e| e.to_string())?;
            loop {
                let mut status: libc::c_int = 0;
                if unsafe { libc::waitpid(pid, &mut status, 0) } != -1 {
                    return Ok((crate::job::exit_code(status), captured));
                }
                if std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
                    return Err(std::io::Error::last_os_error().to_string());
                }
            }
        }
    }
}

/// Process substitution — `<(cmd)` (read side) or `>(cmd)` (write side).
/// Forks `cmd` hooked up to one end of a real pipe and returns a
/// `/dev/fd/<n>` path for the *other* end, which the shell process itself
/// keeps open (verified directly: this is exactly how real bash implements
/// it on Linux — a genuine pipe plus `/dev/fd`'s magic-symlink-to-an-open-fd
/// trick, not a named FIFO, which bash only falls back to on platforms
/// without `/dev/fd` at all — not a concern here).
///
/// Unlike `$(...)`, this never blocks waiting for `cmd` to finish (verified
/// directly: `diff <(sleep 1; echo a) <(sleep 1; echo b)` takes ~1s total,
/// not ~2s serialized, and a slow substitution's output can legitimately
/// arrive *after* the main command has already finished). The kept-open fd
/// must survive, unclosed, until *after* the caller has finished spawning
/// whatever command this substitution's path was expanded into — only then
/// does the spawned child actually inherit it (fork+exec inherits open,
/// non-`CLOEXEC` fds unchanged, and `make_pipe`'s raw `libc::pipe` already
/// doesn't set `CLOEXEC`, so no extra bookkeeping is needed there) — so the
/// `File` is stashed in `PENDING_PROC_SUBS` rather than dropped here, for
/// `close_pending_proc_subs` to close once that's safe to do.
#[cfg(unix)]
pub(crate) fn process_substitute(src: &str, write_side: bool) -> Result<String, String> {
    use std::os::unix::io::AsRawFd;

    let list = crate::parser::parse(src).map_err(|e| e.to_string())?;
    let (read, write) = make_pipe()?;
    match unsafe { libc::fork() } {
        -1 => Err(std::io::Error::last_os_error().to_string()),
        0 => {
            // Rust's runtime sets `SIGPIPE` to `SIG_IGN` at startup, so a
            // write to a closed pipe surfaces as an ordinary `Err` instead
            // of the signal killing the process outright — which
            // `println!`/`print!` then *panic* on, dumping a backtrace
            // rather than exiting quietly. A real, unread `<(cmd)` is an
            // entirely normal thing to write (verified directly: real
            // bash's own substituted commands just get `SIGPIPE`d and
            // silently disappear the same way `yes | head -1` disappears
            // once `head` stops reading — no error, nothing printed).
            // `std::process::Command` resets this for a real spawned
            // child automatically; this child runs the parsed command
            // list in-process instead, so it needs the same reset by hand.
            unsafe {
                libc::signal(libc::SIGPIPE, libc::SIG_DFL);
            }
            // Child: `>(cmd)` reads from the pipe (its stdin); `<(cmd)`
            // writes to it (its stdout). Neither original fd is needed
            // once dup2'd onto the right one.
            let (use_end, target_fd) = if write_side { (&read, 0) } else { (&write, 1) };
            unsafe {
                libc::dup2(use_end.as_raw_fd(), target_fd);
            }
            drop(read);
            drop(write);
            let status = run_list(&list).unwrap_or(1);
            crate::trap::exit_shell(status);
        }
        pid => {
            // Parent: drop the end `cmd`'s own copy uses, keep the other —
            // its fd number becomes the exposed `/dev/fd/<n>` path. `$!`
            // reflects this pid, matching real bash exactly (verified
            // directly: `: <(echo hi); echo $!` prints a real, distinct
            // pid each time) — it's deliberately *not* added to the job
            // table, though: real bash's own `jobs -l` doesn't list a
            // process substitution either, even though `$!`/`wait $!` can
            // still reach it directly by pid.
            let (keep, other) = if write_side { (write, read) } else { (read, write) };
            drop(other);
            let fd = keep.as_raw_fd();
            crate::vars::set_last_bg_pid(pid);
            PENDING_PROC_SUBS.with(|p| p.borrow_mut().push((keep, pid)));
            Ok(format!("/dev/fd/{fd}"))
        }
    }
}

/// No `fork` on this platform.
#[cfg(not(unix))]
pub(crate) fn process_substitute(_src: &str, _write_side: bool) -> Result<String, String> {
    Err("process substitution is not supported on this platform".into())
}

#[cfg(unix)]
thread_local! {
    /// Process-substitution pipe fds opened while expanding the pipeline
    /// that's currently being (or was just) spawned, each paired with its
    /// child's pid. Kept alive here — not dropped at the point of creation
    /// — specifically so a spawned child inherits the fd; see
    /// `process_substitute`'s own doc comment.
    static PENDING_PROC_SUBS: std::cell::RefCell<Vec<(File, i32)>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Close every process-substitution fd opened while expanding the pipeline
/// that was just spawned, and best-effort (non-blocking) reap its child so
/// it doesn't linger as a zombie — called once spawning is done, at each of
/// the handful of places a whole pipeline gets run (`run_foreground`,
/// backgrounding, and `$(...)` capture), covering every path a process
/// substitution's word could have been expanded into (a builtin, a
/// function call, or a real spawned child) without needing to duplicate
/// this at each of *those* individually. A non-blocking reap only —
/// matching real bash, which doesn't wait for these either; anything not
/// yet exited just gets reaped later (the ordinary background-job sweep,
/// or by `init` once rush itself exits) rather than blocking here.
#[cfg(unix)]
fn close_pending_proc_subs() {
    let pending = PENDING_PROC_SUBS.with(|p| std::mem::take(&mut *p.borrow_mut()));
    for (file, pid) in pending {
        drop(file);
        let mut status: libc::c_int = 0;
        unsafe {
            libc::waitpid(pid, &mut status, libc::WNOHANG);
        }
    }
}

/// Plain spawn-and-wait runner: used for capture, and as the foreground runner
/// on non-Unix platforms. Returns `(exit status, captured stdout)`; the string
/// is empty unless `capture` is set.
fn run(pipeline: &Pipeline, capture: bool) -> Result<(i32, String), String> {
    let n = pipeline.commands.len();
    let mut children: Vec<Child> = Vec::with_capacity(n);
    // Stdin for the next stage: the read end of the previous stage's pipe.
    let mut prev_stdout: Option<Stdio> = None;
    let mut captured = String::new();

    for (i, stage) in pipeline.commands.iter().enumerate() {
        let cmd = match stage {
            Stage::Simple(cmd) => cmd,
            // Unix's job-control runner (`job::spawn_pipeline`) can fork a
            // compound stage; this plain runner (capture, and the foreground
            // runner off Unix) can't — no `fork` available off Unix, and
            // capturing a compound that's one stage among several remains a
            // narrower, separate limitation from the sole-compound case
            // `capture_compound` already handles.
            Stage::Compound(_) => {
                return Err(
                    "a compound command as one stage of a multi-command pipeline isn't \
                     supported here (capturing output, or this platform's foreground runner)"
                        .into(),
                );
            }
        };
        let is_last = i == n - 1;
        let (mut command, real_pipe_read) = build_stage(cmd, prev_stdout.take(), is_last, capture)?;

        let mut child = match command.spawn() {
            Ok(c) => c,
            // A standalone command (not one stage among several): nothing
            // else to unwind or wait for, so there's a real, simple status
            // to report directly (C37) instead of aborting the whole
            // script — matching real bash's own "command not found"/status
            // 127 (or 126 for "found but couldn't run"). A failing stage
            // *within* a multi-command pipeline keeps today's existing
            // behavior — see the identical, more-detailed comment in
            // `job::spawn_pipeline` for why that narrower case isn't
            // covered here too.
            Err(e) if i == 0 && is_last => {
                return Ok((spawn_failure_status(&cmd.argv[0], &e), captured));
            }
            Err(e) => return Err(format!("{}: {e}", cmd.argv[0])),
        };
        // `Command` keeps any file-backed `Stdio` (our manually-made pipe
        // included) alive in its own fields until dropped. For an ordinary
        // file that's harmless, but a lingering parent-side copy of a pipe's
        // write end stops the reader below from ever seeing EOF — so drop it
        // now, before reading, not at the end of the loop iteration.
        drop(command);
        feed_heredoc(&mut child, cmd);

        if let Some(read) = real_pipe_read {
            // `2>&1` forced a real pipe (see `build_stage`): its read end is
            // the next stage's stdin, or — on the last, captured stage — what
            // we read stdout+stderr from directly.
            if is_last && capture {
                let mut out = read;
                out.read_to_string(&mut captured).map_err(|e| e.to_string())?;
            } else {
                prev_stdout = Some(Stdio::from(read));
            }
        } else if !is_last {
            prev_stdout = child.stdout.take().map(Stdio::from);
        } else if capture {
            if let Some(mut out) = child.stdout.take() {
                out.read_to_string(&mut captured).map_err(|e| e.to_string())?;
            }
        }
        children.push(child);
    }

    let mut statuses = Vec::with_capacity(n);
    for mut child in children {
        let exit = child.wait().map_err(|e| e.to_string())?;
        statuses.push(exit.code().unwrap_or(1));
    }

    Ok((pipeline_status(&statuses), captured))
}

/// The exit status reported for a whole pipeline of `stage_statuses`
/// (stage order, first to last): without `set -o pipefail`, always the last
/// stage's own status, matching every shell; with it, the *rightmost*
/// non-zero status among all stages, or 0 if every stage succeeded —
/// verified directly against real bash (not "the first failure", nor "any
/// failure" — specifically the one closest to the end).
pub(crate) fn pipeline_status(stage_statuses: &[i32]) -> i32 {
    if crate::vars::pipefail() {
        stage_statuses.iter().rev().find(|&&s| s != 0).copied().unwrap_or(0)
    } else {
        *stage_statuses.last().unwrap_or(&0)
    }
}

/// Resolve `program` to what should actually be `exec`'d — deliberately
/// *not* just handing the bare name to `Command::new` and letting its own
/// built-in search find it, which always consults the real OS environment
/// variable directly, bypassing rush's own (possibly `unset`-modified)
/// `$PATH` entirely (C40 — the same root cause C36 fixed for `command -v`/
/// `type`/`hash`'s own lookups, but for actually spawning a command
/// instead). A direct path (containing `/`) is used as-is — its own
/// `spawn()` error, if any, is classified by `spawn_failure_status` (C37)
/// exactly as before, unaffected by this. A bare name that resolves via
/// `builtins::resolve_in_path` (rush's own `$PATH`) is spawned by that
/// resolved, absolute path, so `Command`'s own search never runs at all.
/// A bare name that *doesn't* resolve there gets a trailing `/` appended —
/// containing a `/`, so `Command` treats it as a direct path too (skipping
/// its own search), and guaranteed to fail with `NotFound` (verified
/// directly) — routing it through the exact same not-found handling a
/// missing command already gets, without a second error path to keep
/// consistent with it.
fn resolve_program(program: &str) -> String {
    if program.contains('/') {
        return program.to_string();
    }
    match crate::builtins::resolve_in_path(program) {
        Some(path) => path.to_string_lossy().into_owned(),
        None => format!("{program}/"),
    }
}

/// Build the `std::process::Command` for one pipeline stage: program, args, and
/// stdio. An explicit `<`/`>`/`>>` redirect wins over pipe wiring; otherwise a
/// non-final stage (or any stage when capturing) gets a piped stdout. Shared by
/// the plain runner and the Unix job runner.
/// Second return value: on Unix, `Some(read_end)` if `2>&1` forced us to
/// materialize a real pipe for fd 1 (see `clone_or_materialize`) — the caller
/// must use it as the next stage's stdin (or read it directly, when capturing)
/// instead of taking `child.stdout`.
/// The value a command-prefix assignment (`NAME=value cmd`) contributes to
/// the spawned child's own environment — `None` for an array (see
/// `build_stage`'s own comment). `+=` reads the *shell's* current value (if
/// any) and appends to it, without touching the shell's own variable table
/// — prefix assignments never persist past the one command, matching the
/// plain `=` case's existing behavior.
fn prefix_env_value(name: &str, op: &crate::vars::AssignOp) -> Option<String> {
    use crate::vars::{AssignOp, AssignValue};
    match op {
        AssignOp::Set(AssignValue::Scalar(v)) => Some(v.clone()),
        AssignOp::Append(AssignValue::Scalar(v)) => {
            Some(format!("{}{v}", crate::vars::get(name).unwrap_or_default()))
        }
        // An array or assoc array (whole or one element) isn't
        // representable in a child's environment — same reasoning as
        // `exported()`'s own array skip.
        AssignOp::Set(AssignValue::Array(_) | AssignValue::Assoc(_))
        | AssignOp::Append(AssignValue::Array(_) | AssignValue::Assoc(_))
        | AssignOp::SetKey(..)
        | AssignOp::AppendKey(..) => None,
    }
}

pub(crate) fn build_stage(
    cmd: &Command,
    stdin_src: Option<Stdio>,
    is_last: bool,
    capture: bool,
) -> Result<(OsCommand, Option<File>), String> {
    let program = cmd
        .argv
        .first()
        .ok_or_else(|| "empty command".to_string())?;
    let mut command = OsCommand::new(resolve_program(program));
    command.args(&cmd.argv[1..]);

    // Seed the environment: exported shell variables first, then this command's
    // own `NAME=value` prefixes (which override). An array-valued prefix
    // (`arr=(a b c) cmd`) is silently skipped — there's no portable
    // representation for an array as an environment variable, same as
    // `exported()` already skips one held in the shell's own table.
    //
    // `env_clear` first (C40): `Command` otherwise inherits the real OS
    // environment by default, and `.envs()` only adds/overrides on top of
    // that — never removes — so `unset`-ing an inherited/exported name
    // would leave a spawned child still seeing its original value. Since
    // `main.rs` seeds `vars`'s own table from that same inherited
    // environment at startup (C36), `vars::exported()` is already a
    // complete, accurate picture of what a child's environment should
    // be — rebuilding it from scratch here, rather than layering onto the
    // default inheritance, is what makes `unset` actually take effect.
    command.env_clear();
    command.envs(crate::vars::exported());
    for (name, op) in &cmd.assignments {
        // A prefix assignment naming a readonly variable errors but still
        // runs the command, with the assignment dropped (the child sees
        // the readonly's exported value, if any — not the new one) —
        // verified directly against real bash (C45).
        if crate::vars::is_readonly(name) {
            eprintln!("rush: {name}: readonly variable");
            continue;
        }
        if let Some(value) = prefix_env_value(name, op) {
            command.env(name, value);
        }
    }

    // Resolve the three standard descriptors. fd1 defaults to a pipe when this
    // stage feeds another (or is being captured); the redirects below override
    // in source order, so `> f 2>&1` sends both to `f`.
    let mut stdin_sink: Option<Stdio> = stdin_src;
    let mut stdout_sink = if !is_last || capture { Sink::Pipe } else { Sink::Inherit };
    let mut stderr_sink = Sink::Inherit;
    let mut real_pipe_read: Option<File> = None;
    // Any fd other than 0/1/2 (`cmd 3>file`, `cmd 4<&3`) — `Command` only
    // exposes `.stdin()`/`.stdout()`/`.stderr()`, so these are applied via a
    // `pre_exec` `dup2` sequence instead (see below), in the same source
    // order they appear in `cmd.redirects` so a later entry can reference an
    // earlier one (`3>file 4>&3`).
    let mut extra_fds: Vec<FdAction> = Vec::new();

    for r in &cmd.redirects {
        match r {
            Redirect::File { fd, file, mode } => {
                let f = match mode {
                    RedirMode::Read => File::open(file).map_err(|e| format!("{file}: {e}"))?,
                    RedirMode::Write | RedirMode::Clobber | RedirMode::Append => open_write(file, *mode)?,
                };
                match fd {
                    0 => stdin_sink = Some(Stdio::from(f)),
                    1 => stdout_sink = Sink::File(f),
                    2 => stderr_sink = Sink::File(f),
                    _ => extra_fds.push(FdAction::Open(f, *fd)),
                }
            }
            Redirect::Both { file, append } => {
                let f = open_write(file, if *append { crate::parser::RedirMode::Append } else { crate::parser::RedirMode::Write })?;
                let g = f.try_clone().map_err(|e| e.to_string())?;
                stdout_sink = Sink::File(f);
                stderr_sink = Sink::File(g);
            }
            Redirect::Dup { fd, target } => {
                if matches!(fd, 0..=2) && matches!(target, 0..=2) {
                    let cloned = if *target == 2 {
                        clone_or_materialize(&mut stderr_sink, &mut real_pipe_read)?
                    } else {
                        clone_or_materialize(&mut stdout_sink, &mut real_pipe_read)?
                    };
                    match fd {
                        2 => stderr_sink = cloned,
                        _ => stdout_sink = cloned,
                    }
                } else {
                    // Either side is 3+: nothing to clone from a `Sink` (that
                    // machinery only tracks 0/1/2) — `target`'s own value is
                    // whatever the child's fd table holds for it by this
                    // point in the sequence (Rust's own stdio setup for
                    // 0/1/2, or an earlier entry here for 3+), so a plain
                    // `dup2(target, fd)` at `pre_exec` time is enough.
                    extra_fds.push(FdAction::Dup { source: *target, dest: *fd });
                }
            }
        }
    }

    // A here-document feeds stdin from a pipe we write after spawn.
    if cmd.heredoc.is_some() {
        stdin_sink = Some(Stdio::piped());
    }

    if let Some(s) = stdin_sink {
        command.stdin(s);
    }
    if let Some(s) = stdout_sink.into_stdio()? {
        command.stdout(s);
    }
    if let Some(s) = stderr_sink.into_stdio()? {
        command.stderr(s);
    }

    #[cfg(unix)]
    if !extra_fds.is_empty() {
        use std::os::unix::io::AsRawFd;
        use std::os::unix::process::CommandExt;
        // SAFETY: the closure only calls `dup2`/inspects `errno` — both
        // async-signal-safe, the requirement `pre_exec` documents. It owns
        // `extra_fds` (including any opened `File`s), keeping their fds open
        // through `fork()` regardless of what the parent does with its own
        // copies afterward (matching the existing pattern already used for
        // pipeline fds elsewhere in this shell).
        unsafe {
            command.pre_exec(move || {
                for action in &extra_fds {
                    let (source, dest) = match action {
                        FdAction::Open(f, dest) => (f.as_raw_fd(), *dest),
                        FdAction::Dup { source, dest } => (*source as i32, *dest),
                    };
                    if libc::dup2(source, dest as i32) == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }
    }
    #[cfg(not(unix))]
    let _ = extra_fds; // No raw `dup2` equivalent off Unix — same platform limit `redirect_stdio` documents.

    Ok((command, real_pipe_read))
}

/// One `pre_exec`-time step for setting up an fd other than 0/1/2 in a
/// spawned child — see `build_stage`'s own doc comment on `extra_fds`.
/// Built on every platform (it's simplest to collect while walking
/// `cmd.redirects` uniformly), but only ever read under `#[cfg(unix)]` —
/// `pre_exec`/`dup2` have no off-Unix equivalent, so fd 3+ stays a no-op
/// there, same as `redirect_stdio`'s own platform split.
#[cfg_attr(not(unix), allow(dead_code))]
enum FdAction {
    /// Duplicate an already-opened file's fd onto `dest`.
    Open(File, u32),
    /// Duplicate whatever `source` already resolves to (in the child, at
    /// this point in the sequence) onto `dest`.
    Dup { source: u32, dest: u32 },
}

/// Where one descriptor is routed. Files are kept as handles so `2>&1` can
/// `try_clone` them.
enum Sink {
    Inherit,
    Pipe,
    File(File),
}

impl Sink {
    /// `None` means "leave inherited".
    fn into_stdio(self) -> Result<Option<Stdio>, String> {
        Ok(match self {
            Sink::Inherit => None,
            Sink::Pipe => Some(Stdio::piped()),
            Sink::File(f) => Some(Stdio::from(f)),
        })
    }
}

/// Clone `sink` (for `fd>&target`). `Stdio::piped()` doesn't hand us the write
/// end before spawn, so a plain `Sink::Pipe` can't be shared with a second
/// descriptor as-is. On Unix, materialize a real OS pipe we own instead:
/// `sink` becomes a `File` wrapping the write end (so both descriptors get
/// independent fds onto the *same* pipe), and the read end is stashed in
/// `real_pipe_read` for the caller to use as the next stage's stdin.
#[cfg(unix)]
fn clone_or_materialize(sink: &mut Sink, real_pipe_read: &mut Option<File>) -> Result<Sink, String> {
    if matches!(sink, Sink::Pipe) {
        let (read, write) = make_pipe()?;
        *real_pipe_read = Some(read);
        *sink = Sink::File(write);
    }
    match sink {
        Sink::Inherit | Sink::Pipe => Ok(Sink::Inherit),
        Sink::File(f) => f.try_clone().map(Sink::File).map_err(|e| e.to_string()),
    }
}

/// Off Unix there's no way to materialize a shareable pipe before spawn, so
/// duping a piped fd falls back to inherit (see docs/ARCHITECTURE.md's Windows
/// note).
#[cfg(not(unix))]
fn clone_or_materialize(sink: &mut Sink, _real_pipe_read: &mut Option<File>) -> Result<Sink, String> {
    match sink {
        Sink::Inherit | Sink::Pipe => Ok(Sink::Inherit),
        Sink::File(f) => f.try_clone().map(Sink::File).map_err(|e| e.to_string()),
    }
}

/// Prints the usual "command not found"/"found but couldn't run it"-style
/// message for a failed spawn and returns the matching POSIX exit status —
/// 127 specifically for "no such command" (`io::ErrorKind::NotFound`), 126
/// for anything else (permission denied, is a directory, …) — matching
/// every comparison shell's own convention here (verified directly against
/// real bash: `126` for `/some/dir` or a non-executable file, `127` for a
/// plain typo). Doesn't try to match bash's own message wording, only its
/// functional behavior, same as every other error message in this shell.
pub(crate) fn spawn_failure_status(name: &str, err: &std::io::Error) -> i32 {
    if err.kind() == std::io::ErrorKind::NotFound {
        // A trailing `/` here is (almost always) the synthetic one
        // `resolve_program`/`command_bypass` append to force a clean
        // NotFound instead of a PATH search — don't leak it into the
        // diagnostic.
        eprintln!("rush: {}: command not found", name.strip_suffix('/').unwrap_or(name));
        127
    } else {
        eprintln!("rush: {name}: {err}");
        126
    }
}

/// Create a real, parent-owned pipe (Unix only) so its write end can be shared
/// across two descriptors (`stdout` and `stderr`) before spawn — something
/// `Stdio::piped()` can't do, since it only exposes the pipe to `std` internals.
#[cfg(unix)]
pub(crate) fn make_pipe() -> Result<(File, File), String> {
    use std::os::unix::io::FromRawFd;

    let mut fds = [0i32; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    // SAFETY: `pipe(2)` just handed us two fresh, valid, owned descriptors.
    let read = unsafe { File::from_raw_fd(fds[0]) };
    let write = unsafe { File::from_raw_fd(fds[1]) };
    Ok((read, write))
}

fn open_write(file: &str, mode: crate::parser::RedirMode) -> Result<File, String> {
    use crate::parser::RedirMode;
    // `set -C` (noclobber, C50): a plain `>` refuses to truncate an
    // existing *regular* file — writing to an existing device
    // (`> /dev/null`) stays fine, per POSIX and verified against bash.
    // `>|` (Clobber) and `>>` are exempt.
    if mode == RedirMode::Write
        && crate::vars::noclobber()
        && std::fs::metadata(file).is_ok_and(|m| m.is_file())
    {
        return Err(format!("{file}: cannot overwrite existing file"));
    }
    let append = mode == RedirMode::Append;
    OpenOptions::new()
        .write(true)
        .create(true)
        .append(append)
        .truncate(!append)
        .open(file)
        .map_err(|e| format!("{file}: {e}"))
}

/// Write a command's here-document body to its stdin on a background thread, so
/// a large body can't deadlock against a child that hasn't started reading.
pub(crate) fn feed_heredoc(child: &mut Child, cmd: &Command) {
    if let Some(body) = &cmd.heredoc {
        if let Some(mut stdin) = child.stdin.take() {
            let body = body.clone();
            std::thread::spawn(move || {
                use std::io::Write;
                let _ = stdin.write_all(body.as_bytes());
            });
        }
    }
}

/// A human-readable rendering of a pipeline, for the `jobs` listing. Only the
/// Unix job runner uses it. A compound stage isn't reconstructed back to
/// source text (its body is a full `CommandList`) — just labeled by kind.
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) fn pipeline_text(pipeline: &Pipeline) -> String {
    pipeline
        .commands
        .iter()
        .map(|stage| match stage {
            Stage::Simple(cmd) => cmd.argv.join(" "),
            Stage::Compound(stage) => match stage.compound.as_ref() {
                Compound::If { .. } => "if ...".to_string(),
                Compound::Loop { until: false, .. } => "while ...".to_string(),
                Compound::Loop { until: true, .. } => "until ...".to_string(),
                Compound::For { .. } => "for ...".to_string(),
                Compound::CFor { .. } => "for ((...)) ...".to_string(),
                Compound::Select { .. } => "select ...".to_string(),
                Compound::Case { .. } => "case ...".to_string(),
                Compound::Arith(_) => "((...))".to_string(),
                Compound::Cond(_) => "[[ ... ]]".to_string(),
                Compound::Coproc { name, .. } => format!("coproc {name} ..."),
                Compound::Group(_) => "{ ... }".to_string(),
                Compound::Subshell(_) => "( ... )".to_string(),
                Compound::FuncDef { name, .. } => format!("{name}() {{ ... }}"),
            },
        })
        .collect::<Vec<_>>()
        .join(" | ")
}

