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
//! Everything else falls through to plain filename completion.
//! Separately, as you type, a greyed-out inline suggestion (fish's/
//! zsh-autosuggestions' "history autosuggestion") shows the rest of the most
//! recent history entry that starts with what's typed so far — accept it
//! with the right arrow at the end of the line, or just keep typing to
//! ignore it.

use std::collections::HashSet;

/// One completion candidate (the editor crate's type): the text shown in
/// the columned list, and the text inserted into the buffer.
pub use rusty_lines::Candidate;

/// A `Candidate` whose display and replacement are the same name.
fn plain(name: String) -> Candidate {
    Candidate { display: name.clone(), replacement: name }
}

/// The completion entry point: candidates for the word at `pos`, plus the
/// byte offset that word starts at (the editor replaces `start..pos`).
pub fn complete(line: &str, pos: usize) -> (usize, Vec<Candidate>) {
    if in_command_position(line, pos) {
        return complete_command(line, pos);
    }
    if let Some(result) = complete_variable(line, pos) {
        return result;
    }
    // A registered programmable-completion spec (C93) wins over the
    // built-in per-command completers.
    if let Some(cmd) = current_command(line, pos)
        && let Some(spec) = programmable::get(cmd).or_else(programmable::default_spec)
    {
        let start = word_start(line, pos);
        let word = &line[start..pos];
        let seg_start = segment_start(line, pos);
        let words: Vec<String> =
            line[seg_start..pos].split_whitespace().map(str::to_string).collect();
        let cword = words.len().saturating_sub(if word.is_empty() { 0 } else { 1 });
        let mut full_words = words.clone();
        if word.is_empty() {
            full_words.push(String::new());
        }
        let candidates: Vec<Candidate> =
            programmable::generate(&spec, word, &full_words, cword, &line[seg_start..pos])
                .into_iter()
                .map(plain)
                .collect();
        if !candidates.is_empty() {
            return (start, candidates);
        }
        // A spec with `-o default` falls back to filename completion.
        if spec.options.iter().any(|o| o == "default") {
            return complete_path(line, pos);
        }
        return (start, candidates);
    }
    match current_command(line, pos) {
        Some("cd") => complete_directory(line, pos),
        Some("export") | Some("unset") | Some("local") | Some("declare") => {
            complete_variable_name_arg(line, pos)
        }
        Some("alias") | Some("unalias") => complete_alias_name(line, pos),
        #[cfg(unix)]
        Some("fg") | Some("bg") | Some("kill") | Some("wait") => complete_job_spec(line, pos),
        _ => complete_path(line, pos),
    }
}

/// Plain filename completion — the fallback for anything without a more
/// specific completer. The word splits at its last `/`: the directory
/// part is scanned (with a leading `~` resolved through `$HOME` for the
/// scan while the replacement keeps the user's own spelling), dotfiles
/// offered only when the typed name already starts with a dot, and
/// directory candidates get a trailing `/` so completion can keep
/// descending.
fn complete_path(line: &str, pos: usize) -> (usize, Vec<Candidate>) {
    let start = word_start(line, pos);
    let word = &line[start..pos];
    let (dir_part, name_prefix) = match word.rfind('/') {
        Some(i) => (&word[..=i], &word[i + 1..]),
        None => ("", word),
    };
    let scan_dir = if dir_part.is_empty() {
        ".".to_string()
    } else if let Some(rest) = dir_part.strip_prefix("~/") {
        format!("{}/{}", crate::vars::get("HOME").unwrap_or_default(), rest)
    } else {
        dir_part.to_string()
    };
    let mut candidates = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&scan_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            // `completion-ignore-case` (C128): fold the prefix compare.
            let prefix_ok = if readline_flag("completion-ignore-case") {
                name.to_lowercase().starts_with(&name_prefix.to_lowercase())
            } else {
                name.starts_with(name_prefix)
            };
            if !prefix_ok || (name.starts_with('.') && !name_prefix.starts_with('.')) {
                continue;
            }
            let is_dir = entry.path().is_dir();
            let display = if is_dir { format!("{name}/") } else { name.clone() };
            let replacement = format!("{dir_part}{name}{}", if is_dir { "/" } else { "" });
            candidates.push(Candidate { display, replacement });
        }
    }
    candidates.sort_by(|a, b| a.display.cmp(&b.display));
    (start, candidates)
}

