//! Expansion: the stage between parse and exec.
//!
//! Turns a [`RawPipeline`] (words still carrying their quoting) into an
//! [`exec::Pipeline`] of concrete strings, applying:
//!
//!   * tilde expansion   — a leading unquoted `~` becomes `$HOME`
//!   * variables         — `$VAR`, `${VAR}` (unset → empty)
//!   * command substitution — `$(...)` runs a sub-pipeline and inlines its stdout
//!   * globbing          — `*`, `?`, `[…]` match against the filesystem
//!
//! Single-quoted and backslash-escaped text is taken verbatim, and only
//! metacharacters originating from *unquoted* text are active for globbing
//! (`"*.rs"` is literal). Globbing can turn one word into several arguments;
//! a pattern that matches nothing is left as its literal text. An unquoted
//! expansion is also split into fields on `$IFS` (default: space/tab/newline;
//! see `Ifs`) — a bare expansion that comes out empty drops out the way
//! `echo $UNSET` does in a real shell.

use std::iter::Peekable;
use std::str::Chars;

use crate::exec::{Command, Pipeline, Redirect};
use crate::lexer::{Word, WordPart};
use crate::parser::{self, RawCommand, RawPipeline, RawRedirect, RawSimple};

pub fn expand(raw: &RawPipeline) -> Result<Pipeline, String> {
    let mut commands = Vec::with_capacity(raw.commands.len());
    for rc in &raw.commands {
        let stage = match rc {
            RawCommand::Simple(s) => crate::exec::Stage::Simple(expand_simple(s)?),
            // The compound's own body isn't expanded here (it's expanded
            // lazily, same as a sole compound) — just carried through. Only
            // Unix's job-control runner can actually run one as one stage
            // among several (it forks); elsewhere that's still an error, but
            // one raised at run time, not here at expansion time. Any
            // redirects trailing its close (`done < file`) *are* expanded now,
            // same as a simple command's.
            RawCommand::Compound(rc) => {
                let (redirects, heredoc) = expand_redirects(&rc.redirects)?;
                crate::exec::Stage::Compound(crate::exec::CompoundStage {
                    compound: rc.compound.clone(),
                    redirects,
                    heredoc,
                })
            }
        };
        commands.push(stage);
    }
    Ok(Pipeline { commands })
}

/// Expand a list of words into arguments (splitting + globbing) — used by the
/// `for` loop to compute its iteration values.
pub fn expand_words(words: &[Word]) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for w in words {
        out.extend(expand_argv_word(w)?);
    }
    Ok(out)
}

/// Expand a word to a single string (no splitting or globbing) — used for a
/// `case` subject.
pub fn expand_to_string(word: &Word) -> Result<String, String> {
    expand_word(word)
}

/// Expand a `case` pattern: like a glob pattern, metacharacters from quoted or
/// literal parts are escaped so only unquoted `*?[` stay active. No tilde or
/// word-splitting (a pattern is a single match template).
pub fn expand_pattern(word: &Word) -> Result<String, String> {
    let mut pattern = String::new();
    for part in word {
        match part {
            WordPart::Literal(s) => escape_meta_into(&mut pattern, s),
            WordPart::Quoted(s) => escape_meta_into(&mut pattern, &expand_dollars(s)?),
            WordPart::Unquoted(s) => pattern.push_str(&expand_dollars(s)?),
            // `ArrayLiteral` only ever appears right after an `Unquoted`
            // part shaped like `NAME=`/`NAME+=` (see `WordPart`'s own doc
            // comment), which `assignment_split` always intercepts before
            // a word reaches here — unreachable in practice.
            WordPart::ArrayLiteral(_) => {
                return Err("array literal isn't valid as a case pattern".into());
            }
        }
    }
    Ok(pattern)
}

fn expand_simple(rc: &RawSimple) -> Result<Command, String> {
    use crate::vars::{AssignOp, AssignValue};

    // Leading `NAME=value` words are assignments; they stop at the first word
    // that isn't one (the program name).
    let mut assignments = Vec::new();
    let mut idx = 0;
    while idx < rc.argv.len() {
        match assignment_split(&rc.argv[idx]) {
            Some((name, RawAssign::Whole(append, raw_value))) => {
                let value = match raw_value {
                    RawAssignValue::Scalar(word) => AssignValue::Scalar(expand_word(&word)?),
                    // If `name` is *already* an associative array (from an
                    // earlier `declare -A`), a literal's elements are
                    // `[key]=value` pairs — same rule `declare -A`/
                    // `local -A`'s own literal uses, just triggered by
                    // `name`'s existing type here instead of a `-A` flag in
                    // this same statement (verified directly: `arr+=([c]=3
                    // [a]=99)` on an already-`-A` `arr` merges by key, not
                    // by position). Otherwise, each element word can itself
                    // expand to several fields (a glob, or an unquoted
                    // `$(...)`/`$var` splitting on `$IFS`) — same rule as
                    // ordinary argv words. (An *explicit*-index literal on
                    // a plain indexed array, `arr=([5]=x [2]=y z)`, is a
                    // separate, undocumented-here bash feature this doesn't
                    // support — its elements are just treated as plain
                    // words instead.)
                    RawAssignValue::Array(elements) if crate::vars::is_assoc(&name) => {
                        let mut pairs = Vec::new();
                        for el in &elements {
                            if let Some((key, value_word)) = parse_assoc_literal_element(el) {
                                pairs.push((resolve_subscript_text(&key)?, expand_word(&value_word)?));
                            }
                        }
                        AssignValue::Assoc(pairs)
                    }
                    RawAssignValue::Array(elements) => {
                        let mut values = Vec::new();
                        for el in &elements {
                            values.extend(expand_argv_word(el)?);
                        }
                        AssignValue::Array(values)
                    }
                };
                assignments.push((name, if append { AssignOp::Append(value) } else { AssignOp::Set(value) }));
                idx += 1;
            }
            Some((name, RawAssign::Index(subscript, append, value_word))) => {
                // Only `$`-expand the subscript here — whether it's then
                // treated as an arithmetic index or a literal associative
                // key can only be decided once `name`'s actual type is
                // known, which `vars::key_set`/`key_append` (via `assign`)
                // check at the point the assignment is actually applied,
                // not here at parse time (see `AssignOp::SetKey`'s own doc
                // comment).
                let subscript = resolve_subscript_text(&subscript)?;
                let value = expand_word(&value_word)?;
                let op = if append { AssignOp::AppendKey(subscript, value) } else { AssignOp::SetKey(subscript, value) };
                assignments.push((name, op));
                idx += 1;
            }
            None => break,
        }
    }

    // `local`/`declare` are the two commands whose own arguments can carry
    // an array or associative-array literal (`local arr=(a b c)`,
    // `declare -A arr=([k]=v ...)`) — a plain `Vec<String>` argv can't
    // represent either, so declarations are parsed here (reusing
    // `assignment_split`, same as a leading prefix) into `local_decls`
    // instead of going through the ordinary `expand_argv_word` path below.
    // (`declare` always applies to the current/global scope in rush,
    // unlike bash's own quirk of `declare` acting like `local` when used
    // inside a function — an accepted, documented simplification; use
    // `local` explicitly for function-scoped declarations.)
    let decl_word = if idx < rc.argv.len() { expand_argv_word(&rc.argv[idx])?.into_iter().next() } else { None };
    // `declare -p`/`-f`/`-F` are introspection, not declaration (C96):
    // route them down the plain-argv path so the builtin dispatcher can
    // see the flag and hand off to `declare_print`.
    let declare_print_form = matches!(decl_word.as_deref(), Some("declare") | Some("typeset"))
        && matches!(rc.argv.get(idx + 1).map(|w| w.as_slice()),
            Some([WordPart::Unquoted(s)]) if matches!(s.as_str(), "-p" | "-f" | "-F"));
    // `export NAME=(...)` — route through the decl path (C132) so the
    // array literal parses; only when an array literal is actually
    // present, so `export -n`/`-f`/bare `export NAME` keep the builtin
    // path.
    let export_array_form = decl_word.as_deref() == Some("export")
        && rc.argv[idx + 1..].iter().any(|w| {
            w.iter().any(|p| matches!(p, WordPart::ArrayLiteral(_)))
        });
    let (argv, local_decls, decl_attrs) = if !declare_print_form
        && (export_array_form
            || matches!(decl_word.as_deref(), Some("local") | Some("declare") | Some("typeset") | Some("readonly"))) {
        let cmd_name = decl_word.unwrap();
        let mut rest = &rc.argv[idx + 1..];
        // Leading flags apply to every name that follows in this same
        // invocation — bash allows mixing plain names in with flags too,
        // but this scope only needs the common case. `-A` (associative) and
        // `-a` (indexed) select the value shape; `-u`/`-l`/`-i` (C43)
        // declare attribute transforms. Flags cluster (`declare -ui n`),
        // same as bash. A word with any *other* letter isn't a recognized
        // flag and ends flag parsing (falling through to the old
        // treated-as-a-name path), keeping `-r`/`-n`/`-x`/`-p` — still
        // unimplemented, tracked as C45/C62/C48 — no worse than before.
        let mut assoc = false;
        let mut array = false;
        let mut attrs = crate::vars::Attrs::default();
        while let Some(word) = rest.first() {
            match word.as_slice() {
                [WordPart::Unquoted(s)]
                    if s.len() > 1
                        && s.starts_with('-')
                        && s[1..].chars().all(|c| matches!(c, 'a' | 'A' | 'u' | 'l' | 'i' | 'r' | 'n')) =>
                {
                    for c in s[1..].chars() {
                        match c {
                            'A' => assoc = true,
                            'a' => array = true,
                            'u' => attrs.upper = true,
                            'l' => attrs.lower = true,
                            'i' => attrs.integer = true,
                            'r' => attrs.readonly = true,
                            'n' => attrs.nameref = true,
                            _ => unreachable!(),
                        }
                    }
                }
                _ => break,
            }
            rest = &rest[1..];
        }

        let mut decls = Vec::new();
        // `local`/`declare`'s own arguments are ordinary command-argument
        // words, not assignment-statement syntax, so — unlike a bare
        // `x={a,b}` statement or a `FOO={a,b} cmd` prefix assignment,
        // neither of which brace-expand — `local x={a,b}` does (verified
        // directly: it becomes two words, `x=a` then `x=b`, applied in
        // order, leaving `x=b`). Brace-expand each word here, before
        // `assignment_split` ever sees it.
        for word in rest.iter().flat_map(brace_expand) {
            let word = &word;
            match assignment_split(word) {
                Some((name, RawAssign::Whole(append, raw_value))) => {
                    let value = match raw_value {
                        RawAssignValue::Scalar(w) => AssignValue::Scalar(expand_word(&w)?),
                        RawAssignValue::Array(elements) if assoc => {
                            let mut pairs = Vec::new();
                            for el in &elements {
                                if let Some((key, value_word)) = parse_assoc_literal_element(el) {
                                    pairs.push((resolve_subscript_text(&key)?, expand_word(&value_word)?));
                                }
                            }
                            AssignValue::Assoc(pairs)
                        }
                        RawAssignValue::Array(elements) => {
                            let mut values = Vec::new();
                            for el in &elements {
                                values.extend(expand_argv_word(el)?);
                            }
                            AssignValue::Array(values)
                        }
                    };
                    decls.push((name, Some(if append { AssignOp::Append(value) } else { AssignOp::Set(value) })));
                }
                // `local arr[i]=x` (indexing a not-yet-local array in the
                // same breath) isn't supported — falls through to being
                // treated as a bare name, an accepted, documented gap.
                Some((_, RawAssign::Index(..))) | None => {
                    for name in expand_argv_word(word)? {
                        // A bare `local -A arr`/`declare -A arr` (no
                        // initializer) makes an *empty* array right away —
                        // but only when the name doesn't already hold one
                        // (C132): re-declaring an existing array without a
                        // value must NOT wipe it, matching bash.
                        let already = crate::vars::is_assoc(&name)
                            || crate::vars::is_indexed_array(&name)
                            || crate::vars::get(&name).is_some();
                        let init = if already {
                            None
                        } else if assoc {
                            Some(AssignOp::Set(AssignValue::Assoc(Vec::new())))
                        } else if array {
                            Some(AssignOp::Set(AssignValue::Array(Vec::new())))
                        } else {
                            None
                        };
                        decls.push((name, init));
                    }
                }
            }
        }
        (vec![cmd_name], decls, attrs)
    } else {
        let mut argv = Vec::new();
        for word in &rc.argv[idx..] {
            argv.extend(expand_argv_word(word)?);
        }

        // A single, non-recursive alias substitution: `ll -a` with `alias
        // ll='ls -l'` becomes `ls -l -a`. The expanded words aren't
        // re-checked against the alias table, so `alias ls='ls
        // --color=auto'` can't loop.
        if let Some(value) = argv.first().and_then(|first| crate::alias::get(first)) {
            let mut expanded: Vec<String> = value.split_whitespace().map(String::from).collect();
            expanded.extend(argv.into_iter().skip(1));
            argv = expanded;
        }
        (argv, Vec::new(), crate::vars::Attrs::default())
    };

    let (redirects, heredoc) = expand_redirects(&rc.redirects)?;
    Ok(Command { argv, redirects, assignments, heredoc, local_decls, decl_attrs })
}

/// Expand a raw redirect list into concrete `Redirect`s plus an optional
/// here-doc body (kept separate since it feeds stdin rather than naming a
/// target file) — shared by simple commands and compound commands, since a
/// redirect can trail either (`echo hi > f`, `while …; done < f`).
pub(crate) fn expand_redirects(raw: &[RawRedirect]) -> Result<(Vec<Redirect>, Option<String>), String> {
    let mut redirects = Vec::with_capacity(raw.len());
    let mut heredoc = None;
    for r in raw {
        match r {
            RawRedirect::File { fd, file, mode } => redirects.push(Redirect::File {
                fd: *fd,
                file: expand_word(file)?,
                mode: *mode,
            }),
            RawRedirect::Both { file, append } => redirects.push(Redirect::Both {
                file: expand_word(file)?,
                append: *append,
            }),
            RawRedirect::Dup { fd, target } => {
                redirects.push(Redirect::Dup { fd: *fd, target: *target })
            }
            RawRedirect::Move { fd, target } => {
                redirects.push(Redirect::Move { fd: *fd, target: *target })
            }
            // `{name}>…` (C115): expand the wrapped redirect, then tag it
            // with the variable that receives the allocated fd number.
            RawRedirect::VarFd { name, inner } => {
                let (mut inner_redirects, _) = expand_redirects(std::slice::from_ref(inner))?;
                if let Some(inner_redirect) = inner_redirects.pop() {
                    redirects.push(Redirect::VarFd {
                        name: name.clone(),
                        inner: Box::new(inner_redirect),
                    });
                }
            }
            // `fd>&$word` (C66): the word must expand to an fd number —
            // the coproc idiom `<&"${COPROC[0]}"`.
            RawRedirect::DupWord { fd, word } => {
                let text = expand_word(word)?;
                let text = text.trim();
                // `fd>&-` closes; `fd>&N-` moves (dup + close source) —
                // C111. A plain number is the ordinary dup.
                if text == "-" {
                    redirects.push(Redirect::Close { fd: *fd });
                } else if let Some(num) = text.strip_suffix('-')
                    && let Ok(target) = num.parse::<u32>()
                {
                    redirects.push(Redirect::Move { fd: *fd, target });
                } else {
                    let target = text
                        .parse::<u32>()
                        .map_err(|_| format!("{text}: bad file descriptor"))?;
                    redirects.push(Redirect::Dup { fd: *fd, target });
                }
            }
            // Its `$`-expansions run unless the delimiter was quoted.
            RawRedirect::Heredoc { body, expand } => {
                heredoc = Some(if *expand { expand_dollars(body)? } else { body.clone() });
            }
            // A single word, same expansion as any other redirect target
            // (no splitting/globbing — verified directly: `x="a b"; cat
            // <<< $x` still feeds `a b` as one line, not two words), plus
            // a trailing `\n` — always appended, even if the expanded
            // text already ends in one, matching real bash exactly.
            RawRedirect::HereString(word) => {
                heredoc = Some(format!("{}\n", expand_word(word)?));
            }
        }
    }
    Ok((redirects, heredoc))
}

