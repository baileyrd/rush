//! Shell state that outlives a single command: the last exit status (`$?`) and
//! shell variables (`FOO=bar`, `export`).
//!
//! The REPL is single-threaded, so a thread-local `RefCell` is all the
//! synchronisation we need — the same approach `job` uses for its job table.
//!
//! Variables live only in this map, not the process environment. Lookups for
//! `$VAR` consult the map first and fall back to the real environment, and only
//! variables marked *exported* are pushed into child processes (see
//! `exec::build_stage`). Non-exported variables stay private to the shell.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};

/// A variable's actual payload: bash's ordinary scalar, an indexed array
/// (`arr=(a b c)`), or an associative array (`declare -A`, then
/// `arr=([k]=v ...)`). `BTreeMap` rather than `Vec`/`HashMap` because bash
/// arrays are genuinely sparse (`arr[5]=x` on a 2-element array doesn't
/// create indices 2–4) and an associative array's own iteration order is
/// unspecified in bash anyway (a real hash table) — `BTreeMap` gives
/// deterministic, sorted iteration for both `${arr[@]}`/`${!arr[@]}` for
/// free, which is a strictly *more* predictable superset of what bash
/// itself guarantees, not a behavior this needs to match exactly.
#[derive(Clone)]
pub enum VarValue {
    Scalar(String),
    Array(BTreeMap<usize, String>),
    Assoc(BTreeMap<String, String>),
}

struct Var {
    value: VarValue,
    exported: bool,
}

/// The value side of an assignment word — scalar (`NAME=value`), a whole
/// indexed-array literal's already-expanded elements (`NAME=(a b c)`), or
/// an associative-array literal's key/value pairs (`NAME=([k]=v ...)`,
/// only ever produced when `declare -A`/`local -A` says so — see
/// `expand::parse_decl_value`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssignValue {
    Scalar(String),
    Array(Vec<String>),
    Assoc(Vec<(String, String)>),
}

/// An assignment word's full operation, as `expand::assignment_split`
/// distinguishes it: `=` (replace) or `+=` (append) on the whole name, or
/// `NAME[subscript]=value`/`NAME[subscript]+=value` targeting one specific
/// element — see `assign`. The subscript is carried as raw, `$`-expanded
/// (but not yet arithmetic-evaluated) text: whether it's an array index or
/// an associative key can only be decided at `assign` time, by checking
/// `name`'s *current* type (verified directly against real bash: `arr[a]=x`
/// treats `a` as an arithmetic expression — evaluating to 0 — unless `arr`
/// is already declared `-A`, in which case `a` is the literal string key).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssignOp {
    Set(AssignValue),
    Append(AssignValue),
    SetKey(String, String),
    AppendKey(String, String),
}

/// Apply a parsed assignment word to `name` — the single entry point
/// `exec.rs` uses for both a bare assignment statement (`arr=(a b c)`,
/// `arr[2]=x`) and a command's own leading `NAME=value` prefixes, via the
/// existing per-case functions (`set`/`set_array`/`set_assoc`/
/// `append_scalar`/`array_append`/`assoc_merge`/`key_set`/`key_append`),
/// each already verified directly against real bash.
pub fn assign(name: &str, op: &AssignOp) {
    match op {
        AssignOp::Set(AssignValue::Scalar(s)) => set(name, s),
        AssignOp::Set(AssignValue::Array(elements)) => set_array(name, elements.clone()),
        AssignOp::Set(AssignValue::Assoc(pairs)) => set_assoc(name, pairs.clone()),
        AssignOp::Append(AssignValue::Scalar(s)) => append_scalar(name, s),
        AssignOp::Append(AssignValue::Array(elements)) => array_append(name, elements.clone()),
        AssignOp::Append(AssignValue::Assoc(pairs)) => assoc_merge(name, pairs.clone()),
        AssignOp::SetKey(subscript, value) => key_set(name, subscript, value),
        AssignOp::AppendKey(subscript, value) => key_append(name, subscript, value),
    }
}

/// A pending `break`/`continue` request, carrying how many enclosing loops it
/// applies to (`break 2`). The executor consumes it level by level.
#[derive(Clone, Copy)]
pub enum LoopCtl {
    Break(u32),
    Continue(u32),
}

thread_local! {
    static LAST_STATUS: RefCell<i32> = const { RefCell::new(0) };
    static VARS: RefCell<HashMap<String, Var>> = RefCell::new(HashMap::new());
    static LOOP_CTL: RefCell<Option<LoopCtl>> = const { RefCell::new(None) };
    static RETURNING: RefCell<Option<i32>> = const { RefCell::new(None) };
    // `$0` (shell/script name) and `$1`, `$2`, … (positional parameters).
    static SHELL_NAME: RefCell<String> = RefCell::new("rush".to_string());
    static ARGS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    // `set -e`: a failing command exits the shell (see exec::exec_list_impl).
    static ERREXIT: RefCell<bool> = const { RefCell::new(false) };
    // `set -u`: referencing an unset variable is an error (see
    // `expand::var_lookup_checked`).
    static NOUNSET: RefCell<bool> = const { RefCell::new(false) };
    // `set -o pipefail`: a pipeline's own exit status is the rightmost
    // non-zero stage, not just its last (see `exec::pipeline_status`).
    static PIPEFAIL: RefCell<bool> = const { RefCell::new(false) };
    // `set -x`: echo each command to stderr before running it (see
    // `exec::trace_command`).
    static XTRACE: RefCell<bool> = const { RefCell::new(false) };
    // How many levels of `$(...)` command substitution are currently being
    // expanded — `set -x`'s prefix repeats its first character once per
    // level, matching real bash (see `exec::trace_command`).
    static TRACE_DEPTH: RefCell<u32> = const { RefCell::new(0) };
    // The exit status of the most recent command substitution performed
    // while expanding a command's words, if any (see `reset_last_subst_status`).
    static LAST_SUBST_STATUS: RefCell<Option<i32>> = const { RefCell::new(None) };
    // One frame per active function call, pushed/popped by
    // `push_local_frame`/`pop_local_frame`. Each frame lists the names
    // `local` has shadowed *in that call*, alongside what they were
    // beforehand (`None` meaning "didn't exist") — see `declare_local`.
    static LOCAL_STACK: RefCell<Vec<LocalFrame>> = const { RefCell::new(Vec::new()) };
    // `getopts`'s internal progress within the *current* `$OPTIND` word —
    // `(optind, char_pos)`. Not a shell-visible variable (bash doesn't
    // expose one either); see `getopts_char_pos`.
    static GETOPTS_POS: RefCell<(usize, usize)> = const { RefCell::new((0, 0)) };
    // `$!`: the most recently backgrounded job's own last-stage pid (see
    // `set_last_bg_pid`).
    static LAST_BG_PID: RefCell<Option<i32>> = const { RefCell::new(None) };
}

