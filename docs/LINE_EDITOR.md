# rush's line editor — capability survey vs. the field

`src/editor.rs` is rush's hand-rolled line editor (it replaced `rustyline`;
see C71 in `CAPABILITY_GAPS.md` for why). This document is the follow-up
audit: a feature comparison against the line editors in wide use, and the
work that closed the gaps it found. Editors surveyed:

- **GNU readline** — bash's editor and the de-facto reference; also ksh93's
  emacs mode ancestor. Sets the baseline for the emacs keymap, the kill
  ring, undo, and the vi editing mode readline embeds.
- **libedit / editline** — the BSD-licensed readline workalike (used by
  dash builds with editing, macOS bash-replacement tooling); a strict
  subset of readline's surface.
- **zsh ZLE** — the most featureful of the shell-native editors: widgets,
  `$RPS1`, multi-line buffer editing, region highlighting.
- **fish** — autosuggestions, syntax highlighting, abbreviations, paginated
  completion, prefix-aware history on Up.
- **linenoise** (and linenoise-ng) — the minimal end of the spectrum:
  basic emacs keys, completion + hints callbacks, no kill ring/undo/vi.
- **replxx** — linenoise's featureful descendant (highlighting, hints,
  incremental search).
- **rustyline** — the Rust readline-alike rush used to embed.
- **reedline** — nushell's editor (right prompt, menus, undo, vi mode,
  bracketed paste).
- **prompt_toolkit / PSReadLine** — the Python and PowerShell equivalents,
  checked for anything the Unix-side editors miss (PSReadLine's
  predictive history ≈ fish autosuggestions).

## Feature matrix

"✓" = implemented in `src/editor.rs`; references are to the shells/editors
that established the behavior rush matches.

| Capability | Reference behavior | rush |
|---|---|---|
| Emacs basics: C-a/C-e/C-b/C-f, C-d, C-h, C-t, arrows, Home/End/Delete | readline, everywhere | ✓ |
| Kill ring: C-k, C-u, C-w, M-d, M-Backspace kill *into* a ring; C-y yanks; M-y rotates; consecutive kills grow one entry (append forward / prepend backward) | readline, ZLE, fish | ✓ (ring persists across lines, capped at 32) |
| Word flavors: M-b/M-f/M-d/M-Backspace use alphanumeric words, C-w whitespace words (unix-word-rubout) | readline's two word classes | ✓ |
| Ctrl-arrow / Alt-arrow word motion (`CSI 1;5C` etc.) | every modern terminal editor | ✓ |
| Undo: C-_ , C-x C-u (and C-z, fish-style); runs of self-insert undo as one unit | readline, ZLE, fish | ✓ (no redo — same as readline) |
| Transpose: C-t chars (two-before at EOL), M-t words | readline | ✓ |
| Case ops: M-u / M-l / M-c | readline, ZLE | ✓ |
| Insert last argument: M-. / M-_ , repeat cycles older entries | readline, ZLE | ✓ |
| Quoted insert: C-v / C-q; control chars render `^X`-style | readline | ✓ |
| Edit line in `$VISUAL`/`$EDITOR`: C-x C-e (emacs), `v` (vi normal); result executes | readline, ZLE, fish (Alt-e) | ✓ |
| History: Up/Down with draft preservation, C-p/C-n, M-< / M-> | readline | ✓ |
| Incremental search: C-r backward *and* C-s forward (IXON is off), direction switching mid-search | readline, ZLE | ✓ |
| Prefix history search | ZLE `history-beginning-search`, fish Up, PSReadLine | ✓ PageUp/PageDown, M-p/M-n |
| History hints (autosuggestions), Right/End accepts | fish, PSReadLine | ✓ (C33) |
| Syntax highlighting while typing | fish, ZLE plugins, replxx | ✓ (C68) |
| Tab completion: LCP insertion + columned candidate list | readline `CompletionType::List` | ✓ (C34/C69) |
| Abbreviation expansion on space | fish `abbr` | ✓ (C70) |
| Right-side prompt `$RPS1`, hidden when the line grows into it | zsh, fish, reedline | ✓ (C71) |
| Bracketed paste: paste arrives as one event — tabs/ESC insert literally, nothing executes until Enter; multi-line pastes keep their newlines (shown `⏎`) and run as a unit | readline 8.1+, ZLE, fish, reedline | ✓ (multi-line history entries stored bash-`cmdhist`-style, joined with `; `) |
| vi mode (`set -o vi`): counts; `d`/`c`/`y` operators over motions; `h l 0 ^ $ w W b B e E f F t T ; ,`; `x X D C s S Y r ~ p P u`; `i I a A`; `k`/`j` history; `cw`≡`ce` quirk; Esc backs the cursor up one | readline vi mode, ksh, ZLE `viins`/`vicmd` | ✓ |
| Wide chars + UTF-8 input assembly; ANSI-aware width math; soft-wrap repaint | all modern | ✓ |
| Multi-byte paste/quoted-insert safety (`^X` visualization keeps cursor math exact) | readline | ✓ |

## Deliberate narrowings

Checked against the same field and consciously not modeled — each is
either niche, terminal-hostile, or a different program's job:

- **Multi-line *buffer editing*** (zsh/fish/reedline edit a `\n`-separated
  buffer with per-line cursor movement). rush's buffer is one logical
  line; embedded newlines (from a paste or C-v C-j) render as `⏎` and
  execute correctly, but Up/Down navigate history, not buffer rows. The
  `> ` continuation prompt covers incremental multi-line entry, and
  C-x C-e hands real multi-line editing to `$EDITOR`.
- **Programmable keybindings** (readline's `bind`/`.inputrc`, ZLE's
  `zle -N` widgets, fish's `bind`). The keymap is fixed.
- **Keyboard macros** (readline C-x `(` … `)`), **numeric arguments in
  emacs mode** (M-digit; vi counts are supported), **mark/region**
  (C-@, C-x C-x), **redo** (readline has none either).
- **vi registers, `.` repeat, `/` history search** (C-r covers search from
  insert mode; the unnamed register is the kill ring).
- **Completion paging/menu-select** (fish's pager, ZLE menu-select):
  long candidate lists print unpaged.
- **Non-tty / non-Unix**: piped stdin gets a plain line read (as
  rustyline did); non-Unix builds get a buffered prompt-and-read.

## Verification

Pure helpers (word motions in all three flavors, vi find targets, kill
ring append/prepend, yank-pop rotation, undo, case ops, word transpose,
last-arg cycling, prefix search, control-char visualization, CSI decode)
are unit-tested in `src/editor.rs`. End-to-end behavior — including the
raw-mode escape-sequence handling no unit test can reach — is exercised
by `tests/pty/editor_pty_test.py` under a real pseudo-terminal: 28
scenarios covering every row of the matrix above.