/// A parsed (not yet expanded) assignment value: the ordinary scalar case —
/// a `Word` reassembled from whatever followed `=`/`+=` — or, for an
/// array-literal assignment (`arr=(a b c)`), the element list straight from
/// the lexer's `WordPart::ArrayLiteral`.
enum RawAssignValue {
    Scalar(Word),
    Array(Vec<Word>),
}

/// The two assignment shapes `assignment_split` recognizes: the whole name
/// (`NAME=`/`NAME+=`, value may be scalar or an array literal) or one
/// specific element (`NAME[subscript]=`/`NAME[subscript]+=` — a single
/// index only, never an array literal on the right: `arr[i]=(...)` isn't
/// meaningful and bash doesn't support it either).
enum RawAssign {
    Whole(bool, RawAssignValue),
    Index(String, bool, Word),
}

/// If `word` has the form `NAME=...`/`NAME+=...` (or `NAME[subscript]=...`/
/// `NAME[subscript]+=...`) with `NAME` a valid identifier whose `=`/`+=`
/// comes from unquoted text, split it into the name and the rest — see
/// [`RawAssign`]. Otherwise `None`.
fn assignment_split(word: &Word) -> Option<(String, RawAssign)> {
    let WordPart::Unquoted(text) = word.first()? else {
        return None;
    };

    // `NAME[subscript]=value` / `NAME[subscript]+=value` — checked first,
    // since a bracketed name never matches the plain-name path below at all
    // (`[`/`]` aren't name characters).
    if let Some(bracket) = text.find('[') {
        let name = &text[..bracket];
        // A quoted key (`a["x y"]=1`, `a[$'k']=v` — C84): the `[` and the
        // `]=` land in *different* word parts, with the key's quoted text
        // in between. Stitch the subscript across parts (quoted/literal
        // text joins verbatim) up to the part whose unquoted text carries
        // the closing `]=`/`]+=`.
        if is_valid_name(name) && !text[bracket..].contains(']') {
            let mut subscript = text[bracket + 1..].to_string();
            for (i, part) in word.iter().enumerate().skip(1) {
                match part {
                    WordPart::Quoted(s) | WordPart::Literal(s) => subscript.push_str(s),
                    WordPart::Unquoted(s) => {
                        let close = s.find(']')?;
                        subscript.push_str(&s[..close]);
                        let after = &s[close + 1..];
                        let (append, after_op) = if let Some(rest) = after.strip_prefix("+=") {
                            (true, rest)
                        } else if let Some(rest) = after.strip_prefix('=') {
                            (false, rest)
                        } else {
                            return None;
                        };
                        let mut value: Word = vec![WordPart::Unquoted(after_op.to_string())];
                        value.extend(word[i + 1..].iter().cloned());
                        return Some((
                            name.to_string(),
                            RawAssign::Index(subscript, append, value),
                        ));
                    }
                    WordPart::ArrayLiteral(_) => return None,
                }
            }
            return None;
        }
        if is_valid_name(name)
            && let Some(close) = text[bracket..].find(']').map(|i| i + bracket)
        {
            let subscript = text[bracket + 1..close].to_string();
            let after_bracket = &text[close + 1..];
            let (append, after_op) = if let Some(rest) = after_bracket.strip_prefix("+=") {
                (true, rest)
            } else if let Some(rest) = after_bracket.strip_prefix('=') {
                (false, rest)
            } else {
                return None;
            };
            let mut value: Word = vec![WordPart::Unquoted(after_op.to_string())];
            value.extend(word[1..].iter().cloned());
            return Some((name.to_string(), RawAssign::Index(subscript, append, value)));
        }
    }

    let eq = text.find('=')?;
    let (name, append) = match text[..eq].strip_suffix('+') {
        Some(name) => (name, true),
        None => (&text[..eq], false),
    };

    if !is_valid_name(name) {
        return None;
    }

    // `NAME=(...)`/`NAME+=(...)`: nothing else on the first part, and the
    // whole rest of the word is exactly one `ArrayLiteral` (how the lexer
    // always shapes it — see `WordPart::ArrayLiteral`'s own doc comment).
    let after_eq = &text[eq + 1..];
    if after_eq.is_empty()
        && let [WordPart::ArrayLiteral(elements)] = &word[1..]
    {
        return Some((name.to_string(), RawAssign::Whole(append, RawAssignValue::Array(elements.clone()))));
    }

    let mut value: Word = vec![WordPart::Unquoted(after_eq.to_string())];
    value.extend(word[1..].iter().cloned());
    Some((name.to_string(), RawAssign::Whole(append, RawAssignValue::Scalar(value))))
}

/// Parse one associative-array-literal element (`[key]=value`, no leading
/// name — unlike `assignment_split`'s indexed-assignment form, a literal's
/// own elements never have one) into its raw key subscript text (still
/// needing `resolve_subscript_text`, same as any other subscript — a
/// literal key can itself be `$`-expanded, e.g. `[$k]=v`) and a value
/// `Word`. Only ever consulted when `declare -A`/`local -A` says an
/// enclosing array literal's elements should be read this way instead of
/// as plain words — see `expand_simple`'s "local"/"declare" handling.
fn parse_assoc_literal_element(word: &Word) -> Option<(String, Word)> {
    let WordPart::Unquoted(text) = word.first()? else {
        return None;
    };
    let key_src = text.strip_prefix('[')?;
    let close = key_src.find(']')?;
    let key = key_src[..close].to_string();
    let after_bracket = &key_src[close + 1..];
    let after_eq = after_bracket.strip_prefix('=')?;
    let mut value: Word = vec![WordPart::Unquoted(after_eq.to_string())];
    value.extend(word[1..].iter().cloned());
    Some((key, value))
}

/// Expand a word destined for `argv`, possibly into several arguments.
///
/// Brace expansion (`{a,b,c}`, `{1..5}`) runs first, on the word's raw,
/// unexpanded text — same order as real bash, so `{$x,y}` expands the
/// braces into two words *before* `$x` resolves in whichever one it lands
/// in. Each resulting word is then expanded independently below.
fn expand_argv_word(word: &Word) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for w in brace_expand(word) {
        out.extend(expand_argv_word_after_braces(&w)?);
    }
    Ok(out)
}

/// Whitespace inside an *unquoted* expansion splits the word into fields
/// (`x="a b"; echo $x` → two args) — and since the lexer already split on
/// literal whitespace, any whitespace left in an unquoted part can only have
/// come from a `$`/`$(…)` expansion. Quoted and literal text never split.
///
/// Each field is also a glob pattern: metacharacters from quoted/literal text
/// are escaped, so only unquoted `*?[` are active. A field that matches files is
/// replaced by the sorted matches; otherwise its literal text is used. A field
/// that is entirely unquoted and empty (e.g. `$UNSET`) drops out; a quoted empty
/// (`""`) is kept.
fn expand_argv_word_after_braces(word: &Word) -> Result<Vec<String>, String> {
    // A standalone `"$@"` expands to one argument per positional parameter,
    // preserving any spaces within each — the common arg-forwarding idiom.
    if let [WordPart::Quoted(s)] = word.as_slice() {
        if s == "$@" {
            return Ok(crate::vars::args());
        }
        // Likewise `"${arr[@]}"` and `"${!arr[@]}"`: one argument per
        // element/key, spaces and all — the array analogue of `"$@"`,
        // verified directly against real bash (`for k in "${!arr[@]}"` is
        // the standard way to iterate an associative array by key, and
        // needs this exactly as much as `"${arr[@]}"` does — a key can
        // itself contain spaces). `"${arr[*]}"`/`"${!arr[*]}"` are *not*
        // the same case: always one joined string regardless of quoting,
        // which the ordinary `Quoted` handling below (unsplit, but still
        // one field) already produces correctly via `expand_braced`'s own
        // `[*]` handling.
        if let Some(kind) = parse_whole_array_at(s) {
            return Ok(match kind {
                WholeArrayAt::Values(name) => crate::vars::array_values(&name),
                WholeArrayAt::Keys(name) if crate::vars::is_assoc(&name) => crate::vars::assoc_keys(&name),
                WholeArrayAt::Keys(name) => {
                    crate::vars::array_indices(&name).iter().map(usize::to_string).collect()
                }
            });
        }
    }

    let ifs = Ifs::current();
    let mut sp = Splitter::default();

    for (i, part) in word.iter().enumerate() {
        match part {
            WordPart::Literal(s) => sp.add_unsplit(s),
            WordPart::Quoted(s) => {
                // `"x${a[@]}y"` / `"x$@y"` (C77): the prefix attaches to
                // the first element and the suffix to the last, with each
                // middle element its own field — bash's rule. The
                // standalone forms were already handled above.
                if let Some((pre, values, post)) = quoted_embedded_at(s)? {
                    if values.is_empty() {
                        if !(pre.is_empty() && post.is_empty()) || word.len() > 1 {
                            sp.add_unsplit(&format!("{pre}{post}"));
                        }
                    } else {
                        sp.add_unsplit(&format!("{pre}{}", values[0]));
                        for v in &values[1..] {
                            sp.soft_pending = true;
                            sp.add_unsplit(v);
                        }
                        sp.add_unsplit(&post);
                    }
                    continue;
                }
                sp.add_unsplit(&expand_dollars(s)?)
            }
            WordPart::Unquoted(s) => {
                let text = if i == 0 { tilde_expand(s) } else { s.clone() };
                // Only expansion output splits on `$IFS` (C74) — literal
                // source text (a tilde result included) never does.
                for (chunk, from_expansion) in expand_unquoted_chunks(&text)? {
                    if from_expansion {
                        sp.add_split(&chunk, &ifs);
                    } else {
                        sp.add_literal_unquoted(&chunk);
                    }
                }
            }
            // See `WordPart::ArrayLiteral`'s own doc comment: `assignment_split`
            // always intercepts a word shaped like this before it reaches here.
            WordPart::ArrayLiteral(_) => {
                return Err("array literal isn't valid outside an assignment".into());
            }
        }
    }

    // A single trailing non-whitespace IFS delimiter at the very end of the
    // text doesn't produce a trailing empty field — real bash keeps a
    // *leading* one (`IFS=,`'s `,a` is `""`, `a`) but drops a *trailing* one
    // (`a,` is just `a`, not `a`, `""`) even though internal and repeated
    // trailing delimiters still do (`a,,` is `a`, `""`). The last field is
    // exactly this "opened by a hard boundary, never touched again" case iff
    // it's still unquoted and empty by the time every part's been processed.
    let mut fields = sp.fields;
    if matches!(fields.last(), Some(f) if f.explicit && f.plain.is_empty() && !f.quoted) {
        fields.pop();
    }

    let mut out = Vec::new();
    for field in fields {
        if field.globbable && !crate::vars::noglob() {
            let mut matches = crate::glob::glob(&field.pattern);
            // `$GLOBIGNORE` (C108's remainder): drop matches that match
            // any of its colon-separated patterns; a set (even empty-
            // pattern) GLOBIGNORE also never yields `.`/`..`, like bash.
            if let Some(ignore) = crate::vars::get("GLOBIGNORE").filter(|g| !g.is_empty()) {
                matches.retain(|m| {
                    let base = m.rsplit('/').next().unwrap_or(m);
                    base != "." && base != ".."
                        && !ignore.split(':').filter(|p| !p.is_empty()).any(|p| {
                            crate::glob::match_component(p, m)
                        })
                });
            }
            if !matches.is_empty() {
                out.extend(matches);
                continue;
            }
            // No match (C58): `failglob` makes it a hard error (bash
            // aborts the whole `-c` script there — verified); `nullglob`
            // drops the word entirely; the default keeps the literal.
            if crate::vars::shopt("failglob") {
                return Err(format!("no match: {}", field.plain));
            }
            if crate::vars::shopt("nullglob") {
                continue;
            }
        }
        if field.plain.is_empty() && !field.quoted && !field.explicit {
            continue; // unquoted-empty field drops out, unless $IFS itself demarcated it
        }
        out.push(field.plain);
    }
    Ok(out)
}

/// Brace expansion (`{a,b,c}`, `{1..5}`, `{a..z..2}`) — bash/ksh/zsh, not
/// POSIX; not applied to assignment-statement words (`x=value`, a prefix
/// `FOO=value cmd`), case subjects/patterns, or redirect targets, matching
/// real bash exactly for the first, and an accepted, documented scope
/// narrowing for the rest (verified directly: real bash *does*
/// brace-expand a redirect target, producing "ambiguous redirect" if it's
/// more than one word — rush's own redirect-target expansion doesn't go
/// through this path at all).
///
/// Runs on the word's raw, unexpanded text, exactly like real bash: a `$`
/// or `$(...)` inside a brace group is only resolved *after* the group
/// itself is expanded (`{$x,y}` splits into two words first; `$x` then
/// resolves normally in whichever one it lands in), and an endpoint that
/// isn't a literal integer/single-letter at this stage (`{1..$n}`) makes
/// the whole group invalid — left as literal text — even though `$n`
/// itself still expands normally afterwards. Returns `vec![word.clone()]`
/// unchanged when no valid group exists anywhere in the word (the common
/// case).
fn brace_expand(word: &Word) -> Vec<Word> {
    brace_expand_atoms(&word_to_atoms(word)).into_iter().map(|a| atoms_to_word(&a)).collect()
}

/// One atomic unit of a word's text for brace-expansion scanning: a single
/// unquoted character (eligible to be `{`/`,`/`.`, or ordinary text) or an
/// opaque quoted/literal/array-literal chunk — inert to brace syntax (a
/// quoted `,` or `}` never acts as a delimiter) but still carried through
/// verbatim into whichever alternative it ends up in (`pre{"a,b",c}post`
/// splits on the *unquoted* comma only, verified directly against bash).
#[derive(Clone)]
enum BraceAtom {
    Ch(char),
    Opaque(WordPart),
}

fn word_to_atoms(word: &Word) -> Vec<BraceAtom> {
    let mut atoms = Vec::new();
    for part in word {
        match part {
            WordPart::Unquoted(s) => atoms.extend(s.chars().map(BraceAtom::Ch)),
            other => atoms.push(BraceAtom::Opaque(other.clone())),
        }
    }
    atoms
}

fn atoms_to_word(atoms: &[BraceAtom]) -> Word {
    let mut parts: Word = Vec::new();
    for atom in atoms {
        match atom {
            BraceAtom::Ch(c) => match parts.last_mut() {
                Some(WordPart::Unquoted(s)) => s.push(*c),
                _ => parts.push(WordPart::Unquoted(c.to_string())),
            },
            BraceAtom::Opaque(part) => parts.push(part.clone()),
        }
    }
    parts
}