/// `cd`'s own argument: ordinary path completion filtered down to
/// directories only — matching fish/zsh's own default `cd` completion,
/// unlike bash's, which offers plain files alongside directories without
/// the separate bash-completion project.
fn complete_directory(line: &str, pos: usize) -> (usize, Vec<Candidate>) {
    let (start, candidates) = complete_path(line, pos);
    let dir_part_end = line[start..pos].rfind('/').map(|i| start + i + 1).unwrap_or(start);
    let _ = dir_part_end;
    (start, candidates.into_iter().filter(|c| is_directory(&c.replacement)).collect())
}

/// Whether `path` (a candidate's replacement text, so relative to the
/// shell's own cwd unless absolute — with a leading `~` resolved through
/// `$HOME` first) names a directory.
fn is_directory(path: &str) -> bool {
    let resolved = if let Some(rest) = path.strip_prefix("~/") {
        format!("{}/{}", crate::vars::get("HOME").unwrap_or_default(), rest)
    } else {
        path.to_string()
    };
    let trimmed = resolved.trim_end_matches(std::path::MAIN_SEPARATOR);
    std::path::Path::new(trimmed).is_dir()
}

/// The history autosuggestion (C33): the rest of the most recent history
/// entry starting with `line` — only for a non-empty line, and never when
/// the line already *is* that entry. (The editor only asks with the
/// cursor at the end of the line, matching fish/zsh-autosuggestions.)
pub fn hint(line: &str, history: &[String]) -> Option<String> {
    if line.is_empty() {
        return None;
    }
    history
        .iter()
        .rev()
        .find(|h| h.starts_with(line) && h.len() > line.len())
        .map(|h| h[line.len()..].to_string())
}