/// A prior value (`value`, `exported`) to restore when a `local`-shadowed
/// name's function call returns, or `None` if the name didn't exist before.
type PriorValue = Option<(VarValue, bool)>;
/// One function call's set of `local`-shadowed names, in declaration order.
type LocalFrame = Vec<(String, PriorValue)>;

pub fn set_errexit(on: bool) {
    ERREXIT.with(|e| *e.borrow_mut() = on);
}

pub fn errexit() -> bool {
    ERREXIT.with(|e| *e.borrow())
}

pub fn set_nounset(on: bool) {
    NOUNSET.with(|e| *e.borrow_mut() = on);
}

pub fn nounset() -> bool {
    NOUNSET.with(|e| *e.borrow())
}

pub fn set_pipefail(on: bool) {
    PIPEFAIL.with(|e| *e.borrow_mut() = on);
}

pub fn pipefail() -> bool {
    PIPEFAIL.with(|e| *e.borrow())
}

pub fn set_xtrace(on: bool) {
    XTRACE.with(|e| *e.borrow_mut() = on);
}

pub fn xtrace() -> bool {
    XTRACE.with(|e| *e.borrow())
}

pub fn trace_depth() -> u32 {
    TRACE_DEPTH.with(|d| *d.borrow())
}

/// Run `f` with the command-substitution trace depth one level deeper —
/// wraps a `$(...)` expansion so any tracing inside it gets the right
/// number of repeated prefix characters, restored afterward regardless of
/// how `f` returns.
pub fn with_deeper_trace<T>(f: impl FnOnce() -> T) -> T {
    TRACE_DEPTH.with(|d| *d.borrow_mut() += 1);
    let result = f();
    TRACE_DEPTH.with(|d| *d.borrow_mut() -= 1);
    result
}

/// Clear the "did a command substitution just run" marker. Called right
/// before expanding a simple command's words, so that afterward
/// `take_last_subst_status` reflects only a substitution that happened
/// during *this* command's own expansion — not a stale one left over from
/// something unrelated.
pub fn reset_last_subst_status() {
    LAST_SUBST_STATUS.with(|s| *s.borrow_mut() = None);
}

/// Record a command substitution's exit status — its last job's status, same
/// as `$?` would see from inside it. Used to give a variable-assignment-only
/// command (`x=$(false)`) POSIX's exit-status rule: it's the last
/// substitution's status, not always 0.
pub fn set_last_subst_status(code: i32) {
    LAST_SUBST_STATUS.with(|s| *s.borrow_mut() = Some(code));
}

/// Consume the marker set by `set_last_subst_status`, if any command
/// substitution ran since the last `reset_last_subst_status`.
pub fn take_last_subst_status() -> Option<i32> {
    LAST_SUBST_STATUS.with(|s| s.borrow_mut().take())
}

/// Set `$0` and the positional parameters (`$1`…).
pub fn set_args(name: String, args: Vec<String>) {
    SHELL_NAME.with(|n| *n.borrow_mut() = name);
    ARGS.with(|a| *a.borrow_mut() = args);
}

/// `$n`: `$0` is the shell/script name, `$1`… the positional parameters.
pub fn arg(n: usize) -> Option<String> {
    if n == 0 {
        Some(SHELL_NAME.with(|s| s.borrow().clone()))
    } else {
        ARGS.with(|a| a.borrow().get(n - 1).cloned())
    }
}

/// `$#` — the number of positional parameters.
pub fn arg_count() -> usize {
    ARGS.with(|a| a.borrow().len())
}

/// All positional parameters (`$@` / `$*`).
pub fn args() -> Vec<String> {
    ARGS.with(|a| a.borrow().clone())
}

/// `shift n`: drop the first `n` positional parameters. Returns `false` (and
/// leaves them untouched) if `n` is greater than `$#` — the `shift` builtin
/// reports that as a usage error, matching bash.
pub fn shift(n: usize) -> bool {
    ARGS.with(|a| {
        let mut a = a.borrow_mut();
        if n > a.len() {
            return false;
        }
        a.drain(0..n);
        true
    })
}

/// `getopts`'s cached within-word progress for the given `$OPTIND` value:
/// `0` if `optind` doesn't match what's cached — a fresh word, or the
/// script just reset `$OPTIND` itself — else the character index to resume
/// at (a prior call already consumed an earlier combined short flag in the
/// same word, e.g. `-ab`'s `a`).
pub fn getopts_char_pos(optind: usize) -> usize {
    GETOPTS_POS.with(|p| {
        let p = p.borrow();
        if p.0 == optind { p.1 } else { 0 }
    })
}

/// Record `getopts`'s within-word progress for its next call.
pub fn set_getopts_char_pos(optind: usize, char_pos: usize) {
    GETOPTS_POS.with(|p| *p.borrow_mut() = (optind, char_pos));
}

/// Record a pending loop-control request (from the `break`/`continue` builtins).
pub fn set_loop_ctl(ctl: Option<LoopCtl>) {
    LOOP_CTL.with(|c| *c.borrow_mut() = ctl);
}

