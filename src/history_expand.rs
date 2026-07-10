//! `!`-history expansion (bash/ksh/zsh's csh-style bang-history recall) —
//! interactive-only, matching real bash's own `histexpand` default (on
//! interactively, off in scripts). A textual preprocessing pass over the
//! raw input line, run before parsing, exactly like real bash's own
//! readline/history layer does it — so it applies regardless of what the
//! line eventually parses as, and a failed reference blocks execution
//! entirely rather than becoming a shell syntax error.
//!
//! Scope: whole-event recall (`!!`, `!n`, `!-n`, `!string`, `!?string?`)
//! and the previous command's own word designators (`!$`, `!^`, `!*`,
//! `!:n`). Not supported, an accepted, documented gap: combining an
//! explicit event specifier with a word designator (`!2:1`, `!echo:$`) —
//! real bash supports this, but the two forms above cover the
//! overwhelming majority of real usage (`sudo !!`, reusing `!$`) on their
//! own.

/// Expand any `!`-history references in `line` against `history` (oldest
/// first, matching real bash's own absolute numbering — entry 1 is
/// `history[0]`, matching what `history`'s own listing shows). `Ok(None)`:
/// nothing to expand, pass `line` through unchanged (the common — and
/// only — case for the vast majority of lines, verified directly against
/// real bash to be a total no-op). `Ok(Some(expanded))`: something
/// changed — the caller should echo it before running it, matching real
/// bash (except for a bare `\!` escape, which isn't itself an "event" and
/// doesn't get echoed, verified directly). `Err(message)`: a reference
/// couldn't be resolved ("event not found"/"bad word specifier") — the
/// caller should print the error and run nothing, matching real bash.
pub fn expand(line: &str, history: &[String]) -> Result<Option<String>, String> {
    if !line.contains('!') {
        return Ok(None);
    }

    let chars: Vec<char> = line.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    let mut changed = false;

    while i < chars.len() {
        let c = chars[i];
        // A single-quoted span suppresses expansion (verified directly:
        // `echo '!!'` prints the literal `!!`) — unlike double quotes,
        // which do *not* protect against it, matching real bash exactly.
        if c == '\'' {
            out.push(c);
            i += 1;
            while i < chars.len() && chars[i] != '\'' {
                out.push(chars[i]);
                i += 1;
            }
            if i < chars.len() {
                out.push(chars[i]);
                i += 1;
            }
            continue;
        }
        // `\!` — escaped, suppresses expansion, drops the backslash. Not
        // itself treated as an "event", so it doesn't trigger the
        // echo-before-running behavior (verified directly).
        if c == '\\' && chars.get(i + 1) == Some(&'!') {
            out.push('!');
            i += 2;
            continue;
        }
        if c == '!'
            && let Some((replacement, consumed)) = resolve(&chars[i..], history)?
        {
            out.push_str(&replacement);
            i += consumed;
            changed = true;
            continue;
        }
        out.push(c);
        i += 1;
    }

    Ok(if changed { Some(out) } else { None })
}

