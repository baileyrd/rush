//! Tab completion, and history-based hinting, for the interactive REPL.
//!
//! In command position (the first word of a pipeline/segment) completion
//! completes builtin names and executables found on `$PATH`; everywhere else
//! it defers to rustyline's own `FilenameCompleter`. Separately, as you type,
//! a greyed-out inline suggestion (fish's/zsh-autosuggestions' "history
//! autosuggestion") shows the rest of the most recent history entry that
//! starts with what's typed so far — accept it with the right arrow at the
//! end of the line, or just keep typing to ignore it.

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
            Ok(complete_command(line, pos))
        } else {
            self.files.complete_path(line, pos)
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

/// A rough (not lexer-accurate) check for whether the word being completed at
/// `pos` is the first word of its pipeline/segment: everything since the last
/// separator (`|`, `;`, `&`, `(`, newline), trimmed of leading whitespace,
/// contains no whitespace of its own.
fn in_command_position(line: &str, pos: usize) -> bool {
    let before = &line[..pos];
    let seg_start = before
        .rfind(['|', ';', '&', '(', '\n'])
        .map(|i| i + 1)
        .unwrap_or(0);
    !before[seg_start..].trim_start().contains(char::is_whitespace)
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