/// The pending loop-control request, if any.
pub fn loop_ctl() -> Option<LoopCtl> {
    LOOP_CTL.with(|c| *c.borrow())
}

/// Record a pending `return` (from the `return` builtin) with its exit code.
pub fn set_returning(code: Option<i32>) {
    RETURNING.with(|r| *r.borrow_mut() = code);
}

/// The pending `return` code, if a function should unwind.
pub fn returning() -> Option<i32> {
    RETURNING.with(|r| *r.borrow())
}

/// Whether any non-local control flow (`break`/`continue`/`return`) is pending,
/// so a list should stop running further commands.
pub fn flow_pending() -> bool {
    loop_ctl().is_some() || returning().is_some()
}

/// The exit status of the most recently completed pipeline — exposed as `$?`.
pub fn last_status() -> i32 {
    LAST_STATUS.with(|s| *s.borrow())
}

pub fn set_last_status(code: i32) {
    LAST_STATUS.with(|s| *s.borrow_mut() = code);
}

/// `$!` — the most recently backgrounded job's own last-stage pid, or
/// unset if nothing has been backgrounded yet this session.
pub fn last_bg_pid() -> Option<i32> {
    LAST_BG_PID.with(|p| *p.borrow())
}

/// Record `$!` when a job is backgrounded (`job::run_background`).
pub fn set_last_bg_pid(pid: i32) {
    LAST_BG_PID.with(|p| *p.borrow_mut() = Some(pid));
}

/// Look up a shell variable's scalar value (not the environment — see
/// `expand`). For an array (indexed or associative), this is element/key
/// `0`/`"0"` (`$arr` == `${arr[0]}`, verified directly against real bash) —
/// absent if that particular slot isn't set, same as any other unset
/// variable.
pub fn get(name: &str) -> Option<String> {
    VARS.with(|v| {
        v.borrow().get(name).and_then(|x| match &x.value {
            VarValue::Scalar(s) => Some(s.clone()),
            VarValue::Array(a) => a.get(&0).cloned(),
            VarValue::Assoc(a) => a.get("0").cloned(),
        })
    })
}

/// Whether `name` is currently declared as an *associative* array — the
/// one piece of runtime state that decides how a `[subscript]` is
/// interpreted everywhere else (arithmetic index vs. literal string key;
/// see `key_set`/`key_append` and `expand::eval_subscript_for`). `false`
/// for a scalar, an indexed array, or an unset name.
pub fn is_assoc(name: &str) -> bool {
    VARS.with(|v| matches!(v.borrow().get(name).map(|x| &x.value), Some(VarValue::Assoc(_))))
}

/// Remove a shell variable (`unset NAME`) — scalar or array, the whole thing.
pub fn unset(name: &str) {
    VARS.with(|v| {
        v.borrow_mut().remove(name);
    });
}

/// Set a variable's scalar value, preserving its exported flag if it already
/// existed. If `name` is currently an *array* (indexed or associative),
/// this targets element/key `0`/`"0"` only, leaving the rest untouched —
/// matching bash exactly (`arr=x` after `arr=(a b c)` still leaves
/// `arr[1]`/`arr[2]` alone, verified directly for both array kinds); a
/// plain, never-arrayed variable is unaffected by this rule.
pub fn set(name: &str, value: &str) {
    VARS.with(|v| {
        let mut m = v.borrow_mut();
        match m.get_mut(name) {
            Some(var) => match &mut var.value {
                VarValue::Array(a) => {
                    a.insert(0, value.to_string());
                }
                VarValue::Assoc(a) => {
                    a.insert("0".to_string(), value.to_string());
                }
                VarValue::Scalar(s) => value.clone_into(s),
            },
            None => {
                m.insert(name.to_string(), Var { value: VarValue::Scalar(value.to_string()), exported: false });
            }
        }
    });
}

/// Set a variable and mark it exported (`export NAME=value`) — always a
/// plain scalar replacement; exporting an array doesn't apply (see
/// `exported`), so this niche interaction with a pre-existing array isn't
/// specially handled.
pub fn set_exported(name: &str, value: &str) {
    VARS.with(|v| {
        v.borrow_mut().insert(
            name.to_string(),
            Var { value: VarValue::Scalar(value.to_string()), exported: true },
        );
    });
}

/// Mark an existing (or newly-created, empty scalar) variable exported
/// (`export NAME`).
pub fn export(name: &str) {
    VARS.with(|v| {
        v.borrow_mut()
            .entry(name.to_string())
            .or_insert_with(|| Var { value: VarValue::Scalar(String::new()), exported: false })
            .exported = true;
    });
}

/// Replace `name` entirely with a fresh 0-indexed array (`arr=(a b c)`),
/// discarding whatever was there before — scalar or array — same as any
/// other whole-variable assignment. Preserves the exported flag mechanically
/// (arrays are never actually exported — see `exported` — but there's no
/// reason to drop the flag if a future export-arrays feature needs it).
pub fn set_array(name: &str, elements: Vec<String>) {
    let array: BTreeMap<usize, String> = elements.into_iter().enumerate().collect();
    VARS.with(|v| {
        let mut m = v.borrow_mut();
        let exported = m.get(name).is_some_and(|x| x.exported);
        m.insert(name.to_string(), Var { value: VarValue::Array(array), exported });
    });
}

/// Replace `name` entirely with a fresh associative array (`declare -A
/// arr=([k]=v ...)`), discarding whatever was there before — the
/// associative-array analogue of `set_array`. Later pairs win over earlier
/// ones for a repeated key, matching an ordinary map build.
pub fn set_assoc(name: &str, pairs: Vec<(String, String)>) {
    let assoc: BTreeMap<String, String> = pairs.into_iter().collect();
    VARS.with(|v| {
        let mut m = v.borrow_mut();
        let exported = m.get(name).is_some_and(|x| x.exported);
        m.insert(name.to_string(), Var { value: VarValue::Assoc(assoc), exported });
    });
}