/// Try to resolve a `!`-reference starting at `rest[0] == '!'`, returning
/// the replacement text and how many characters (from `rest[0]`) it
/// consumed. `Ok(None)` if `rest[1]` isn't a valid trigger character at
/// all — an ordinary literal `!` (`a!=b`, `!` at end of line or before a
/// space), verified directly to be left completely untouched by real bash
/// too, with no error.
fn resolve(rest: &[char], history: &[String]) -> Result<Option<(String, usize)>, String> {
    let Some(&next) = rest.get(1) else { return Ok(None) };

    // The previous command's own word designators — `$`/`^`/`*` always
    // refer to it; bare `:N` (no event specifier before the `:`) does too.
    if matches!(next, '$' | '^' | '*') {
        let words = previous_words(history, &format!("!{next}"))?;
        return Ok(Some((word_designator(&words, next)?, 2)));
    }
    if next == ':' && matches!(rest.get(2), Some(d) if d.is_ascii_digit()) {
        let (n, len) = take_digits(rest, 2);
        let words = previous_words(history, &format!("!:{n}"))?;
        let word = words
            .get(n)
            .copied()
            .ok_or_else(|| format!("!:{n}: bad word specifier"))?;
        return Ok(Some((word.to_string(), len)));
    }

    // Whole-event recall.
    if next == '!' {
        let event = history.last().ok_or("!!: event not found")?;
        return Ok(Some((event.clone(), 2)));
    }
    if next == '-' && matches!(rest.get(2), Some(d) if d.is_ascii_digit()) {
        let (n, len) = take_digits(rest, 2);
        let event = (n > 0)
            .then(|| history.len().checked_sub(n))
            .flatten()
            .and_then(|idx| history.get(idx))
            .ok_or_else(|| format!("!-{n}: event not found"))?;
        return Ok(Some((event.clone(), len)));
    }
    if next.is_ascii_digit() {
        let (n, len) = take_digits(rest, 1);
        let event = n
            .checked_sub(1)
            .and_then(|idx| history.get(idx))
            .ok_or_else(|| format!("!{n}: event not found"))?;
        return Ok(Some((event.clone(), len)));
    }
    if next == '?' {
        let mut j = 2;
        let mut needle = String::new();
        while j < rest.len() && rest[j] != '?' {
            needle.push(rest[j]);
            j += 1;
        }
        let closed = rest.get(j) == Some(&'?');
        let len = j + usize::from(closed);
        let event = history
            .iter()
            .rev()
            .find(|entry| entry.contains(&needle))
            .ok_or_else(|| format!("!?{needle}: event not found"))?;
        return Ok(Some((event.clone(), len)));
    }
    // A bare word: search backwards for the most recent entry starting
    // with it. Excludes `=` (so `a!=b`, the common `test`/`[[` idiom,
    // is never mistaken for a search) and whitespace, matching real bash.
    if next != '=' && !next.is_whitespace() {
        let mut j = 1;
        let mut needle = String::new();
        while j < rest.len() && !rest[j].is_whitespace() && rest[j] != '!' {
            needle.push(rest[j]);
            j += 1;
        }
        let event = history
            .iter()
            .rev()
            .find(|entry| entry.starts_with(&needle))
            .ok_or_else(|| format!("!{needle}: event not found"))?;
        return Ok(Some((event.clone(), j)));
    }

    Ok(None)
}

/// The previous command's words, split on plain whitespace — a documented
/// simplification of real bash's own quote-aware word splitting for this
/// purpose (`echo "a b" c` then `!:1` gives real bash's `"a b"` as one
/// word; this gives `"a`, matching a plain `split_whitespace` instead).
/// `event_desc` is just this reference's own text, for the error message.
fn previous_words<'h>(history: &'h [String], event_desc: &str) -> Result<Vec<&'h str>, String> {
    let prev = history.last().ok_or_else(|| format!("{event_desc}: event not found"))?;
    Ok(prev.split_whitespace().collect())
}

/// `$` (last word), `^` (first argument, word 1), or `*` (all arguments,
/// words 1.. joined by a space) — verified directly: on a previous command
/// with no arguments at all (just the bare command word), `$` falls back to
/// that command word (there's nothing else to be "last"), `*` is an
/// accepted-empty result, but `^` — which specifically means "the first
/// *argument*" — errors, matching real bash's own "bad word specifier".
fn word_designator(words: &[&str], designator: char) -> Result<String, String> {
    match designator {
        '$' => words.last().map(|s| s.to_string()).ok_or_else(|| "!$: event not found".into()),
        '^' => words.get(1).map(|s| s.to_string()).ok_or_else(|| "!^: bad word specifier".into()),
        '*' => Ok(words.get(1..).unwrap_or(&[]).join(" ")),
        _ => unreachable!(),
    }
}

