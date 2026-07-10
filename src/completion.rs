//! Tab completion, and history-based hinting, for the interactive REPL.
//!
//! In command position (the first word of a pipeline/segment) completion
//! completes builtin names and executables found on `$PATH`. Elsewhere, it's
//! *argument-aware* for a handful of commands where plain filename completion
//! is rarely what's wanted: a bare `$`/`${` completes shell/environment
//! variable names, `cd`'s argument completes directories only (not files),
//! `export`/`unset`/`local`/`declare`'s arguments complete variable names,
//! `alias`/`unalias`'s complete existing alias names, and (Unix only)
//! `fg`/`bg`/`kill`/`wait`'s complete `%n` job specs from the live job table.
//! Everything else still falls through to rustyline's own `FilenameCompleter`.
//! Separately, as you type, a greyed-out inline suggestion (fish's/
//! zsh-autosuggestions' "history autosuggestion") shows the rest of the most
//! recent history entry that starts with what's typed so far — accept it
//! with the right arrow at the end of the line, or just keep typing to
//! ignore it.

use std::borrow::Cow;
use std::collections::HashSet;

use rustyline::completion::{Completer, FilenameCompleter, Pair};
use rustyline::highlight::Highlighter;
use rustyline::hint::{Hinter, HistoryHinter};
use rustyline::validate::Validator;
use rustyline::{Context, Helper};

pub struct RushHelper {
    files: FilenameCompleter,
    hints: HistoryHinter,
}

impl RushHelper {
    pub fn new() -> Self {
        Self { files: FilenameCompleter::new(), hints: HistoryHinter::new() }
    }

    /// `cd`'s own argument: reuses rustyline's own `FilenameCompleter` (so
    /// path-splitting, escaping, and matching against the actual filesystem
    /// stay identical to ordinary file completion), then filters its
    /// candidates down to directories only — matching fish/zsh's own default
    /// `cd` completion, unlike bash's, which offers plain files alongside
    /// directories without the separate bash-completion project.
    fn complete_directory(&self, line: &str, pos: usize) -> rustyline::Result<(usize, Vec<Pair>)> {
        let (start, candidates) = self.files.complete_path(line, pos)?;
        let dirs = candidates.into_iter().filter(|p| is_directory(&p.replacement)).collect();
        Ok((start, dirs))
    }
}

/// Whether `path` (a `FilenameCompleter` candidate's replacement text, so
/// relative to the shell's own cwd unless absolute) names a directory.
/// `FilenameCompleter` appends a trailing path separator to directory
/// candidates — stripped first since the filesystem check needs the bare
/// path.
fn is_directory(path: &str) -> bool {
    let trimmed = path.trim_end_matches(std::path::MAIN_SEPARATOR);
    std::path::Path::new(trimmed).is_dir()
}

impl Completer for RushHelper {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _ctx: &Context<'_>,
    ) -> rustyline::Result<(usize, Vec<Pair>)> {
        if in_command_position(line, pos) {
            return Ok(complete_command(line, pos));
        }
        if let Some(result) = complete_variable(line, pos) {
            return Ok(result);
        }
        match current_command(line, pos) {
            Some("cd") => self.complete_directory(line, pos),
            Some("export") | Some("unset") | Some("local") | Some("declare") => {
                Ok(complete_variable_name_arg(line, pos))
            }
            Some("alias") | Some("unalias") => Ok(complete_alias_name(line, pos)),
            #[cfg(unix)]
            Some("fg") | Some("bg") | Some("kill") | Some("wait") => {
                Ok(complete_job_spec(line, pos))
            }
            _ => self.files.complete_path(line, pos),
        }
    }
}

impl Hinter for RushHelper {
    type Hint = String;

    fn hint(&self, line: &str, pos: usize, ctx: &Context<'_>) -> Option<String> {
        self.hints.hint(line, pos, ctx)
    }
}
impl Highlighter for RushHelper {
    // Dims the suggestion (ANSI SGR 2) so it reads as a suggestion rather
    // than text already on the line — the same visual language fish and
    // zsh-autosuggestions use for this.
    fn highlight_hint<'h>(&self, hint: &'h str) -> Cow<'h, str> {
        Cow::Owned(format!("\x1b[2m{hint}\x1b[0m"))
    }
}
impl Validator for RushHelper {}
impl Helper for RushHelper {}