/// `${arr[index]}` — a specific *indexed*-array element, or a plain
/// scalar's own value if `index` is 0 (a never-arrayed variable behaves
/// like a 1-element array, verified directly against real bash) and `None`
/// for any other index. `None` for an associative array too — that's what
/// `assoc_get` is for (see `is_assoc`, which callers check first).
pub fn array_get(name: &str, index: usize) -> Option<String> {
    VARS.with(|v| {
        v.borrow().get(name).and_then(|x| match &x.value {
            VarValue::Array(a) => a.get(&index).cloned(),
            VarValue::Scalar(s) => (index == 0).then(|| s.clone()),
            VarValue::Assoc(_) => None,
        })
    })
}

/// `${arr[key]}` — a specific associative-array element (`None` if unset,
/// or if `name` isn't actually an associative array).
pub fn assoc_get(name: &str, key: &str) -> Option<String> {
    VARS.with(|v| {
        v.borrow().get(name).and_then(|x| match &x.value {
            VarValue::Assoc(a) => a.get(key).cloned(),
            VarValue::Array(_) | VarValue::Scalar(_) => None,
        })
    })
}

/// `${arr[@]}`/`${arr[*]}` — every element, in key/index order (a plain
/// scalar is just its one value, matching `array_get`'s index-0
/// treatment). Shared between indexed and associative arrays: both
/// ultimately just need "every value in this map," regardless of what the
/// keys look like.
pub fn array_values(name: &str) -> Vec<String> {
    VARS.with(|v| {
        v.borrow()
            .get(name)
            .map(|x| match &x.value {
                VarValue::Array(a) => a.values().cloned().collect(),
                VarValue::Assoc(a) => a.values().cloned().collect(),
                VarValue::Scalar(s) => vec![s.clone()],
            })
            .unwrap_or_default()
    })
}

/// `${!arr[@]}` for an *indexed* array — the indices actually set (gaps in
/// a sparse array are skipped entirely, not listed); `[0]` for a plain
/// scalar. See `assoc_keys` for an associative array's own version.
pub fn array_indices(name: &str) -> Vec<usize> {
    VARS.with(|v| {
        v.borrow()
            .get(name)
            .map(|x| match &x.value {
                VarValue::Array(a) => a.keys().copied().collect(),
                VarValue::Scalar(_) => vec![0],
                VarValue::Assoc(_) => vec![],
            })
            .unwrap_or_default()
    })
}

/// `${!arr[@]}` for an associative array — every key actually set, sorted
/// (bash's own iteration order is unspecified — a real hash table — so
/// this is a strictly more predictable superset, not a behavior being
/// matched exactly; see `VarValue`'s own doc comment).
pub fn assoc_keys(name: &str) -> Vec<String> {
    VARS.with(|v| {
        v.borrow()
            .get(name)
            .map(|x| match &x.value {
                VarValue::Assoc(a) => a.keys().cloned().collect(),
                VarValue::Array(_) | VarValue::Scalar(_) => vec![],
            })
            .unwrap_or_default()
    })
}

/// `${#arr[@]}` — the number of elements actually set, *not* one past the
/// highest index for an indexed array (a sparse `arr=(a b); arr[5]=x` has 3
/// elements, not 6, verified directly); a plain scalar counts as 1, an
/// unset name as 0. Shared between indexed and associative arrays, same
/// reasoning as `array_values`.
pub fn array_len(name: &str) -> usize {
    VARS.with(|v| {
        v.borrow()
            .get(name)
            .map(|x| match &x.value {
                VarValue::Array(a) => a.len(),
                VarValue::Assoc(a) => a.len(),
                VarValue::Scalar(_) => 1,
            })
            .unwrap_or(0)
    })
}

/// `arr[index]=value` — set one element. Auto-vivifies an array out of
/// nothing if `name` didn't exist; if `name` was a plain scalar, it's
/// promoted to an array with the old value preserved at index 0 (unless
/// `index` itself *is* 0, which just overwrites it) — both verified
/// directly against real bash.
pub fn array_set(name: &str, index: usize, value: &str) {
    VARS.with(|v| {
        let mut m = v.borrow_mut();
        match m.get_mut(name) {
            Some(var) => match &mut var.value {
                VarValue::Array(a) => {
                    a.insert(index, value.to_string());
                }
                VarValue::Scalar(s) => {
                    let old = std::mem::take(s);
                    let mut a = BTreeMap::new();
                    if index != 0 {
                        a.insert(0, old);
                    }
                    a.insert(index, value.to_string());
                    var.value = VarValue::Array(a);
                }
                // Unreachable via the normal dispatch path: `key_set`
                // checks `is_assoc` first and calls `assoc_set` instead.
                VarValue::Assoc(_) => {}
            },
            None => {
                let mut a = BTreeMap::new();
                a.insert(index, value.to_string());
                m.insert(name.to_string(), Var { value: VarValue::Array(a), exported: false });
            }
        }
    });
}

/// `arr[index]+=value` — append the string to that one element (or, if
/// nothing's there yet at `index`, this is the same as just setting it —
/// nothing to append *to*), same auto-vivify/scalar-promotion rules as
/// `array_set`, both verified directly against real bash.
pub fn array_append_index(name: &str, index: usize, value: &str) {
    VARS.with(|v| {
        let mut m = v.borrow_mut();
        match m.get_mut(name) {
            Some(var) => match &mut var.value {
                VarValue::Array(a) => a.entry(index).or_default().push_str(value),
                VarValue::Scalar(s) => {
                    let old = std::mem::take(s);
                    let mut a = BTreeMap::new();
                    if index == 0 {
                        a.insert(0, old + value);
                    } else {
                        a.insert(0, old);
                        a.insert(index, value.to_string());
                    }
                    var.value = VarValue::Array(a);
                }
                // Unreachable via the normal dispatch path — see `array_set`.
                VarValue::Assoc(_) => {}
            },
            None => {
                let mut a = BTreeMap::new();
                a.insert(index, value.to_string());
                m.insert(name.to_string(), Var { value: VarValue::Array(a), exported: false });
            }
        }
    });
}