/// Consume a run of ASCII digits starting at `rest[start]`, returning the
/// parsed number and the total length from `rest[0]` (not `start`) that
/// this consumed.
fn take_digits(rest: &[char], start: usize) -> (usize, usize) {
    let mut j = start;
    let mut digits = String::new();
    while matches!(rest.get(j), Some(d) if d.is_ascii_digit()) {
        digits.push(rest[j]);
        j += 1;
    }
    (digits.parse().unwrap_or(0), j)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hist(entries: &[&str]) -> Vec<String> {
        entries.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn no_bang_is_a_no_op() {
        assert_eq!(expand("echo hi", &hist(&["a", "b"])), Ok(None));
    }

    #[test]
    fn bang_bang_repeats_the_last_command() {
        assert_eq!(expand("!!", &hist(&["echo one", "echo two"])), Ok(Some("echo two".into())));
    }

    #[test]
    fn absolute_and_relative_event_numbers() {
        let h = hist(&["echo one", "echo two", "echo three"]);
        assert_eq!(expand("!2", &h), Ok(Some("echo two".into())));
        assert_eq!(expand("!-2", &h), Ok(Some("echo two".into())));
        assert!(expand("!99", &h).is_err());
        assert!(expand("!-99", &h).is_err());
    }

    #[test]
    fn prefix_and_contains_search() {
        let h = hist(&["echo foo", "ls /tmp", "echo bar"]);
        assert_eq!(expand("!echo", &h), Ok(Some("echo bar".into())));
        assert_eq!(expand("!?foo", &h), Ok(Some("echo foo".into())));
        assert_eq!(expand("!?foo?", &h), Ok(Some("echo foo".into())));
    }

    #[test]
    fn word_designators_on_the_previous_command() {
        let h = hist(&["echo a b c"]);
        assert_eq!(expand("echo !$", &h), Ok(Some("echo c".into())));
        assert_eq!(expand("echo !^", &h), Ok(Some("echo a".into())));
        assert_eq!(expand("echo !*", &h), Ok(Some("echo a b c".into())));
        assert_eq!(expand("echo !:0", &h), Ok(Some("echo echo".into())));
        assert_eq!(expand("echo !:2", &h), Ok(Some("echo b".into())));
    }

    #[test]
    fn no_arg_previous_command_word_designators() {
        let h = hist(&["echo"]);
        assert_eq!(expand("echo !$", &h), Ok(Some("echo echo".into())));
        assert!(expand("echo !^", &h).is_err());
        assert_eq!(expand("echo !*", &h), Ok(Some("echo ".into())));
    }

    #[test]
    fn concatenates_mid_word_like_sudo_bang_bang() {
        assert_eq!(expand("sudo !!", &hist(&["echo hi"])), Ok(Some("sudo echo hi".into())));
    }

    #[test]
    fn quoting_and_escaping_suppress_expansion() {
        let h = hist(&["echo hi"]);
        assert_eq!(expand("echo '!!'", &h), Ok(None));
        // `\!` de-escapes to a literal `!` with no echo, matching real bash
        // exactly — verified directly that bash's history file stores the
        // *raw*, still-backslashed line, not a de-escaped one. Since rush's
        // own lexer already strips `\X` -> literal `X` for any `X` outside
        // quotes, passing the raw line through unchanged (`Ok(None)`) here
        // produces the identical end result without duplicating that logic.
        assert_eq!(expand(r"echo \!!", &h), Ok(None));
        // Double quotes do *not* suppress it, unlike single quotes.
        assert_eq!(expand(r#"echo "!!""#, &h), Ok(Some(r#"echo "echo hi""#.into())));
    }

    #[test]
    fn bang_without_a_valid_trigger_char_is_untouched() {
        let h = hist(&["echo hi"]);
        assert_eq!(expand("a!=b", &h), Ok(None));
        assert_eq!(expand("echo hi ! there", &h), Ok(None));
    }

    #[test]
    fn empty_history_errors() {
        assert!(expand("!!", &[]).is_err());
        assert!(expand("echo !$", &[]).is_err());
    }
}