/// Scan left to right for the first *valid* `{...}` group (a comma-list or
/// a range) and expand it, recursing into the suffix for any further group
/// (`{a,b}{c,d}` is a cross product). A `{` that turns out invalid (no
/// top-level comma and not a range — `{a}`, `{1..$n}`, unterminated) is
/// left as a literal character and the scan resumes right after it, so an
/// invalid group doesn't block a valid one later in the same word
/// (`{{a,b}` → `{a`, `{b`: the outer `{` is unterminated as its own group
/// since the first `}` closes the inner one, so it falls back to literal
/// and the scan finds `{a,b}` starting one character later — verified
/// directly against bash).
fn brace_expand_atoms(atoms: &[BraceAtom]) -> Vec<Vec<BraceAtom>> {
    let mut i = 0;
    while i < atoms.len() {
        // A `{` immediately after `$` is a parameter expansion `${...}`,
        // never a brace list (C132) — `${x,,}` must not brace-expand into
        // `$x`, `$`, `$`. Same for `$` before it being part of `$(`/etc.
        let dollar_prefixed = i > 0 && matches!(atoms[i - 1], BraceAtom::Ch('$'));
        if !dollar_prefixed
            && matches!(atoms[i], BraceAtom::Ch('{'))
            && let Some(j) = matching_close(atoms, i)
            && let Some(alternatives) = expand_group(&atoms[i + 1..j])
        {
            let prefix = &atoms[..i];
            let suffix_alts = brace_expand_atoms(&atoms[j + 1..]);
            let mut out = Vec::new();
            for alt in &alternatives {
                for suffix in &suffix_alts {
                    let mut combined = prefix.to_vec();
                    combined.extend(alt.iter().cloned());
                    combined.extend(suffix.iter().cloned());
                    out.push(combined);
                }
            }
            return out;
        }
        i += 1;
    }
    vec![atoms.to_vec()]
}

/// The position of the `}` matching the `{` at `atoms[open]`, tracking
/// nested depth — `None` if unterminated.
fn matching_close(atoms: &[BraceAtom], open: usize) -> Option<usize> {
    let mut depth = 1;
    for (k, atom) in atoms.iter().enumerate().skip(open + 1) {
        match atom {
            BraceAtom::Ch('{') => depth += 1,
            BraceAtom::Ch('}') => {
                depth -= 1;
                if depth == 0 {
                    return Some(k);
                }
            }
            _ => {}
        }
    }
    None
}

/// Try to expand a `{...}` group's inner content as a comma-list
/// (`a,b,c`, split only on *top-level* commas — one inside a nested
/// `{...}` doesn't count) or, failing that, a range (`1..5`, `a..z..2`).
/// `None` if it's neither — an invalid/malformed group, left as literal
/// text by the caller.
fn expand_group(content: &[BraceAtom]) -> Option<Vec<Vec<BraceAtom>>> {
    let segments = split_top_level_commas(content);
    if segments.len() > 1 {
        let mut out = Vec::new();
        for seg in &segments {
            out.extend(brace_expand_atoms(seg));
        }
        return Some(out);
    }
    expand_range(content)
}

fn split_top_level_commas(content: &[BraceAtom]) -> Vec<Vec<BraceAtom>> {
    let mut segments = Vec::new();
    let mut current = Vec::new();
    let mut depth = 0;
    for atom in content {
        match atom {
            BraceAtom::Ch('{') => {
                depth += 1;
                current.push(atom.clone());
            }
            BraceAtom::Ch('}') => {
                depth -= 1;
                current.push(atom.clone());
            }
            BraceAtom::Ch(',') if depth == 0 => segments.push(std::mem::take(&mut current)),
            _ => current.push(atom.clone()),
        }
    }
    segments.push(current);
    segments
}

/// `{X..Y}` / `{X..Y..Z}` — a numeric or single-letter range. `None` if
/// `content` isn't a valid range expression: not plain unquoted text, not
/// exactly two or three `..`-separated fields, or the endpoints aren't
/// both integers or both single ASCII letters (verified directly: a
/// mismatched pair like `{1..a}` or a quoted endpoint like `{"1"..5}` is
/// left as literal text, same as any other invalid group).
fn expand_range(content: &[BraceAtom]) -> Option<Vec<Vec<BraceAtom>>> {
    let mut text = String::new();
    for atom in content {
        match atom {
            BraceAtom::Ch(c) => text.push(*c),
            BraceAtom::Opaque(_) => return None,
        }
    }
    let fields: Vec<&str> = text.split("..").collect();
    let (start, end, step_field) = match fields.as_slice() {
        [a, b] => (*a, *b, None),
        [a, b, c] => (*a, *b, Some(*c)),
        _ => return None,
    };
    let step = match step_field {
        Some(s) => Some(parse_range_int(s)?.value),
        None => None,
    };

    let strings = if let (Some(a), Some(b)) = (parse_range_int(start), parse_range_int(end)) {
        numeric_range(&a, &b, step)
    } else {
        let a = single_letter(start)?;
        let b = single_letter(end)?;
        char_range(a, b, step.unwrap_or(1))
    };

    Some(strings.into_iter().map(|s| s.chars().map(BraceAtom::Ch).collect()).collect())
}

/// A parsed numeric range endpoint: its value, whether it triggers
/// zero-padding (its digits — sign aside — start with `0` and there's more
/// than one of them, bash's own trigger, verified directly), and the total
/// character width its own literal token occupies once padding is
/// triggered (a leading `+` is dropped and doesn't count; a leading `-`
/// stays and does — `{-01..05}` pads to width 3, `-01`'s own length, not
/// 2).
struct RangeEndpoint {
    value: i64,
    width: usize,
    pads: bool,
}

fn parse_range_int(s: &str) -> Option<RangeEndpoint> {
    let (negative, digits) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let magnitude: i64 = digits.parse().ok()?;
    Some(RangeEndpoint {
        value: if negative { -magnitude } else { magnitude },
        width: usize::from(negative) + digits.len(),
        pads: digits.len() > 1 && digits.starts_with('0'),
    })
}

fn numeric_range(a: &RangeEndpoint, b: &RangeEndpoint, explicit_step: Option<i64>) -> Vec<String> {
    let step = match explicit_step.map(i64::abs) {
        None | Some(0) => 1,
        Some(s) => s,
    };
    let pad = a.pads || b.pads;
    let width = a.width.max(b.width);
    let mut out = Vec::new();
    let mut v = a.value;
    if a.value <= b.value {
        while v <= b.value {
            out.push(format_range_int(v, pad, width));
            v += step;
        }
    } else {
        while v >= b.value {
            out.push(format_range_int(v, pad, width));
            v -= step;
        }
    }
    out
}

fn format_range_int(v: i64, pad: bool, width: usize) -> String {
    if !pad {
        return v.to_string();
    }
    if v < 0 {
        format!("-{:0width$}", -v, width = width.saturating_sub(1))
    } else {
        format!("{v:0width$}")
    }
}

/// A single ASCII letter, and nothing else — `{ab..cd}` isn't a range.
fn single_letter(s: &str) -> Option<char> {
    let mut chars = s.chars();
    let c = chars.next()?;
    (chars.next().is_none() && c.is_ascii_alphabetic()).then_some(c)
}

/// A character range steps raw ASCII code points between the two
/// endpoints — same as real bash, including a mixed-case pair like
/// `{A..z}` stepping through the punctuation between `Z` and `a` in the
/// ASCII table (verified directly), not just same-case letter ranges.
fn char_range(a: char, b: char, step: i64) -> Vec<String> {
    let step = match step.abs() {
        0 => 1,
        s => s,
    };
    let (a, b) = (a as i64, b as i64);
    let mut out = Vec::new();
    let mut v = a;
    if a <= b {
        while v <= b {
            if let Some(c) = char::from_u32(v as u32) {
                out.push(c.to_string());
            }
            v += step;
        }
    } else {
        while v >= b {
            if let Some(c) = char::from_u32(v as u32) {
                out.push(c.to_string());
            }
            v -= step;
        }
    }
    out
}

/// `$IFS`'s field-splitting rules (POSIX §2.6.5). Unset IFS defaults to
/// space/tab/newline. An explicit empty IFS (`IFS=`) disables field
/// splitting entirely. Otherwise, space/tab/newline characters *present in
/// the value* are the collapsing "IFS whitespace" class (runs collapse, and
/// leading/trailing runs vanish with no empty field); every other character
/// in the value is a "non-whitespace" delimiter where *each occurrence*
/// starts a new field on its own, even with nothing in it — `IFS=,` on
/// `a,,b` is three fields (`a`, ``, `b`), not two.
struct Ifs {
    whitespace: Vec<char>,
    other: Vec<char>,
    disabled: bool,
    /// The separator unquoted `$*` joins positional parameters with: IFS's
    /// first character, a space if IFS is unset, or nothing if IFS is set
    /// but empty.
    star_sep: String,
}

impl Ifs {
    fn current() -> Ifs {
        match var_raw("IFS") {
            None => Ifs {
                whitespace: vec![' ', '\t', '\n'],
                other: Vec::new(),
                disabled: false,
                star_sep: " ".to_string(),
            },
            Some(s) if s.is_empty() => Ifs {
                whitespace: Vec::new(),
                other: Vec::new(),
                disabled: true,
                star_sep: String::new(),
            },
            Some(s) => {
                let mut whitespace = Vec::new();
                let mut other = Vec::new();
                for c in s.chars() {
                    let bucket = if matches!(c, ' ' | '\t' | '\n') { &mut whitespace } else { &mut other };
                    if !bucket.contains(&c) {
                        bucket.push(c);
                    }
                }
                let star_sep = s.chars().next().unwrap().to_string();
                Ifs { whitespace, other, disabled: false, star_sep }
            }
        }
    }

    fn is_whitespace(&self, c: char) -> bool {
        self.whitespace.contains(&c)
    }

    fn is_delim(&self, c: char) -> bool {
        self.whitespace.contains(&c) || self.other.contains(&c)
    }
}

/// One argument under construction: its literal text, its glob pattern (with
/// non-active metacharacters escaped), whether any of it was quoted or has
/// active glob metacharacters, and whether `$IFS` itself demarcated this
/// field (kept even if empty, unlike an ordinary empty unquoted expansion).
#[derive(Default)]
struct Field {
    plain: String,
    pattern: String,
    quoted: bool,
    globbable: bool,
    explicit: bool,
}

/// Assembles a word's parts into fields, splitting on `$IFS` from unquoted
/// expansions.
#[derive(Default)]
struct Splitter {
    fields: Vec<Field>,
    /// An IFS-whitespace run was seen: the *next* real content opens a new
    /// field, but nothing is forced if none follows (trailing whitespace
    /// produces no empty field). A non-whitespace IFS delimiter is handled
    /// separately by `hard_boundary`, which opens (and closes) a field
    /// immediately, empty or not.
    soft_pending: bool,
}

impl Splitter {
    /// The field currently accepting content, opening a new one if a boundary
    /// is pending or none exists yet.
    fn current(&mut self) -> &mut Field {
        if self.soft_pending || self.fields.is_empty() {
            self.fields.push(Field::default());
            self.soft_pending = false;
        }
        self.fields.last_mut().unwrap()
    }

    /// Add quoted/literal text: never split, metacharacters escaped.
    fn add_unsplit(&mut self, s: &str) {
        let f = self.current();
        f.plain.push_str(s);
        escape_meta_into(&mut f.pattern, s);
        f.quoted = true;
    }

    /// Add literal unquoted source text (C74): never split — the only
    /// text `$IFS` may split is what an expansion produced — but glob
    /// metacharacters stay active, and the field is not marked quoted
    /// (an all-empty unquoted field still drops).
    fn add_literal_unquoted(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        let f = self.current();
        f.plain.push_str(s);
        f.pattern.push_str(s);
        if has_glob_meta(s) {
            f.globbable = true;
        }
    }

    /// Add the result of an unquoted expansion: `$IFS` characters become
    /// field boundaries (whitespace collapses; non-whitespace delimiters
    /// don't), and metacharacters stay active for globbing.
    fn add_split(&mut self, text: &str, ifs: &Ifs) {
        if ifs.disabled {
            let f = self.current();
            f.plain.push_str(text);
            f.pattern.push_str(text);
            if has_glob_meta(text) {
                f.globbable = true;
            }
            return;
        }

        let mut chars = text.chars().peekable();
        while let Some(&c) = chars.peek() {
            if ifs.is_delim(c) {
                // A maximal run of IFS characters: each non-whitespace one is
                // its own delimiter (hard boundary); whitespace anywhere in
                // the run is filler, absorbed rather than adding a boundary
                // of its own — but only when at least one non-whitespace
                // delimiter is actually present in this run.
                let mut hard = 0usize;
                while let Some(&next) = chars.peek() {
                    if !ifs.is_delim(next) {
                        break;
                    }
                    if !ifs.is_whitespace(next) {
                        hard += 1;
                    }
                    chars.next();
                }
                if hard > 0 {
                    for _ in 0..hard {
                        self.hard_boundary();
                    }
                } else {
                    self.soft_pending = true;
                }
            } else {
                let mut chunk = String::new();
                while matches!(chars.peek(), Some(&c) if !ifs.is_delim(c)) {
                    chunk.push(chars.next().unwrap());
                }
                let f = self.current();
                f.plain.push_str(&chunk);
                f.pattern.push_str(&chunk);
                if has_glob_meta(&chunk) {
                    f.globbable = true;
                }
            }
        }
    }

    /// A non-whitespace `$IFS` character always delimits a field on its own,
    /// even with nothing on one (or both) sides.
    fn hard_boundary(&mut self) {
        if self.fields.is_empty() {
            self.fields.push(Field::default());
        }
        self.fields.last_mut().unwrap().explicit = true;
        self.fields.push(Field::default());
        self.fields.last_mut().unwrap().explicit = true;
        self.soft_pending = false;
    }
}

/// Expand a `[[ ]]` `==`/`!=` right-hand side into a glob pattern (C55):
/// unquoted parts keep their metacharacters active (including ones that
/// arrive via `$var` — `p="*.txt"; [[ foo.txt = $p ]]` is true in bash,
/// verified), while quoted/literal parts are backslash-escaped so they
/// only ever match themselves (`[[ $x = "a"* ]]` needs a literal `a`).
pub(crate) fn expand_cond_pattern(word: &Word) -> Result<String, String> {
    let mut pattern = String::new();
    for (i, part) in word.iter().enumerate() {
        match part {
            WordPart::Literal(s) => escape_meta_into(&mut pattern, s),
            WordPart::Quoted(s) => escape_meta_into(&mut pattern, &expand_dollars(s)?),
            WordPart::Unquoted(s) => {
                let text = if i == 0 { tilde_expand(s) } else { s.clone() };
                pattern.push_str(&expand_unquoted(&text)?);
            }
            WordPart::ArrayLiteral(_) => {}
        }
    }
    Ok(pattern)
}