/// The start (byte offset into `line`) of the current pipeline/segment
/// containing `pos`: just after the last `|`/`;`/`&`/`(`/newline before it,
/// or the start of the line if there is none.
fn segment_start(line: &str, pos: usize) -> usize {
    line[..pos]
        .rfind(['|', ';', '&', '(', '\n'])
        .map(|i| i + 1)
        .unwrap_or(0)
}

/// A rough (not lexer-accurate) check for whether the word being completed at
/// `pos` is the first word of its pipeline/segment: everything since the
/// segment's own start, trimmed of leading whitespace, contains no
/// whitespace of its own.
fn in_command_position(line: &str, pos: usize) -> bool {
    let seg_start = segment_start(line, pos);
    !line[seg_start..pos].trim_start().contains(char::is_whitespace)
}

/// The current segment's own command name (its first word), or `None` on an
/// empty segment. Meant to be called only once the caller already knows
/// `pos` isn't itself in command position (there'd be no command yet to be
/// the argument of); doesn't check that itself. Same not-lexer-accurate
/// caveat as [`in_command_position`]: a quoted or otherwise unusual command
/// word isn't specially handled.
fn current_command(line: &str, pos: usize) -> Option<&str> {
    let seg_start = segment_start(line, pos);
    line[seg_start..pos].split_whitespace().next()
}

/// The start (byte offset into `line`) of the word being completed at `pos`.
fn word_start(line: &str, pos: usize) -> usize {
    line[..pos]
        .rfind(|c: char| c.is_whitespace() || "|;&(".contains(c))
        .map(|i| i + 1)
        .unwrap_or(0)
}

fn complete_command(line: &str, pos: usize) -> (usize, Vec<Pair>) {
    let start = word_start(line, pos);
    let prefix = &line[start..pos];

    let mut seen = HashSet::new();
    let mut candidates: Vec<Pair> = Vec::new();
    for name in matching_names(crate::builtins::all_names(), prefix)
        .into_iter()
        .chain(matching_names(path_executables(), prefix))
    {
        if seen.insert(name.clone()) {
            candidates.push(Pair { display: name.clone(), replacement: name });
        }
    }
    candidates.sort_by(|a, b| a.display.cmp(&b.display));
    (start, candidates)
}

fn matching_names<S: AsRef<str>>(names: impl IntoIterator<Item = S>, prefix: &str) -> Vec<String> {
    names
        .into_iter()
        .filter(|name| name.as_ref().starts_with(prefix))
        .map(|name| name.as_ref().to_string())
        .collect()
}

/// If the word being completed at `pos` is a bare `$name` or `${name}`
/// reference (unclosed so far — `${` with no `}` typed yet), returns its
/// completions: every shell or environment variable name starting with
/// whatever's typed after the `$`/`${`, reconstructing the `$`/`${...}`
/// form in the replacement. `None` for anything else, so the caller falls
/// through to argument- or filename-completion instead. A deliberate,
/// documented simplification: only recognized when the `$`/`${` starts the
/// whole word (not `foo$bar`, concatenated text plus a reference), and not
/// specially unwrapped out of an open double quote (`"$HO` completes as a
/// literal word starting with `"`, not as a variable reference) — matching
/// this module's existing not-lexer-accurate approach elsewhere.
fn complete_variable(line: &str, pos: usize) -> Option<(usize, Vec<Pair>)> {
    let start = word_start(line, pos);
    let word = &line[start..pos];
    let (prefix, braced) = if let Some(rest) = word.strip_prefix("${") {
        (rest, true)
    } else if let Some(rest) = word.strip_prefix('$') {
        (rest, false)
    } else {
        return None;
    };
    // A real variable name is just `[A-Za-z0-9_]*` — anything else in what's
    // typed so far (e.g. `$(`, a already-closed `${...}`) means this isn't a
    // bare, still-open variable reference.
    if prefix.contains(|c: char| !(c.is_alphanumeric() || c == '_')) {
        return None;
    }

    let mut names = variable_names();
    names.retain(|n| n.starts_with(prefix));
    names.sort();
    names.dedup();
    let candidates = names
        .into_iter()
        .map(|n| {
            let replacement = if braced { format!("${{{n}}}") } else { format!("${n}") };
            Pair { display: n, replacement }
        })
        .collect();
    Some((start, candidates))
}

/// Every shell variable name, plus every environment variable name (a
/// process can have env vars — inherited or set before rush started — that
/// were never assigned as a shell variable too), deduplicated.
fn variable_names() -> Vec<String> {
    let mut names = crate::vars::names();
    names.extend(std::env::vars().map(|(k, _)| k));
    names.sort();
    names.dedup();
    names
}

