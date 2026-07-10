# rush's line editor

The hand-rolled line editor that used to live at `src/editor.rs` — and
this document's capability survey against GNU readline, libedit, zsh
ZLE, fish, ksh93, linenoise, replxx, rustyline, reedline, and
prompt_toolkit/PSReadLine — now live in the editor's own repository:

**<https://github.com/baileyrd/rusty_lines>**

Rush pulls it in as a git dependency and integrates through the crate's
`Hooks` trait (`ShellHooks` in `src/main.rs`); see the crate README for
the full feature matrix and the documented narrowings. Rush keeps the
end-to-end pty harness (`tests/pty/editor_pty_test.py`, 28 scenarios),
since it exercises the editor together with rush's completion,
highlighting, abbreviations, and `$RPS1` expansion.
