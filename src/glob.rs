//! Filename globbing — hand-rolled, no external crate.
//!
//! Two layers:
//!   * [`match_component`] matches one path component against a pattern using
//!     `*`, `?`, and `[…]` (with ranges and `!`/`^` negation). A backslash
//!     escapes the next character so quoted metacharacters can be passed
//!     through literally.
//!   * [`glob`] walks the filesystem component-by-component, so `src/*.rs` and
//!     `*/*.rs` work. Unmatched patterns return nothing; the caller falls back
//!     to the literal word (POSIX no-match behaviour).
//!
//! Like a POSIX shell, a leading `.` in a filename is only matched when the
//! pattern's component itself begins with a literal `.` — so `*` skips dotfiles.

use std::fs;
use std::path::{Path, PathBuf};

/// Expand `pattern` against the filesystem, returning matching paths sorted
/// lexically. An empty result means "no match" — the caller keeps the literal.
pub fn glob(pattern: &str) -> Vec<String> {
    let (base, prefix, rest) = if let Some(r) = pattern.strip_prefix('/') {
        (PathBuf::from("/"), String::from("/"), r)
    } else {
        (PathBuf::from("."), String::new(), pattern)
    };

    let segs: Vec<&str> = rest.split('/').filter(|s| !s.is_empty()).collect();
    if segs.is_empty() {
        return Vec::new();
    }

    let mut out = Vec::new();
    walk(&base, &segs, 0, &prefix, &mut out);
    out.sort();
    out.dedup();
    out
}

fn walk(dir: &Path, segs: &[&str], i: usize, prefix: &str, out: &mut Vec<String>) {
    let seg = segs[i];
    let is_last = i + 1 == segs.len();

    // A component with no metacharacters is a literal path step: no need to
    // scan the directory, just check it exists / descend into it.
    if !has_meta(seg) {
        let name = unescape(seg);
        let child = dir.join(&name);
        let display = format!("{prefix}{name}");
        if is_last {
            if child.exists() {
                out.push(display);
            }
        } else if child.is_dir() {
            walk(&child, segs, i + 1, &format!("{display}/"), out);
        }
        return;
    }

    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    let pattern_is_dotted = seg.starts_with('.');
    let dotglob = crate::vars::shopt("dotglob");

    // `**` (globstar, C58; opt-in via `shopt -s globstar`, same as bash):
    // zero or more directory levels. Without the option, the pre-existing
    // behavior stands — adjacent `*`s collapse, so `**` acts like `*`.
    if seg == "**" && crate::vars::shopt("globstar") {
        if is_last {
            // Zero levels: the prefix directory itself, printed with its
            // trailing slash (`a/**` lists `a/` first — verified bash).
            if !prefix.is_empty() {
                out.push(prefix.to_string());
            }
            collect_recursive(dir, prefix, dotglob, out);
        } else {
            walk(dir, segs, i + 1, prefix, out); // zero levels
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.starts_with('.') && !dotglob {
                    continue;
                }
                if entry.path().is_dir() {
                    walk(&entry.path(), segs, i, &format!("{prefix}{name}/"), out);
                }
            }
        }
        return;
    }

    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') && !pattern_is_dotted && !dotglob {
            continue;
        }
        // `nocaseglob` (C120): fold both pattern and name for filename
        // matching only (distinct from `nocasematch`, which is `case`/
        // `[[ == ]]`).
        let matched = if crate::vars::shopt("nocaseglob") {
            match_component(&seg.to_lowercase(), &name.to_lowercase())
        } else {
            match_component(seg, &name)
        };
        if !matched {
            continue;
        }
        let display = format!("{prefix}{name}");
        if is_last {
            out.push(display);
        } else if entry.path().is_dir() {
            walk(&entry.path(), segs, i + 1, &format!("{display}/"), out);
        }
    }
}

/// Every descendant of `dir` (files and directories alike, directories
/// without a trailing slash), for a trailing `**` — bash's own format,
/// verified directly.
fn collect_recursive(dir: &Path, prefix: &str, dotglob: bool, out: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') && !dotglob {
            continue;
        }
        let display = format!("{prefix}{name}");
        if entry.path().is_dir() {
            collect_recursive(&entry.path(), &format!("{display}/"), dotglob, out);
        }
        out.push(display);
    }
}