/// Completes a variable-name argument (`export`/`unset`/`local`/`declare`) —
/// not a flag (`-x`, `-A`, …), which this deliberately leaves uncompleted
/// rather than nonsensically offering variable names for it.
fn complete_variable_name_arg(line: &str, pos: usize) -> (usize, Vec<Pair>) {
    let start = word_start(line, pos);
    let prefix = &line[start..pos];
    if prefix.starts_with('-') {
        return (start, Vec::new());
    }
    let mut names = variable_names();
    names.retain(|n| n.starts_with(prefix));
    (start, names.into_iter().map(|n| Pair { display: n.clone(), replacement: n }).collect())
}

/// Completes an alias-name argument (`alias`/`unalias`) from the existing
/// alias table — only while still typing the bare name (before an `=`,
/// which starts the new definition's value instead, arbitrary text that
/// isn't itself an alias name to complete against).
fn complete_alias_name(line: &str, pos: usize) -> (usize, Vec<Pair>) {
    let start = word_start(line, pos);
    let prefix = &line[start..pos];
    if prefix.contains('=') {
        return (start, Vec::new());
    }
    let mut names: Vec<String> = crate::alias::all().into_iter().map(|(name, _)| name).collect();
    names.retain(|n| n.starts_with(prefix));
    names.sort();
    (start, names.into_iter().map(|n| Pair { display: n.clone(), replacement: n }).collect())
}

/// Completes a `%n` job-spec argument (`fg`/`bg`/`kill`/`wait`) from the
/// live job table, in exactly the plain `%N` format those builtins
/// themselves parse — Unix only, matching job control itself.
#[cfg(unix)]
fn complete_job_spec(line: &str, pos: usize) -> (usize, Vec<Pair>) {
    let start = word_start(line, pos);
    let prefix = &line[start..pos];
    (start, job_spec_candidates(crate::job::ids(), prefix))
}

/// The candidate-building half of [`complete_job_spec`], factored out so it
/// can be tested without a real job table (which needs an actual spawned
/// background process to populate).
#[cfg(unix)]
fn job_spec_candidates(ids: Vec<usize>, prefix: &str) -> Vec<Pair> {
    ids.into_iter()
        .map(|id| format!("%{id}"))
        .filter(|spec| spec.starts_with(prefix))
        .map(|spec| Pair { display: spec.clone(), replacement: spec })
        .collect()
}

/// Every executable filename found in a `$PATH` directory. Scanned fresh on
/// each call rather than cached — simple, and PATH rarely has enough entries
/// for a linear directory scan to matter.
fn path_executables() -> Vec<String> {
    let mut out = Vec::new();
    let Some(path) = std::env::var_os("PATH") else {
        return out;
    };
    for dir in std::env::split_paths(&path) {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if !is_executable(&entry) {
                continue;
            }
            if let Some(name) = entry.file_name().to_str() {
                out.push(name.to_string());
            }
        }
    }
    out
}

