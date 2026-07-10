# Regex dependency analysis: what `regex = "1"` buys us, and what replacing it takes

Status: analysis / assessment (no decision made). Companion design doc lives in
the `rusty_regx` repo (`DESIGN.md`).

## 1. Where the dependency is used

The `regex` crate backs exactly **one shell feature**: the `[[ $s =~ pattern ]]`
conditional operator (capability C56). Every use site:

| Site | API used | Purpose |
|------|----------|---------|
| `src/exec.rs` (`"=~"` arm, ~line 1006) | `Regex::new`, `Regex::captures`, `Captures::get`, `Captures::len` | Compile the expanded pattern, run an unanchored search, populate `BASH_REMATCH[0..n]` (unmatched optional groups become empty strings; failed match unsets the array). |
| `src/expand.rs` (`expand_cond_regex`, ~line 1022) | `regex::escape` | Quoted/literal word parts of the `=~` RHS must match themselves, so they are regex-escaped before splicing into the live pattern. |

That is the **entire** API surface: `new`, `captures`, `get`, `len`, `escape`.
No `find_iter`, no replacement, no `RegexSet`, no byte-regex, no lazy statics.

The lexer (`lex_regex_word`, `src/lexer.rs`) and expander do the bash-specific
heavy lifting themselves (paren-balanced word lexing, quoted-part escaping,
`$var` splicing). The engine only ever sees a finished pattern string.

## 2. What the pattern language actually needs

Bash's `=~` is **POSIX ERE** (bash delegates to the platform's
`regcomp(REG_EXTENDED)`/`regexec`). The constructs a replacement engine must
support:

- Concatenation, alternation `|`, grouping `( )` — all groups capture (ERE has
  no non-capturing groups).
- Quantifiers `*`, `+`, `?`, and intervals `{m}`, `{m,}`, `{m,n}`. No lazy or
  possessive variants.
- Anchors `^`, `$`; the any-char `.`.
- Bracket expressions: negation `[^...]`, ranges `a-z`, literal `]` first
  (`[]a]`), trailing `-`, POSIX classes `[[:alpha:]]`, `[[:digit:]]`, etc.
- Backslash-escaped metacharacters (`\.`, `\(`, …).

Explicitly **not** needed: backreferences, lookaround, named groups, lazy
quantifiers, `\d`/`\w`-style Perl classes (not POSIX ERE), Unicode property
classes, replacement APIs. That excludes everything that makes a regex engine
hard-hard; what remains is a well-understood compiler + VM problem.

The existing C56 tests (`tests/exec_behavior.rs` ~2460–2495) exercise: classes,
`+`, `{4}`/`{2}` intervals, `^`, captures, optional groups, `$var` patterns,
quoted-literal semantics, escaped `.`, whitespace inside groups, and
invalid-pattern error handling (`(`, `[` → status 2, script continues).

## 3. The regex standards landscape (and where rush sits)

"Regex" is not one standard; the flavors below are the ones that matter for
placing bash and the `regex` crate relative to each other.

- **POSIX BRE (Basic Regular Expressions).** IEEE 1003.1; the default in
  `grep`/`sed`/`ed`. Grouping and intervals are *escaped* (`\(...\)`,
  `\{m,n\}`), classic BRE lacks `|`/`+`/`?`, and backreferences (`\1`) are
  included. Irrelevant to `=~` except as a contrast.
- **POSIX ERE (Extended Regular Expressions).** IEEE 1003.1; used by
  `grep -E`, `awk`, and **bash's `[[ =~ ]]`** (via `regcomp(REG_EXTENDED)`).
  Unescaped metacharacters, all groups capture, *no* backreferences or
  lookaround. POSIX additionally mandates **leftmost-longest** match
  semantics and bracket-expression features: `[[:alpha:]]` classes, collating
  symbols `[.ch.]`, equivalence classes `[=e=]`. **This is the target
  standard for a replacement engine.**