/// Expand a `[[ ]]` `=~` right-hand side into a regex pattern (C56):
/// unquoted parts are live regex syntax (including via `$var` — the
/// common `p="^a.c$"; [[ $s =~ $p ]]` idiom), while quoted/literal parts
/// match themselves (`[[ abc =~ "a.c" ]]` is false in bash — verified —
/// because the quoted `.` is literal).
pub(crate) fn expand_cond_regex(word: &Word) -> Result<String, String> {
    let mut pattern = String::new();
    for (i, part) in word.iter().enumerate() {
        match part {
            WordPart::Literal(s) => pattern.push_str(&rusty_regx::escape(s)),
            WordPart::Quoted(s) => pattern.push_str(&rusty_regx::escape(&expand_dollars(s)?)),
            WordPart::Unquoted(s) => {
                let text = if i == 0 { tilde_expand(s) } else { s.clone() };
                pattern.push_str(&expand_unquoted(&text)?);
            }
            WordPart::ArrayLiteral(_) => {}
        }
    }
    Ok(pattern)
}

/// Whether unquoted text contains anything the glob matcher would treat
/// as a metacharacter — `*`/`?`/`[`, or an extglob opener (`@(`/`+(`/`!(`,
/// C57; `?(`/`*(` are already covered by their first character).
fn has_glob_meta(text: &str) -> bool {
    if text.contains(['*', '?', '[']) {
        return true;
    }
    let b = text.as_bytes();
    (1..b.len()).any(|i| b[i] == b'(' && matches!(b[i - 1], b'@' | b'+' | b'!'))
}

/// Append `s` to a glob pattern, backslash-escaping characters that would
/// otherwise be metacharacters — used for text that must stay literal.
fn escape_meta_into(pattern: &mut String, s: &str) {
    for c in s.chars() {
        if matches!(c, '*' | '?' | '[' | '\\') {
            pattern.push('\\');
        }
        pattern.push(c);
    }
}

/// Expand a word's parts into one string with `$`/`$(...)`/quote handling
/// but no word-splitting and no globbing — `local x=$v`-style values, and
/// (C55) `[[ ]]` operands, which is the whole point of `[[`.
pub(crate) fn expand_word(word: &Word) -> Result<String, String> {
    let mut out = String::new();
    for (i, part) in word.iter().enumerate() {
        match part {
            WordPart::Literal(s) => out.push_str(s),
            WordPart::Quoted(s) => out.push_str(&expand_dollars(s)?),
            WordPart::Unquoted(s) => {
                // Tilde only expands at the very start of a word.
                let text = if i == 0 { tilde_expand(s) } else { s.clone() };
                out.push_str(&expand_unquoted(&text)?);
            }
            // See `WordPart::ArrayLiteral`'s own doc comment: `assignment_split`
            // always intercepts a word shaped like this before it reaches here.
            WordPart::ArrayLiteral(_) => {
                return Err("array literal isn't valid outside an assignment".into());
            }
        }
    }
    Ok(out)
}

/// If `s` is *exactly* `${NAME[@]}` (no surrounding text — same "whole word,
/// not embedded" restriction real bash's own `"$@"` special case has, and
/// that this codebase already applies to it above), return `NAME`. Used to
/// recognize `"${arr[@]}"` as the array analogue of `"$@"`: one field per
/// element, not the single joined string every other quoted expansion
/// produces.
/// Which whole-array form `parse_whole_array_at` matched: `${arr[@]}`
/// (values) or `${!arr[@]}` (keys/indices) — see that function and its one
/// call site in `expand_argv_word`.
enum WholeArrayAt {
    Values(String),
    Keys(String),
}

fn parse_whole_array_at(s: &str) -> Option<WholeArrayAt> {
    let inner = s.strip_prefix("${")?.strip_suffix("[@]}")?;
    if let Some(name) = inner.strip_prefix('!') {
        is_valid_name(name).then(|| WholeArrayAt::Keys(name.to_string()))
    } else {
        is_valid_name(inner).then(|| WholeArrayAt::Values(inner.to_string()))
    }
}

/// Scan quoted text for an embedded `$@`, `${@}`, `${arr[@]}`, or
/// `${!arr[@]}` (C77). `Some((pre, values, post))` when found — `pre` and
/// `post` already `$`-expanded; only the *first* occurrence is treated
/// this way (multiple whole-array references in one quoted word is out of
/// scope). `None` means "no embedded whole-array reference: expand
/// normally".
fn quoted_embedded_at(s: &str) -> Result<Option<(String, Vec<String>, String)>, String> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'$' {
            i += 1;
            continue;
        }
        // `$@`
        if bytes.get(i + 1) == Some(&b'@') {
            let pre = expand_dollars(&s[..i])?;
            let post = expand_dollars(&s[i + 2..])?;
            return Ok(Some((pre, crate::vars::args(), post)));
        }
        if bytes.get(i + 1) == Some(&b'{')
            && let Some(close) = s[i + 2..].find('}').map(|c| c + i + 2)
        {
            let inner = &s[i + 2..close];
            let values = if inner == "@" {
                Some(crate::vars::args())
            } else {
                match parse_whole_array_at(&format!("${{{inner}}}")) {
                    Some(WholeArrayAt::Values(name)) => Some(crate::vars::array_values(&name)),
                    Some(WholeArrayAt::Keys(name)) if crate::vars::is_assoc(&name) => {
                        Some(crate::vars::assoc_keys(&name))
                    }
                    Some(WholeArrayAt::Keys(name)) => Some(
                        crate::vars::array_indices(&name).iter().map(usize::to_string).collect(),
                    ),
                    None => None,
                }
            };
            if let Some(values) = values {
                let pre = expand_dollars(&s[..i])?;
                let post = expand_dollars(&s[close + 1..])?;
                return Ok(Some((pre, values, post)));
            }
            i = close + 1;
            continue;
        }
        i += 1;
    }
    Ok(None)
}

/// Tilde expansion at the start of a string (C117): `~` is `$HOME`, `~+`
/// is `$PWD`, `~-` is `$OLDPWD`, and `~user` is that user's home directory
/// from the password database. An unresolvable prefix (unknown user, unset
/// `OLDPWD`) is left untouched, same as bash.
fn tilde_expand(text: &str) -> String {
    if let Some(rest) = text.strip_prefix('~') {
        let (prefix, tail) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, ""),
        };
        let expansion = match prefix {
            "" => home_dir(),
            "+" => crate::vars::get("PWD")
                .or_else(|| std::env::current_dir().ok().map(|d| d.display().to_string())),
            "-" => crate::vars::get("OLDPWD"),
            user => passwd_home(user),
        };
        if let Some(dir) = expansion {
            return format!("{dir}{tail}");
        }
    }
    text.to_string()
}

/// `~user`'s home directory, read straight from `/etc/passwd` (field 6) —
/// the shell carries no NSS dependency, so LDAP-style exotic setups fall
/// back to the literal text, an accepted narrowing.
fn passwd_home(user: &str) -> Option<String> {
    let passwd = std::fs::read_to_string("/etc/passwd").ok()?;
    passwd.lines().find_map(|line| {
        let mut fields = line.split(':');
        (fields.next() == Some(user)).then(|| fields.nth(4).map(str::to_string))?
    })
}

/// Scan a string for `$VAR`, `${VAR}`, and `$(...)`, expanding each in place.
/// `pub(crate)`, not just private: also used directly for `$PS3` (`select`'s
/// prompt undergoes ordinary `$`/command-substitution expansion, unlike
/// `$PS1`'s own bespoke backslash-escape codes in `main.rs`).
pub(crate) fn expand_dollars(text: &str) -> Result<String, String> {
    expand_dollars_impl(text, false)
}

/// Like [`expand_dollars`], but also recognizes `<(cmd)`/`>(cmd)` process
/// substitution — used only for genuinely *unquoted* text (ordinary argv
/// words, assignment values, redirect targets, case subjects), since
/// quoting fully suppresses process substitution in real bash (verified
/// directly: `echo "<(echo hi)"` and `echo '<(echo hi)'` both print the
/// literal text `<(echo hi)`), unlike `$(...)`, which *does* still expand
/// inside double quotes — so this is deliberately a separate function from
/// `expand_dollars` rather than a flag threaded through every call site.
pub(crate) fn expand_unquoted(text: &str) -> Result<String, String> {
    expand_dollars_impl(text, true)
}

fn expand_dollars_impl(text: &str, allow_process_sub: bool) -> Result<String, String> {
    Ok(expand_dollars_chunks(text, allow_process_sub)?.into_iter().map(|(s, _)| s).collect())
}

/// Chunked output for [`expand_dollars_impl`]: accumulated text spans
/// tagged with whether they came from an expansion (C74) — only those
/// spans are subject to `$IFS` field splitting. Literal source text never
/// splits, so `IFS=x; echo axb` keeps `axb` whole like bash.
#[derive(Default)]
struct Chunks(Vec<(String, bool)>);

impl Chunks {
    fn lit_char(&mut self, c: char) {
        match self.0.last_mut() {
            Some((s, false)) => s.push(c),
            _ => self.0.push((c.to_string(), false)),
        }
    }
    fn lit(&mut self, text: &str) {
        match self.0.last_mut() {
            Some((s, false)) => s.push_str(text),
            _ => self.0.push((text.to_string(), false)),
        }
    }
    fn exp(&mut self, text: &str) {
        match self.0.last_mut() {
            Some((s, true)) => s.push_str(text),
            _ => self.0.push((text.to_string(), true)),
        }
    }
}

/// As [`expand_dollars`], but for unquoted argv text: process substitution
/// allowed, and the result arrives as (text, from_expansion) chunks — see
/// [`Chunks`].
pub(crate) fn expand_unquoted_chunks(text: &str) -> Result<Vec<(String, bool)>, String> {
    expand_dollars_chunks(text, true)
}

fn expand_dollars_chunks(text: &str, allow_process_sub: bool) -> Result<Vec<(String, bool)>, String> {
    let mut out = Chunks::default();
    let mut chars = text.chars().peekable();

    while let Some(c) = chars.next() {
        if allow_process_sub && matches!(c, '<' | '>') && chars.peek() == Some(&'(') {
            chars.next(); // consume '('
            let inner = take_balanced_paren(&mut chars)?;
            out.lit(&crate::exec::process_substitute(&inner, c == '>')?);
            continue;
        }
        if c != '$' {
            out.lit_char(c);
            continue;
        }

        match chars.peek() {
            Some('(') => {
                chars.next(); // consume the first '('
                if chars.peek() == Some(&'(') {
                    // `$((expr))` — arithmetic. `$`-references inside (e.g. `$1`,
                    // `$x`) are expanded first, then the result is evaluated.
                    chars.next();
                    let expr = take_arith(&mut chars)?;
                    let expr = expand_dollars(&expr)?;
                    out.exp(&crate::arith::eval(&expr)?.to_string());
                } else {
                    // `$(...)` — command substitution. Drops trailing newlines
                    // (and the `\r` that precedes them on Windows).
                    let inner = take_balanced_paren(&mut chars)?;
                    let output = command_substitute(&inner)?;
                    out.exp(output.trim_end_matches(['\n', '\r']));
                }
            }
            // `$[ expr ]` — bash's deprecated arithmetic expansion (C131),
            // equivalent to `$(( expr ))`.
            Some('[') => {
                chars.next();
                let mut expr = String::new();
                let mut depth = 1;
                for c in chars.by_ref() {
                    match c {
                        '[' => depth += 1,
                        ']' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    expr.push(c);
                }
                let expr = expand_dollars(&expr)?;
                out.exp(&crate::arith::eval(&expr)?.to_string());
            }
            // `$?` — the last pipeline's exit status.
            Some('?') => {
                chars.next();
                out.exp(&crate::vars::last_status().to_string());
            }
            // `$!` — the most recently backgrounded job's pid; empty if
            // nothing has been backgrounded yet.
            Some('!') => {
                chars.next();
                if let Some(pid) = crate::vars::last_bg_pid() {
                    out.exp(&pid.to_string());
                }
            }
            // `$$` — the shell's own pid (C41). One process per shell here,
            // so `std::process::id()` is the answer everywhere — rush runs
            // subshell-ish constructs in-process, and real bash's `$$` is
            // likewise the *parent* shell's pid even inside `(...)`/`$(...)`.
            Some('$') => {
                chars.next();
                out.exp(&crate::vars::shell_pid().to_string());
            }
            // `$-` — the currently-set single-letter options (C41).
            Some('-') => {
                chars.next();
                out.exp(&crate::vars::option_flags());
            }
            // `$#` — number of positional parameters.
            Some('#') => {
                chars.next();
                out.exp(&crate::vars::arg_count().to_string());
            }
            // `$@` — all positional parameters, space-joined here. (A
            // standalone `"$@"` keeps each parameter separate; see below.)
            Some('@') => {
                chars.next();
                out.exp(&crate::vars::args().join(" "));
            }
            // `$*` — all positional parameters, joined with `$IFS`'s first
            // character (space if unset, nothing if IFS is set but empty).
            Some('*') => {
                chars.next();
                out.exp(&crate::vars::args().join(&Ifs::current().star_sep));
            }
            // `$0`–`$9` — positional parameters.
            Some(&c) if c.is_ascii_digit() => {
                chars.next();
                let n = (c as u8 - b'0') as usize;
                out.exp(&arg_checked(n)?);
            }
            Some('{') => {
                chars.next(); // consume '{'
                let mut inner = String::new();
                let mut depth = 1usize;
                let mut closed = false;
                for c in chars.by_ref() {
                    if c == '{' {
                        depth += 1;
                    } else if c == '}' {
                        depth -= 1;
                        if depth == 0 {
                            closed = true;
                            break;
                        }
                    }
                    inner.push(c);
                }
                if !closed {
                    return Err("unterminated `${`".into());
                }
                out.exp(&expand_braced(&inner, allow_process_sub)?);
            }
            Some(&c2) if c2 == '_' || c2.is_ascii_alphabetic() => {
                let mut name = String::new();
                while let Some(&nc) = chars.peek() {
                    if nc == '_' || nc.is_ascii_alphanumeric() {
                        name.push(nc);
                        chars.next();
                    } else {
                        break;
                    }
                }
                out.exp(&var_lookup_checked(&name)?);
            }
            // A lone `$` (or one before punctuation/digits we don't handle yet)
            // is just a literal dollar sign.
            _ => out.lit_char('$'),
        }
    }

    Ok(out.0)
}

/// Read up to the matching `)` after an already-consumed `(`, returning the
/// inner text. Tracks nesting and quoted spans.
fn take_balanced_paren(chars: &mut Peekable<Chars>) -> Result<String, String> {
    let mut inner = String::new();
    let mut depth = 1usize;
    let mut quote: Option<char> = None;

    for c in chars.by_ref() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                }
            }
            None => match c {
                '\'' | '"' => quote = Some(c),
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(inner);
                    }
                }
                _ => {}
            },
        }
        inner.push(c);
    }

    Err("unterminated `$(`".into())
}

/// Read an arithmetic expression up to the closing `))`, after `$((` has been
/// consumed. Inner parentheses are balanced.
fn take_arith(chars: &mut Peekable<Chars>) -> Result<String, String> {
    let mut expr = String::new();
    let mut depth = 0usize;
    loop {
        match chars.next() {
            None => return Err("unterminated `$((`".into()),
            Some('(') => {
                depth += 1;
                expr.push('(');
            }
            Some(')') if depth > 0 => {
                depth -= 1;
                expr.push(')');
            }
            // A `)` at depth 0 must be the first of the closing `))`.
            Some(')') => {
                return match chars.next() {
                    Some(')') => Ok(expr),
                    _ => Err("unterminated `$((`".into()),
                };
            }
            Some(c) => expr.push(c),
        }
    }
}