/// `arr+=(elements...)` — append after the current highest index (0 if the
/// array is empty or `name` didn't exist yet). A plain scalar is promoted
/// to an array first, its old value kept at index 0, then the new elements
/// appended from index 1 — matching real bash exactly (verified directly).
pub fn array_append(name: &str, elements: Vec<String>) {
    VARS.with(|v| {
        let mut m = v.borrow_mut();
        match m.get_mut(name) {
            Some(var) => {
                // Unreachable via the normal dispatch path (an already-`-A`
                // name always takes `AssignValue::Assoc` instead) — but
                // still needs *some* fallback for exhaustiveness.
                if matches!(var.value, VarValue::Assoc(_)) {
                    return;
                }
                let a = match &mut var.value {
                    VarValue::Array(a) => a,
                    VarValue::Scalar(s) => {
                        let old = std::mem::take(s);
                        let mut a = BTreeMap::new();
                        a.insert(0, old);
                        var.value = VarValue::Array(a);
                        let VarValue::Array(a) = &mut var.value else { unreachable!() };
                        a
                    }
                    VarValue::Assoc(_) => unreachable!("checked above"),
                };
                let mut next = a.keys().next_back().map_or(0, |k| k + 1);
                for e in elements {
                    a.insert(next, e);
                    next += 1;
                }
            }
            None => {
                let array: BTreeMap<usize, String> = elements.into_iter().enumerate().collect();
                m.insert(name.to_string(), Var { value: VarValue::Array(array), exported: false });
            }
        }
    });
}

/// `x+=value` — append the literal string `value`: to a plain scalar's own
/// text, or (matching real bash exactly, verified directly) to *element/key
/// `0`/`"0"`* of an existing array (indexed or associative), leaving every
/// other element untouched. Creates a fresh scalar if `name` didn't exist
/// yet.
pub fn append_scalar(name: &str, value: &str) {
    VARS.with(|v| {
        let mut m = v.borrow_mut();
        match m.get_mut(name) {
            Some(var) => match &mut var.value {
                VarValue::Array(a) => a.entry(0).or_default().push_str(value),
                VarValue::Assoc(a) => a.entry("0".to_string()).or_default().push_str(value),
                VarValue::Scalar(s) => s.push_str(value),
            },
            None => {
                m.insert(name.to_string(), Var { value: VarValue::Scalar(value.to_string()), exported: false });
            }
        }
    });
}

/// `unset 'arr[index]'` — remove just that one element, leaving a genuine
/// gap in a sparse array (not merely emptying it) — a no-op if `name` isn't
/// an *indexed* array or that index isn't set (see `assoc_unset_key` for
/// an associative array's own version).
pub fn array_unset_index(name: &str, index: usize) {
    VARS.with(|v| {
        if let Some(var) = v.borrow_mut().get_mut(name)
            && let VarValue::Array(a) = &mut var.value
        {
            a.remove(&index);
        }
    });
}

/// `unset 'arr[key]'` for an associative array — a no-op if `name` isn't
/// one, or that key isn't set.
pub fn assoc_unset_key(name: &str, key: &str) {
    VARS.with(|v| {
        if let Some(var) = v.borrow_mut().get_mut(name)
            && let VarValue::Assoc(a) = &mut var.value
        {
            a.remove(key);
        }
    });
}

/// `arr[key]=value` on an associative array — auto-vivifies *only* if
/// `name` is already known to be one (see `key_set`, which is the real
/// entry point; calling this directly on a non-associative name would
/// silently convert it, which is why it's not `pub`).
fn assoc_set(name: &str, key: &str, value: &str) {
    VARS.with(|v| {
        let mut m = v.borrow_mut();
        match m.get_mut(name) {
            Some(var) => {
                if let VarValue::Assoc(a) = &mut var.value {
                    a.insert(key.to_string(), value.to_string());
                }
            }
            None => {
                let mut a = BTreeMap::new();
                a.insert(key.to_string(), value.to_string());
                m.insert(name.to_string(), Var { value: VarValue::Assoc(a), exported: false });
            }
        }
    });
}

/// `arr[key]+=value` on an associative array — append to that key's own
/// string (or set it, if nothing's there yet).
fn assoc_append_key(name: &str, key: &str, value: &str) {
    VARS.with(|v| {
        let mut m = v.borrow_mut();
        match m.get_mut(name) {
            Some(var) => {
                if let VarValue::Assoc(a) = &mut var.value {
                    a.entry(key.to_string()).or_default().push_str(value);
                }
            }
            None => {
                let mut a = BTreeMap::new();
                a.insert(key.to_string(), value.to_string());
                m.insert(name.to_string(), Var { value: VarValue::Assoc(a), exported: false });
            }
        }
    });
}

/// `arr+=([k]=v ...)` — merge new key/value pairs into an existing
/// associative array (a later pair overwrites an earlier one for the same
/// key, matching real bash, verified directly); creates a fresh one if
/// `name` didn't exist. Never called on a non-associative name (`assign`
/// only reaches this arm when `expand::parse_decl_value` already committed
/// to `-A`'s shape) — but for exhaustiveness/robustness, a `Scalar`/`Array`
/// target is just replaced outright rather than corrupted in place.
pub fn assoc_merge(name: &str, pairs: Vec<(String, String)>) {
    VARS.with(|v| {
        let mut m = v.borrow_mut();
        match m.get_mut(name) {
            Some(var) if matches!(var.value, VarValue::Assoc(_)) => {
                let VarValue::Assoc(a) = &mut var.value else { unreachable!() };
                a.extend(pairs);
            }
            _ => {
                let exported = m.get(name).is_some_and(|x| x.exported);
                m.insert(name.to_string(), Var { value: VarValue::Assoc(pairs.into_iter().collect()), exported });
            }
        }
    });
}

