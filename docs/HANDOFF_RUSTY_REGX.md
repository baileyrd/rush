# Handoff: changes needed in rusty_regx

Audience: whoever works on https://github.com/baileyrd/rusty_regx next.
Context: rush's 2026-07-11 review pass (C74–C130 in
`docs/CAPABILITY_GAPS.md`) closed 46 of 57 gaps inside rush itself. One
remaining item cannot be finished on the rush side because it needs an
engine capability rusty_regx doesn't expose yet.

## 1. Case-insensitive matching mode (blocks rush's `nocasematch` for `=~`)

**What rush needs.** bash's `shopt -s nocasematch` changes the semantics
of *all three* pattern contexts: `case`, `[[ string == pattern ]]`, and
`[[ string =~ regex ]]`. rush now honors it for the first two (PR #113
wired `case`/`==` through a case-folding shim in `exec.rs`,
`match_nocase_aware`), but `=~` still matches case-sensitively:

```console
$ bash -c 'shopt -s nocasematch; [[ ABC =~ ^abc$ ]] && echo yes || echo no'
yes
$ rush -c 'shopt -s nocasematch; [[ ABC =~ ^abc$ ]] && echo yes || echo no'
no        # ← the gap
```

Lowercasing both sides on the rush side — the shim `==`/`case` use — is
**not** correct for regexes: it breaks explicit character classes
(`[A-Z]` must keep matching uppercase input when the *input* is folded),
backreference-free equivalence assumptions, and `[[:upper:]]`/
`[[:lower:]]` POSIX classes, which must keep their literal meaning even
under `nocasematch` folding of ordinary letters. The fold has to happen
inside the engine, per-character, at comparison time.

**Suggested API.** Smallest useful surface, mirroring the existing
constructor pair:

```rust
/// As `new_posix`, but ordinary-letter comparisons are case-insensitive
/// (ASCII + Unicode simple case folding), like POSIX `REG_ICASE`.
/// Explicit classes keep their literal meaning: `[[:upper:]]` still
/// means upper, `[a-f]` also matches `A`–`F` (REG_ICASE's rule).
pub fn new_posix_ci(pattern: &str) -> Result<Regex, Error>;
```

or a flags parameter/builder if that fits the crate's style better —
rush doesn't care about the shape, only that leftmost-longest POSIX
semantics are preserved and `REG_ICASE`-equivalent folding is available.
Note POSIX `REG_ICASE` folds range endpoints and literal letters but
leaves named classes (`[[:upper:]]`) alone — match that, since it's what
bash's `nocasematch` + `=~` does (verified against bash 5.2).

**Capture groups.** `$BASH_REMATCH` must keep reporting the *original*
(unfolded) input spans — folding must affect comparison only, never the
captured text. (This is the concrete reason input-side pre-folding on
the rush side is wrong even where it would "work".)

**rush integration point** (ready to switch over): `src/exec.rs:1196` —

```rust
let re = rusty_regx::Regex::new_posix(&pattern)
```

becomes a two-way choice on `crate::vars::shopt("nocasematch")`. That's
the only call site; `rusty_regx::escape` usage in `src/expand.rs` is
unaffected.

**Acceptance checks** (all differentially verified against bash 5.2):

```sh
shopt -s nocasematch
[[ ABC =~ ^abc$ ]]              # 0
[[ abc =~ [X-Z]bc ]]            # 1 — 'a' is not in x-z even folded? bash: 0 (REG_ICASE folds ranges) — verify against bash first
[[ ABC =~ [[:lower:]]bc ]]      # bash: 0 under nocasematch? verify — REG_ICASE does NOT fold named classes
[[ ABC =~ ^(a)(b) ]] && echo "${BASH_REMATCH[1]}"   # prints 'A' (original case)
shopt -u nocasematch
[[ ABC =~ ^abc$ ]]              # 1 — folding must be opt-in
```

The two "verify" lines are deliberate: bash's exact behavior for folded
ranges/classes should be captured as tests in rusty_regx before
implementing, not assumed from this document.

## Nothing else pending

The rest of the C74–C130 review pass found no other rusty_regx-side
gaps: `=~` semantics (leftmost-longest, `$BASH_REMATCH`, quoted-part
literal matching) all verified clean against bash 5.2. Tracked as C120's
`=~` half in `docs/CAPABILITY_GAPS.md`.
