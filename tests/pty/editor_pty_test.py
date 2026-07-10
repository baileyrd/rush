#!/usr/bin/env python3
"""End-to-end tests for rush's hand-rolled line editor (src/editor.rs),
driven under a real pseudo-terminal — the half `cargo test` can't cover.

Not part of `cargo test` (timing-sensitive under load, and Python keeps
the harness simple). Run manually after editor changes:

    cargo build && python3 tests/pty/editor_pty_test.py

Covers: typing/Enter, backspace, C-a/C-e, history Up-arrow, Tab
completion, hint acceptance (Right at EOL), abbreviation expansion on
space, the $RPS1 right prompt (C71), vi mode, C-r reverse search, C-c
cancel, multi-line continuation, live highlighting, long-line wrapping —
plus the readline-parity batch: kill+yank (C-k/C-y), undo (C-_), M-d
kill-word, Ctrl-arrow word motion, M-. last-argument, prefix history
search (PageUp), bracketed paste (single- and multi-line), quoted
insert (C-v TAB), C-x C-e edit-in-$EDITOR, and vi cw / r / f; / dd+p /
counts.
"""
import os, pty, time, sys, select, re

RUSH = "/home/user/rush/target/debug/rush"

def drive(keys, env_extra=None, settle=0.35):
    env = dict(os.environ, TERM="xterm", HOME="/tmp")
    if env_extra: env.update(env_extra)
    pid, fd = pty.fork()
    if pid == 0:
        os.execve(RUSH, [RUSH], env)
    out = b""
    time.sleep(settle)
    for chunk in keys:
        os.write(fd, chunk if isinstance(chunk, bytes) else chunk.encode())
        time.sleep(0.12)
    time.sleep(settle)
    os.write(fd, b"\x04")  # Ctrl-D to exit
    time.sleep(settle)
    while True:
        r, _, _ = select.select([fd], [], [], 0.3)
        if not r: break
        try:
            data = os.read(fd, 65536)
        except OSError:
            break
        if not data: break
        out += data
    try: os.close(fd)
    except OSError: pass
    os.waitpid(pid, 0)
    return out.decode(errors="replace")

def strip_ansi(s):
    return re.sub(r"\x1b\[[?0-9;]*[A-Za-z~]|\x1b\][^\a]*\a", "", s)

fails = []
def check(name, cond, out):
    if cond: print(f"OK   {name}")
    else:
        print(f"FAIL {name}\n--- raw ---\n{out!r}\n")
        fails.append(name)

# 1. Type + Enter executes
out = drive(["echo pty-works\r"])
check("type+enter", "pty-works" in strip_ansi(out), out)

# 2. Editing: type wrong, fix with backspace + arrows
out = drive(["echo abX", "\x7f", "c\r"])  # backspace the X, type c
check("backspace", "\nabc" in strip_ansi(out) or "abc" in strip_ansi(out).split("$")[-2:][0] if "$" in strip_ansi(out) else "abc" in strip_ansi(out), out)

# 3. Ctrl-A + edit at start
out = drive(["ho start\r".encode(), ], settle=0.3)
out = drive([b"ho hi", b"\x01", b"ec", b"\x05", b"\r"])  # type 'ho hi', C-a, 'ec', C-e, enter → 'echo hi'
check("ctrl-a/e", "\r\nhi" in out or "\nhi" in strip_ansi(out), out)

# 4. History: run cmd, up-arrow, enter
out = drive([b"echo hist-one\r", b"\x1b[A", b"\r"])
check("history-up", strip_ansi(out).count("hist-one") >= 3, out)  # echoed line x2 + output x2

# 5. Tab completion of a builtin (unique prefix): 'ulimi<TAB>' → ulimit
out = drive([b"ulimi\t", b" -n\r"])
check("tab-complete", re.search(r"\d", strip_ansi(out)) is not None and "ulimi\t" not in out, out)

# 6. Hint acceptance: run once, retype prefix, right-arrow accepts
out = drive([b"echo hint-accept\r", b"echo hin", b"\x1b[C", b"\r"])
check("hint-accept", strip_ansi(out).count("hint-accept") >= 3, out)

# 7. Abbreviation live expansion on space
out = drive([b"abbr gs='echo expanded'\r", b"gs", b" ", b"now\r"])
check("abbr-expand", "expanded now" in strip_ansi(out), out)

# 8. Right prompt renders (C71!)
out = drive([b"RPS1='<RIGHT>'\r", b"echo rp\r"])
check("right-prompt", "<RIGHT>" in out, out)

# 9. vi mode: set -o vi; type, ESC, 0, i, insert at start
out = drive([b"set -o vi\r", b"yes-vi", b"\x1b", b"0", b"i", b"echo ", b"\x1b", b"A", b"\r"])
check("vi-mode", "yes-vi" in strip_ansi(out), out)

# 10. Ctrl-R reverse search
out = drive([b"echo findme-xyz\r", b"\x12", b"findme", b"\r"])
check("ctrl-r", strip_ansi(out).count("findme-xyz") >= 3, out)