- **PCRE (Perl-Compatible Regular Expressions).** The de facto standard
  rather than a formal one: Perl syntax as codified by the PCRE/PCRE2
  library. Adds backreferences, lookaround, lazy/possessive quantifiers,
  non-capturing and named groups, `\d`/`\w`/`\b`, inline flags — with
  **leftmost-first** (first-alternative-wins) semantics. Most language
  runtimes (Python `re`, Ruby, PHP, `grep -P`) are PCRE-ish dialects.
- **ECMAScript regex.** The other formally specified flavor: ECMA-262
  precisely defines JavaScript's regex grammar and semantics (named groups,
  lookbehind, `\u{...}`); it is also `std::regex`'s default mode in C++.
- **UTS #18 (Unicode Technical Standard #18).** Not a syntax but a
  conformance standard for *Unicode support* in regex engines — property
  classes (`\p{Greek}`), case folding, etc. The `regex` crate targets UTS #18
  Level 1, which is a large part of why it carries the table-heavy machinery
  rush doesn't need.
- **The Rust `regex` crate dialect** (rush today) sits in between: Perl-ish
  syntax deliberately restricted to what can run in guaranteed linear time
  (no backreferences or lookaround), leftmost-first semantics, UTS #18
  Unicode.

The tension for rush in one line: bash speaks **POSIX ERE, leftmost-longest**;
the crate speaks a **PCRE-ish subset, leftmost-first**. They agree on the
everyday constructs (which is why the C56 tests pass) and diverge on
alternation submatching and Perl escapes — detailed in §6.

## 4. Cost of the current dependency

From `Cargo.lock`, the full tree is 8 packages. `regex` accounts for **5 of the
7 external ones**:

```
regex 1.13.0 → regex-automata 0.4.15 → regex-syntax 0.8.11, aho-corasick 1.1.4, memchr 2.8.0
```

- Combined, these crates are on the order of 100k+ lines of Rust — several
  times the size of rush itself (~16k lines of src) — compiled to support one
  operator.
- Clean-build compile time and binary size are dominated by this stack (it is
  effectively rush's only heavyweight dependency; the others are `libc` and
  `unicode-width`, both thin table/binding crates).
- Supply-chain exposure: 5 externally maintained crates vs. potentially 0.

## 5. What the crate gives us that is easy to underestimate

1. **Guaranteed linear-time matching.** A shell compiles *user/script-supplied*
   patterns; the regex crate's NFA/lazy-DFA design makes catastrophic
   backtracking (ReDoS) impossible. A naive backtracking replacement would
   reintroduce it — `[[ $x =~ (a+)+b ]]` hanging the shell is a real regression
   class. Any replacement must be a Thompson-construction/Pike-VM design, not a
   backtracker.
2. **Deterministic cross-platform behavior.** Real bash's `=~` behavior varies
   by libc (glibc vs. macOS regexec differ in corners). The regex crate — and
   equally a homegrown engine — gives rush *one* behavior everywhere. This is a
   wash between the two options, but worth naming: "match bash exactly" is not
   a fully-defined target.
3. **Years of fuzzing and edge-case hardening**, especially in bracket-expression
   parsing and interval handling. This is the main thing we would be
   re-earning with tests.

## 6. Known divergences from bash we'd have a chance to fix

- **Leftmost-first vs. leftmost-longest.** POSIX requires leftmost-*longest*
  alternation/submatch semantics; the regex crate implements Perl-style
  leftmost-*first*. `[[ ab =~ a|ab ]]` sets `BASH_REMATCH[0]=ab` in bash but
  `a` under the regex crate. This is the "its syntax isn't a byte-for-byte
  POSIX ERE match" caveat in `Cargo.toml`. A homegrown engine could implement
  POSIX-longest and get *closer* to bash than we are today (it is also the
  single hardest part of the project — see §7).