/// `arr[subscript]=value` — the real entry point `assign` uses: if `name`
/// is *already* declared associative (`is_assoc`), `subscript` is the
/// literal string key; otherwise it's evaluated as an arithmetic index (via
/// `expand::eval_subscript`, called by the one caller of this that has
/// access to it — `exec.rs`'s assignment-application path) and dispatched
/// to `array_set`. Both halves verified directly against real bash,
/// including the headline case: `arr[a]=x` on a plain/unset `arr` treats
/// `a` as an arithmetic expression (evaluating to 0), *not* a string key —
/// only `declare -A`/`local -A` unlocks string-keyed subscripts at all.
pub fn key_set(name: &str, subscript: &str, value: &str) {
    if is_assoc(name) {
        assoc_set(name, subscript, value);
    } else if let Some(index) = crate::expand::eval_subscript(subscript) {
        array_set(name, index, value);
    }
}

/// As [`key_set`], for `arr[subscript]+=value`.
pub fn key_append(name: &str, subscript: &str, value: &str) {
    if is_assoc(name) {
        assoc_append_key(name, subscript, value);
    } else if let Some(index) = crate::expand::eval_subscript(subscript) {
        array_append_index(name, index, value);
    }
}

/// Push a fresh, empty local-variable frame — called when entering a
/// function call (`exec::call_function`).
pub fn push_local_frame() {
    LOCAL_STACK.with(|s| s.borrow_mut().push(Vec::new()));
}

/// Pop the current function call's local-variable frame, restoring each name
/// `local` shadowed in it to whatever it was beforehand — or removing it, if
/// it didn't exist before the call. Nesting falls out naturally: an inner
/// call's frame captures whatever the *enclosing* call's own locals had
/// already shadowed things to, so popping the inner frame restores the
/// outer call's local value, not the top-level one (verified against real
/// bash directly).
pub fn pop_local_frame() {
    let Some(frame) = LOCAL_STACK.with(|s| s.borrow_mut().pop()) else {
        return;
    };
    VARS.with(|v| {
        let mut m = v.borrow_mut();
        for (name, prior) in frame {
            match prior {
                Some((value, exported)) => {
                    m.insert(name, Var { value, exported });
                }
                None => {
                    m.remove(&name);
                }
            }
        }
    });
}

/// `local [name[=value]]...`: shadow `name` with a fresh binding for the
/// current function call, restored automatically (see `pop_local_frame`)
/// when it returns. `value: None` (a bare `local name`) leaves `name`
/// genuinely unset within the function, matching bash — not merely set to
/// `""` (`${name-default}` inside the function sees it as unset). Returns
/// `false` if there's no active function call to declare into (the `local`
/// builtin reports that as a usage error); a name already made local earlier
/// in *this same* call keeps its originally-captured prior value, so a
/// second `local x` in one call still restores to the pre-call value, not
/// the first `local`'s.
pub fn declare_local(name: &str, value: Option<&str>) -> bool {
    let declared = capture_for_local(name);
    if declared {
        match value {
            Some(v) => set(name, v),
            None => unset(name),
        }
    }
    declared
}

/// As [`declare_local`], but for `local arr=(a b c)` — the array-literal
/// form. Same shadow/restore contract, just setting a fresh array instead
/// of a scalar.
pub fn declare_local_array(name: &str, elements: Vec<String>) -> bool {
    let declared = capture_for_local(name);
    if declared {
        set_array(name, elements);
    }
    declared
}

/// As [`declare_local`], but for `local -A arr=([k]=v ...)` — the
/// associative-array-literal form.
pub fn declare_local_assoc(name: &str, pairs: Vec<(String, String)>) -> bool {
    let declared = capture_for_local(name);
    if declared {
        set_assoc(name, pairs);
    }
    declared
}

/// Shared by `declare_local`/`declare_local_array`: capture `name`'s prior
/// value into the current function call's frame (only the *first* time
/// this name is made local within that one call — see `declare_local`'s own
/// doc comment for why a second `local` in the same call must not
/// re-capture), reporting whether there's an active frame to declare into
/// at all.
fn capture_for_local(name: &str) -> bool {
    LOCAL_STACK.with(|s| {
        let mut stack = s.borrow_mut();
        let Some(frame) = stack.last_mut() else {
            return false;
        };
        if !frame.iter().any(|(n, _)| n == name) {
            let prior = VARS.with(|v| v.borrow().get(name).map(|x| (x.value.clone(), x.exported)));
            frame.push((name.to_string(), prior));
        }
        true
    })
}

/// A snapshot of all variables, for isolating a subshell on platforms without
/// `fork` (see `exec::run_compound`'s `Compound::Subshell` arm) — Unix forks a
/// real child instead, so these are unused there.
#[cfg(not(unix))]
pub type Snapshot = Vec<(String, VarValue, bool)>;

#[cfg(not(unix))]
pub fn snapshot() -> Snapshot {
    VARS.with(|v| {
        v.borrow()
            .iter()
            .map(|(k, x)| (k.clone(), x.value.clone(), x.exported))
            .collect()
    })
}

#[cfg(not(unix))]
pub fn restore(snap: Snapshot) {
    VARS.with(|v| {
        let mut m = v.borrow_mut();
        m.clear();
        for (name, value, exported) in snap {
            m.insert(name, Var { value, exported });
        }
    });
}