#[cfg(unix)]
fn is_executable(entry: &std::fs::DirEntry) -> bool {
    use std::os::unix::fs::PermissionsExt;
    entry
        .metadata()
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(entry: &std::fs::DirEntry) -> bool {
    entry.metadata().map(|m| m.is_file()).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_position_detection() {
        assert!(in_command_position("ec", 2));
        assert!(!in_command_position("echo fo", 7));
        assert!(in_command_position("ls foo | gr", 11));
        assert!(in_command_position("true && ec", 10));
        assert!(in_command_position("cmd1; ec", 8));
        assert!(in_command_position("(ec", 3));
        assert!(!in_command_position("echo hi > /tmp/f", 16));
    }

    #[test]
    fn word_start_finds_current_word() {
        assert_eq!(word_start("echo fo", 7), 5);
        assert_eq!(word_start("ls foo | gr", 11), 9);
        assert_eq!(word_start("ec", 2), 0);
    }

    #[test]
    fn matching_names_filters_by_prefix() {
        let names = ["cd", "cat", "echo", "export"];
        assert_eq!(matching_names(names, "e"), vec!["echo".to_string(), "export".to_string()]);
    }

    #[test]
    fn current_command_returns_the_segments_own_command_name() {
        assert_eq!(current_command("echo hi", 7), Some("echo"));
        assert_eq!(current_command("ls foo | grep b", 15), Some("grep"));
        assert_eq!(current_command("cd ", 3), Some("cd"));
        assert_eq!(current_command("", 0), None);
    }

    #[test]
    fn complete_variable_offers_matching_names_for_dollar_and_braced_dollar() {
        crate::vars::set("RUSH_TEST_COMPLETION_VAR", "1");

        let line = "echo $RUSH_TEST_COMPLETION_V";
        let (start, pairs) = complete_variable(line, line.len()).unwrap();
        assert_eq!(start, 5);
        assert!(pairs.iter().any(|p| p.replacement == "$RUSH_TEST_COMPLETION_VAR"));

        let line = "echo ${RUSH_TEST_COMPLETION_V";
        let (start, pairs) = complete_variable(line, line.len()).unwrap();
        assert_eq!(start, 5);
        assert!(pairs.iter().any(|p| p.replacement == "${RUSH_TEST_COMPLETION_VAR}"));
    }

    #[test]
    fn complete_variable_is_none_outside_a_bare_dollar_reference() {
        assert!(complete_variable("echo hi", 7).is_none());
        // `$(` is command substitution, not a bare variable reference.
        let line = "echo $(cmd";
        assert!(complete_variable(line, line.len()).is_none());
    }

    #[test]
    fn complete_variable_name_arg_skips_flags_but_offers_names() {
        crate::vars::set("RUSH_TEST_COMPLETION_VAR2", "1");

        let (_, pairs) = complete_variable_name_arg("declare -A", 10);
        assert!(pairs.is_empty());

        let line = "unset RUSH_TEST_COMPLETION_VAR2";
        let (_, pairs) = complete_variable_name_arg(line, line.len());
        assert!(pairs.iter().any(|p| p.display == "RUSH_TEST_COMPLETION_VAR2"));
    }

    #[test]
    fn complete_alias_name_stops_completing_after_equals() {
        crate::alias::set("rush_test_completion_alias", "ls");

        let line = "unalias rush_test_completion_al";
        let (_, pairs) = complete_alias_name(line, line.len());
        assert!(pairs.iter().any(|p| p.display == "rush_test_completion_alias"));

        let line = "alias rush_test_completion_alias=";
        let (_, pairs) = complete_alias_name(line, line.len());
        assert!(pairs.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn job_spec_candidates_formats_as_percent_n_and_filters_by_prefix() {
        let pairs = job_spec_candidates(vec![1, 2, 10], "%1");
        assert_eq!(
            pairs.iter().map(|p| p.replacement.as_str()).collect::<Vec<_>>(),
            vec!["%1", "%10"]
        );

        assert_eq!(job_spec_candidates(vec![1, 2, 10], "").len(), 3);
    }

    #[test]
    fn is_directory_checks_the_filesystem_after_stripping_a_trailing_separator() {
        assert!(is_directory("/tmp"));
        assert!(is_directory(&format!("/tmp{}", std::path::MAIN_SEPARATOR)));
        assert!(!is_directory("/this/path/should/not/exist/anywhere/hopefully"));
    }

    /// `rustyline::hint::HistoryHinter` only offers a hint with the cursor at
    /// the end of the line (`pos == line.len()`) — matching fish/
    /// zsh-autosuggestions' own behavior of only suggesting while typing at
    /// the end, not mid-line editing.
    #[test]
    fn hints_the_rest_of_the_most_recent_matching_history_entry() {
        use rustyline::history::{DefaultHistory, History};

        let helper = RushHelper::new();
        let mut history = DefaultHistory::new();
        history.add("echo hello world").unwrap();
        let ctx = Context::new(&history);

        assert_eq!(helper.hint("echo he", 7, &ctx).as_deref(), Some("llo world"));
    }

    #[test]
    fn no_hint_on_an_empty_line_or_an_exact_history_match() {
        use rustyline::history::{DefaultHistory, History};

        let helper = RushHelper::new();
        let mut history = DefaultHistory::new();
        history.add("echo hi").unwrap();
        let ctx = Context::new(&history);

        assert_eq!(helper.hint("", 0, &ctx), None);
        assert_eq!(helper.hint("echo hi", 7, &ctx), None);
    }

    #[test]
    fn highlight_hint_dims_the_suggestion() {
        let helper = RushHelper::new();
        assert_eq!(helper.highlight_hint("llo world"), "\x1b[2mllo world\x1b[0m");
    }
}