/// Run `src` as its own command line (operators and all) and capture its
/// stdout. One level deeper for `set -x`'s own nesting-depth indicator
/// (`crate::vars::with_deeper_trace`) — a command run here is one level of
/// `$(...)` down from whatever's expanding this substitution.
fn command_substitute(src: &str) -> Result<String, String> {
    let list = parser::parse(src).map_err(|e| e.to_string())?;
    crate::vars::with_deeper_trace(|| crate::exec::capture_list(&list))
}

/// A variable's value, or `None` if unset. `vars::get` alone is a complete
/// answer — `main.rs` seeds every inherited environment variable into
/// `vars`'s own table at startup (C36), so there's nothing left in the real
/// OS environment `vars::get` wouldn't already know about. Falling back to
/// `std::env::var` here too (as this used to) would silently resurrect an
/// inherited variable's original value after `unset` (C40) — `vars::get`
/// correctly returns `None` for both "never set" and "explicitly unset",
/// same as real bash doesn't distinguish them either.
fn var_raw(name: &str) -> Option<String> {
    crate::vars::get(name)
}

/// As [`var_raw`], but honors `set -u` (nounset): an unset variable is an
/// error instead of expanding to empty. Used at every "plain value" call
/// site (`$name`, `${name}`, `${#name}`, the `#`/`##`/`%`/`%%` pattern-removal
/// operators) — but *not* the `:-`/`:=`/`:+`/`:?` default/alternate family,
/// which handle an unset variable themselves and are exempt from the check
/// in real bash (verified directly), nor the `@`/`*`/`#` special parameters
/// or `$0`, which are always considered set even when empty.
fn var_lookup_checked(name: &str) -> Result<String, String> {
    match var_raw(name) {
        Some(v) => Ok(v),
        None if crate::vars::nounset() => Err(format!("{name}: unbound variable")),
        None => Ok(String::new()),
    }
}

/// As [`crate::vars::arg`], but honors `set -u`: `$N`/`${N}` for a positional
/// parameter beyond `$#` is an error under nounset (verified directly against
/// real bash — unlike `$@`/`$*`, a numbered positional parameter *is* subject
/// to the check).
fn arg_checked(n: usize) -> Result<String, String> {
    match crate::vars::arg(n) {
        Some(v) => Ok(v),
        None if crate::vars::nounset() => Err(format!("{n}: unbound variable")),
        None => Ok(String::new()),
    }
}

fn is_valid_name(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c == '_' || c.is_ascii_alphabetic())
        && chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

/// An array subscript, as parsed out of a trailing `[...]` by
/// `parse_subscript` — `@`/`*` (the whole array, differing only in how
/// multiple elements join when quoted/unquoted — see `expand_argv_word`'s
/// own `"${arr[@]}"` special case) or a single index, still as raw
/// unevaluated source text (see `eval_subscript`).
enum Subscript<'a> {
    At,
    Star,
    Index(&'a str),
}

/// Split `inner` into a valid name and a trailing `[...]` subscript, if it's
/// shaped that way (`arr[0]`, `arr[@]`, `arr[*]`, `arr[i+1]`) — `None` for
/// anything else (a plain `name`, or `name` followed by an operator like
/// `#`/`:-` instead of `[`), so the caller falls through to the existing
/// non-array handling.
fn parse_subscript(inner: &str) -> Option<(&str, Subscript<'_>)> {
    let name_end = inner.find(|c: char| !(c == '_' || c.is_ascii_alphanumeric())).unwrap_or(inner.len());
    let name = &inner[..name_end];
    if !is_valid_name(name) {
        return None;
    }
    let inside = inner[name_end..].strip_prefix('[')?.strip_suffix(']')?;
    Some((
        name,
        match inside {
            "@" => Subscript::At,
            "*" => Subscript::Star,
            expr => Subscript::Index(expr),
        },
    ))
}

/// `$`-expand a subscript's raw text — the one step that always applies,
/// *regardless* of whether the subscript ends up treated as an arithmetic
/// index or a literal associative-array key (verified directly:
/// `${arr[$i]}`/`arr[$i]=x` resolve `$i` either way — only what happens to
/// the result afterward differs by `name`'s current type).
fn resolve_subscript_text(expr: &str) -> Result<String, String> {
    expand_dollars(expr)
}

/// Evaluate an already-`$`-expanded subscript as an arithmetic expression
/// against `name`'s own indices — `arith::eval` resolves a *bare* name
/// directly too, so `${arr[i+1]}` needs no `$` either, verified directly
/// against real bash. A negative result resolves bash-style (C85):
/// counting back from the maximum assigned index plus one, so `${a[-1]}`
/// is the last element of `a=(x y z)` and `a[-1]=Q` overwrites it.
/// Still-negative after that (out of range) is `None`, the same "nothing
/// there" outcome an ordinary out-of-range index already has.
pub(crate) fn eval_index(name: &str, expr: &str) -> Option<usize> {
    let v = crate::arith::eval(expr).ok()?;
    if v >= 0 {
        return usize::try_from(v).ok();
    }
    let n = crate::vars::array_indices(name).last().map_or(0, |m| m + 1) as i64;
    usize::try_from(n + v).ok()
}

/// Resolve a single-element subscript's value for a read (`${arr[N]}`) —
/// dispatches on whether `name` is *currently* declared associative
/// (`crate::vars::is_assoc`): if so, the resolved text is a literal string
/// key; otherwise it's evaluated as an arithmetic index. Same rule
/// `key_set`/`key_append` use for writes — see their own doc comments for
/// why this can only be decided at the point `name`'s type is actually
/// known, not baked into the subscript's own parsed shape.
fn read_subscript(name: &str, expr: &str) -> Result<Option<String>, String> {
    let resolved = resolve_subscript_text(expr)?;
    Ok(if crate::vars::is_assoc(name) {
        crate::vars::assoc_get(name, &resolved)
    } else {
        eval_index(name, &resolved).and_then(|i| crate::vars::array_get(name, i))
    })
}

/// What `unset 'arr[subscript]'` targets, once `subscript` is resolved
/// against `arr`'s actual current type.
pub(crate) enum UnsetTarget {
    Index(String, usize),
    Key(String, String),
}

/// `unset 'arr[i]'`/`unset 'arr[key]'`: split `text` into a name and its
/// resolved target, if it's shaped like a single-element subscript
/// (`arr[i]`, not `arr[@]`/`arr[*]`) — resolved the same way a read
/// (`${arr[i]}`) is, including a `$`-reference (verified directly against
/// real bash: `unset 'arr[$i]'` resolves `$i` even though the single quotes
/// mean the shell itself never touched it — `unset`'s own subscript is
/// evaluated independently of ordinary shell quoting/expansion) and the
/// same associative-vs-indexed type check as everywhere else.
pub(crate) fn parse_array_unset_index(text: &str) -> Result<Option<UnsetTarget>, String> {
    let Some((name, Subscript::Index(expr))) = parse_subscript(text) else {
        return Ok(None);
    };
    let resolved = resolve_subscript_text(expr)?;
    Ok(if crate::vars::is_assoc(name) {
        Some(UnsetTarget::Key(name.to_string(), resolved))
    } else {
        eval_index(name, &resolved).map(|i| UnsetTarget::Index(name.to_string(), i))
    })
}

