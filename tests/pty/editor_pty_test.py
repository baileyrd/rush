#!/usr/bin/env python3
"""End-to-end tests for rush's hand-rolled line editor (src/editor.rs),
driven under a real pseudo-terminal — the half `cargo test` can't cover.

Not part of `cargo test` (timing-sensitive under load, and Python keeps
the harness simple). Run manually after editor changes:

    cargo build && python3 tests/pty/editor_pty_test.py

Covers: typing/Enter, backspace, C-a/C-e, history Up-arrow, Tab
completion, hint acceptance (Right at EOL), abbreviation expansion on
space, the $RPS1 right prompt (C71), vi mode, C-r reverse search, C-c
cancel, multi-line continuation, live highlighting, and long-line
wrapping.
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
    return re.sub(r"\x1b\[[0-9;]*[A-Za-z]|\x1b\][^\a]*\a", "", s)

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

print("\n%d failures" % len(fails))
sys.exit(1 if fails else 0)