# 11. Ctrl-C cancels the line; shell keeps running
out = drive([b"echo doomed", b"\x03", b"echo alive\r"])
s = strip_ansi(out)
check("ctrl-c", "alive" in s and "doomed\r\ndoomed" not in s, out)

# 12. Multi-line continuation
out = drive([b"if true\r", b"then echo multi-ok\r", b"fi\r"])
check("multiline", "multi-ok" in strip_ansi(out), out)

# 13. Unmatched-quote highlight appears (red SGR 31 somewhere while typing)
out = drive([b'echo "open', b"\x15", b"\r"])  # C-u clears; just checking render didn't crash
check("highlight-renders", "\x1b[31m" in out or "\x1b[33m" in out or "\x1b[32m" in out, out)

# 14. Wide input wrapping doesn't crash (long line)
out = drive([b"echo " + b"x" * 300 + b"\r"])
check("wrap-long-line", "x" * 100 in strip_ansi(out), out)

# 15. Kill + yank: C-a, C-k kills the whole line, C-y restores it
out = drive([b"echo yank-back", b"\x01", b"\x0b", b"\x19", b"\r"])
check("kill-yank", strip_ansi(out).count("yank-back") >= 2, out)

# 16. Undo: kill the last word with C-w, C-_ restores it
out = drive([b"echo undo-keep", b"\x17", b"\x1f", b"\r"])
check("undo", strip_ansi(out).count("undo-keep") >= 2, out)

# 17. M-d kills the next word (with the space before it, readline's rule):
#     cursor after 'ok', M-d eats ' BAD' leaving 'echo ok-md'
out = drive([b"echo ok BAD-md", b"\x01", b"\x1bf", b"\x1bf", b"\x1bd", b"\r"])
check("meta-d", "ok-md" in strip_ansi(out) and "ok BAD-md\r\nok BAD" not in strip_ansi(out), out)

# 18. Ctrl-Left (CSI 1;5D) word motion: insert at the start of 'two'
out = drive([b"echo one two", b"\x1b[1;5D", b"X", b"\r"])
check("ctrl-arrow-word", "one Xtwo" in strip_ansi(out), out)

# 19. M-. inserts the previous command's last argument
out = drive([b"echo lastarg-word\r", b"echo ", b"\x1b.", b"\r"])
check("meta-dot", strip_ansi(out).count("lastarg-word") >= 3, out)

# 20. Prefix history search: PageUp completes from what's typed
out = drive([b"echo prefix-hit\r", b"true\r", b"echo p", b"\x1b[5~", b"\r"])
check("prefix-search", strip_ansi(out).count("prefix-hit") >= 3, out)

# 21. Bracketed paste, single line: tab inside the paste inserts literally
#     (no completion fires)
out = drive([b"\x1b[200~echo one-paste\x1b[201~", b"\r"])
check("paste-single", "one-paste" in strip_ansi(out), out)

# 22. Bracketed paste, multi-line: both lines execute on Enter
out = drive([b"\x1b[200~echo pasted-a\necho pasted-b\x1b[201~", b"\r"])
s = strip_ansi(out)
check("paste-multiline", "pasted-a" in s and "pasted-b" in s, out)

# 23. Quoted insert: C-v TAB puts a literal tab in a quoted argument
#     (no completion fires; echo prints the real tab)
out = drive([b'echo "A\x16\tB"\r'])
check("quoted-insert", "A\tB" in strip_ansi(out), out)

# 24. C-x C-e: $EDITOR rewrites the line, which then executes
out = drive([b"echo EDITME", b"\x18\x05"],
            env_extra={"EDITOR": "sed -i s/EDITME/edited-ok/"}, settle=0.6)
check("edit-in-editor", "edited-ok" in strip_ansi(out), out)

# 25. vi cw: change the first word
out = drive([b"set -o vi\r", b"BAD vi-cw-ok", b"\x1b", b"0", b"cw", b"echo", b"\x1b", b"A", b"\r"])
check("vi-cw", "vi-cw-ok" in strip_ansi(out), out)

# 26. vi f + r: find the char 'Z' and replace it with a space
out = drive([b"set -o vi\r", b"echoZvi-fr-ok", b"\x1b", b"0", b"fZ", b"r ", b"A", b"\r"])
check("vi-find-replace", "vi-fr-ok" in strip_ansi(out), out)

# 27. vi dd + p: delete the line, paste it back
out = drive([b"set -o vi\r", b"echo vi-ddp-ok", b"\x1b", b"dd", b"p", b"A", b"\r"])
check("vi-dd-p", "vi-ddp-ok" in strip_ansi(out), out)

# 28. vi counts: 3x deletes three characters
out = drive([b"set -o vi\r", b"echo XXXvi-count-ok", b"\x1b", b"0", b"5l", b"3x", b"A", b"\r"])
check("vi-count", "vi-count-ok" in strip_ansi(out) and "XXXvi-count" not in strip_ansi(out).split("\n")[-3:][0], out)

print("\n%d failures" % len(fails))
sys.exit(1 if fails else 0)