/// Expand the inside of a `${...}`: a plain name, `${#name}` (length), one of
/// the pattern-removal operators `#` `##` `%` `%%`, or one of the
/// default/alternate operators `:-` `-` `:=` `=` `:+` `+` `:?` `?`. With a
/// colon the test is "unset *or* empty"; without, just "unset". (Unlike the
/// default/alternate family, `#`/`##`/`%`/`%%` have no colon form — bash
/// doesn't define one either.) Also handles indexed-array forms: `${arr[N]}`,
/// `${arr[@]}`/`${arr[*]}`, `${#arr[@]}`/`${#arr[N]}`, `${!arr[@]}` — but
/// *not* a subscript combined with pattern-removal or a default/alternate
/// operator (`${arr[0]#pat}`, `${arr[@]:-x}`), a documented, accepted scope
/// limit (bash supports these; this codebase doesn't yet).
fn expand_braced(inner: &str, unquoted: bool) -> Result<String, String> {
    // `${!arr[@]}` / `${!arr[*]}` — the array's own set indices/keys, not
    // the values (skips gaps in a sparse indexed array entirely, same as
    // `${arr[@]}`).
    if let Some(rest) = inner.strip_prefix('!')
        && let Some((name, sub)) = parse_subscript(rest)
    {
        let keys: Vec<String> = if crate::vars::is_assoc(name) {
            crate::vars::assoc_keys(name)
        } else {
            crate::vars::array_indices(name).iter().map(usize::to_string).collect()
        };
        return Ok(match sub {
            Subscript::Star => keys.join(&Ifs::current().star_sep),
            _ => keys.join(" "),
        });
    }

    // `${!prefix@}` / `${!prefix*}` — every variable name starting with
    // `prefix`, sorted (C60); joined like `$@`/`$*` would join.
    if let Some(rest0) = inner.strip_prefix('!')
        && let Some((prefix, star)) = rest0
            .strip_suffix('@')
            .map(|p| (p, false))
            .or_else(|| rest0.strip_suffix('*').map(|p| (p, true)))
        && !prefix.is_empty()
        && is_valid_name(prefix)
    {
        let mut names: Vec<String> =
            crate::vars::names().into_iter().filter(|n| n.starts_with(prefix)).collect();
        names.sort();
        return Ok(if star { names.join(&Ifs::current().star_sep) } else { names.join(" ") });
    }

    // `${!var}` — indirect expansion (C60): `$var`'s own value names the
    // parameter to expand (a variable name or a positional-parameter
    // number). Any trailing operators compose by re-dispatching through
    // this same function (`${!v:-def}` applies the default to the
    // *referent*, matching bash).
    if let Some(rest0) = inner.strip_prefix('!') {
        let name_end =
            rest0.find(|c: char| !(c == '_' || c.is_ascii_alphanumeric())).unwrap_or(rest0.len());
        let (target, ops) = (&rest0[..name_end], &rest0[name_end..]);
        if is_valid_name(target) {
            let referent = var_lookup_checked(target)?;
            if referent.is_empty() {
                // bash: an empty/unset referent is a hard error, not an
                // empty expansion (verified: `w=; echo "${!w}"` aborts).
                return Err(format!("{target}: invalid variable name"));
            }
            return expand_braced(&format!("{referent}{ops}"), unquoted);
        }
    }

    // Special parameters: `${#}`, `${@}`/`${*}`, and numeric `${10}`.
    match inner {
        "#" => return Ok(crate::vars::arg_count().to_string()),
        "@" => return Ok(crate::vars::args().join(" ")),
        "*" => return Ok(crate::vars::args().join(&Ifs::current().star_sep)),
        // `${$}` and `${-}` — braced spellings of `$$`/`$-` (C41), same as
        // real bash.
        "$" => return Ok(crate::vars::shell_pid().to_string()),
        "-" => return Ok(crate::vars::option_flags()),
        _ if !inner.is_empty() && inner.bytes().all(|b| b.is_ascii_digit()) => {
            let n: usize = inner.parse().map_err(|_| format!("${{{inner}}}: bad substitution"))?;
            return arg_checked(n);
        }
        _ => {}
    }

    // `${@OP}` / `${*OP}` — `@`-transforms and the default/alternate
    // family on the positional parameters (C132): `${@@Q}`, `${*@U}`,
    // `${@:-def}` (the colon slicing/default is handled just below; this
    // covers the colon-less transforms the array `[@]` path also does).
    if let Some(first) = inner.chars().next()
        && matches!(first, '@' | '*')
        && inner.len() > 1
    {
        let rest = &inner[1..];
        let star = first == '*';
        let values = crate::vars::args();
        let sep = if star { Ifs::current().star_sep.clone() } else { " ".to_string() };
        if let Some(op) = rest.strip_prefix('@') {
            let mut out = Vec::with_capacity(values.len());
            for v in &values {
                out.push(at_transform(v, op).ok_or_else(|| format!("${{{inner}}}: bad substitution"))?);
            }
            return Ok(out.join(&sep));
        }
        if let Some(result) = whole_array_default(&values, star, rest, inner)? {
            return Ok(result);
        }
    }

    // `${@:off[:len]}` / `${*:off[:len]}` — positional-parameter slicing
    // (C86). The list is `$0` followed by the positional parameters, so
    // offset 0 starts at the shell/script name and offset 1 at `$1`,
    // matching bash. A `:` immediately followed by `-`/`+`/`=`/`?` is the
    // default/alternate family instead, same disambiguation bash applies
    // (`${@: -1}` slices, `${@:-x}` defaults).
    // `${@:-word}` / `${*:-word}` and the rest of the default/alternate
    // family on the positional parameters: the word substitutes when there
    // are no positional parameters (or, with the `:` form, when they join
    // to an empty string) — `=` can't assign to `$@` and errors like bash.
    if let Some(first) = inner.chars().next()
        && matches!(first, '@' | '*')
        && let rest = &inner[1..]
        && let Some(op) = rest
            .strip_prefix(':')
            .and_then(|r| r.chars().next())
            .filter(|c| matches!(c, '-' | '+' | '?'))
    {
        let word = expand_dollars(&rest[1 + op.len_utf8()..])?;
        let joined = if first == '*' {
            crate::vars::args().join(&Ifs::current().star_sep)
        } else {
            crate::vars::args().join(" ")
        };
        let null = joined.is_empty();
        return match op {
            '-' => Ok(if null { word } else { joined }),
            '+' => Ok(if null { String::new() } else { word }),
            _ => {
                if null {
                    Err(if word.is_empty() {
                        format!("{first}: parameter null or not set")
                    } else {
                        word
                    })
                } else {
                    Ok(joined)
                }
            }
        };
    }

    if let Some(first) = inner.chars().next()
        && matches!(first, '@' | '*')
        && let Some(spec) = inner[1..].strip_prefix(':')
        && !matches!(spec.chars().next(), Some('-' | '+' | '=' | '?'))
    {
        let mut values = vec![arg_checked(0)?];
        values.extend(crate::vars::args());
        let (off_src, len_src) = match spec.find(':') {
            Some(i) => (&spec[..i], Some(&spec[i + 1..])),
            None => (spec, None),
        };
        let off = crate::arith::eval(&expand_dollars(off_src)?)?;
        let n = values.len() as i64;
        let start = if off < 0 { (n + off).max(0) } else { off.min(n) };
        let end = match len_src {
            None => n,
            Some(src) => {
                let len = crate::arith::eval(&expand_dollars(src)?)?;
                if len < 0 {
                    return Err(format!("{len}: substring expression < 0"));
                }
                (start + len).min(n)
            }
        };
        let sep = if first == '*' { Ifs::current().star_sep.clone() } else { " ".to_string() };
        return Ok(values[start as usize..end.max(start) as usize].join(&sep));
    }

    // `${#name}` / `${#arr[@]}` (element count) / `${#arr[N]}` (that
    // element's own string length). `${#*}`/`${#@}` are the positional
    // count, same as `$#` (C86).
    if let Some(name_and_sub) = inner.strip_prefix('#') {
        if matches!(name_and_sub, "*" | "@") {
            return Ok(crate::vars::arg_count().to_string());
        }
        if let Some((name, sub)) = parse_subscript(name_and_sub) {
            return Ok(match sub {
                Subscript::At | Subscript::Star => crate::vars::array_len(name).to_string(),
                Subscript::Index(expr) => {
                    read_subscript(name, expr)?.map_or(0, |v| v.chars().count()).to_string()
                }
            });
        }
        if !is_valid_name(name_and_sub) {
            return Err(format!("${{{inner}}}: bad substitution"));
        }
        return Ok(var_lookup_checked(name_and_sub)?.chars().count().to_string());
    }

    // `${arr[N]}` / `${arr[@]}` / `${arr[*]}` — see this function's own doc
    // comment for why a subscript can't combine with what follows below.
    if let Some((name, sub)) = parse_subscript(inner) {
        return match sub {
            Subscript::At => Ok(crate::vars::array_values(name).join(" ")),
            Subscript::Star => Ok(crate::vars::array_values(name).join(&Ifs::current().star_sep)),
            Subscript::Index(expr) => match read_subscript(name, expr)? {
                Some(v) => Ok(v),
                // `set -u` (C132): a missing element is an error, same as
                // a missing scalar — bash aborts with `a[i]: unbound`.
                None if crate::vars::nounset() => {
                    Err(format!("{name}[{expr}]: unbound variable"))
                }
                None => Ok(String::new()),
            },
        };
    }

    // Array-wide transformations (C59): `${arr[@]/pat/repl}`,
    // A *single* array element combined with an operator (C131):
    // `${arr[i]:-def}`, `${arr[i]#pat}`, `${arr[i]/a/b}`, `${arr[i]^^}`,
    // `${arr[i]@Q}` — resolve the element, then apply the operator exactly
    // as the scalar path below does. (The `@`/`*` whole-array forms are
    // handled separately, just after.)
    if let Some(open) = inner.find('[')
        && is_valid_name(&inner[..open])
        && let Some(close) = inner[open..].find(']').map(|i| open + i)
        && !matches!(&inner[open + 1..close], "@" | "*")
        && close + 1 < inner.len()
    {
        let name = &inner[..open];
        let subscript = &inner[open + 1..close];
        let rest = &inner[close + 1..];
        let value = read_subscript(name, subscript)?;
        return apply_scalar_op(value.as_deref(), rest, inner);
    }

    // `${arr[@]^^}`, `${arr[@]:1:2}`… — the scalar operator applied to
    // every element, results joined like `${arr[@]}`/`${arr[*]}` would
    // join. (`#`/`%` prefix/suffix strips ride along too.)
    if let Some(open) = inner.find('[')
        && is_valid_name(&inner[..open])
        && let Some(close) = inner[open..].find(']').map(|i| open + i)
        && matches!(&inner[open + 1..close], "@" | "*")
        && close + 1 < inner.len()
    {
        let name = &inner[..open];
        let rest = &inner[close + 1..];
        let values = crate::vars::array_values(name);
        // `${arr[@]:off[:len]}` is array *slicing* — a range of elements,
        // not a per-element substring (verified: `${arr[@]:1:2}` yields
        // elements 1 and 2). Every other operator applies per element.
        if rest.starts_with(':') && is_transform_op(rest) {
            let spec = &rest[1..];
            let (off_src, len_src) = match spec.find(':') {
                Some(i) => (&spec[..i], Some(&spec[i + 1..])),
                None => (spec, None),
            };
            let off = crate::arith::eval(&expand_dollars(off_src)?)?;
            let n = values.len() as i64;
            let start = if off < 0 { (n + off).max(0) } else { off.min(n) };
            let end = match len_src {
                None => n,
                Some(src) => {
                    let len = crate::arith::eval(&expand_dollars(src)?)?;
                    if len < 0 {
                        return Err(format!("{len}: substring expression < 0"));
                    }
                    (start + len).min(n)
                }
            };
            let sep = if &inner[open + 1..close] == "*" { Ifs::current().star_sep.clone() } else { " ".to_string() };
            return Ok(values[start as usize..end.max(start) as usize].join(&sep));
        }
        // Default/alternate family on the whole array (C132): the array is
        // "unset/empty" for `:-`/`:+`/`:?` when it has no elements (or, with
        // the colon, joins to an empty string).
        let star = &inner[open + 1..close] == "*";
        if let Some(result) = whole_array_default(&values, star, rest, inner)? {
            return Ok(result);
        }
        // Per-element `@`-transforms (C118): `${a[@]@U}` etc., plus the
        // whole-array `@K`/`@k` forms, which emit key/value *pairs* (their
        // whole point is `declare -A b=( ${a[@]@K} )` round-tripping).
        if let Some(op) = rest.strip_prefix('@') {
            let sep =
                if &inner[open + 1..close] == "*" { Ifs::current().star_sep.clone() } else { " ".to_string() };
            if matches!(op, "K" | "k") {
                let keys: Vec<String> = if crate::vars::is_assoc(name) {
                    crate::vars::assoc_keys(name)
                } else {
                    crate::vars::array_indices(name).iter().map(usize::to_string).collect()
                };
                let pairs: Vec<String> = keys
                    .iter()
                    .zip(&values)
                    .map(|(k, v)| {
                        if op == "K" { format!("{k} {}", shell_quote(v)) } else { format!("{k} {v}") }
                    })
                    .collect();
                return Ok(pairs.join(&sep));
            }
            let mut transformed = Vec::with_capacity(values.len());
            for v in &values {
                transformed.push(
                    at_transform(v, op).ok_or_else(|| format!("${{{inner}}}: bad substitution"))?,
                );
            }
            return Ok(transformed.join(&sep));
        }
        let mut transformed = Vec::with_capacity(values.len());
        for v in &values {
            transformed.push(if is_transform_op(rest) {
                string_transform(v, rest)?
            } else if let Some(word_src) = rest.strip_prefix("##") {
                strip_prefix_pattern(v, &expand_dollars(word_src)?, true)
            } else if let Some(word_src) = rest.strip_prefix('#') {
                strip_prefix_pattern(v, &expand_dollars(word_src)?, false)
            } else if let Some(word_src) = rest.strip_prefix("%%") {
                strip_suffix_pattern(v, &expand_dollars(word_src)?, true)
            } else if let Some(word_src) = rest.strip_prefix('%') {
                strip_suffix_pattern(v, &expand_dollars(word_src)?, false)
            } else {
                return Err(format!("${{{inner}}}: bad substitution"));
            });
        }
        let sep = if &inner[open + 1..close] == "*" { Ifs::current().star_sep.clone() } else { " ".to_string() };
        return Ok(transformed.join(&sep));
    }

    let name_end = inner
        .find(|c: char| !(c == '_' || c.is_ascii_alphanumeric()))
        .unwrap_or(inner.len());
    let name = &inner[..name_end];
    let rest = &inner[name_end..];

    if !is_valid_name(name) {
        return Err(format!("${{{inner}}}: bad substitution"));
    }
    if rest.is_empty() {
        return var_lookup_checked(name);
    }

    // Pattern-removal: `##`/`%%` before `#`/`%` so the doubled (greedy) form
    // isn't mistaken for the single form plus a literal leading `#`/`%`.
    if let Some(word_src) = rest.strip_prefix("##") {
        let pattern = expand_dollars(word_src)?;
        return Ok(strip_prefix_pattern(&var_lookup_checked(name)?, &pattern, true));
    }
    if let Some(word_src) = rest.strip_prefix('#') {
        let pattern = expand_dollars(word_src)?;
        return Ok(strip_prefix_pattern(&var_lookup_checked(name)?, &pattern, false));
    }
    if let Some(word_src) = rest.strip_prefix("%%") {
        let pattern = expand_dollars(word_src)?;
        return Ok(strip_suffix_pattern(&var_lookup_checked(name)?, &pattern, true));
    }
    if let Some(word_src) = rest.strip_prefix('%') {
        let pattern = expand_dollars(word_src)?;
        return Ok(strip_suffix_pattern(&var_lookup_checked(name)?, &pattern, false));
    }

    // `${v@Q}` / `@E` / `@a` / `@A` — bash 4.4's parameter transformations
    // (C60): shell-requote, ANSI-C unescape, attribute letters, and
    // reconstruct-as-assignment — plus the case transforms `@U`/`@u`/`@L`
    // (bash 5.1), `@K`/`@k` quoted round-tripping, and `@P` prompt
    // expansion (C118).
    if let Some(op) = rest.strip_prefix('@') {
        return match op {
            "a" => Ok(attr_letters(name)),
            "A" => Ok(reconstruct_assignment(name)),
            _ => at_transform(&var_lookup_checked(name)?, op)
                .ok_or_else(|| format!("${{{inner}}}: bad substitution")),
        };
    }

    // String transformations (C59): `${v/pat/repl}` (and `//`, `/#`, `/%`),
    // `${v:offset[:length]}` substrings, and `${v^}`/`${v^^}`/`${v,}`/
    // `${v,,}` case conversion. Checked before the `:-` default family —
    // `is_transform_op` only claims a `:` whose next character can't start
    // that family, so `${v:-x}` still means "default".
    if is_transform_op(rest) {
        return string_transform(&var_lookup_checked(name)?, rest);
    }

    let colon = rest.starts_with(':');
    let ops = if colon { &rest[1..] } else { rest };
    let op = ops.chars().next();
    let word = expand_braced_word(&ops[op.map_or(0, char::len_utf8)..], unquoted)?;

    let value = var_raw(name);
    let use_word = match &value {
        None => true,
        Some(v) => colon && v.is_empty(),
    };

    match op {
        // `:-` / `-`: substitute the word when unset (or empty).
        Some('-') => Ok(if use_word { word } else { value.unwrap() }),
        // `:=` / `=`: also assign the word back to the variable.
        Some('=') => {
            if use_word {
                crate::vars::set(name, &word);
                Ok(word)
            } else {
                Ok(value.unwrap())
            }
        }
        // `:+` / `+`: substitute the word only when set (and non-empty).
        Some('+') => Ok(if use_word { String::new() } else { word }),
        // `:?` / `?`: error out when unset (or empty).
        Some('?') => {
            if use_word {
                let msg = if word.is_empty() {
                    format!("{name}: parameter null or not set")
                } else {
                    word
                };
                Err(msg)
            } else {
                Ok(value.unwrap())
            }
        }
        _ => Err(format!("${{{inner}}}: bad substitution")),
    }
}

/// Apply a `${...OP}` operator to an already-resolved value (C131) — the
/// shared logic for a single array element `${arr[i]OP}`, which resolves
/// to a value the same way a scalar does. `value` is `None` for an unset
/// element (so the default/alternate family fires). Covers pattern
/// removal, string transforms, value-based `@`-transforms, and the
/// default/alternate family; `:=` write-back to an element isn't
/// supported (a documented narrowing — rare).
fn apply_scalar_op(value: Option<&str>, rest: &str, inner: &str) -> Result<String, String> {
    let present = value.unwrap_or("");
    // Pattern removal (`##`/`%%` before the single forms).
    if let Some(word_src) = rest.strip_prefix("##") {
        return Ok(strip_prefix_pattern(present, &expand_dollars(word_src)?, true));
    }
    if let Some(word_src) = rest.strip_prefix('#') {
        return Ok(strip_prefix_pattern(present, &expand_dollars(word_src)?, false));
    }
    if let Some(word_src) = rest.strip_prefix("%%") {
        return Ok(strip_suffix_pattern(present, &expand_dollars(word_src)?, true));
    }
    if let Some(word_src) = rest.strip_prefix('%') {
        return Ok(strip_suffix_pattern(present, &expand_dollars(word_src)?, false));
    }
    if let Some(op) = rest.strip_prefix('@') {
        return at_transform(present, op).ok_or_else(|| format!("${{{inner}}}: bad substitution"));
    }
    if is_transform_op(rest) {
        return string_transform(present, rest);
    }
    // Default/alternate family.
    let colon = rest.starts_with(':');
    let ops = if colon { &rest[1..] } else { rest };
    let op = ops.chars().next();
    let word = expand_braced_word(&ops[op.map_or(0, char::len_utf8)..], false)?;
    let use_word = match value {
        None => true,
        Some(v) => colon && v.is_empty(),
    };
    match op {
        Some('-') | Some('=') => Ok(if use_word { word } else { present.to_string() }),
        Some('+') => Ok(if use_word { String::new() } else { word }),
        Some('?') => {
            if use_word {
                Err(if word.is_empty() { format!("{inner}: parameter null or not set") } else { word })
            } else {
                Ok(present.to_string())
            }
        }
        _ => Err(format!("${{{inner}}}: bad substitution")),
    }
}

/// The default/alternate family (`:-`/`:+`/`:?`, and colon-less) applied
/// to a whole value list (C132) — `${a[@]:-x}`, `${@:-x}`. `Ok(None)`
/// means "not a default-family operator, try something else". `sep` joins
/// the present values (space for `@`, IFS-first for `*`).
fn whole_array_default(
    values: &[String],
    star: bool,
    rest: &str,
    inner: &str,
) -> Result<Option<String>, String> {
    let colon = rest.starts_with(':');
    let ops = if colon { &rest[1..] } else { rest };
    let op = match ops.chars().next() {
        Some(c @ ('-' | '+' | '?')) => c,
        _ => return Ok(None),
    };
    let sep = if star { Ifs::current().star_sep.clone() } else { " ".to_string() };
    let joined = values.join(&sep);
    // "Unset/empty": no elements, or (with colon) they join to empty.
    let empty = values.is_empty() || (colon && joined.is_empty());
    let word = expand_braced_word(&ops[op.len_utf8()..], false)?;
    Ok(Some(match op {
        '-' => {
            if empty {
                word
            } else {
                joined
            }
        }
        '+' => {
            if empty {
                String::new()
            } else {
                word
            }
        }
        _ => {
            if empty {
                return Err(if word.is_empty() {
                    format!("{inner}: parameter null or not set")
                } else {
                    word
                });
            }
            joined
        }
    }))
}