/// Classify `line` into colored spans — live syntax highlighting (C68),
/// fish-style: command words are green when they resolve
/// (keyword/builtin/function/alias/`$PATH`) and red when they don't;
/// strings yellow (an *unmatched* quote's span red — the live-validation
/// half); comments dimmed; `$`-expansions cyan; operators magenta. Built
/// on a small, error-tolerant span scanner — deliberately not the real
/// lexer, which returns tokens without byte spans and hard-errors on
/// exactly the incomplete input an in-progress line is made of.
pub fn highlight_line(line: &str) -> String {
    const RESET: &str = "\x1b[0m";
    const GREEN: &str = "\x1b[32m";
    const RED: &str = "\x1b[31m";
    const YELLOW: &str = "\x1b[33m";
    const CYAN: &str = "\x1b[36m";
    const MAGENTA: &str = "\x1b[35m";
    const DIM: &str = "\x1b[2m";

    let mut out = String::with_capacity(line.len() * 2);
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    let mut command_position = true;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '#' => {
                // Comment to end of line.
                out.push_str(DIM);
                out.extend(&chars[i..]);
                out.push_str(RESET);
                break;
            }
            '\'' | '"' => {
                let quote = c;
                let close = chars[i + 1..].iter().position(|&q| q == quote).map(|p| i + 1 + p);
                match close {
                    Some(end) => {
                        out.push_str(YELLOW);
                        out.extend(&chars[i..=end]);
                        out.push_str(RESET);
                        i = end + 1;
                    }
                    None => {
                        // Unmatched quote: the rest of the line flags red.
                        out.push_str(RED);
                        out.extend(&chars[i..]);
                        out.push_str(RESET);
                        break;
                    }
                }
            }
            '$' => {
                let mut end = i + 1;
                if chars.get(end) == Some(&'{') {
                    while end < chars.len() && chars[end] != '}' {
                        end += 1;
                    }
                    end = (end + 1).min(chars.len());
                } else {
                    while end < chars.len() && (chars[end] == '_' || chars[end].is_ascii_alphanumeric()) {
                        end += 1;
                    }
                }
                out.push_str(CYAN);
                out.extend(&chars[i..end]);
                out.push_str(RESET);
                i = end.max(i + 1);
            }
            '|' | '&' | ';' | '<' | '>' | '(' | ')' => {
                out.push_str(MAGENTA);
                out.push(c);
                out.push_str(RESET);
                command_position = true;
                i += 1;
            }
            c if c.is_whitespace() => {
                out.push(c);
                i += 1;
            }
            _ => {
                let start = i;
                while i < chars.len()
                    && !chars[i].is_whitespace()
                    && !matches!(chars[i], '|' | '&' | ';' | '<' | '>' | '(' | ')' | '\'' | '"' | '$' | '#')
                {
                    i += 1;
                }
                let word: String = chars[start..i].iter().collect();
                if command_position && !word.contains('=') {
                    let ok = crate::parser::RESERVED.contains(&word.as_str())
                        || crate::builtins::is_builtin(&word)
                        || crate::func::exists(&word)
                        || crate::alias::get(&word).is_some()
                        || crate::builtins::resolve_in_path(&word).is_some();
                    out.push_str(if ok { GREEN } else { RED });
                    out.push_str(&word);
                    out.push_str(RESET);
                    command_position = false;
                } else {
                    // Assignment prefixes (`FOO=bar cmd`) keep command
                    // position for the next word.
                    if !(command_position && word.contains('=')) {
                        command_position = false;
                    }
                    out.push_str(&word);
                }
            }
        }
    }
    out
}
/// The abbreviation expansion (C70) that should replace the word ending
/// at `pos`, if any: the word must be a defined abbreviation *and* be in
/// command position — pure logic, unit-testable apart from the key-event
/// plumbing in `AbbrSpaceHandler`. Returns the byte offset the word
/// starts at and its expansion.
pub(crate) fn abbr_expansion(line: &str, pos: usize) -> Option<(usize, String)> {
    let start = line[..pos]
        .rfind(char::is_whitespace)
        .map(|i| i + 1)
        .unwrap_or(0)
        .max(segment_start(line, pos));
    let word = &line[start..pos];
    if word.is_empty() {
        return None;
    }
    // Command position: nothing but whitespace between the segment start
    // and the word itself.
    if !line[segment_start(line, pos)..start].trim().is_empty() {
        return None;
    }
    crate::alias::abbr_get(word).map(|exp| (start, exp))
}

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