- **Perl escapes.** The regex crate accepts `\d`, `\w`, `\b` etc., which POSIX
  ERE does not define; scripts relying on rush-only patterns would be
  non-portable to bash. A strict-ERE homegrown parser would reject or
  literalize them, matching bash.

## 7. What rolling our own requires

Recommended architecture (detailed in `rusty_regx/DESIGN.md`): a classic
four-stage engine —

```
pattern &str → ERE parser → AST → bytecode compiler → Pike VM (NFA simulation with capture slots)
```

- **Parser** (~400–600 lines): full ERE grammar incl. bracket-expression corner
  cases and interval validation; structured errors (rush maps them to
  `invalid regex: …`, status 2).
- **Compiler** (~200–300 lines): AST → `Char/Class/Split/Jump/Save/Match`
  instructions; intervals expanded by repetition (with a sanity cap, e.g.
  `{,1000}`, mirroring the crate's size limits).
- **Pike VM** (~300–500 lines): breadth-first thread simulation with per-thread
  capture-slot vectors. Linear time in `pattern × input` by construction — no
  ReDoS. Unanchored search via an implicit `.*?` prefix loop.
- **Semantics decision:** ship leftmost-first initially (bit-for-bit with
  today's behavior → zero regression risk), then optionally add POSIX
  leftmost-longest submatch resolution as a phase-2 mode. Tagged-NFA/POSIX
  disambiguation is the one genuinely research-grade piece; everything else is
  textbook.
- **Unicode:** iterate `char`s over the already-UTF-8 `String` input, classes
  as codepoint ranges. Matches current behavior; no tables needed (POSIX
  classes on ASCII + `char::is_alphabetic`-style fallbacks).
- **`escape()`:** trivial, but must escape *exactly* the new engine's
  metacharacters — `expand_cond_regex` depends on it.
- **Testing (the real cost, ~50% of the effort):**
  - Port/extend the C56 suite.
  - Differential harness: random pattern/input generation checked against the
    `regex` crate (dev-dependency only) and against a real bash oracle.
  - Adversarial inputs: `(a+)+b`-style patterns must complete fast (linear-time
    proof-by-test), deep nesting, huge intervals, pathological brackets.

**Size/effort estimate:** ~1,500–2,500 lines of engine code plus a comparable
volume of tests. Realistically 2–4 focused weeks to reach "trustworthy",
spread over the phases above; the parser + VM matching (no captures) is a
weekend, captures and bracket-expression corners are the long tail.

Integration into rush is a ~10-line diff: swap `regex::Regex::new/captures/escape`
for `rusty_regx` equivalents in `exec.rs` and `expand.rs`, then delete the
dependency (Cargo.lock drops from 8 packages to 3 + rusty_regx).

## 8. Assessment

| | Keep `regex` | Roll `rusty_regx` |
|---|---|---|
| Dependency tree | 5 external crates | 0 (for this feature) |
| ReDoS safety | guaranteed | guaranteed *if* Pike VM (must not backtrack) |
| Bash fidelity | leftmost-first divergence, Perl escapes accepted | can reach strict ERE + optionally POSIX-longest |
| Compile time / binary size | dominates rush's build | negligible |
| Risk | ~none | correctness bugs in bracket/interval corners until test suite matures |
| Ongoing cost | version bumps | we own every bug report |

**Bottom line:** this is one of the rare cases where rolling your own is
defensible rather than hubris. The needed subset (POSIX ERE, captures, one
`captures()` call site) is small, closed, and spec-defined; the project already
hand-rolls comparable machinery (glob/extglob matching, a line editor, arith);
and the payoff is removing ~70% of the external dependency tree while
*improving* bash fidelity. The two non-negotiables for a replacement are
(1) Pike-VM/linear-time execution — a backtracking engine would be a security
regression — and (2) a differential test harness against bash and the regex
crate before the swap. If those feel too expensive, the status quo is cheap to
keep: the crate is stable, and the known divergence is a corner case.