/// Does any unescaped `*`, `?`, `[`, or extglob opener (`@(`, `+(`, `!(`
/// — `?(`/`*(` are already covered by their first character) appear in
/// this component?
fn has_meta(seg: &str) -> bool {
    let mut chars = seg.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                chars.next();
            }
            '*' | '?' | '[' => return true,
            '@' | '+' | '!' if chars.peek() == Some(&'(') => return true,
            _ => {}
        }
    }
    false
}

/// Strip backslash escapes, yielding the literal text of a component.
fn unescape(seg: &str) -> String {
    let mut out = String::new();
    let mut chars = seg.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(n) = chars.next() {
                out.push(n);
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Match a single path component against a glob pattern.
pub fn match_component(pattern: &str, name: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let s: Vec<char> = name.chars().collect();
    matches(&p, 0, &s, 0)
}

fn matches(p: &[char], mut pi: usize, s: &[char], mut si: usize) -> bool {
    loop {
        if pi == p.len() {
            return si == s.len();
        }
        // Extended globs (C57): `?(...)`/`*(...)`/`+(...)`/`@(...)`/`!(...)`
        // with `|`-separated alternatives — always on, like ksh93 (bash
        // gates them behind `shopt -s extglob`; rush has no shopt, and
        // without extglob bash makes these a hard syntax error anyway, so
        // always-on is strictly more compatible).
        if pi + 1 < p.len()
            && matches!(p[pi], '?' | '*' | '+' | '@' | '!')
            && p[pi + 1] == '('
            && crate::vars::shopt("extglob")
            && let Some((alts, rest)) = parse_extglob(p, pi + 1)
        {
            return match_extglob(p[pi], &alts, p, rest, s, si);
        }
        match p[pi] {
            '*' => {
                // Collapse a run of `*`, then try to match the tail at every
                // remaining position.
                let mut npi = pi + 1;
                while npi < p.len() && p[npi] == '*' {
                    npi += 1;
                }
                if npi == p.len() {
                    return true; // trailing `*` swallows the rest
                }
                let mut k = si;
                loop {
                    if matches(p, npi, s, k) {
                        return true;
                    }
                    if k == s.len() {
                        return false;
                    }
                    k += 1;
                }
            }
            '?' => {
                if si == s.len() {
                    return false;
                }
                pi += 1;
                si += 1;
            }
            '[' => match parse_class(p, pi) {
                Some((class, npi)) => {
                    if si == s.len() || !class.matches(s[si]) {
                        return false;
                    }
                    pi = npi;
                    si += 1;
                }
                // Unterminated `[` is a literal bracket.
                None => {
                    if si == s.len() || s[si] != '[' {
                        return false;
                    }
                    pi += 1;
                    si += 1;
                }
            },
            '\\' => {
                let lit = if pi + 1 < p.len() { p[pi + 1] } else { '\\' };
                if si == s.len() || s[si] != lit {
                    return false;
                }
                pi += if pi + 1 < p.len() { 2 } else { 1 };
                si += 1;
            }
            c => {
                if si == s.len() || s[si] != c {
                    return false;
                }
                pi += 1;
                si += 1;
            }
        }
    }
}

/// One member of a bracket expression: an ordinary character range (a single
/// character is a degenerate `c-c` range) or a POSIX named class
/// (`[:alpha:]`, `[:digit:]`, …) mapped to its predicate.
enum ClassItem {
    Range(char, char),
    Named(fn(char) -> bool),
}

/// Split an extglob group starting at `open` (the `(`) into its
/// top-level `|`-separated alternatives, returning them plus the index
/// just past the closing `)`. `None` if the group never closes (the
/// `(` then falls through as a literal character, same as before C57).
fn parse_extglob(p: &[char], open: usize) -> Option<(Vec<Vec<char>>, usize)> {
    let mut alts = Vec::new();
    let mut cur = Vec::new();
    let mut depth = 1usize;
    let mut i = open + 1;
    while i < p.len() {
        match p[i] {
            '(' => {
                depth += 1;
                cur.push('(');
            }
            ')' => {
                depth -= 1;
                if depth == 0 {
                    alts.push(cur);
                    return Some((alts, i + 1));
                }
                cur.push(')');
            }
            '|' if depth == 1 => alts.push(std::mem::take(&mut cur)),
            '\\' => {
                cur.push('\\');
                if i + 1 < p.len() {
                    i += 1;
                    cur.push(p[i]);
                }
            }
            c => cur.push(c),
        }
        i += 1;
    }
    None
}

/// Match one extglob group (`kind` is its prefix character) followed by
/// the rest of the pattern (`p[rest..]`) against `s[si..]`. Alternatives
/// are full glob patterns themselves (nesting recurses naturally); every
/// split point is tried, backtracking-style. Semantics verified against
/// bash (`shopt -s extglob`): `?` is exactly 0 or 1 occurrence (`aax`
/// does NOT match `?(a)x`), `*`/`+` are 0+/1+ repetitions, `@` exactly
/// one, and `!` matches any prefix that is *not* matched in full by any
/// alternative (`abfile` matches `!(a|b)file`; `afile` doesn't).
fn match_extglob(kind: char, alts: &[Vec<char>], p: &[char], rest: usize, s: &[char], si: usize) -> bool {
    let alt_matches = |from: usize, to: usize| alts.iter().any(|alt| matches(alt, 0, &s[from..to], 0));
    match kind {
        '@' => (si..=s.len()).any(|k| alt_matches(si, k) && matches(p, rest, s, k)),
        '?' => {
            matches(p, rest, s, si) || (si..=s.len()).any(|k| alt_matches(si, k) && matches(p, rest, s, k))
        }
        '!' => (si..=s.len()).any(|k| !alt_matches(si, k) && matches(p, rest, s, k)),
        // `*` / `+`: repetitions. Try the tail at every point reachable by
        // consuming zero (`*` only) or more alternative-matched chunks.
        //
        // `from` (with `min_done` always true past the first step) is
        // memoized: without it, this is plain graph reachability over
        // `0..=s.len()` positions explored by unbounded backtracking, which
        // revisits the same `from` through every distinct chunk-split path
        // that reaches it — exponential in `s.len()` for adversarial
        // patterns (e.g. nested `*(...)`/`?(...)` fuzzed input that hung a
        // fuzz run for 30 minutes). Caching each `from` the first time it's
        // resolved caps total work at O(s.len()^2 * alt-match cost).
        '*' | '+' => {
            fn reachable(
                alt_matches: &dyn Fn(usize, usize) -> bool,
                p: &[char],
                rest: usize,
                s: &[char],
                from: usize,
                min_done: bool,
                memo: &mut [Option<bool>],
            ) -> bool {
                if min_done && let Some(cached) = memo[from] {
                    return cached;
                }
                if min_done && matches(p, rest, s, from) {
                    memo[from] = Some(true);
                    return true;
                }
                // Consume one more non-empty alternative-matched chunk.
                let result = ((from + 1)..=s.len())
                    .any(|k| alt_matches(from, k) && reachable(alt_matches, p, rest, s, k, true, memo));
                if min_done {
                    memo[from] = Some(result);
                }
                result
            }
            let mut memo = vec![None; s.len() + 1];
            reachable(&alt_matches, p, rest, s, si, kind == '*', &mut memo)
        }
        _ => false,
    }
}

struct Class {
    negate: bool,
    items: Vec<ClassItem>,
}

impl Class {
    fn matches(&self, ch: char) -> bool {
        let inside = self.items.iter().any(|item| match *item {
            ClassItem::Range(lo, hi) => ch >= lo && ch <= hi,
            ClassItem::Named(pred) => pred(ch),
        });
        inside ^ self.negate
    }
}

/// The standard POSIX class names → predicates (C42). `digit`/`xdigit` are
/// ASCII-only even in a Unicode locale, matching real bash; the letter-ish
/// classes use Rust's Unicode-aware predicates, which agree with bash under
/// the usual UTF-8 locales.
fn named_class(name: &str) -> Option<fn(char) -> bool> {
    Some(match name {
        "alpha" => char::is_alphabetic,
        "digit" => |c| c.is_ascii_digit(),
        "alnum" => |c| c.is_alphabetic() || c.is_ascii_digit(),
        "upper" => char::is_uppercase,
        "lower" => char::is_lowercase,
        "space" => char::is_whitespace,
        "blank" => |c| c == ' ' || c == '\t',
        "punct" => |c| c.is_ascii_punctuation(),
        "cntrl" => char::is_control,
        "graph" => |c| !c.is_whitespace() && !c.is_control(),
        "print" => |c| c == ' ' || (!c.is_whitespace() && !c.is_control()),
        "xdigit" => |c| c.is_ascii_hexdigit(),
        _ => return None,
    })
}

/// Parse a `[...]` class starting at `start` (the `[`). Returns the class and
/// the index just past the closing `]`, or `None` if there is no closing `]`.
fn parse_class(p: &[char], start: usize) -> Option<(Class, usize)> {
    let mut i = start + 1;
    let mut negate = false;
    if i < p.len() && (p[i] == '!' || p[i] == '^') {
        negate = true;
        i += 1;
    }

    let mut items = Vec::new();
    let mut first = true;
    while i < p.len() {
        // A `]` is only the terminator if it isn't the very first class member.
        if p[i] == ']' && !first {
            return Some((Class { negate, items }, i + 1));
        }
        first = false;
        // `[:name:]` — a POSIX named class (C42). An unknown *name* with
        // proper `[: :]` delimiters is a member that matches nothing
        // (verified: `a[[:bogus:]]` matches no file in bash), rather than
        // a parse error. When the `[:` is *not* closed by a `:]`, bash
        // quirkily drops the `[` itself and keeps the rest as ordinary
        // members (verified char-by-char: `a[[:digit]` matches `ad`/`a:`
        // but not `a[` — dash keeps the `[` too, but bash is this
        // codebase's reference), so just the `[` is skipped here.
        if p[i] == '[' && i + 1 < p.len() && p[i + 1] == ':' {
            match (i + 2..p.len().saturating_sub(1)).find(|&j| p[j] == ':' && p[j + 1] == ']') {
                Some(close) => {
                    let name: String = p[i + 2..close].iter().collect();
                    items.push(ClassItem::Named(named_class(&name).unwrap_or(|_| false)));
                    i = close + 2;
                }
                None => i += 1,
            }
            continue;
        }
        if i + 2 < p.len() && p[i + 1] == '-' && p[i + 2] != ']' {
            items.push(ClassItem::Range(p[i], p[i + 2]));
            i += 3;
        } else {
            items.push(ClassItem::Range(p[i], p[i]));
            i += 1;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn star_matches_within_component() {
        assert!(match_component("*.rs", "lexer.rs"));
        assert!(match_component("*", "anything"));
        assert!(match_component("a*c", "abbbc"));
        assert!(!match_component("*.rs", "lexer.txt"));
    }

    #[test]
    fn question_matches_one_char() {
        assert!(match_component("?.rs", "a.rs"));
        assert!(!match_component("?.rs", "ab.rs"));
    }

    #[test]
    fn char_classes() {
        assert!(match_component("[abc].rs", "a.rs"));
        assert!(match_component("[a-z].rs", "m.rs"));
        assert!(!match_component("[a-z].rs", "M.rs"));
        assert!(match_component("[!0-9]*", "abc"));
        assert!(!match_component("[!0-9]*", "9bc"));
    }

    // C42: POSIX named classes inside a bracket expression, all verified
    // against real bash's own results for the same patterns.
    #[test]
    fn posix_named_classes() {
        assert!(match_component("a[[:digit:]]", "a5"));
        assert!(!match_component("a[[:digit:]]", "ab"));
        assert!(match_component("a[[:alpha:]]", "ab"));
        assert!(match_component("a[[:alpha:]]", "aB"));
        assert!(!match_component("a[[:alpha:]]", "a5"));
        assert!(match_component("a[[:upper:]]", "aB"));
        assert!(!match_component("a[[:upper:]]", "ab"));
        assert!(match_component("a[[:xdigit:]]", "aF"));
        assert!(!match_component("a[[:xdigit:]]", "aG"));
        assert!(match_component("a[[:space:]]b", "a b"));
        assert!(match_component("a[[:punct:]]", "a-"));
        // Negation applies to the named class too.
        assert!(match_component("a[![:digit:]]", "ab"));
        assert!(!match_component("a[![:digit:]]", "a5"));
        // Mixed with ordinary members.
        assert!(match_component("a[[:alpha:]5]", "a5"));
        assert!(match_component("a[[:alpha:]5]", "ab"));
        assert!(!match_component("a[[:alpha:]5]", "a6"));
    }

    // The two edge cases, matching real bash exactly (both verified
    // char-by-char against it): a properly-delimited unknown name matches
    // nothing; an unclosed `[:` drops the `[` and keeps the rest as
    // ordinary members.
    #[test]
    fn named_class_edge_cases() {
        assert!(!match_component("a[[:bogus:]]", "ab"));
        assert!(!match_component("a[[:bogus:]]", "a["));
        assert!(match_component("a[[:digit]", "ad"));
        assert!(match_component("a[[:digit]", "a:"));
        assert!(!match_component("a[[:digit]", "a["));
        assert!(!match_component("a[[:digit]", "a5"));
    }

    // C57: extended globs — each expectation mirrors bash under
    // `shopt -s extglob` (verified directly).
    #[test]
    fn extended_globs() {
        assert!(match_component("@(a|b)file", "afile"));
        assert!(match_component("@(a|b)file", "bfile"));
        assert!(!match_component("@(a|b)file", "cfile"));
        assert!(!match_component("@(a|b)file", "abfile"));

        // `?`: exactly 0 or 1 occurrence.
        assert!(match_component("?(a)x", "x"));
        assert!(match_component("?(a)x", "ax"));
        assert!(!match_component("?(a)x", "aax"));

        // `*`: 0+, `+`: 1+.
        assert!(match_component("*(a)", ""));
        assert!(match_component("+(a)", "aaa"));
        assert!(!match_component("+(a)", ""));
        assert!(match_component("+(a|b)file", "abfile"));

        // `!`: any prefix not matched in full by an alternative.
        assert!(match_component("!(a|b)file", "cfile"));
        assert!(match_component("!(a|b)file", "abfile"));
        assert!(!match_component("!(a|b)file", "afile"));

        // Nesting, and alternatives that are themselves glob patterns.
        assert!(match_component("@(f@(o|x)o)", "foo"));
        assert!(match_component("@(*.txt|*.rs)", "glob.rs"));
        assert!(!match_component("@(*.txt|*.rs)", "glob.c"));

        // An unterminated group falls back to literal characters.
        assert!(match_component("a@(b", "a@(b"));
    }

    /// A fuzzer-found input (`glob_match` target) that hung for 30 minutes
    /// pre-fix: nested `?(...)`/`*(...)` groups matched against themselves
    /// drove `match_extglob`'s `*`/`+` reachability search into unmemoized
    /// exponential backtracking. Must resolve near-instantly either way.
    #[test]
    fn nested_star_extglob_does_not_blow_up() {
        let s: String = [
            63u8, 40, 63, 40, 42, 63, 40, 42, 40, 42, 41, 1, 0, 41, 49, 63, 42, 63, 40, 63, 40, 42, 63,
            40, 42, 40, 42, 41, 1, 0, 41, 49, 63, 42, 35, 35,
        ]
        .into_iter()
        .map(|b| b as char)
        .collect();
        let start = std::time::Instant::now();
        let _ = match_component(&s, &s);
        assert!(start.elapsed().as_secs() < 5, "match_component took too long: pathological backtracking regressed");
    }

    #[test]
    fn escaped_metachars_are_literal() {
        assert!(match_component("a\\*b", "a*b"));
        assert!(!match_component("a\\*b", "axb"));
    }

    #[test]
    fn unterminated_class_is_literal_bracket() {
        assert!(match_component("[abc", "[abc"));
    }

    #[test]
    fn glob_against_known_files() {
        // Run from the crate root, these files are stable fixtures.
        let mut m = glob("Cargo.*");
        m.sort();
        assert_eq!(m, vec!["Cargo.lock", "Cargo.toml"]);

        assert_eq!(glob("src/lexer.rs"), vec!["src/lexer.rs"]);
        assert!(glob("src/*.rs").contains(&"src/glob.rs".to_string()));
        assert!(glob("no-such-file-*.zzz").is_empty());
    }
}