/// Every exported *scalar* variable as `(name, value)`, for seeding child
/// environments — arrays are never exported (there's no portable env-var
/// representation for one), so an exported array-valued name is silently
/// skipped here rather than passed through in some serialized form.
pub fn exported() -> Vec<(String, String)> {
    VARS.with(|v| {
        v.borrow()
            .iter()
            .filter(|(_, x)| x.exported)
            .filter_map(|(k, x)| match &x.value {
                VarValue::Scalar(s) => Some((k.clone(), s.clone())),
                VarValue::Array(_) | VarValue::Assoc(_) => None,
            })
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_get_unset_and_export() {
        set("RUSH_V", "1");
        assert_eq!(get("RUSH_V").as_deref(), Some("1"));
        assert!(!exported().iter().any(|(k, _)| k == "RUSH_V"));

        export("RUSH_V");
        assert!(exported().iter().any(|(k, v)| k == "RUSH_V" && v == "1"));

        // Re-setting keeps the exported flag.
        set("RUSH_V", "2");
        assert!(exported().iter().any(|(k, v)| k == "RUSH_V" && v == "2"));

        unset("RUSH_V");
        assert_eq!(get("RUSH_V"), None);
    }

    #[test]
    fn shift_drops_leading_positional_params() {
        set_args("prog".to_string(), vec!["a".to_string(), "b".to_string(), "c".to_string()]);

        assert!(shift(1));
        assert_eq!(args(), vec!["b", "c"]);

        assert!(shift(0)); // no-op, always succeeds
        assert_eq!(args(), vec!["b", "c"]);

        // Greater than the remaining count: rejected, nothing shifted.
        assert!(!shift(3));
        assert_eq!(args(), vec!["b", "c"]);

        assert!(shift(2));
        assert!(args().is_empty());
        assert!(!shift(1)); // now empty: even 1 is too many

        set_args("prog".to_string(), Vec::new());
    }

    #[test]
    fn local_outside_a_function_is_rejected() {
        assert!(!declare_local("RUSH_LOCAL_TOP", Some("1")));
        // Rejected: must not fall through to setting it as a plain global.
        assert_eq!(get("RUSH_LOCAL_TOP"), None);
    }

    #[test]
    fn local_shadows_and_restores_on_frame_pop() {
        set("RUSH_LOCAL_X", "outer");

        push_local_frame();
        assert!(declare_local("RUSH_LOCAL_X", Some("inner")));
        assert_eq!(get("RUSH_LOCAL_X").as_deref(), Some("inner"));

        // A bare `local name` (no `=value`) leaves it genuinely unset, not
        // merely set to `""`.
        assert!(declare_local("RUSH_LOCAL_Y", None));
        assert_eq!(get("RUSH_LOCAL_Y"), None);

        // A second `local` for the same name in the *same* frame doesn't
        // re-capture — it must still restore to the pre-frame value.
        assert!(declare_local("RUSH_LOCAL_X", Some("inner2")));
        assert_eq!(get("RUSH_LOCAL_X").as_deref(), Some("inner2"));

        pop_local_frame();
        assert_eq!(get("RUSH_LOCAL_X").as_deref(), Some("outer"));

        unset("RUSH_LOCAL_X");
    }

    #[test]
    fn nested_frames_restore_to_the_enclosing_frames_own_value() {
        set("RUSH_LOCAL_N", "top");

        push_local_frame();
        declare_local("RUSH_LOCAL_N", Some("outer_call"));

        push_local_frame();
        declare_local("RUSH_LOCAL_N", Some("inner_call"));
        assert_eq!(get("RUSH_LOCAL_N").as_deref(), Some("inner_call"));
        pop_local_frame();

        // Popping the inner call's frame restores the *outer* call's own
        // local value, not the top-level one — matches real bash.
        assert_eq!(get("RUSH_LOCAL_N").as_deref(), Some("outer_call"));
        pop_local_frame();
        assert_eq!(get("RUSH_LOCAL_N").as_deref(), Some("top"));

        unset("RUSH_LOCAL_N");
    }

    #[test]
    fn local_of_a_name_that_never_existed_is_removed_on_pop() {
        unset("RUSH_LOCAL_NEW");
        push_local_frame();
        declare_local("RUSH_LOCAL_NEW", Some("value"));
        assert_eq!(get("RUSH_LOCAL_NEW").as_deref(), Some("value"));
        pop_local_frame();
        assert_eq!(get("RUSH_LOCAL_NEW"), None);
    }

    #[test]
    fn array_basic_set_get_and_whole_array_reads() {
        unset("RUSH_ARR");
        set_array("RUSH_ARR", vec!["a".into(), "b".into(), "c".into()]);
        assert_eq!(array_get("RUSH_ARR", 0).as_deref(), Some("a"));
        assert_eq!(array_get("RUSH_ARR", 2).as_deref(), Some("c"));
        assert_eq!(array_get("RUSH_ARR", 10), None); // out of range: absent, not an error
        assert_eq!(get("RUSH_ARR").as_deref(), Some("a")); // $arr == ${arr[0]}
        assert_eq!(array_values("RUSH_ARR"), vec!["a", "b", "c"]);
        assert_eq!(array_indices("RUSH_ARR"), vec![0, 1, 2]);
        assert_eq!(array_len("RUSH_ARR"), 3);
        unset("RUSH_ARR");
    }

    #[test]
    fn array_is_sparse_not_padded() {
        unset("RUSH_SPARSE");
        set_array("RUSH_SPARSE", vec!["a".into(), "b".into()]);
        array_set("RUSH_SPARSE", 5, "x");
        // Count is the number of *set* indices (3), not one past the
        // highest (6) — and `${arr[@]}`/`${!arr[@]}` skip the gap entirely.
        assert_eq!(array_len("RUSH_SPARSE"), 3);
        assert_eq!(array_values("RUSH_SPARSE"), vec!["a", "b", "x"]);
        assert_eq!(array_indices("RUSH_SPARSE"), vec![0, 1, 5]);

        array_unset_index("RUSH_SPARSE", 1);
        assert_eq!(array_indices("RUSH_SPARSE"), vec![0, 5]);
        assert_eq!(array_len("RUSH_SPARSE"), 2);
        unset("RUSH_SPARSE");
    }

    #[test]
    fn array_set_auto_vivifies_and_promotes_a_scalar() {
        unset("RUSH_PROMOTE");
        array_set("RUSH_PROMOTE", 2, "x"); // never existed: auto-vivifies
        assert_eq!(array_values("RUSH_PROMOTE"), vec!["x"]);
        assert_eq!(array_indices("RUSH_PROMOTE"), vec![2]);

        unset("RUSH_PROMOTE");
        set("RUSH_PROMOTE", "5");
        array_set("RUSH_PROMOTE", 3, "hi"); // scalar promoted, old value kept at [0]
        assert_eq!(array_get("RUSH_PROMOTE", 0).as_deref(), Some("5"));
        assert_eq!(array_get("RUSH_PROMOTE", 3).as_deref(), Some("hi"));
        unset("RUSH_PROMOTE");
    }

    #[test]
    fn plain_scalar_assignment_targets_element_0_of_an_existing_array() {
        unset("RUSH_ARR2");
        set_array("RUSH_ARR2", vec!["a".into(), "b".into(), "c".into()]);
        set("RUSH_ARR2", "x"); // arr=x, not arr=(x) — only index 0 changes
        assert_eq!(array_values("RUSH_ARR2"), vec!["x", "b", "c"]);
        unset("RUSH_ARR2");
    }

    #[test]
    fn array_append_and_scalar_append_quirk() {
        unset("RUSH_APP");
        set_array("RUSH_APP", vec!["a".into(), "b".into(), "c".into()]);
        array_append("RUSH_APP", vec!["d".into(), "e".into()]);
        assert_eq!(array_values("RUSH_APP"), vec!["a", "b", "c", "d", "e"]);

        // `arr+=x` (no parens) appends the *string* to element 0, not a new
        // element — a real bash quirk, verified directly.
        unset("RUSH_APP");
        set_array("RUSH_APP", vec!["a".into(), "b".into(), "c".into()]);
        append_scalar("RUSH_APP", "x");
        assert_eq!(array_values("RUSH_APP"), vec!["ax", "b", "c"]);
        unset("RUSH_APP");
    }

    #[test]
    fn local_array_shadows_and_restores_on_frame_pop() {
        unset("RUSH_LOCAL_ARR");
        set_array("RUSH_LOCAL_ARR", vec!["outer".into()]);

        push_local_frame();
        assert!(declare_local_array("RUSH_LOCAL_ARR", vec!["inner1".into(), "inner2".into()]));
        assert_eq!(array_values("RUSH_LOCAL_ARR"), vec!["inner1", "inner2"]);
        pop_local_frame();

        assert_eq!(array_values("RUSH_LOCAL_ARR"), vec!["outer"]);
        unset("RUSH_LOCAL_ARR");
    }

    #[test]
    fn assoc_basic_set_get_and_whole_array_reads() {
        unset("RUSH_ASSOC");
        set_assoc("RUSH_ASSOC", vec![("a".into(), "1".into()), ("b".into(), "2".into())]);
        assert!(is_assoc("RUSH_ASSOC"));
        assert_eq!(assoc_get("RUSH_ASSOC", "a").as_deref(), Some("1"));
        assert_eq!(assoc_get("RUSH_ASSOC", "missing"), None);
        assert_eq!(array_len("RUSH_ASSOC"), 2);
        assert_eq!(assoc_keys("RUSH_ASSOC"), vec!["a", "b"]);
        assert_eq!(array_values("RUSH_ASSOC"), vec!["1", "2"]);
        unset("RUSH_ASSOC");
    }

    #[test]
    fn key_set_dispatches_on_runtime_type() {
        // A plain/unset name: the subscript is arithmetic, matching
        // ordinary indexed-array behavior — `a` evaluates to 0.
        unset("RUSH_KEY");
        key_set("RUSH_KEY", "a", "x");
        assert!(!is_assoc("RUSH_KEY"));
        assert_eq!(array_get("RUSH_KEY", 0).as_deref(), Some("x"));
        unset("RUSH_KEY");

        // Already declared associative: the subscript is a literal string
        // key instead — verified directly against real bash, this is the
        // headline distinction the whole feature hinges on.
        set_assoc("RUSH_KEY", vec![]);
        key_set("RUSH_KEY", "a", "x");
        assert_eq!(assoc_get("RUSH_KEY", "a").as_deref(), Some("x"));
        unset("RUSH_KEY");
    }

    #[test]
    fn assoc_merge_upserts_and_unset_key_leaves_the_rest() {
        unset("RUSH_MERGE");
        set_assoc("RUSH_MERGE", vec![("a".into(), "1".into()), ("b".into(), "2".into())]);
        // A later pair overwrites an earlier one for the same key.
        assoc_merge("RUSH_MERGE", vec![("c".into(), "3".into()), ("a".into(), "99".into())]);
        assert_eq!(assoc_get("RUSH_MERGE", "a").as_deref(), Some("99"));
        assert_eq!(assoc_get("RUSH_MERGE", "c").as_deref(), Some("3"));
        assert_eq!(array_len("RUSH_MERGE"), 3);

        assoc_unset_key("RUSH_MERGE", "a");
        assert_eq!(assoc_get("RUSH_MERGE", "a"), None);
        assert_eq!(array_len("RUSH_MERGE"), 2);
        unset("RUSH_MERGE");
    }

    #[test]
    fn local_assoc_shadows_and_restores_on_frame_pop() {
        unset("RUSH_LOCAL_ASSOC");
        set_assoc("RUSH_LOCAL_ASSOC", vec![("a".into(), "outer".into())]);

        push_local_frame();
        assert!(declare_local_assoc("RUSH_LOCAL_ASSOC", vec![("a".into(), "inner".into())]));
        assert_eq!(assoc_get("RUSH_LOCAL_ASSOC", "a").as_deref(), Some("inner"));
        pop_local_frame();

        assert_eq!(assoc_get("RUSH_LOCAL_ASSOC", "a").as_deref(), Some("outer"));
        unset("RUSH_LOCAL_ASSOC");
    }
}