fn complete_command(line: &str, pos: usize) -> (usize, Vec<Candidate>) {
    let start = word_start(line, pos);
    let prefix = &line[start..pos];

    let mut seen = HashSet::new();
    let mut candidates: Vec<Candidate> = Vec::new();
    for name in matching_names(crate::builtins::all_names(), prefix)
        .into_iter()
        .chain(matching_names(path_executables(), prefix))
    {
        if seen.insert(name.clone()) {
            candidates.push(plain(name));
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
fn complete_variable(line: &str, pos: usize) -> Option<(usize, Vec<Candidate>)> {
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
            Candidate { display: n, replacement }
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
fn complete_variable_name_arg(line: &str, pos: usize) -> (usize, Vec<Candidate>) {
    let start = word_start(line, pos);
    let prefix = &line[start..pos];
    if prefix.starts_with('-') {
        return (start, Vec::new());
    }
    let mut names = variable_names();
    names.retain(|n| n.starts_with(prefix));
    (start, names.into_iter().map(plain).collect())
}

/// Completes an alias-name argument (`alias`/`unalias`) from the existing
/// alias table — only while still typing the bare name (before an `=`,
/// which starts the new definition's value instead, arbitrary text that
/// isn't itself an alias name to complete against).
fn complete_alias_name(line: &str, pos: usize) -> (usize, Vec<Candidate>) {
    let start = word_start(line, pos);
    let prefix = &line[start..pos];
    if prefix.contains('=') {
        return (start, Vec::new());
    }
    let mut names: Vec<String> = crate::alias::all().into_iter().map(|(name, _)| name).collect();
    names.retain(|n| n.starts_with(prefix));
    names.sort();
    (start, names.into_iter().map(plain).collect())
}

/// Completes a `%n` job-spec argument (`fg`/`bg`/`kill`/`wait`) from the
/// live job table, in exactly the plain `%N` format those builtins
/// themselves parse — Unix only, matching job control itself.
#[cfg(unix)]
fn complete_job_spec(line: &str, pos: usize) -> (usize, Vec<Candidate>) {
    let start = word_start(line, pos);
    let prefix = &line[start..pos];
    (start, job_spec_candidates(crate::job::ids(), prefix))
}

/// The candidate-building half of [`complete_job_spec`], factored out so it
/// can be tested without a real job table (which needs an actual spawned
/// background process to populate).
#[cfg(unix)]
fn job_spec_candidates(ids: Vec<usize>, prefix: &str) -> Vec<Candidate> {
    ids.into_iter()
        .map(|id| format!("%{id}"))
        .filter(|spec| spec.starts_with(prefix))
        .map(plain)
        .collect()
}

/// Every executable filename found in a `$PATH` directory. Scanned fresh on
/// each call rather than cached — simple, and PATH rarely has enough entries
/// for a linear directory scan to matter.
fn path_executables() -> Vec<String> {
    let mut out = Vec::new();
    // `vars::get` alone — no `std::env` fallback (C36/C40): falling back
    // would keep completing from `PATH`'s original value after `unset`.
    let Some(path) = crate::vars::get("PATH") else {
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

    #[test]
    fn hints_the_rest_of_the_most_recent_matching_history_entry() {
        let history = vec!["echo old".to_string(), "echo hello world".to_string()];
        assert_eq!(hint("echo he", &history).as_deref(), Some("llo world"));
    }

    #[test]
    fn no_hint_on_an_empty_line_or_an_exact_history_match() {
        let history = vec!["echo hi".to_string()];
        assert_eq!(hint("", &history), None);
        assert_eq!(hint("echo hi", &history), None);
    }

    // C70: abbreviation expansion decision — command position only.
    #[test]
    fn abbr_expansion_logic() {
        crate::alias::abbr_set("gs", "git status");
        assert_eq!(abbr_expansion("gs", 2), Some((0, "git status".to_string())));
        // After a pipe/semicolon: command position again.
        assert_eq!(abbr_expansion("true | gs", 9), Some((7, "git status".to_string())));
        assert_eq!(abbr_expansion("true; gs", 8), Some((6, "git status".to_string())));
        // Argument position: no expansion.
        assert_eq!(abbr_expansion("echo gs", 7), None);
        // Not an abbreviation / empty word: no expansion.
        assert_eq!(abbr_expansion("ls", 2), None);
        assert_eq!(abbr_expansion("gs ", 3), None);
        crate::alias::abbr_unset("gs");
    }

    // C68: span classification — command words green/red by
    // resolvability, unmatched quotes red, comments dimmed, $vars cyan.
    #[test]
    fn highlight_classifies_spans() {
        let h = highlight_line("echo hi");
        assert!(h.starts_with("\x1b[32mecho\x1b[0m"), "got: {h:?}");

        let h = highlight_line("nosuchcmd_xyz hi");
        assert!(h.starts_with("\x1b[31mnosuchcmd_xyz\x1b[0m"), "got: {h:?}");

        let h = highlight_line("echo \"done");
        assert!(h.contains("\x1b[31m\"done\x1b[0m"), "unmatched quote red: {h:?}");
        let h = highlight_line("echo \"done\"");
        assert!(h.contains("\x1b[33m\"done\"\x1b[0m"), "matched quote yellow: {h:?}");

        let h = highlight_line("echo $HOME # note");
        assert!(h.contains("\x1b[36m$HOME\x1b[0m"), "var cyan: {h:?}");
        assert!(h.contains("\x1b[2m# note\x1b[0m"), "comment dim: {h:?}");

        // The word after a pipe is a command word again; keywords count.
        let h = highlight_line("true | nosuch_c68");
        assert!(h.contains("\x1b[31mnosuch_c68\x1b[0m"), "got: {h:?}");
        let h = highlight_line("if true");
        assert!(h.starts_with("\x1b[32mif\x1b[0m"), "keyword green: {h:?}");

        // An assignment prefix keeps command position for the next word.
        let h = highlight_line("FOO=1 echo x");
        assert!(h.contains("\x1b[32mecho\x1b[0m"), "got: {h:?}");
    }

}

thread_local! {
    // readline variables set via `bind 'set var value'` (C128) — a small
    // set rush's completer can act on.
    static READLINE_VARS: std::cell::RefCell<std::collections::HashMap<String, String>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

/// `bind 'set name value'` (C128): store a readline variable. Returns
/// whether the name is one rush recognizes (others are accepted silently,
/// like bash).
pub fn apply_readline_variable(assignment: &str) -> bool {
    let (name, value) = match assignment.split_once(char::is_whitespace) {
        Some((n, v)) => (n.trim(), v.trim()),
        None => (assignment.trim(), "on"),
    };
    READLINE_VARS.with(|v| v.borrow_mut().insert(name.to_string(), value.to_string()));
    matches!(name, "completion-ignore-case" | "show-all-if-ambiguous" | "menu-complete-display-prefix")
}

/// Whether a readline boolean variable is on (`on`/`1`/`true`).
pub fn readline_flag(name: &str) -> bool {
    READLINE_VARS.with(|v| {
        v.borrow().get(name).map(|s| matches!(s.as_str(), "on" | "1" | "true")).unwrap_or(false)
    })
}

/// Programmable completion (C93): `complete`/`compgen`/`compopt` and the
/// `COMPREPLY`/`COMP_WORDS`/`COMP_CWORD` protocol, so bash-completion
/// scripts load and run.
pub mod programmable {
    use std::cell::RefCell;
    use std::collections::HashMap;

    /// One registered completion spec (`complete [options] name`).
    #[derive(Clone, Default)]
    pub struct Spec {
        /// `-F function` — call this shell function; it fills `COMPREPLY`.
        pub function: Option<String>,
        /// `-C command` — run it; each output line is a candidate.
        pub command: Option<String>,
        /// `-W wordlist` — a `$IFS`-split, then expanded, set of words.
        pub wordlist: Option<String>,
        /// `-a`/`-b`/`-c`/`-d`/`-e`/`-f`/`-j`/`-v`/`-A action` letters.
        pub actions: Vec<String>,
        /// `-P prefix` / `-S suffix` attached to every candidate.
        pub prefix: Option<String>,
        pub suffix: Option<String>,
        /// `-o` options (`nospace`, `default`, `filenames`, …).
        pub options: Vec<String>,
    }

    thread_local! {
        static SPECS: RefCell<HashMap<String, Spec>> = RefCell::new(HashMap::new());
        static DEFAULT_SPEC: RefCell<Option<Spec>> = const { RefCell::new(None) };
    }

    pub fn register(name: &str, spec: Spec) {
        SPECS.with(|s| s.borrow_mut().insert(name.to_string(), spec));
    }

    pub fn register_default(spec: Spec) {
        DEFAULT_SPEC.with(|d| *d.borrow_mut() = Some(spec));
    }

    pub fn remove(name: &str) {
        SPECS.with(|s| s.borrow_mut().remove(name));
    }

    pub fn clear() {
        SPECS.with(|s| s.borrow_mut().clear());
        DEFAULT_SPEC.with(|d| *d.borrow_mut() = None);
    }

    pub fn get(name: &str) -> Option<Spec> {
        SPECS.with(|s| s.borrow().get(name).cloned())
    }

    pub fn default_spec() -> Option<Spec> {
        DEFAULT_SPEC.with(|d| d.borrow().clone())
    }

    pub fn registered_names() -> Vec<String> {
        let mut names: Vec<String> = SPECS.with(|s| s.borrow().keys().cloned().collect());
        names.sort();
        names
    }

    /// Generate the raw candidate list for a spec against `word` (the text
    /// being completed) — shared by `compgen` and the live completer.
    /// `line`/`cword`/`words` seed the completion variables for a `-F`
    /// function.
    pub fn generate(spec: &Spec, word: &str, words: &[String], cword: usize, line: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();

        for action in &spec.actions {
            out.extend(action_candidates(action, word));
        }
        if let Some(wl) = &spec.wordlist {
            let expanded = crate::expand::expand_dollars(wl).unwrap_or_else(|_| wl.clone());
            out.extend(expanded.split_whitespace().map(str::to_string));
        }
        if let Some(cmd) = &spec.command {
            if let Ok(output) = run_capture(cmd) {
                out.extend(output.lines().map(str::to_string));
            }
        }
        if let Some(func) = &spec.function {
            out.extend(run_completion_function(func, words, cword, line));
        }

        // Keep only candidates that start with the word being completed —
        // `-W`/`-A` filter by prefix (a `-F` function is trusted to have
        // filtered COMPREPLY itself, so its output isn't re-filtered).
        let from_function = spec.function.is_some();
        out.retain(|c| from_function || c.starts_with(word));

        for c in &mut out {
            if let Some(p) = &spec.prefix {
                *c = format!("{p}{c}");
            }
            if let Some(s) = &spec.suffix {
                c.push_str(s);
            }
        }
        out
    }

    /// The candidates a single `-A action` (or its `-a`/`-c`/… shorthand)
    /// produces — the bash action set rush can answer locally.
    fn action_candidates(action: &str, word: &str) -> Vec<String> {
        match action {
            "command" => {
                let mut names = crate::builtins::all_names().iter().map(|s| s.to_string()).collect::<Vec<_>>();
                names.extend(super::path_executables());
                names.into_iter().filter(|n| n.starts_with(word)).collect()
            }
            "builtin" => crate::builtins::all_names()
                .iter()
                .filter(|n| n.starts_with(word))
                .map(|s| s.to_string())
                .collect(),
            "alias" => crate::alias::names().into_iter().filter(|n| n.starts_with(word)).collect(),
            "function" => crate::func::names().into_iter().filter(|n| n.starts_with(word)).collect(),
            "variable" => crate::vars::names().into_iter().filter(|n| n.starts_with(word)).collect(),
            "file" | "directory" => {
                let (start, cands) = super::complete_path(word, word.len());
                let _ = start;
                let mut v: Vec<String> = cands.into_iter().map(|c| c.replacement).collect();
                if action == "directory" {
                    v.retain(|p| super::is_directory(p));
                }
                v
            }
            "keyword" => ["if", "then", "else", "elif", "fi", "for", "while", "until", "do",
                "done", "case", "esac", "select", "function", "in", "time", "coproc"]
                .iter()
                .filter(|k| k.starts_with(word))
                .map(|s| s.to_string())
                .collect(),
            "job" => crate::vars::names().into_iter().filter(|_| false).collect(), // no stable API
            "signal" => ["SIGHUP", "SIGINT", "SIGQUIT", "SIGKILL", "SIGTERM", "SIGSTOP",
                "SIGCONT", "SIGUSR1", "SIGUSR2"]
                .iter()
                .filter(|s| s.starts_with(word))
                .map(|s| s.to_string())
                .collect(),
            _ => Vec::new(),
        }
    }

    /// Run a `-F` completion function: seed `COMP_WORDS`/`COMP_CWORD`/
    /// `COMP_LINE`/`COMP_POINT` and the function's positional parameters
    /// (`$1` command, `$2` current word, `$3` previous word — bash's
    /// convention), call it, then read back the `COMPREPLY` array.
    fn run_completion_function(func: &str, words: &[String], cword: usize, line: &str) -> Vec<String> {
        crate::vars::set_array("COMP_WORDS", words.to_vec());
        crate::vars::set("COMP_CWORD", &cword.to_string());
        crate::vars::set("COMP_LINE", line);
        crate::vars::set("COMP_POINT", &line.len().to_string());
        crate::vars::unset("COMPREPLY");

        let cmd = words.first().cloned().unwrap_or_default();
        let cur = words.get(cword).cloned().unwrap_or_default();
        let prev = if cword > 0 { words.get(cword - 1).cloned().unwrap_or_default() } else { String::new() };
        let argv = vec![func.to_string(), cmd, cur, prev];
        let _ = crate::exec::call_function_for_completion(&argv);

        crate::vars::array_values("COMPREPLY")
    }

    fn run_capture(cmd: &str) -> Result<String, String> {
        let list = crate::parser::parse(cmd).map_err(|e| e.to_string())?;
        crate::exec::capture_list(&list)
    }
}
