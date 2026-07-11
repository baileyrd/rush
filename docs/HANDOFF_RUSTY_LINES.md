# Handoff: changes needed in rusty_lines

Audience: whoever works on https://github.com/baileyrd/rusty_lines next.
Context: rush's 2026-07-11 review pass (C74–C130 in
`docs/CAPABILITY_GAPS.md`) closed 46 of 57 gaps inside rush itself. The
items below are the remainder that are blocked on — or best solved by —
rusty_lines API additions. rush pins rusty_lines by git rev in
`Cargo.toml`, so the flow is: land the API there, bump the pin, then
close the rush side.

rush drives the editor through the `Hooks` trait (`ShellHooks` in
`src/main.rs`) plus the public `Editor` methods; everything below
extends that surface. Ordered by leverage.

## 1. Key rebinding API (blocks C128 — `bind`/inputrc, the last L-effort interactive gap)

**Today:** the emacs and vi keymaps are compiled into rusty_lines with
no rebinding surface at all. rush users cannot remap a single key, set
readline-style variables, or bind a shell command to a keystroke — a
hard blocker for anyone with `bind -x` fzf integrations or muscle-memory
bindings.

**Needed, in priority order:**

1. **An action enum + a binding table.** Name the existing edit actions
   (`BeginningOfLine`, `KillLineForward`, `MenuComplete`,
   `HistorySearchBackward`, …) as a public `EditorAction` enum, and let
   the host rebind key sequences:

   ```rust
   pub fn bind(&mut self, keys: &str, action: EditorAction);   // "\C-f", "\ex", "\e[1;5C"
   pub fn unbind(&mut self, keys: &str);
   pub fn bindings(&self) -> impl Iterator<Item = (String, EditorAction)>; // for `bind -P`
   ```

   Key-spec syntax should accept readline's spellings (`\C-x`, `\M-f`,
   `\e[...`), since rush's `bind` builtin will pass user input through
   verbatim.

2. **Host-command bindings (`bind -x`).** A binding whose action is
   "hand the current line + cursor to the host, then redisplay":

   ```rust
   pub fn bind_host(&mut self, keys: &str, tag: String);
   // Hooks grows:
   fn host_binding(&mut self, tag: &str, line: &mut String, cursor: &mut usize) {}
   ```

   rush maps `tag` → shell command, runs it with `READLINE_LINE`/
   `READLINE_POINT` set, and writes them back — bash's exact contract.
   The editor only needs to suspend raw mode around the callback and
   repaint after.

3. **Readline-variable equivalents** as plain setters (some may already
   exist internally): `set_completion_ignore_case(bool)`,
   `set_show_all_if_ambiguous(bool)`, `set_menu_complete(bool)`,
   `set_bell_style(...)`. rush will surface these through `bind
   'set var value'` and/or shopt-style options.

**rush integration point:** a new `bind` builtin (needs the `Editor` handle
threaded to builtins the same way the history mirror works — see
`builtins::history_record` for the pattern rush already uses).

## 2. Terminal facilities: size, echo, read timeout (blocks C129, C130; improves `read -s`)

`term_sys` (`src/term_sys.rs`) already has everything needed —
`tcgetattr_stdin`, `apply_raw_flags`, `poll_stdin`,
`term_cols_stdout` — but the module is private. Three small public
wrappers unblock three rush items:

1. **`pub fn terminal_size() -> Option<(u16, u16)>`** (cols, rows) —
   rush C129: set/refresh `$COLUMNS`/`$LINES` at startup and after each
   foreground command (bash 5's `checkwinsize` default). rush's own
   `select` implementation reads `$COLUMNS` and currently always sees
   nothing. `term_cols_stdout` is half of this already; add rows.

2. **`pub fn with_echo_disabled<T>(f: impl FnOnce() -> T) -> io::Result<T>`**
   (or `set_echo(bool)` + RAII guard) — rush's `read -s` currently
   shells out to `stty -echo`/`stty echo` (documented stopgap in PR
   #112); a termios call is the real fix and removes the external
   dependency. Must restore echo on panic/early return (guard, not
   paired calls).

3. **A read deadline** — rush C130 (`$TMOUT` idle auto-logout, a real
   hardening requirement):

   ```rust
   pub fn read_line_timeout(&mut self, prompt: &str, rprompt: &str,
       hooks: &impl Hooks, timeout: Option<Duration>) -> io::Result<ReadResult>;
   // plus ReadResult::TimedOut
   ```

   `poll_stdin(ms)` already exists — the change is plumbing a deadline
   through the read loop. rush will print bash's "timed out waiting for
   input" and exit.

## 3. History timestamps (closes the C122 remainder — `$HISTTIMEFORMAT`)

**Today:** history entries are plain `Vec<String>`; the file format is
one line per entry. rush PR #115 wired `HISTFILE`/`HISTSIZE`/
`HISTCONTROL`/`HISTIGNORE`, incremental `append_history`, and a mirror
store for the `history` builtin — `HISTTIMEFORMAT` is the one knob left,
and it needs the file format.

**Needed:** bash's format — a `#<epoch>` comment line preceding each
entry:

```
#1752196610
echo hello
```

- Store `(Option<i64>, String)` per entry (or a parallel timestamp vec).
- `add_history_entry` stamps now; `load_history` parses `#<digits>`
  lines as timestamps (and tolerates files without them — both formats
  must round-trip); `save_history`/`append_history` emit them **only
  when asked** (bash writes timestamps only when `HISTTIMEFORMAT` is
  set, gate it on a `set_history_timestamps(bool)` toggle so existing
  plain files aren't rewritten into the new format behind the user's
  back).
- `pub fn history_timestamps(&self) -> &[Option<i64>]` (or fold into an
  entries accessor) so rush's `history` builtin can render
  `strftime(HISTTIMEFORMAT)` before each line (rush already has the
  strftime subset from `printf %(fmt)T`, PR #109).

## 4. Nice-to-have: in-place history replacement

After `history -c`/`-d`, rush currently rebuilds a whole new `Editor`
to resynchronize the editor's list with the builtin's mirror
(`src/main.rs`, the `history_reset_pending` block) — which silently
drops editor-internal state (kill ring, undo stacks). A
`pub fn replace_history(&mut self, entries: Vec<String>)` (resetting
`persisted` appropriately) would make the sync surgical. Low priority;
the rebuild works.

## Explicitly *not* needed from rusty_lines

- `read -e` (readline editing inside `read`): rush currently documents
  this as accepted-without-editing. If it's ever wanted, it falls out of
  item 2's building blocks plus a plain `read_line` call — no new API.
- `IGNOREEOF`: rush-side only (a counter in the REPL loop); listed here
  so nobody builds editor support for it.
- Programmable completion (C93): entirely rush-side — the existing
  `Hooks::complete` surface is sufficient; rush needs `complete`/
  `compgen` builtins and the `COMPREPLY` protocol on its side of the
  hook.