/// The value-based `@`-transform operators, shared between the scalar
/// (`${v@U}`) and per-element array (`${a[@]@U}`) forms (C118). `None` for
/// an unknown operator — the caller owns the "bad substitution" error, so
/// name-based operators (`@a`/`@A`) can be handled before dispatching here.
fn at_transform(value: &str, op: &str) -> Option<String> {
    Some(match op {
        "Q" | "K" | "k" => shell_quote(value),
        "E" => ansi_unescape(value),
        "U" => value.to_uppercase(),
        "L" => value.to_lowercase(),
        "u" => {
            let mut chars = value.chars();
            match chars.next() {
                Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        }
        "P" => {
            // Full prompt expansion: escapes, then `promptvars`-style
            // `$`-expansion — same two passes the live prompt gets.
            let escaped = crate::expand_ps1(value);
            expand_dollars(&escaped).unwrap_or(escaped)
        }
        _ => return None,
    })
}

/// `${v@Q}` (C60): re-quote a value so it can be reused as shell input —
/// single quotes normally (with the `'\''` dance for embedded quotes),
/// or bash's `$'...'` form when control characters are present, matching
/// bash's own output format (verified).
fn shell_quote(value: &str) -> String {
    if value.chars().any(|c| c.is_control()) {
        let mut out = String::from("$'");
        for c in value.chars() {
            match c {
                '\n' => out.push_str("\\n"),
                '\t' => out.push_str("\\t"),
                '\r' => out.push_str("\\r"),
                '\\' => out.push_str("\\\\"),
                '\'' => out.push_str("\\'"),
                c if c.is_control() => out.push_str(&format!("\\x{:02x}", c as u32)),
                c => out.push(c),
            }
        }
        out.push('\'');
        return out;
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// `${v@E}` (C60): interpret backslash escapes the way `$'...'` would.
/// Also used by the lexer for `$'...'` ANSI-C quoting itself.
pub(crate) fn ansi_unescape(value: &str) -> String {
    // Consume up to `max` characters matching `pred` and parse them in
    // `radix` — the shared shape of `\xHH`, `\nnn`, `\uXXXX`, `\UXXXXXXXX`.
    fn take_number(
        chars: &mut std::iter::Peekable<std::str::Chars>,
        max: usize,
        radix: u32,
    ) -> Option<u32> {
        let mut digits = String::new();
        while digits.len() < max
            && let Some(&c) = chars.peek()
            && c.is_digit(radix)
        {
            digits.push(c);
            chars.next();
        }
        if digits.is_empty() { None } else { u32::from_str_radix(&digits, radix).ok() }
    }
    let mut out = String::new();
    let mut chars = value.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.peek().copied() {
            Some('n') => { chars.next(); out.push('\n'); }
            Some('t') => { chars.next(); out.push('\t'); }
            Some('r') => { chars.next(); out.push('\r'); }
            Some('a') => { chars.next(); out.push('\x07'); }
            Some('b') => { chars.next(); out.push('\x08'); }
            Some('f') => { chars.next(); out.push('\x0c'); }
            Some('v') => { chars.next(); out.push('\x0b'); }
            Some('e' | 'E') => { chars.next(); out.push('\x1b'); }
            Some('\\') => { chars.next(); out.push('\\'); }
            Some('\'') => { chars.next(); out.push('\''); }
            Some('"') => { chars.next(); out.push('"'); }
            // `\nnn` — one to three octal digits (C119). `\0` alone still
            // yields NUL as the degenerate one-digit case.
            Some('0'..='7') => {
                let n = take_number(&mut chars, 3, 8).unwrap_or(0);
                out.push(char::from_u32(n).unwrap_or('\u{fffd}'));
            }
            // `\xHH` — one or two hex digits (C119).
            Some('x') => {
                chars.next();
                match take_number(&mut chars, 2, 16) {
                    Some(n) => out.push(char::from_u32(n).unwrap_or('\u{fffd}')),
                    None => out.push_str("\\x"),
                }
            }
            // `\uXXXX` / `\UXXXXXXXX` — up to four/eight hex digits of a
            // Unicode scalar value (C119).
            Some(u @ ('u' | 'U')) => {
                chars.next();
                match take_number(&mut chars, if u == 'u' { 4 } else { 8 }, 16) {
                    Some(n) => out.push(char::from_u32(n).unwrap_or('\u{fffd}')),
                    None => {
                        out.push('\\');
                        out.push(u);
                    }
                }
            }
            // `\cX` — the control character for X (C119): X's uppercase
            // form masked to the low five bits, bash's own rule.
            Some('c') => {
                chars.next();
                match chars.next() {
                    Some(x) => {
                        let x = if x == '\\' { chars.next().unwrap_or('\\') } else { x };
                        out.push(((x.to_ascii_uppercase() as u8) & 0x1f) as char);
                    }
                    None => out.push_str("\\c"),
                }
            }
            Some(other) => {
                chars.next();
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// `${v@a}` (C60): the variable's attribute letters — `a`/`A` for the
/// array kinds, plus C43/C45's declared attributes (`i`, `l`, `u`, `r`),
/// in bash's own letter set.
fn attr_letters(name: &str) -> String {
    let mut out = String::new();
    if crate::vars::is_indexed_array(name) {
        out.push('a');
    }
    if crate::vars::is_assoc(name) {
        out.push('A');
    }
    let attrs = crate::vars::attrs_of(name);
    if attrs.integer {
        out.push('i');
    }
    if attrs.lower {
        out.push('l');
    }
    if attrs.readonly {
        out.push('r');
    }
    if attrs.upper {
        out.push('u');
    }
    if crate::vars::is_exported(name) {
        out.push('x'); // last, matching bash's `declare -irx`-style order
    }
    out
}

/// `${v@A}` (C60): an assignment (or `declare` command) that would
/// recreate the variable — `name='value'` when attribute-free, a
/// `declare -flags` form otherwise (arrays use the modern element-list
/// format, a documented divergence from older bash's odd scalarized
/// array output).
fn reconstruct_assignment(name: &str) -> String {
    let flags = attr_letters(name);
    if crate::vars::is_indexed_array(name) || crate::vars::is_assoc(name) {
        let elems: Vec<String> = if crate::vars::is_assoc(name) {
            crate::vars::assoc_keys(name)
                .into_iter()
                .map(|k| {
                    let v = crate::vars::assoc_get(name, &k).unwrap_or_default();
                    format!("[{k}]={}", shell_quote(&v))
                })
                .collect()
        } else {
            crate::vars::array_indices(name)
                .into_iter()
                .map(|i| format!("[{i}]={}", shell_quote(&crate::vars::array_get(name, i).unwrap_or_default())))
                .collect()
        };
        return format!("declare -{flags} {name}=({})", elems.join(" "));
    }
    let value = shell_quote(&crate::vars::get(name).unwrap_or_default());
    if flags.is_empty() {
        format!("{name}={value}")
    } else {
        format!("declare -{flags} {name}={value}")
    }
}

/// Expand a default/alternate-family word (`${v:-word}`'s `word`) with
/// quote *removal* (C76): `'…'` spans are literal, `"…"` spans expand
/// without their quotes, a backslash escapes the next character, and bare
/// text gets ordinary `$`-expansion. The quotes used to leak into the
/// output verbatim. (When the whole `${…}` is unquoted, the result still
/// splits as one expansion — inner quoting doesn't suppress field
/// splitting here, a documented narrowing; the ubiquitous outer-quoted
/// `"${v:-…}"` form is exact.)
fn expand_braced_word(src: &str, unquoted: bool) -> Result<String, String> {
    let mut out = String::new();
    let mut pending = String::new();
    let mut chars = src.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            // Inside a double-quoted `${…}`, a single quote is an
            // ordinary literal character (bash: `"${v:-'lit $x'}"` keeps
            // the quotes and expands `$x`); only an unquoted `${…}` treats
            // it as quoting.
            '\'' if !unquoted => pending.push('\''),
            '\'' => {
                out.push_str(&expand_dollars(&std::mem::take(&mut pending))?);
                for qc in chars.by_ref() {
                    if qc == '\'' {
                        break;
                    }
                    out.push(qc);
                }
            }
            '"' => {
                out.push_str(&expand_dollars(&std::mem::take(&mut pending))?);
                let mut inner = String::new();
                let mut closed = false;
                while let Some(qc) = chars.next() {
                    match qc {
                        '"' => {
                            closed = true;
                            break;
                        }
                        '\\' => match chars.next() {
                            Some(n @ ('"' | '\\' | '`')) => inner.push(n),
                            Some(n) => {
                                inner.push('\\');
                                inner.push(n);
                            }
                            None => break,
                        },
                        _ => inner.push(qc),
                    }
                }
                let _ = closed;
                out.push_str(&expand_dollars(&inner)?);
            }
            '\\' => {
                out.push_str(&expand_dollars(&std::mem::take(&mut pending))?);
                if let Some(n) = chars.next() {
                    out.push(n);
                }
            }
            _ => pending.push(c),
        }
    }
    out.push_str(&expand_dollars(&pending)?);
    Ok(out)
}

/// One `declare -p` output line (C96), in bash's own format exactly:
/// double-quoted values (`declare -- x="5"` — unlike `${v@A}`, which
/// single-quotes), `--` for a flagless scalar, and the assoc form's
/// trailing space inside the parens (`([k]="1" )`).
pub(crate) fn declare_p_line(name: &str) -> String {
    fn dq(v: &str) -> String {
        let mut out = String::from("\"");
        for c in v.chars() {
            if matches!(c, '"' | '\\' | '$' | '`') {
                out.push('\\');
            }
            out.push(c);
        }
        out.push('"');
        out
    }
    let letters = attr_letters(name);
    let flags = if letters.is_empty() { "--".to_string() } else { format!("-{letters}") };
    if crate::vars::is_assoc(name) {
        let elems: String = crate::vars::assoc_keys(name)
            .into_iter()
            .map(|k| format!("[{k}]={} ", dq(&crate::vars::assoc_get(name, &k).unwrap_or_default())))
            .collect();
        return format!("declare {flags} {name}=({elems})");
    }
    if crate::vars::is_indexed_array(name) {
        let elems: Vec<String> = crate::vars::array_indices(name)
            .into_iter()
            .map(|i| format!("[{i}]={}", dq(&crate::vars::array_get(name, i).unwrap_or_default())))
            .collect();
        return format!("declare {flags} {name}=({})", elems.join(" "));
    }
    format!("declare {flags} {name}={}", dq(&crate::vars::get(name).unwrap_or_default()))
}

/// Whether `rest` (the text after the variable name inside `${...}`)
/// starts one of C59's string-transformation operators. A `:` only
/// counts when what follows can't start the `:-`/`:=`/`:+`/`:?` default
/// family (`${v: -3}`'s mandatory space is exactly bash's own
/// disambiguation).
fn is_transform_op(rest: &str) -> bool {
    match rest.chars().next() {
        Some('/' | '^' | ',') => true,
        Some(':') => !matches!(rest[1..].chars().next(), None | Some('-' | '=' | '+' | '?')),
        _ => false,
    }
}

/// Apply one C59 transformation operator (`rest`, including its leading
/// operator character) to `value`. Semantics verified against bash —
/// see each branch.
fn string_transform(value: &str, rest: &str) -> Result<String, String> {
    // `${v/pat/repl}` family: `//` all, `/#` anchored prefix, `/%`
    // anchored suffix, plain `/` first occurrence; a missing `/repl`
    // deletes. The pattern/replacement split is the first *unescaped*
    // `/` (so `${v/\//_}` replaces a literal slash).
    if let Some(spec) = rest.strip_prefix('/') {
        let (mode, spec) = if let Some(s) = spec.strip_prefix('/') {
            ('A', s)
        } else if let Some(s) = spec.strip_prefix('#') {
            ('P', s)
        } else if let Some(s) = spec.strip_prefix('%') {
            ('S', s)
        } else {
            ('F', spec)
        };
        let mut split = None;
        let mut skip = false;
        for (i, c) in spec.char_indices() {
            if skip {
                skip = false;
                continue;
            }
            match c {
                '\\' => skip = true,
                '/' => {
                    split = Some(i);
                    break;
                }
                _ => {}
            }
        }
        let (pat_src, repl_src) = match split {
            Some(i) => (&spec[..i], &spec[i + 1..]),
            None => (spec, ""),
        };
        let pattern = expand_dollars(pat_src)?;
        let replacement = expand_dollars(repl_src)?;
        return Ok(replace_pattern(value, &pattern, &replacement, mode));
        // (`&` in the replacement is resolved inside `replace_pattern`,
        // gated on the `patsub_replacement` shopt — bash 5.2.)
    }
    // Case conversion: `^`/`^^` upper, `,`/`,,` lower; the doubled form
    // converts every character, the single form just the first. An
    // optional trailing pattern restricts which characters convert
    // (`${v^^[a-f]}`), defaulting to `?` (any).
    for (op, upper, all) in [("^^", true, true), ("^", true, false), (",,", false, true), (",", false, false)] {
        if let Some(pat_src) = rest.strip_prefix(op) {
            let pattern = if pat_src.is_empty() { "?".to_string() } else { expand_dollars(pat_src)? };
            return Ok(case_convert(value, &pattern, upper, all));
        }
    }
    // Substring: `${v:offset}` / `${v:offset:length}` — both sides are
    // full arithmetic expressions. Negative offset counts from the end
    // (out of range → empty, matching bash); negative length means "up
    // to that many characters before the end", and one that lands before
    // the offset is an error (bash: "substring expression < 0").
    if let Some(spec) = rest.strip_prefix(':') {
        let (off_src, len_src) = match spec.find(':') {
            Some(i) => (&spec[..i], Some(&spec[i + 1..])),
            None => (spec, None),
        };
        let off = crate::arith::eval(&expand_dollars(off_src)?)?;
        let chars: Vec<char> = value.chars().collect();
        let n = chars.len() as i64;
        let start = if off < 0 { (n + off).max(0).min(n) } else { off.min(n) };
        let end = match len_src {
            None => n,
            Some(src) => {
                let len = crate::arith::eval(&expand_dollars(src)?)?;
                if len < 0 {
                    let end = n + len;
                    if end < start {
                        return Err(format!("{len}: substring expression < 0"));
                    }
                    end
                } else {
                    (start + len).min(n)
                }
            }
        };
        // A negative offset past the front of the string is empty in
        // bash (`${v: -10}` of `abc` → ``), not the whole string.
        if off < 0 && n + off < 0 {
            return Ok(String::new());
        }
        return Ok(chars[start as usize..end.max(start) as usize].iter().collect());
    }
    Err(format!("bad substitution: {rest}"))
}

/// `${v/pat/repl}`'s engine: replace the longest match of a glob
/// `pattern` at the earliest position (`F`), every non-overlapping such
/// match (`A`), or an anchored prefix (`P`)/suffix (`S`) match.
fn replace_pattern(value: &str, pattern: &str, replacement: &str, mode: char) -> String {
    let chars: Vec<char> = value.chars().collect();
    let n = chars.len();
    let matches_range = |from: usize, to: usize| {
        let s: String = chars[from..to].iter().collect();
        crate::glob::match_component(pattern, &s)
    };
    // `patsub_replacement` (bash 5.2, on by default): an unescaped `&`
    // in the replacement stands for the matched text; `\&` is a
    // literal ampersand.
    let expand_amp = crate::vars::shopt("patsub_replacement") && replacement.contains('&');
    let render = |matched: &str| -> String {
        if !expand_amp {
            return replacement.to_string();
        }
        let mut out = String::new();
        let mut rc = replacement.chars().peekable();
        while let Some(c) = rc.next() {
            match c {
                '\\' if rc.peek() == Some(&'&') => {
                    rc.next();
                    out.push('&');
                }
                '&' => out.push_str(matched),
                _ => out.push(c),
            }
        }
        out
    };
    match mode {
        'P' => {
            for j in (0..=n).rev() {
                if matches_range(0, j) {
                    let matched: String = chars[..j].iter().collect();
                    let rest: String = chars[j..].iter().collect();
                    return format!("{}{rest}", render(&matched));
                }
            }
            value.to_string()
        }
        'S' => {
            for i in 0..=n {
                if matches_range(i, n) {
                    let head: String = chars[..i].iter().collect();
                    let matched: String = chars[i..].iter().collect();
                    return format!("{head}{}", render(&matched));
                }
            }
            value.to_string()
        }
        _ => {
            let mut out = String::new();
            let mut i = 0;
            let mut replaced = false;
            while i < n {
                let hit = (!replaced || mode == 'A')
                    .then(|| (i..=n).rev().find(|&j| matches_range(i, j)))
                    .flatten();
                match hit {
                    Some(j) => {
                        let matched: String = chars[i..j].iter().collect();
                        out.push_str(&render(&matched));
                        replaced = true;
                        if j == i {
                            out.push(chars[i]);
                            i += 1;
                        } else {
                            i = j;
                        }
                    }
                    None => {
                        out.push(chars[i]);
                        i += 1;
                    }
                }
            }
            // A pattern that only matches the empty string at the very
            // end (or an empty value with a match-empty pattern).
            if n == 0 && (!replaced || mode == 'A') && matches_range(0, 0) {
                out.push_str(&render(""));
            }
            out
        }
    }
}

/// `${v^}`/`${v^^}`/`${v,}`/`${v,,}`: case-convert the first (`all ==
/// false`) or every (`all == true`) character whose single-character
/// glob `pattern` matches it.
fn case_convert(value: &str, pattern: &str, upper: bool, all: bool) -> String {
    let mut out = String::new();
    for (i, c) in value.chars().enumerate() {
        let eligible = (all || i == 0) && crate::glob::match_component(pattern, &c.to_string());
        if eligible {
            if upper {
                out.extend(c.to_uppercase());
            } else {
                out.extend(c.to_lowercase());
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// `${var#pattern}` (shortest) / `${var##pattern}` (longest, `greedy`): strip
/// a matching prefix. Tries prefixes of increasing length (shortest first) or
/// decreasing length (longest first) against `pattern` as a whole — a glob
/// pattern, via the same matcher `case` patterns use — and removes the first
/// one that fully matches. No match: the value is returned unchanged.
fn strip_prefix_pattern(value: &str, pattern: &str, greedy: bool) -> String {
    let chars: Vec<char> = value.chars().collect();
    let lens: Box<dyn Iterator<Item = usize>> = if greedy {
        Box::new((0..=chars.len()).rev())
    } else {
        Box::new(0..=chars.len())
    };
    for l in lens {
        let prefix: String = chars[..l].iter().collect();
        if crate::glob::match_component(pattern, &prefix) {
            return chars[l..].iter().collect();
        }
    }
    value.to_string()
}

/// `${var%pattern}` (shortest) / `${var%%pattern}` (longest, `greedy`): strip
/// a matching suffix — the mirror image of [`strip_prefix_pattern`].
fn strip_suffix_pattern(value: &str, pattern: &str, greedy: bool) -> String {
    let chars: Vec<char> = value.chars().collect();
    let starts: Box<dyn Iterator<Item = usize>> = if greedy {
        Box::new(0..=chars.len())
    } else {
        Box::new((0..=chars.len()).rev())
    };
    for start in starts {
        let suffix: String = chars[start..].iter().collect();
        if crate::glob::match_component(pattern, &suffix) {
            return chars[..start].iter().collect();
        }
    }
    value.to_string()
}

/// `~`'s expansion target. Checks `vars::get("HOME")` first — verified
/// directly that real bash's own `~` *does* follow a plain (even
/// unexported) `HOME=/custom` reassignment — falling back to the real
/// environment variable(s) only when `HOME` isn't set in `vars` at all.
/// That fallback is a deliberate exception to the "no `std::env` fallback"
/// rule `var_raw` and everything else in this file now follows (C36/C40):
/// verified directly that unlike an ordinary variable, real bash's `~`
/// keeps resolving even after `unset HOME` (falling back to its own
/// OS-level notion of the user's home directory) rather than breaking —
/// this fallback approximates that with the value from process startup,
/// not a fresh OS lookup, an accepted simplification.
fn home_dir() -> Option<String> {
    crate::vars::get("HOME")
        .or_else(|| std::env::var("HOME").ok())
        .or_else(|| std::env::var("USERPROFILE").ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(input: &str) -> Vec<String> {
        expand_cmd(input).argv
    }

    fn expand_cmd(input: &str) -> Command {
        let list = parser::parse(input).unwrap();
        let pipeline = expand(&list.jobs[0].list.first).unwrap();
        match &pipeline.commands[0] {
            crate::exec::Stage::Simple(cmd) => cmd.clone(),
            crate::exec::Stage::Compound(_) => panic!("expected a simple command"),
        }
    }

    #[test]
    fn variable_tilde_and_quoting() {
        // `vars::set`, not `std::env::set_var`: `var_raw`/`home_dir` read
        // through `vars` only now (C36/C40 — real env vars are seeded into
        // it once at startup, not consulted directly on every read), and
        // `vars`'s own thread-local storage means this needs no `unsafe`
        // process-global mutation confined to a single test either.
        crate::vars::set("RUSH_X", "hello world");
        crate::vars::set("HOME", "/home/rush");
        crate::vars::unset("RUSH_UNSET");

        // Unquoted $VAR / ${VAR} word-split on whitespace; quotes suppress that.
        assert_eq!(one("echo $RUSH_X"), vec!["echo", "hello", "world"]);
        assert_eq!(one("echo ${RUSH_X}"), vec!["echo", "hello", "world"]);
        assert_eq!(one("echo \"$RUSH_X\""), vec!["echo", "hello world"]);

        // Single quotes are literal.
        assert_eq!(one("echo '$RUSH_X'"), vec!["echo", "$RUSH_X"]);

        // Unset → empty. Bare empty drops out; quoted empty is kept.
        assert_eq!(one("echo $RUSH_UNSET done"), vec!["echo", "done"]);
        assert_eq!(one("echo \"$RUSH_UNSET\" done"), vec!["echo", "", "done"]);

        // Tilde at word start only, and joined with the rest of the word.
        assert_eq!(one("echo ~"), vec!["echo", "/home/rush"]);
        assert_eq!(one("echo ~/src"), vec!["echo", "/home/rush/src"]);
        assert_eq!(one("echo a~b"), vec!["echo", "a~b"]);

        // Adjacency: a literal joins the first split field.
        assert_eq!(one("echo pre$RUSH_X"), vec!["echo", "prehello", "world"]);
    }

    #[test]
    fn word_splitting() {
        crate::vars::set("RUSH_LIST", "a b c");
        assert_eq!(one("echo $RUSH_LIST"), vec!["echo", "a", "b", "c"]);

        // Leading/trailing and runs of whitespace collapse.
        crate::vars::set("RUSH_PAD", "  x   y  ");
        assert_eq!(one("echo $RUSH_PAD"), vec!["echo", "x", "y"]);

        // A field that splits away to nothing leaves no argument.
        crate::vars::set("RUSH_EMPTY", "");
        assert_eq!(one("echo a$RUSH_EMPTY b"), vec!["echo", "a", "b"]);

        // Command substitution splits the same way.
        crate::vars::set("RUSH_CS", "one two");
        assert_eq!(one("echo \"$RUSH_CS\""), vec!["echo", "one two"]);
    }

    #[test]
    fn custom_ifs_field_splitting() {
        // A non-whitespace `$IFS` character: each occurrence delimits a
        // field on its own, unlike whitespace's collapsing — `a,,b` is three
        // fields, not two with a merged gap.
        crate::vars::set("IFS", ",");
        crate::vars::set("RUSH_CSV", "a,,b,c");
        assert_eq!(one("echo $RUSH_CSV"), vec!["echo", "a", "", "b", "c"]);

        // A leading delimiter produces a leading empty field; a single
        // *trailing* one at the very end does not (matches real bash) — but
        // a repeated trailing one still leaves one behind.
        crate::vars::set("RUSH_CSV", ",a,");
        assert_eq!(one("echo $RUSH_CSV"), vec!["echo", "", "a"]);
        crate::vars::set("RUSH_CSV", "a,,");
        assert_eq!(one("echo $RUSH_CSV"), vec!["echo", "a", ""]);

        // Mixed whitespace + non-whitespace IFS: whitespace immediately
        // adjacent to a non-whitespace delimiter is absorbed into it rather
        // than adding its own extra boundary.
        crate::vars::set("IFS", " ,");
        crate::vars::set("RUSH_CSV", "a, b,, c");
        assert_eq!(one("echo $RUSH_CSV"), vec!["echo", "a", "b", "", "c"]);

        // `IFS=` (explicitly empty) disables field splitting entirely — the
        // whole expansion is one field, whitespace and all.
        crate::vars::set("IFS", "");
        crate::vars::set("RUSH_CSV", "a  b");
        assert_eq!(one("echo $RUSH_CSV"), vec!["echo", "a  b"]);

        crate::vars::unset("IFS");
    }

    #[test]
    fn star_join_honors_ifs_first_char() {
        crate::vars::set_args("rush".to_string(), vec!["a".to_string(), "b".to_string(), "c".to_string()]);

        // Unset IFS: `$*`/`${*}` join with a space, same as always.
        crate::vars::unset("IFS");
        assert_eq!(one("echo \"$*\""), vec!["echo", "a b c"]);
        assert_eq!(one("echo \"${*}\""), vec!["echo", "a b c"]);

        // Custom IFS: joined with its *first* character, not a literal space.
        crate::vars::set("IFS", ":");
        assert_eq!(one("echo \"$*\""), vec!["echo", "a:b:c"]);
        assert_eq!(one("echo \"${*}\""), vec!["echo", "a:b:c"]);

        // `"x$@y"` (C77): the prefix attaches to the first parameter and
        // the suffix to the last, each middle one its own word —
        // unaffected by IFS (verified against bash).
        assert_eq!(one("echo \"x$@y\""), vec!["echo", "xa", "b", "cy"]);

        crate::vars::unset("IFS");
        crate::vars::set_args("rush".to_string(), Vec::new());
    }

    #[test]
    fn lone_dollar_is_literal() {
        assert_eq!(one("echo $"), vec!["echo", "$"]);
        assert_eq!(one("echo a$ b"), vec!["echo", "a$", "b"]);
    }

    #[test]
    fn last_status_expands() {
        crate::vars::set_last_status(42);
        assert_eq!(one("echo $?"), vec!["echo", "42"]);
        crate::vars::set_last_status(0);
        assert_eq!(one("echo code=$?"), vec!["echo", "code=0"]);
    }

    #[test]
    fn assignments_split_from_argv() {
        use crate::vars::{AssignOp, AssignValue};
        let scalar = |v: &str| AssignOp::Set(AssignValue::Scalar(v.to_string()));

        let c = expand_cmd("FOO=bar");
        assert!(c.argv.is_empty());
        assert_eq!(c.assignments, vec![("FOO".to_string(), scalar("bar"))]);

        let c = expand_cmd("A=1 B=2 echo hi");
        assert_eq!(c.argv, vec!["echo", "hi"]);
        assert_eq!(c.assignments, vec![("A".to_string(), scalar("1")), ("B".to_string(), scalar("2"))]);
    }

    #[test]
    fn not_an_assignment() {
        // After the command word, `NAME=value` is a plain argument.
        let c = expand_cmd("echo FOO=bar");
        assert!(c.assignments.is_empty());
        assert_eq!(c.argv, vec!["echo", "FOO=bar"]);

        // Invalid identifier → not an assignment.
        let c = expand_cmd("1FOO=bar");
        assert!(c.assignments.is_empty());
        assert_eq!(c.argv, vec!["1FOO=bar"]);
    }

    #[test]
    fn assignment_value_is_expanded() {
        use crate::vars::{AssignOp, AssignValue};
        crate::vars::set("RUSH_BASE", "/base");
        let c = expand_cmd("P=$RUSH_BASE/x");
        assert_eq!(c.assignments, vec![("P".to_string(), AssignOp::Set(AssignValue::Scalar("/base/x".to_string())))]);
    }

    #[test]
    fn shell_var_shadows_env() {
        crate::vars::set("RUSH_SHADOW", "shellval");
        assert_eq!(one("echo $RUSH_SHADOW"), vec!["echo", "shellval"]);
    }

    #[test]
    fn braced_default_and_alternate() {
        crate::vars::unset("RUSH_D");
        // :- substitutes a default for unset/empty (default may have spaces).
        assert_eq!(one("echo ${RUSH_D:-fallback}"), vec!["echo", "fallback"]);
        assert_eq!(one("echo \"${RUSH_D:-a b}\""), vec!["echo", "a b"]);

        crate::vars::set("RUSH_D", "set");
        assert_eq!(one("echo ${RUSH_D:-fallback}"), vec!["echo", "set"]);
        // :+ is the mirror: word only when set.
        assert_eq!(one("echo ${RUSH_D:+yes}"), vec!["echo", "yes"]);
        crate::vars::set("RUSH_D", "");
        assert_eq!(one("echo ${RUSH_D:+yes}"), vec!["echo"]); // empty → dropped
    }

    #[test]
    fn braced_assign_default_and_length() {
        crate::vars::unset("RUSH_A");
        // := assigns the default back to the variable...
        assert_eq!(one("echo ${RUSH_A:=created}"), vec!["echo", "created"]);
        // ...so a later reference sees it.
        assert_eq!(one("echo $RUSH_A"), vec!["echo", "created"]);
        // ${#name} is the length.
        assert_eq!(one("echo ${#RUSH_A}"), vec!["echo", "7"]);
    }

    #[test]
    fn braced_error_when_unset() {
        crate::vars::unset("RUSH_Q");
        let list = parser::parse("echo ${RUSH_Q:?missing}").unwrap();
        let err = expand(&list.jobs[0].list.first).unwrap_err();
        assert!(err.contains("missing"));
    }

    #[test]
    fn braced_prefix_and_suffix_pattern_removal() {
        crate::vars::set("RUSH_P", "/usr/local/bin/rush");
        // `#`/`%` remove the shortest match; `##`/`%%` the longest.
        assert_eq!(one("echo ${RUSH_P#*/}"), vec!["echo", "usr/local/bin/rush"]);
        assert_eq!(one("echo ${RUSH_P##*/}"), vec!["echo", "rush"]);

        crate::vars::set("RUSH_P", "archive.tar.gz");
        assert_eq!(one("echo ${RUSH_P%.*}"), vec!["echo", "archive.tar"]);
        assert_eq!(one("echo ${RUSH_P%%.*}"), vec!["echo", "archive"]);

        // No match: the value is returned unchanged.
        crate::vars::set("RUSH_P", "hello");
        assert_eq!(one("echo ${RUSH_P#foo}"), vec!["echo", "hello"]);

        // `*` can match zero characters, so the shortest-match forms are a
        // no-op while the longest-match forms consume the whole value.
        // Quoted so the brackets can't be mistaken for a glob character class.
        assert_eq!(one("echo \"[${RUSH_P#*}]\""), vec!["echo", "[hello]"]);
        assert_eq!(one("echo \"[${RUSH_P##*}]\""), vec!["echo", "[]"]);

        // Unset: empty string in, empty string out.
        crate::vars::unset("RUSH_P");
        assert_eq!(one("echo \"[${RUSH_P#foo}]\""), vec!["echo", "[]"]);
    }

    // Globbing tests run from the crate root against stable repo fixtures.
    #[test]
    fn glob_expands_unquoted_pattern() {
        let mut got = one("ls Cargo.*");
        got.sort();
        assert_eq!(got, vec!["Cargo.lock", "Cargo.toml", "ls"]);
    }

    #[test]
    fn quoted_pattern_is_literal() {
        // The `*` came from a quoted part, so it must not glob.
        assert_eq!(one("ls \"Cargo.*\""), vec!["ls", "Cargo.*"]);
    }

    #[test]
    fn unmatched_glob_stays_literal() {
        assert_eq!(one("ls no-such-*.zzz"), vec!["ls", "no-such-*.zzz"]);
    }
}

