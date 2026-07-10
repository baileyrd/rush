//! Hand-rolled line editor — rush's own replacement for `rustyline` (C71's
//! unblocking, and a deliberate roll-our-own decision in the spirit of the
//! hand-rolled lexer/parser/glob matcher). Feature-set audited against the
//! other line editors in the wild — GNU readline, zsh ZLE, fish, ksh93's
//! emacs/vi modes, libedit, linenoise, replxx, rustyline, reedline — see
//! docs/LINE_EDITOR.md for the survey and the documented narrowings.
//!
//! Layers, bottom to top:
//!   * raw terminal mode (termios) behind an RAII guard, so every exit path
//!     — including panics — restores the terminal; bracketed-paste mode
//!     behind a second guard, so a paste arrives as one event instead of a
//!     stream of fake keystrokes;
//!   * key decoding: UTF-8 assembly plus escape-sequence parsing (CSI/SS3
//!     with modifier parameters — Ctrl/Alt-arrows — Alt- chords, and the
//!     bracketed-paste envelope), with a short poll to tell a lone ESC
//!     from a sequence;
//!   * a render engine that repaints the whole edit region per keystroke:
//!     display-width math (via `unicode_width`, ANSI-aware), a readline-
//!     style `^X` visualization for control characters in the buffer,
//!     soft-wrap row accounting, forced wraps at exact column boundaries
//!     (avoiding the delayed-wrap ambiguity), syntax highlighting (C68),
//!     the dimmed history hint (C33), and — newly possible now that rush
//!     owns this layer — the right-side prompt `$RPS1` (C71), shown while
//!     the first row has room for it;
//!   * keymaps: the emacs set (kill ring with yank/yank-pop, undo,
//!     word-wise motion/kill/case/transpose, insert-last-argument,
//!     quoted-insert, edit-in-$EDITOR) by default, plus a vi mode
//!     (`set -o vi`, C73) with counts, the `d`/`c`/`y` operators over
//!     motions, `f F t T ; ,` character finds, and the standard normal-
//!     mode edits — checked live on every `read_line`, so switching needs
//!     no editor rebuild at all;
//!   * history: in-memory with consecutive-dedup (multi-line entries
//!     stored bash-style with `; ` joining), file persistence (tolerating
//!     rustyline's old `#V2` header on load), Up/Down navigation with
//!     draft preservation, Ctrl-R/Ctrl-S incremental search in both
//!     directions, and prefix search (PageUp/PageDown, Alt-p/Alt-n);
//!   * completion (Tab: longest-common-prefix insertion, then a columned
//!     candidate list — C69's display, C34's candidate sources) and
//!     abbreviation expansion on space (C70).

use std::io::{self, Read, Write};

pub enum ReadResult {
    /// A complete line (Enter).
    Line(String),
    /// Ctrl-C at the prompt.
    Interrupted,
    /// Ctrl-D on an empty line.
    Eof,
}

pub struct Editor {
    history: Vec<String>,
    /// The kill ring (readline's): survives across lines within a session.
    kill_ring: Vec<String>,
}

/// The piped-stdin path: one line, no prompt, no editing.
#[cfg(unix)]
fn read_line_plain() -> io::Result<ReadResult> {
    let mut line = Vec::new();
    let mut b = [0u8; 1];
    loop {
        match io::stdin().read(&mut b) {
            Ok(0) => {
                if line.is_empty() {
                    return Ok(ReadResult::Eof);
                }
                break;
            }
            Ok(_) if b[0] == b'\n' => break,
            Ok(_) => line.push(b[0]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(ReadResult::Line(String::from_utf8_lossy(&line).into_owned()))
}

impl Editor {
    pub fn new() -> Self {
        Editor { history: Vec::new(), kill_ring: Vec::new() }
    }

    pub fn history(&self) -> &[String] {
        &self.history
    }

    /// Append to history, skipping a consecutive duplicate. A multi-line
    /// entry (a bracketed paste) is joined with `; ` — bash's `cmdhist`
    /// behavior — so recall and the line-oriented history file both work.
    pub fn add_history_entry(&mut self, line: &str) {
        let entry =
            if line.contains('\n') { line.replace('\n', "; ") } else { line.to_string() };
        if self.history.last() != Some(&entry) {
            self.history.push(entry);
        }
    }

    /// Load history from `path` — plain lines; a leading `#V2` header (the
    /// format rustyline's `FileHistory` wrote before this editor existed)
    /// is skipped so an existing history file keeps working.
    pub fn load_history(&mut self, path: &std::path::Path) -> io::Result<()> {
        let text = std::fs::read_to_string(path)?;
        for (i, line) in text.lines().enumerate() {
            if i == 0 && line == "#V2" {
                continue;
            }
            if !line.is_empty() {
                self.add_history_entry(line);
            }
        }
        Ok(())
    }

    pub fn save_history(&self, path: &std::path::Path) -> io::Result<()> {
        std::fs::write(path, self.history.join("\n") + "\n")
    }

    /// Read one line interactively. `rprompt` is the already-expanded
    /// right-side prompt text (`$RPS1`, C71), or empty for none.
    pub fn read_line(&mut self, prompt: &str, rprompt: &str) -> io::Result<ReadResult> {
        #[cfg(unix)]
        {
            // A non-tty stdin (scripts piped into an "interactive" rush —
            // the bang-history tests do exactly this) can't enter raw
            // mode; fall back to a plain silent read, like rustyline did.
            if unsafe { libc::isatty(0) } == 0 {
                return read_line_plain();
            }
            read_line_raw(self, prompt, rprompt)
        }
        #[cfg(not(unix))]
        {
            // No raw terminal on this platform: a plain buffered read with
            // no editing — a documented narrowing (rush's non-Unix build
            // is already reduced; see docs/ARCHITECTURE.md).
            let _ = rprompt;
            print!("{prompt}");
            io::stdout().flush()?;
            let mut line = String::new();
            if io::stdin().read_line(&mut line)? == 0 {
                return Ok(ReadResult::Eof);
            }
            while line.ends_with(['\n', '\r']) {
                line.pop();
            }
            Ok(ReadResult::Line(line))
        }
    }
}

/// One decoded input event.
#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq)]
enum Key {
    Char(char),
    Ctrl(char), // Ctrl('a') for ^A …; Ctrl('_') for ^_
    Alt(char),
    AltBackspace, // ESC DEL — backward-kill-word
    Enter,
    Tab,
    Backspace,
    Delete,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    WordLeft,  // Ctrl/Alt-Left
    WordRight, // Ctrl/Alt-Right
    PageUp,
    PageDown,
    /// A bracketed paste: the whole pasted text as one event, so tabs and
    /// escape bytes inside it insert literally instead of firing bindings.
    Paste(String),
    Esc,
    Other,
}

/// Raw-mode RAII guard: restores the saved termios on drop, whatever the
/// exit path. Keeps both states so `suspend`/`resume` can hand the
/// terminal to an external `$EDITOR` (C-x C-e) and take it back.
#[cfg(unix)]
struct RawMode {
    saved: libc::termios,
    raw: libc::termios,
}

#[cfg(unix)]
impl RawMode {
    fn enable() -> io::Result<RawMode> {
        unsafe {
            let mut t: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(0, &mut t) != 0 {
                return Err(io::Error::last_os_error());
            }
            let saved = t;
            // Input: no Ctrl-S/Q flow control (freeing C-s for forward
            // search), no CR→NL mangling. Local: no canonical buffering,
            // no echo, no signal generation (^C becomes a key we handle),
            // no ^V. Output stays cooked so ordinary `println!` keeps
            // working for lists and job notices.
            t.c_iflag &= !(libc::IXON | libc::ICRNL | libc::INLCR);
            t.c_lflag &= !(libc::ICANON | libc::ECHO | libc::ISIG | libc::IEXTEN);
            t.c_cc[libc::VMIN] = 1;
            t.c_cc[libc::VTIME] = 0;
            if libc::tcsetattr(0, libc::TCSADRAIN, &t) != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(RawMode { saved, raw: t })
        }
    }

    /// Back to the shell's normal (cooked) state, for an external editor.
    fn suspend(&self) {
        unsafe {
            libc::tcsetattr(0, libc::TCSADRAIN, &self.saved);
        }
    }

    fn resume(&self) {
        unsafe {
            libc::tcsetattr(0, libc::TCSADRAIN, &self.raw);
        }
    }
}

#[cfg(unix)]
impl Drop for RawMode {
    fn drop(&mut self) {
        unsafe {
            libc::tcsetattr(0, libc::TCSADRAIN, &self.saved);
        }
    }
}

/// Bracketed-paste RAII guard: terminals wrap a paste in
/// `ESC[200~ … ESC[201~` while enabled, and the decoder turns that into a
/// single `Key::Paste` event.
#[cfg(unix)]
struct BracketedPaste;

#[cfg(unix)]
impl BracketedPaste {
    fn enable() -> BracketedPaste {
        print!("\x1b[?2004h");
        let _ = io::stdout().flush();
        BracketedPaste
    }
}

#[cfg(unix)]
impl Drop for BracketedPaste {
    fn drop(&mut self) {
        print!("\x1b[?2004l");
        let _ = io::stdout().flush();
    }
}

/// Whether fd 0 has a byte ready within `ms` milliseconds — the lone-ESC
/// vs escape-sequence disambiguation.
#[cfg(unix)]
fn input_ready(ms: i32) -> bool {
    let mut pfd = libc::pollfd { fd: 0, events: libc::POLLIN, revents: 0 };
    unsafe { libc::poll(&mut pfd, 1, ms) > 0 }
}

/// One byte straight off fd 0 — deliberately *not* through
/// `io::stdin()`, whose userspace buffer would swallow the rest of an
/// escape sequence and make `input_ready`'s `poll` lie about it (the
/// arrow keys literally didn't work through the buffered reader).
#[cfg(unix)]
fn read_byte() -> io::Result<Option<u8>> {
    let mut b = [0u8; 1];
    loop {
        let n = unsafe { libc::read(0, b.as_mut_ptr().cast(), 1) };
        match n {
            0 => return Ok(None),
            1 => return Ok(Some(b[0])),
            _ => {
                let e = io::Error::last_os_error();
                if e.kind() == io::ErrorKind::Interrupted {
                    // A signal (e.g. a deferred TERM) landed mid-read; let
                    // trap machinery see it at the next safe point and
                    // keep reading.
                    crate::trap::check_pending();
                    continue;
                }
                return Err(e);
            }
        }
    }
}

/// Assemble one UTF-8 character whose first byte is `first`.
#[cfg(unix)]
fn read_utf8(first: u8) -> io::Result<char> {
    let need = match first {
        0x00..=0x7f => 0,
        0xc0..=0xdf => 1,
        0xe0..=0xef => 2,
        _ => 3,
    };
    let mut buf = vec![first];
    for _ in 0..need {
        if let Some(b) = read_byte()? {
            buf.push(b);
        }
    }
    Ok(String::from_utf8_lossy(&buf).chars().next().unwrap_or('\u{fffd}'))
}

/// Map a CSI escape sequence's final byte (plus parameters) to a key —
/// pure, so the quirk table is unit-testable. Modifier parameters `1;5`
/// (Ctrl) and `1;3` (Alt) on the arrows become word motions.
#[cfg(unix)]
fn csi_key(params: &str, final_byte: u8) -> Key {
    match (params, final_byte) {
        ("1;5" | "1;3", b'C') => Key::WordRight,
        ("1;5" | "1;3", b'D') => Key::WordLeft,
        (_, b'A') => Key::Up,
        (_, b'B') => Key::Down,
        (_, b'C') => Key::Right,
        (_, b'D') => Key::Left,
        (_, b'H') => Key::Home,
        (_, b'F') => Key::End,
        ("1", b'~') | ("7", b'~') => Key::Home,
        ("4", b'~') | ("8", b'~') => Key::End,
        ("3", b'~') => Key::Delete,
        ("5", b'~') => Key::PageUp,
        ("6", b'~') => Key::PageDown,
        _ => Key::Other,
    }
}

/// Collect a bracketed paste: everything up to the closing `ESC[201~`.
#[cfg(unix)]
fn read_paste() -> io::Result<Key> {
    const END: &[u8] = b"\x1b[201~";
    let mut buf: Vec<u8> = Vec::new();
    while !buf.ends_with(END) {
        match read_byte()? {
            Some(b) => buf.push(b),
            None => break,
        }
    }
    if buf.ends_with(END) {
        buf.truncate(buf.len() - END.len());
    }
    Ok(Key::Paste(String::from_utf8_lossy(&buf).into_owned()))
}

#[cfg(unix)]
fn read_key() -> io::Result<Option<Key>> {
    let Some(b) = read_byte()? else {
        return Ok(None);
    };
    Ok(Some(match b {
        b'\r' | b'\n' => Key::Enter,
        b'\t' => Key::Tab,
        0x7f | 0x08 => Key::Backspace,
        0x1b => {
            if !input_ready(30) {
                return Ok(Some(Key::Esc));
            }
            match read_byte()? {
                Some(b'[') => {
                    let mut params = String::new();
                    loop {
                        match read_byte()? {
                            Some(c @ (b'0'..=b'9' | b';')) => params.push(c as char),
                            Some(final_byte) => {
                                if params == "200" && final_byte == b'~' {
                                    return read_paste().map(Some);
                                }
                                return Ok(Some(csi_key(&params, final_byte)));
                            }
                            None => return Ok(Some(Key::Other)),
                        }
                    }
                }
                Some(b'O') => match read_byte()? {
                    Some(b'H') => Key::Home,
                    Some(b'F') => Key::End,
                    Some(b'A') => Key::Up,
                    Some(b'B') => Key::Down,
                    Some(b'C') => Key::Right,
                    Some(b'D') => Key::Left,
                    _ => Key::Other,
                },
                Some(0x7f) => Key::AltBackspace,
                Some(c) if c.is_ascii_graphic() => Key::Alt(c as char),
                _ => Key::Other,
            }
        }
        0x1f => Key::Ctrl('_'),
        0x01..=0x1a => Key::Ctrl((b - 1 + b'a') as char),
        _ => Key::Char(read_utf8(b)?),
    }))
}

/// Terminal width in columns (fallback 80).
#[cfg(unix)]
fn term_cols() -> usize {
    unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        if libc::ioctl(1, libc::TIOCGWINSZ, &mut ws) == 0 && ws.ws_col > 0 {
            return ws.ws_col as usize;
        }
    }
    80
}

/// Display width of `s`, skipping ANSI SGR escape sequences — the prompt
/// and the highlighted buffer both carry them.
fn display_width(s: &str) -> usize {
    use unicode_width::UnicodeWidthChar;
    let mut w = 0;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // Skip `ESC [ ... final` (or a lone two-char escape).
            if chars.peek() == Some(&'[') {
                chars.next();
                for e in chars.by_ref() {
                    if e.is_ascii_alphabetic() || e == '~' {
                        break;
                    }
                }
            } else {
                chars.next();
            }
            continue;
        }
        w += c.width().unwrap_or(0);
    }
    w
}

/// The buffer as painted: control characters shown readline-style —
/// `^X` for C0 bytes, `^?` for DEL, `⏎` for an embedded newline (a
/// multi-line bracketed paste), a tab as four spaces. The raw buffer is
/// what Enter returns; this transform exists only so the render and its
/// cursor math never emit a raw control byte at the terminal.
#[cfg(unix)]
fn visualize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\n' => out.push('⏎'),
            '\t' => out.push_str("    "),
            '\u{7f}' => out.push_str("^?"),
            c if (c as u32) < 0x20 => {
                out.push('^');
                out.push(((c as u8) ^ 0x40) as char);
            }
            c => out.push(c),
        }
    }
    out
}

/// What the previous key did — drives undo coalescing (runs of plain
/// inserts undo as one unit), kill-ring appending (consecutive kills grow
/// one ring entry), yank-pop eligibility, and M-. cycling.
#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Insert,
    KillFwd,
    KillBack,
    Yank,
    LastArg,
    Undo,
    Other,
}

/// The per-`read_line` editing state.
#[cfg(unix)]
struct LineState<'a> {
    buffer: String,
    cursor: usize, // byte offset into buffer
    prompt: &'a str,
    rprompt: &'a str,
    /// Rows the previous paint occupied, and which row the cursor was
    /// left on — the starting point for the next repaint.
    painted_rows: usize,
    painted_cursor_row: usize,
    /// History navigation: index into `history` (None = live line), the
    /// draft stashed when navigation started, and the anchored prefix for
    /// PageUp/PageDown prefix search.
    hist_index: Option<usize>,
    draft: String,
    prefix: String,
    /// vi mode (C73): true when in normal mode; `vi_op` holds a pending
    /// `d`/`c`/`y` operator, `vi_find` a pending `f F t T`, `vi_count`
    /// the accumulated count, `vi_replace` a pending `r`, `last_find`
    /// the target `;`/`,` repeat.
    vi: bool,
    vi_normal: bool,
    vi_count: usize,
    vi_op: Option<char>,
    vi_find: Option<char>,
    vi_replace: bool,
    last_find: Option<(char, char)>,
    /// Ctrl-R/Ctrl-S incremental search, when active.
    search: Option<SearchState>,
    /// Undo stack: (buffer, cursor) snapshots taken before mutations.
    undo: Vec<(String, usize)>,
    prev_action: Action,
    this_action: Action,
    /// Last yank's span and ring index, for M-y yank-pop.
    yank: Option<(usize, usize, usize)>,
    /// Last M-. insertion: (history index, span start, span end).
    lastarg: Option<(usize, usize, usize)>,
}

#[cfg(unix)]
struct SearchState {
    query: String,
    /// Index into history of the current match, if any.
    hit: Option<usize>,
    /// Search direction: C-r steps older, C-s newer.
    forward: bool,
}

#[cfg(unix)]
fn read_line_raw(ed: &mut Editor, prompt: &str, rprompt: &str) -> io::Result<ReadResult> {
    let raw = RawMode::enable()?;
    let _paste = BracketedPaste::enable();
    let Editor { history, kill_ring } = ed;
    let mut st = LineState {
        buffer: String::new(),
        cursor: 0,
        prompt,
        rprompt,
        painted_rows: 1,
        painted_cursor_row: 0,
        hist_index: None,
        draft: String::new(),
        prefix: String::new(),
        vi: crate::vars::edit_mode_vi(),
        vi_normal: false,
        vi_count: 0,
        vi_op: None,
        vi_find: None,
        vi_replace: false,
        last_find: None,
        search: None,
        undo: Vec::new(),
        prev_action: Action::Other,
        this_action: Action::Other,
        yank: None,
        lastarg: None,
    };
    render(&mut st, history)?;

    loop {
        let Some(key) = read_key()? else {
            // EOF on stdin itself.
            finish_line(&mut st)?;
            return Ok(ReadResult::Eof);
        };

        // Ctrl-R/Ctrl-S search intercepts everything while active.
        if st.search.is_some() {
            match handle_search_key(&mut st, key, history)? {
                SearchOutcome::Continue | SearchOutcome::Exit => {
                    render(&mut st, history)?;
                    continue;
                }
                SearchOutcome::Accept => {
                    finish_line(&mut st)?;
                    return Ok(ReadResult::Line(st.buffer));
                }
            }
        }

        let snapshot = (st.buffer.clone(), st.cursor);
        st.this_action = Action::Other;

        match key {
            Key::Enter => {
                finish_line(&mut st)?;
                return Ok(ReadResult::Line(st.buffer));
            }
            Key::Ctrl('c') => {
                finish_line(&mut st)?;
                return Ok(ReadResult::Interrupted);
            }
            Key::Ctrl('d') if st.buffer.is_empty() => {
                finish_line(&mut st)?;
                return Ok(ReadResult::Eof);
            }
            Key::Ctrl('r') => {
                st.search = Some(SearchState { query: String::new(), hit: None, forward: false });
            }
            Key::Ctrl('s') => {
                st.search = Some(SearchState { query: String::new(), hit: None, forward: true });
            }
            Key::Ctrl('l') => {
                print!("\x1b[2J\x1b[H");
                st.painted_rows = 1;
                st.painted_cursor_row = 0;
            }
            Key::Ctrl('_') | Key::Ctrl('z') => undo_cmd(&mut st),
            Key::Ctrl('x') => {
                // The readline C-x chords rush supports: C-x C-e (edit the
                // line in $EDITOR) and C-x C-u (undo).
                match read_key()? {
                    Some(Key::Ctrl('e')) => {
                        if let Some(line) = edit_in_editor(&mut st, &raw)? {
                            return Ok(ReadResult::Line(line));
                        }
                    }
                    Some(Key::Ctrl('u')) => undo_cmd(&mut st),
                    _ => {}
                }
            }
            Key::Ctrl('v') | Key::Ctrl('q') => quoted_insert(&mut st)?,
            Key::Char('v')
                if st.vi
                    && st.vi_normal
                    && st.vi_op.is_none()
                    && st.vi_find.is_none()
                    && !st.vi_replace =>
            {
                // vi normal-mode `v`: edit the line in $EDITOR, readline's
                // own vi binding.
                if let Some(line) = edit_in_editor(&mut st, &raw)? {
                    return Ok(ReadResult::Line(line));
                }
            }
            Key::Tab => {
                complete_at_cursor(&mut st)?;
            }
            Key::Paste(s) => {
                // Insert the paste verbatim (normalizing line endings) —
                // no completion, no abbreviations, no history motion.
                let s = s.replace("\r\n", "\n").replace('\r', "\n");
                st.buffer.insert_str(st.cursor, &s);
                st.cursor += s.len();
            }
            key if st.vi && st.vi_normal => handle_vi_normal(&mut st, key, history, kill_ring),
            key => handle_insert(&mut st, key, history, kill_ring),
        }

        // Undo bookkeeping: snapshot any mutation, coalescing runs of
        // plain self-insert (readline groups those too).
        if st.buffer != snapshot.0 && st.this_action != Action::Undo {
            let coalesce =
                st.this_action == Action::Insert && st.prev_action == Action::Insert;
            if !coalesce {
                st.undo.push(snapshot);
                if st.undo.len() > 200 {
                    st.undo.remove(0);
                }
            }
        }
        st.prev_action = st.this_action;

        render(&mut st, history)?;
    }
}

/// Move to the end of the painted region and start a fresh terminal line,
/// so whatever runs next begins below the edit region.
#[cfg(unix)]
fn finish_line(st: &mut LineState) -> io::Result<()> {
    let down = st.painted_rows.saturating_sub(1 + st.painted_cursor_row);
    if down > 0 {
        print!("\x1b[{down}B");
    }
    println!();
    io::stdout().flush()
}

/// The emacs (and vi-insert) key handling.
#[cfg(unix)]
fn handle_insert(st: &mut LineState, key: Key, history: &[String], ring: &mut Vec<String>) {
    match key {
        Key::Esc if st.vi => {
            st.vi_normal = true;
            // vi leaves the cursor on the last inserted character.
            if let Some(prev) = prev_char_start(&st.buffer, st.cursor) {
                st.cursor = prev;
            }
        }
        Key::Char(' ') => {
            // Abbreviations (C70): a space after a defined abbreviation in
            // command position rewrites it in place first.
            if let Some((start, expansion)) = crate::completion::abbr_expansion(&st.buffer, st.cursor) {
                st.buffer.replace_range(start..st.cursor, &expansion);
                st.cursor = start + expansion.len();
            }
            insert_char(st, ' ');
            st.this_action = Action::Insert;
        }
        Key::Char(c) => {
            insert_char(st, c);
            st.this_action = Action::Insert;
        }
        Key::Backspace | Key::Ctrl('h') => {
            if let Some(prev) = prev_char_start(&st.buffer, st.cursor) {
                st.buffer.replace_range(prev..st.cursor, "");
                st.cursor = prev;
            }
        }
        Key::Delete | Key::Ctrl('d') => {
            if let Some(next) = next_char_end(&st.buffer, st.cursor) {
                st.buffer.replace_range(st.cursor..next, "");
            }
        }
        Key::Left | Key::Ctrl('b') => {
            if let Some(prev) = prev_char_start(&st.buffer, st.cursor) {
                st.cursor = prev;
            }
        }
        Key::Right | Key::Ctrl('f') => {
            // At end of line, the right arrow accepts the history hint
            // (C33's affordance).
            if st.cursor == st.buffer.len() {
                if let Some(hint) = crate::completion::hint(&st.buffer, history) {
                    st.buffer.push_str(&hint);
                    st.cursor = st.buffer.len();
                }
            } else if let Some(next) = next_char_end(&st.buffer, st.cursor) {
                st.cursor = next;
            }
        }
        Key::Home | Key::Ctrl('a') => st.cursor = 0,
        Key::End | Key::Ctrl('e') => {
            // End at end-of-line also accepts the hint (fish's behavior).
            if st.cursor == st.buffer.len()
                && let Some(hint) = crate::completion::hint(&st.buffer, history)
            {
                st.buffer.push_str(&hint);
            }
            st.cursor = st.buffer.len();
        }
        Key::WordLeft | Key::Alt('b') => st.cursor = word_back_alnum(&st.buffer, st.cursor),
        Key::WordRight | Key::Alt('f') => st.cursor = word_forward_alnum(&st.buffer, st.cursor),
        Key::Ctrl('k') => kill_span(st, ring, st.cursor, st.buffer.len(), true),
        Key::Ctrl('u') => kill_span(st, ring, 0, st.cursor, false),
        Key::Ctrl('w') => {
            // unix-word-rubout: whitespace-delimited, unlike M-Backspace.
            let start = word_back(&st.buffer, st.cursor);
            kill_span(st, ring, start, st.cursor, false);
        }
        Key::Alt('d') => {
            let end = word_forward_alnum(&st.buffer, st.cursor);
            kill_span(st, ring, st.cursor, end, true);
        }
        Key::AltBackspace => {
            let start = word_back_alnum(&st.buffer, st.cursor);
            kill_span(st, ring, start, st.cursor, false);
        }
        Key::Ctrl('y') => yank(st, ring),
        Key::Alt('y') => yank_pop(st, ring),
        Key::Ctrl('t') => transpose(st),
        Key::Alt('t') => transpose_words(st),
        Key::Alt('u') => case_word(st, CaseOp::Upper),
        Key::Alt('l') => case_word(st, CaseOp::Lower),
        Key::Alt('c') => case_word(st, CaseOp::Capital),
        Key::Alt('.') | Key::Alt('_') => insert_last_arg(st, history),
        Key::Alt('<') => history_first(st, history),
        Key::Alt('>') => history_last(st),
        Key::Up | Key::Ctrl('p') => history_prev(st, history),
        Key::Down | Key::Ctrl('n') => history_next(st, history),
        Key::PageUp | Key::Alt('p') => history_prefix_prev(st, history),
        Key::PageDown | Key::Alt('n') => history_prefix_next(st, history),
        _ => {}
    }
}

/// vi normal mode (C73): counts, the `d`/`c`/`y` operators over motions
/// (`h l 0 ^ $ w W b B e E f F t T ; ,` and doubled `dd cc yy`), edits
/// `x X D C s S Y r ~ p P u`, inserts `i I a A`, history `k j`. See
/// docs/LINE_EDITOR.md for what's deliberately not modeled (registers,
/// `.` repeat, `/` search).
#[cfg(unix)]
fn handle_vi_normal(st: &mut LineState, key: Key, history: &[String], ring: &mut Vec<String>) {
    // A pending `r`: the next character replaces the one under the cursor.
    if st.vi_replace {
        st.vi_replace = false;
        st.vi_count = 0;
        if let Key::Char(c) = key
            && let Some(next) = next_char_end(&st.buffer, st.cursor)
        {
            st.buffer.replace_range(st.cursor..next, &c.to_string());
        }
        return;
    }
    // A pending `f F t T`: the next character is the find target.
    if let Some(kind) = st.vi_find.take() {
        let n = st.vi_count.max(1);
        st.vi_count = 0;
        if let Key::Char(c) = key {
            st.last_find = Some((kind, c));
            let mut hit: Option<(usize, bool)> = None;
            let mut from = st.cursor;
            for _ in 0..n {
                match vi_find_target(&st.buffer, from, kind, c) {
                    Some((t, inc)) => {
                        hit = Some((t, inc));
                        from = t;
                    }
                    None => {
                        hit = None;
                        break;
                    }
                }
            }
            match (hit, st.vi_op.take()) {
                (Some((t, inc)), Some(op)) => vi_apply_op(st, ring, op, t, inc),
                (Some((t, _)), None) => st.cursor = t,
                (None, _) => {}
            }
        } else {
            st.vi_op = None;
        }
        return;
    }
    // Count accumulation (`0` is a motion unless a count has started).
    if let Key::Char(c @ '0'..='9') = key
        && (c != '0' || st.vi_count > 0)
    {
        st.vi_count = st.vi_count * 10 + (c as usize - '0' as usize);
        return;
    }

    let n = st.vi_count.max(1);

    // Motions: resolve to a target position (and operator inclusivity).
    let motion: Option<(usize, bool)> = match &key {
        Key::Char('h') | Key::Left | Key::Backspace => {
            Some((back_n(&st.buffer, st.cursor, n), false))
        }
        Key::Char('l' | ' ') | Key::Right => Some((fwd_n(&st.buffer, st.cursor, n), false)),
        Key::Char('0') | Key::Home => Some((0, false)),
        Key::Char('^') => Some((first_nonblank(&st.buffer), false)),
        Key::Char('$') | Key::End => Some((st.buffer.len(), false)),
        Key::Char('w' | 'W') => {
            // vi quirk: `cw` behaves like `ce`.
            if st.vi_op == Some('c') {
                Some((apply_n(&st.buffer, st.cursor, n, vi_word_end), true))
            } else {
                Some((apply_n(&st.buffer, st.cursor, n, vi_word_fwd), false))
            }
        }
        Key::Char('b' | 'B') => Some((apply_n(&st.buffer, st.cursor, n, vi_word_back), false)),
        Key::Char('e' | 'E') => Some((apply_n(&st.buffer, st.cursor, n, vi_word_end), true)),
        Key::Char(k @ ('f' | 'F' | 't' | 'T')) => {
            st.vi_find = Some(*k);
            return;
        }
        Key::Char(sc @ (';' | ',')) => st.last_find.and_then(|(kind, target)| {
            let k = if *sc == ',' { invert_find(kind) } else { kind };
            vi_find_target(&st.buffer, st.cursor, k, target)
        }),
        _ => None,
    };
    if let Some((target, inclusive)) = motion {
        st.vi_count = 0;
        if let Some(op) = st.vi_op.take() {
            vi_apply_op(st, ring, op, target, inclusive);
        } else {
            st.cursor = target.min(st.buffer.len());
        }
        return;
    }

    match key {
        Key::Char(o @ ('d' | 'c' | 'y')) => {
            if st.vi_op == Some(o) {
                // dd / cc / yy: linewise.
                st.vi_op = None;
                match o {
                    'd' => kill_span(st, ring, 0, st.buffer.len(), true),
                    'c' => {
                        kill_span(st, ring, 0, st.buffer.len(), true);
                        st.vi_normal = false;
                    }
                    _ => {
                        if !st.buffer.is_empty() {
                            push_ring(ring, st.buffer.clone());
                        }
                        st.cursor = 0;
                    }
                }
            } else {
                st.vi_op = Some(o);
                return; // keep any count for the motion
            }
        }
        Key::Char('x') => {
            let end = fwd_n(&st.buffer, st.cursor, n);
            kill_span(st, ring, st.cursor, end, true);
        }
        Key::Char('X') => {
            let start = back_n(&st.buffer, st.cursor, n);
            kill_span(st, ring, start, st.cursor, false);
        }
        Key::Char('s') => {
            let end = fwd_n(&st.buffer, st.cursor, n);
            kill_span(st, ring, st.cursor, end, true);
            st.vi_normal = false;
        }
        Key::Char('D') => kill_span(st, ring, st.cursor, st.buffer.len(), true),
        Key::Char('C') => {
            kill_span(st, ring, st.cursor, st.buffer.len(), true);
            st.vi_normal = false;
        }
        Key::Char('S') => {
            kill_span(st, ring, 0, st.buffer.len(), true);
            st.vi_normal = false;
        }
        Key::Char('Y') => {
            if !st.buffer.is_empty() {
                push_ring(ring, st.buffer.clone());
            }
        }
        Key::Char('r') => {
            st.vi_replace = true;
            return;
        }
        Key::Char('~') => {
            for _ in 0..n {
                let Some(next) = next_char_end(&st.buffer, st.cursor) else { break };
                let flipped: String = st.buffer[st.cursor..next]
                    .chars()
                    .flat_map(|c| {
                        if c.is_uppercase() {
                            c.to_lowercase().collect::<Vec<_>>()
                        } else {
                            c.to_uppercase().collect()
                        }
                    })
                    .collect();
                let len = flipped.len();
                st.buffer.replace_range(st.cursor..next, &flipped);
                st.cursor += len;
            }
        }
        Key::Char('p') => {
            if let Some(text) = ring.last().cloned()
                && !text.is_empty()
            {
                let at = next_char_end(&st.buffer, st.cursor).unwrap_or(st.cursor);
                st.buffer.insert_str(at, &text);
                st.cursor = prev_char_start(&st.buffer, at + text.len()).unwrap_or(at);
            }
        }
        Key::Char('P') => {
            if let Some(text) = ring.last().cloned()
                && !text.is_empty()
            {
                let at = st.cursor;
                st.buffer.insert_str(at, &text);
                st.cursor = prev_char_start(&st.buffer, at + text.len()).unwrap_or(at);
            }
        }
        Key::Char('u') => undo_cmd(st),
        Key::Char('i') => st.vi_normal = false,
        Key::Char('I') => {
            st.cursor = 0;
            st.vi_normal = false;
        }
        Key::Char('a') => {
            if let Some(next) = next_char_end(&st.buffer, st.cursor) {
                st.cursor = next;
            }
            st.vi_normal = false;
        }
        Key::Char('A') => {
            st.cursor = st.buffer.len();
            st.vi_normal = false;
        }
        Key::Char('k') | Key::Up => history_prev(st, history),
        Key::Char('j') | Key::Down => history_next(st, history),
        Key::Delete => {
            if let Some(next) = next_char_end(&st.buffer, st.cursor) {
                st.buffer.replace_range(st.cursor..next, "");
            }
        }
        _ => {}
    }
    st.vi_count = 0;
    st.vi_op = None; // an unrecognized key cancels a pending operator
}

/// Apply a vi operator over the span from the cursor to `target`.
#[cfg(unix)]
fn vi_apply_op(st: &mut LineState, ring: &mut Vec<String>, op: char, target: usize, inclusive: bool) {
    let (s, e) = if target >= st.cursor {
        let e = if inclusive {
            next_char_end(&st.buffer, target).unwrap_or(st.buffer.len())
        } else {
            target
        };
        (st.cursor, e)
    } else {
        (target, st.cursor)
    };
    if s >= e {
        if op == 'c' {
            st.vi_normal = false;
        }
        return;
    }
    match op {
        'd' => kill_span(st, ring, s, e, target >= st.cursor),
        'c' => {
            kill_span(st, ring, s, e, target >= st.cursor);
            st.vi_normal = false;
        }
        _ => {
            push_ring(ring, st.buffer[s..e].to_string());
            st.cursor = s;
        }
    }
}

#[cfg(unix)]
fn invert_find(kind: char) -> char {
    match kind {
        'f' => 'F',
        'F' => 'f',
        't' => 'T',
        _ => 't',
    }
}

/// Resolve `f F t T` to a target position (and operator inclusivity).
#[cfg(unix)]
fn vi_find_target(s: &str, pos: usize, kind: char, target: char) -> Option<(usize, bool)> {
    match kind {
        'f' | 't' => {
            let from = next_char_end(s, pos)?;
            let found = s[from..].char_indices().find(|&(_, c)| c == target).map(|(i, _)| from + i)?;
            let t = if kind == 't' { prev_char_start(s, found)? } else { found };
            if t <= pos { None } else { Some((t, true)) }
        }
        _ => {
            let found = s[..pos].char_indices().rev().find(|&(_, c)| c == target).map(|(i, _)| i)?;
            let t = if kind == 'T' { next_char_end(s, found)? } else { found };
            if t >= pos { None } else { Some((t, false)) }
        }
    }
}

#[cfg(unix)]
enum SearchOutcome {
    Continue,
    Accept,
    Exit,
}

/// Ctrl-R/Ctrl-S incremental search (bash's `(reverse-i-search)`); C-r
/// steps to older matches, C-s to newer ones.
#[cfg(unix)]
fn handle_search_key(st: &mut LineState, key: Key, history: &[String]) -> io::Result<SearchOutcome> {
    let search = st.search.as_mut().expect("search active");
    match key {
        Key::Enter => {
            if let Some(hit) = search.hit {
                st.buffer = history[hit].clone();
                st.cursor = st.buffer.len();
            }
            st.search = None;
            return Ok(SearchOutcome::Accept);
        }
        Key::Ctrl('g') | Key::Esc | Key::Ctrl('c') => {
            st.search = None;
            return Ok(SearchOutcome::Exit);
        }
        Key::Ctrl('r') => {
            // Next older match.
            search.forward = false;
            let below = search.hit.unwrap_or(history.len());
            search.hit = find_match(history, &search.query, below).or(search.hit);
        }
        Key::Ctrl('s') => {
            // Next newer match.
            search.forward = true;
            let above = search.hit.map(|h| h + 1).unwrap_or(0);
            search.hit = find_match_fwd(history, &search.query, above).or(search.hit);
        }
        Key::Backspace => {
            search.query.pop();
            search.hit = find_match(history, &search.query, history.len());
        }
        Key::Char(c) => {
            search.query.push(c);
            search.hit = find_match(history, &search.query, history.len());
        }
        _ => {
            // Any other key: keep the match as the edit buffer and leave
            // search mode.
            if let Some(hit) = search.hit {
                st.buffer = history[hit].clone();
                st.cursor = st.buffer.len();
            }
            st.search = None;
            return Ok(SearchOutcome::Exit);
        }
    }
    Ok(SearchOutcome::Continue)
}

/// Most recent history entry (strictly before `below`) containing `query`.
#[cfg(unix)]
fn find_match(history: &[String], query: &str, below: usize) -> Option<usize> {
    if query.is_empty() {
        return None;
    }
    history[..below.min(history.len())].iter().rposition(|h| h.contains(query))
}

/// Earliest history entry at or after `from` containing `query`.
#[cfg(unix)]
fn find_match_fwd(history: &[String], query: &str, from: usize) -> Option<usize> {
    if query.is_empty() || from >= history.len() {
        return None;
    }
    history[from..].iter().position(|h| h.contains(query)).map(|p| p + from)
}

#[cfg(unix)]
fn insert_char(st: &mut LineState, c: char) {
    st.buffer.insert(st.cursor, c);
    st.cursor += c.len_utf8();
}

#[cfg(unix)]
fn prev_char_start(s: &str, pos: usize) -> Option<usize> {
    s[..pos].char_indices().next_back().map(|(i, _)| i)
}

#[cfg(unix)]
fn next_char_end(s: &str, pos: usize) -> Option<usize> {
    s[pos..].chars().next().map(|c| pos + c.len_utf8())
}

#[cfg(unix)]
fn fwd_n(s: &str, pos: usize, n: usize) -> usize {
    let mut p = pos;
    for _ in 0..n {
        match next_char_end(s, p) {
            Some(q) => p = q,
            None => break,
        }
    }
    p
}

#[cfg(unix)]
fn back_n(s: &str, pos: usize, n: usize) -> usize {
    let mut p = pos;
    for _ in 0..n {
        match prev_char_start(s, p) {
            Some(q) => p = q,
            None => break,
        }
    }
    p
}

#[cfg(unix)]
fn apply_n(s: &str, pos: usize, n: usize, f: fn(&str, usize) -> usize) -> usize {
    let mut p = pos;
    for _ in 0..n {
        p = f(s, p);
    }
    p
}

#[cfg(unix)]
fn first_nonblank(s: &str) -> usize {
    s.char_indices().find(|&(_, c)| !c.is_whitespace()).map(|(i, _)| i).unwrap_or(0)
}

/// Start of the word before `pos` (whitespace-delimited — C-w's
/// unix-word-rubout and the vi big-word flavor).
fn word_back(s: &str, pos: usize) -> usize {
    let before = &s[..pos];
    let trimmed = before.trim_end();
    match trimmed.rfind(char::is_whitespace) {
        Some(i) => i + 1,
        None => 0,
    }
}

/// Start of the alphanumeric word before `pos` (readline's M-b flavor).
fn word_back_alnum(s: &str, pos: usize) -> usize {
    let chars: Vec<(usize, char)> = s[..pos].char_indices().collect();
    let mut i = chars.len();
    while i > 0 && !chars[i - 1].1.is_alphanumeric() {
        i -= 1;
    }
    while i > 0 && chars[i - 1].1.is_alphanumeric() {
        i -= 1;
    }
    if i < chars.len() { chars[i].0 } else { pos }
}

/// End of the alphanumeric word after `pos` (readline's M-f flavor).
fn word_forward_alnum(s: &str, pos: usize) -> usize {
    let chars: Vec<(usize, char)> = s[pos..].char_indices().collect();
    let mut i = 0;
    while i < chars.len() && !chars[i].1.is_alphanumeric() {
        i += 1;
    }
    while i < chars.len() && chars[i].1.is_alphanumeric() {
        i += 1;
    }
    if i < chars.len() { pos + chars[i].0 } else { s.len() }
}

/// vi small-word character class: whitespace / word (alnum + `_`) /
/// punctuation — each non-blank run of one class is a word.
fn vi_class(c: char) -> u8 {
    if c.is_whitespace() {
        0
    } else if c.is_alphanumeric() || c == '_' {
        1
    } else {
        2
    }
}

/// vi `w`: start of the next small word.
fn vi_word_fwd(s: &str, pos: usize) -> usize {
    let mut it = s[pos..].char_indices().peekable();
    let Some(&(_, c0)) = it.peek() else { return s.len() };
    let cls = vi_class(c0);
    if cls != 0 {
        while let Some(&(_, c)) = it.peek() {
            if vi_class(c) == cls {
                it.next();
            } else {
                break;
            }
        }
    }
    while let Some(&(_, c)) = it.peek() {
        if vi_class(c) == 0 {
            it.next();
        } else {
            break;
        }
    }
    it.peek().map(|&(i, _)| pos + i).unwrap_or(s.len())
}

/// vi `b`: start of the small word before `pos`.
fn vi_word_back(s: &str, pos: usize) -> usize {
    let chars: Vec<(usize, char)> = s[..pos].char_indices().collect();
    let mut i = chars.len();
    while i > 0 && vi_class(chars[i - 1].1) == 0 {
        i -= 1;
    }
    if i == 0 {
        return 0;
    }
    let cls = vi_class(chars[i - 1].1);
    while i > 0 && vi_class(chars[i - 1].1) == cls {
        i -= 1;
    }
    chars.get(i).map(|&(b, _)| b).unwrap_or(0)
}

/// vi `e`: byte index of the last character of the current/next small
/// word (the cursor lands *on* it; operators over it are inclusive).
fn vi_word_end(s: &str, pos: usize) -> usize {
    let Some(start) = s[pos..].chars().next().map(|c| pos + c.len_utf8()) else {
        return pos;
    };
    let chars: Vec<(usize, char)> = s[start..].char_indices().map(|(i, c)| (start + i, c)).collect();
    let mut i = 0;
    while i < chars.len() && vi_class(chars[i].1) == 0 {
        i += 1;
    }
    if i >= chars.len() {
        return pos;
    }
    let cls = vi_class(chars[i].1);
    let mut last = chars[i].0;
    while i < chars.len() && vi_class(chars[i].1) == cls {
        last = chars[i].0;
        i += 1;
    }
    last
}

/// Delete `start..end` into the kill ring. Consecutive kills grow one
/// ring entry (appending for forward kills, prepending for backward) —
/// readline's rule, so C-w C-w C-y restores both words.
#[cfg(unix)]
fn kill_span(st: &mut LineState, ring: &mut Vec<String>, start: usize, end: usize, forward: bool) {
    if start >= end {
        return;
    }
    let text = st.buffer[start..end].to_string();
    let appending = matches!(st.prev_action, Action::KillFwd | Action::KillBack);
    if appending && let Some(last) = ring.last_mut() {
        if forward {
            last.push_str(&text);
        } else {
            last.insert_str(0, &text);
        }
    } else {
        push_ring(ring, text);
    }
    st.buffer.replace_range(start..end, "");
    st.cursor = start;
    st.this_action = if forward { Action::KillFwd } else { Action::KillBack };
}

#[cfg(unix)]
fn push_ring(ring: &mut Vec<String>, text: String) {
    ring.push(text);
    if ring.len() > 32 {
        ring.remove(0);
    }
}

/// C-y: insert the top of the kill ring at the cursor.
#[cfg(unix)]
fn yank(st: &mut LineState, ring: &[String]) {
    let Some(text) = ring.last() else { return };
    let start = st.cursor;
    st.buffer.insert_str(start, text);
    st.cursor = start + text.len();
    st.yank = Some((start, st.cursor, ring.len() - 1));
    st.this_action = Action::Yank;
}

/// M-y immediately after a yank: rotate the yanked text to the previous
/// ring entry.
#[cfg(unix)]
fn yank_pop(st: &mut LineState, ring: &[String]) {
    if st.prev_action != Action::Yank || ring.is_empty() {
        return;
    }
    let Some((start, end, idx)) = st.yank else { return };
    let new_idx = if idx == 0 { ring.len() - 1 } else { idx - 1 };
    let text = &ring[new_idx];
    st.buffer.replace_range(start..end, text);
    st.cursor = start + text.len();
    st.yank = Some((start, st.cursor, new_idx));
    st.this_action = Action::Yank;
}

/// C-_ / C-x C-u / vi `u`: pop the undo stack.
#[cfg(unix)]
fn undo_cmd(st: &mut LineState) {
    if let Some((buf, cur)) = st.undo.pop() {
        st.cursor = cur.min(buf.len());
        st.buffer = buf;
    }
    st.this_action = Action::Undo;
}

/// C-t: transpose the characters around the cursor (at end of line, the
/// two before it — readline's rule).
#[cfg(unix)]
fn transpose(st: &mut LineState) {
    if st.cursor == st.buffer.len()
        && let Some(prev) = prev_char_start(&st.buffer, st.cursor)
    {
        st.cursor = prev;
    }
    if let Some(prev) = prev_char_start(&st.buffer, st.cursor)
        && let Some(next) = next_char_end(&st.buffer, st.cursor)
    {
        let a: String = st.buffer[prev..st.cursor].to_string();
        let b: String = st.buffer[st.cursor..next].to_string();
        st.buffer.replace_range(prev..next, &format!("{b}{a}"));
        st.cursor = prev + b.len() + a.len();
    }
}

/// M-t: transpose the word at/after the cursor with the one before it,
/// leaving the cursor after the moved pair.
#[cfg(unix)]
fn transpose_words(st: &mut LineState) {
    let e2 = word_forward_alnum(&st.buffer, st.cursor);
    let s2 = {
        let chars: Vec<(usize, char)> = st.buffer[..e2].char_indices().collect();
        let mut i = chars.len();
        while i > 0 && chars[i - 1].1.is_alphanumeric() {
            i -= 1;
        }
        chars.get(i).map(|&(b, _)| b).unwrap_or(e2)
    };
    let chars: Vec<(usize, char)> = st.buffer[..s2].char_indices().collect();
    let mut i = chars.len();
    while i > 0 && !chars[i - 1].1.is_alphanumeric() {
        i -= 1;
    }
    let e1 = if i < chars.len() { chars[i].0 } else { s2 };
    while i > 0 && chars[i - 1].1.is_alphanumeric() {
        i -= 1;
    }
    let s1 = chars.get(i).map(|&(b, _)| b).unwrap_or(e1);
    if s1 >= e1 || s2 >= e2 || e1 > s2 {
        return;
    }
    let w1 = st.buffer[s1..e1].to_string();
    let sep = st.buffer[e1..s2].to_string();
    let w2 = st.buffer[s2..e2].to_string();
    st.buffer.replace_range(s1..e2, &format!("{w2}{sep}{w1}"));
    st.cursor = s1 + w2.len() + sep.len() + w1.len();
}

#[cfg(unix)]
enum CaseOp {
    Upper,
    Lower,
    Capital,
}

/// M-u / M-l / M-c: upcase / downcase / capitalize from the cursor to the
/// end of the word, moving the cursor past it.
#[cfg(unix)]
fn case_word(st: &mut LineState, op: CaseOp) {
    let end = word_forward_alnum(&st.buffer, st.cursor);
    if end <= st.cursor {
        return;
    }
    let seg = st.buffer[st.cursor..end].to_string();
    let mut out = String::with_capacity(seg.len());
    let mut first = true;
    for c in seg.chars() {
        match op {
            CaseOp::Upper => out.extend(c.to_uppercase()),
            CaseOp::Lower => out.extend(c.to_lowercase()),
            CaseOp::Capital => {
                if c.is_alphanumeric() && first {
                    first = false;
                    out.extend(c.to_uppercase());
                } else if c.is_alphanumeric() {
                    out.extend(c.to_lowercase());
                } else {
                    out.push(c);
                }
            }
        }
    }
    st.buffer.replace_range(st.cursor..end, &out);
    st.cursor += out.len();
}

/// M-. / M-_: insert the last word of the previous history entry;
/// repeated presses cycle back through older entries, replacing the
/// previous insertion.
#[cfg(unix)]
fn insert_last_arg(st: &mut LineState, history: &[String]) {
    if history.is_empty() {
        return;
    }
    let (idx, span) = if st.prev_action == Action::LastArg
        && let Some((i, s, e)) = st.lastarg
    {
        (i.saturating_sub(1), Some((s, e)))
    } else {
        (history.len() - 1, None)
    };
    let word = history[idx].split_whitespace().last().unwrap_or("").to_string();
    let start = match span {
        Some((s, e)) => {
            st.buffer.replace_range(s..e, "");
            s
        }
        None => st.cursor,
    };
    st.buffer.insert_str(start, &word);
    st.cursor = start + word.len();
    st.lastarg = Some((idx, start, start + word.len()));
    st.this_action = Action::LastArg;
}

/// C-v / C-q: insert the next key literally (a tab, an ESC, a ^C…);
/// the render shows it `^X`-style.
#[cfg(unix)]
fn quoted_insert(st: &mut LineState) -> io::Result<()> {
    if let Some(b) = read_byte()? {
        let c = if b < 0x80 { b as char } else { read_utf8(b)? };
        insert_char(st, c);
        st.this_action = Action::Insert;
    }
    Ok(())
}

/// C-x C-e (emacs) / `v` (vi normal): hand the line to `$VISUAL`/`$EDITOR`
/// in a temp file; on a clean exit the edited text is returned and — like
/// bash — executed immediately.
#[cfg(unix)]
fn edit_in_editor(st: &mut LineState, raw: &RawMode) -> io::Result<Option<String>> {
    let path = std::env::temp_dir().join(format!("rush-edit-{}.sh", std::process::id()));
    std::fs::write(&path, &st.buffer)?;
    finish_line(st)?;
    raw.suspend();
    let editor = crate::vars::get("VISUAL")
        .filter(|e| !e.is_empty())
        .or_else(|| crate::vars::get("EDITOR").filter(|e| !e.is_empty()))
        .unwrap_or_else(|| "vi".to_string());
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("{editor} {}", path.display()))
        .status();
    raw.resume();
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let _ = std::fs::remove_file(&path);
    match status {
        Ok(s) if s.success() => Ok(Some(text.trim_end_matches('\n').to_string())),
        _ => {
            // Editor failed or was declined: repaint on a fresh region.
            st.painted_rows = 1;
            st.painted_cursor_row = 0;
            Ok(None)
        }
    }
}

#[cfg(unix)]
fn history_prev(st: &mut LineState, history: &[String]) {
    let next_index = match st.hist_index {
        None if history.is_empty() => return,
        None => {
            st.draft = st.buffer.clone();
            history.len() - 1
        }
        Some(0) => 0,
        Some(i) => i - 1,
    };
    st.hist_index = Some(next_index);
    st.buffer = history[next_index].clone();
    st.cursor = st.buffer.len();
}

#[cfg(unix)]
fn history_next(st: &mut LineState, history: &[String]) {
    match st.hist_index {
        None => {}
        Some(i) if i + 1 < history.len() => {
            st.hist_index = Some(i + 1);
            st.buffer = history[i + 1].clone();
            st.cursor = st.buffer.len();
        }
        Some(_) => {
            st.hist_index = None;
            st.buffer = std::mem::take(&mut st.draft);
            st.cursor = st.buffer.len();
        }
    }
}

/// M-<: jump to the oldest history entry.
#[cfg(unix)]
fn history_first(st: &mut LineState, history: &[String]) {
    if history.is_empty() {
        return;
    }
    if st.hist_index.is_none() {
        st.draft = st.buffer.clone();
    }
    st.hist_index = Some(0);
    st.buffer = history[0].clone();
    st.cursor = st.buffer.len();
}

/// M->: back to the live (draft) line.
#[cfg(unix)]
fn history_last(st: &mut LineState) {
    if st.hist_index.is_some() {
        st.hist_index = None;
        st.buffer = std::mem::take(&mut st.draft);
        st.cursor = st.buffer.len();
    }
}

/// PageUp / M-p: previous history entry starting with the text before the
/// cursor (zsh's `history-beginning-search-backward`, fish's Up).
#[cfg(unix)]
fn history_prefix_prev(st: &mut LineState, history: &[String]) {
    if st.hist_index.is_none() {
        st.prefix = st.buffer[..st.cursor].to_string();
        st.draft = st.buffer.clone();
    }
    let below = st.hist_index.unwrap_or(history.len());
    if let Some(i) = history[..below.min(history.len())]
        .iter()
        .rposition(|h| h.starts_with(&st.prefix) && *h != st.buffer)
    {
        st.hist_index = Some(i);
        st.buffer = history[i].clone();
        st.cursor = st.buffer.len();
    }
}

/// PageDown / M-n: next prefix match, or back to the draft.
#[cfg(unix)]
fn history_prefix_next(st: &mut LineState, history: &[String]) {
    let Some(cur) = st.hist_index else { return };
    if let Some(off) = history[cur + 1..].iter().position(|h| h.starts_with(&st.prefix)) {
        let i = cur + 1 + off;
        st.hist_index = Some(i);
        st.buffer = history[i].clone();
        st.cursor = st.buffer.len();
    } else {
        st.hist_index = None;
        st.buffer = std::mem::take(&mut st.draft);
        st.cursor = st.buffer.len();
    }
}

/// Tab completion (C34's sources, C69's list display): insert the longest
/// common prefix; when that makes no progress, print the columned
/// candidate list below the line.
#[cfg(unix)]
fn complete_at_cursor(st: &mut LineState) -> io::Result<()> {
    let (start, candidates) = crate::completion::complete(&st.buffer, st.cursor);
    if candidates.is_empty() {
        return Ok(());
    }
    let lcp = longest_common_prefix(&candidates.iter().map(|c| c.replacement.as_str()).collect::<Vec<_>>());
    let current = &st.buffer[start..st.cursor];
    if lcp.len() > current.len() {
        st.buffer.replace_range(start..st.cursor, &lcp);
        st.cursor = start + lcp.len();
        return Ok(());
    }
    if candidates.len() > 1 {
        // Leave the edit region and print the columned list; the next
        // render starts a fresh region below it.
        finish_line(st)?;
        let width = candidates.iter().map(|c| display_width(&c.display)).max().unwrap_or(0) + 2;
        let cols = (term_cols() / width.max(1)).max(1);
        for chunk in candidates.chunks(cols) {
            let row: Vec<String> =
                chunk.iter().map(|c| format!("{:<w$}", c.display, w = width)).collect();
            println!("{}", row.join("").trim_end());
        }
        st.painted_rows = 1;
        st.painted_cursor_row = 0;
    }
    Ok(())
}

fn longest_common_prefix(names: &[&str]) -> String {
    let Some(first) = names.first() else {
        return String::new();
    };
    let mut prefix = first.to_string();
    for name in &names[1..] {
        while !name.starts_with(&prefix) {
            prefix.pop();
            if prefix.is_empty() {
                return prefix;
            }
        }
    }
    prefix
}

/// Repaint the whole edit region and reposition the cursor.
///
/// Layout math: everything is measured in display columns (ANSI-skipped,
/// wide-character-aware, control characters via `visualize`). When a
/// painted row ends exactly at the terminal width, a newline is emitted to
/// force the wrap immediately — sidestepping terminals' delayed-wrap
/// state, which would otherwise break the relative cursor movements the
/// next repaint starts with.
#[cfg(unix)]
fn render(st: &mut LineState, history: &[String]) -> io::Result<()> {
    let cols = term_cols().max(2);
    let mut out = String::new();

    // Return to the region's first row/column and clear everything below.
    out.push('\r');
    if st.painted_cursor_row > 0 {
        out.push_str(&format!("\x1b[{}A", st.painted_cursor_row));
    }
    out.push_str("\x1b[J");

    // Search mode paints its own prompt instead of PS1/buffer.
    if let Some(search) = &st.search {
        let shown = search.hit.map(|i| history[i].as_str()).unwrap_or("");
        let label = if search.forward { "(i-search)" } else { "(reverse-i-search)" };
        let line = format!("{label}`{}': {}", search.query, visualize(shown));
        out.push_str(&line);
        let w = display_width(&line);
        st.painted_rows = w / cols + 1;
        st.painted_cursor_row = w / cols;
        print!("{out}");
        return io::stdout().flush();
    }

    let vis = visualize(&st.buffer);
    let highlighted = crate::completion::highlight_line(&vis);
    let hint = if st.cursor == st.buffer.len() {
        visualize(&crate::completion::hint(&st.buffer, history).unwrap_or_default())
    } else {
        String::new()
    };

    let wp = display_width(st.prompt);
    let wb = display_width(&vis);
    let wh = display_width(&hint);
    let wcursor = wp + display_width(&visualize(&st.buffer[..st.cursor]));
    let wtotal = wp + wb + wh;

    out.push_str(st.prompt);
    out.push_str(&highlighted);
    if !hint.is_empty() {
        out.push_str(&format!("\x1b[2m{hint}\x1b[0m"));
    }

    // The right prompt (C71): shown while everything fits on one row with
    // a gap; hidden (zsh-style) once the line grows into it.
    let wr = display_width(st.rprompt);
    if wr > 0 && wtotal + wr + 1 < cols {
        out.push_str(&format!("\x1b[{}G", cols - wr + 1));
        out.push_str(st.rprompt);
    }

    // Force the wrap when the content ends exactly on the boundary —
    // after which the cursor sits at (wtotal / cols, 0) either way.
    if wtotal > 0 && wtotal.is_multiple_of(cols) {
        out.push_str("\r\n");
    }

    let total_rows = wtotal / cols + 1;
    let end_row = wtotal / cols;
    let cursor_row = wcursor / cols;
    let cursor_col = wcursor % cols;

    // Reposition from the end of the paint to the cursor.
    let up = end_row.saturating_sub(cursor_row);
    if up > 0 {
        out.push_str(&format!("\x1b[{up}A"));
    }
    out.push_str(&format!("\r\x1b[{}G", cursor_col + 1));

    st.painted_rows = total_rows;
    st.painted_cursor_row = cursor_row;

    print!("{out}");
    io::stdout().flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn state(buf: &str, cursor: usize) -> LineState<'static> {
        LineState {
            buffer: buf.to_string(),
            cursor,
            prompt: "",
            rprompt: "",
            painted_rows: 1,
            painted_cursor_row: 0,
            hist_index: None,
            draft: String::new(),
            prefix: String::new(),
            vi: false,
            vi_normal: false,
            vi_count: 0,
            vi_op: None,
            vi_find: None,
            vi_replace: false,
            last_find: None,
            search: None,
            undo: Vec::new(),
            prev_action: Action::Other,
            this_action: Action::Other,
            yank: None,
            lastarg: None,
        }
    }

    #[test]
    fn width_skips_ansi_and_counts_wide_chars() {
        assert_eq!(display_width("plain"), 5);
        assert_eq!(display_width("\x1b[32mgreen\x1b[0m"), 5);
        assert_eq!(display_width("日本"), 4); // two double-width chars
        assert_eq!(display_width(""), 0);
    }

    #[test]
    fn common_prefix() {
        assert_eq!(longest_common_prefix(&["echo", "ech", "echelon"]), "ech");
        assert_eq!(longest_common_prefix(&["abc"]), "abc");
        assert_eq!(longest_common_prefix(&["x", "y"]), "");
        assert_eq!(longest_common_prefix(&[]), "");
    }

    #[test]
    fn word_motion() {
        assert_eq!(word_back("echo hello", 10), 5);
        assert_eq!(word_back("echo hello", 5), 0);
        assert_eq!(word_back("word", 4), 0);
    }

    #[test]
    fn word_motion_alnum() {
        // M-b/M-f stop at non-alphanumerics, unlike C-w's whitespace rule.
        assert_eq!(word_back_alnum("a-b/c.txt", 9), 6); // back over "txt"
        assert_eq!(word_back_alnum("echo hello", 10), 5);
        assert_eq!(word_back_alnum("  ", 2), 0); // no word: to line start
        assert_eq!(word_forward_alnum("a-b/c.txt", 0), 1); // just "a"
        assert_eq!(word_forward_alnum("--foo bar", 0), 5); // skips the dashes
    }

    #[test]
    fn vi_small_words() {
        //           0123456789
        let s = "ab cd-ef gh";
        assert_eq!(vi_word_fwd(s, 0), 3); // ab → cd
        assert_eq!(vi_word_fwd(s, 3), 5); // cd → the "-" (its own word)
        assert_eq!(vi_word_fwd(s, 5), 6); // "-" → ef
        assert_eq!(vi_word_back(s, 6), 5); // ef → "-"
        assert_eq!(vi_word_back(s, 3), 0);
        assert_eq!(vi_word_end(s, 0), 1); // on the "b" of "ab"
        assert_eq!(vi_word_end(s, 1), 4); // b → d of "cd"
    }

    #[cfg(unix)]
    #[test]
    fn vi_find_targets() {
        //       0123456
        let s = "echo ab";
        assert_eq!(vi_find_target(s, 0, 'f', 'o'), Some((3, true)));
        assert_eq!(vi_find_target(s, 0, 't', 'o'), Some((2, true)));
        assert_eq!(vi_find_target(s, 6, 'F', 'e'), Some((0, false)));
        assert_eq!(vi_find_target(s, 6, 'T', 'e'), Some((1, false)));
        assert_eq!(vi_find_target(s, 0, 'f', 'z'), None);
    }

    #[test]
    fn visualize_control_chars() {
        #[cfg(unix)]
        {
            assert_eq!(visualize("plain"), "plain");
            assert_eq!(visualize("a\x1bb"), "a^[b");
            assert_eq!(visualize("a\tb"), "a    b");
            assert_eq!(visualize("a\nb"), "a⏎b");
            assert_eq!(visualize("\u{7f}"), "^?");
        }
    }

    #[cfg(unix)]
    #[test]
    fn csi_sequences() {
        assert_eq!(csi_key("", b'A'), Key::Up);
        assert_eq!(csi_key("", b'D'), Key::Left);
        assert_eq!(csi_key("3", b'~'), Key::Delete);
        assert_eq!(csi_key("1", b'~'), Key::Home);
        assert_eq!(csi_key("1;5", b'C'), Key::WordRight);
        assert_eq!(csi_key("1;3", b'D'), Key::WordLeft);
        assert_eq!(csi_key("5", b'~'), Key::PageUp);
        assert_eq!(csi_key("6", b'~'), Key::PageDown);
        assert_eq!(csi_key("99", b'~'), Key::Other);
    }

    #[test]
    fn history_dedups_consecutive_only() {
        let mut ed = Editor::new();
        ed.add_history_entry("a");
        ed.add_history_entry("a");
        ed.add_history_entry("b");
        ed.add_history_entry("a");
        assert_eq!(ed.history(), &["a", "b", "a"]);
    }

    #[test]
    fn history_joins_multiline_entries() {
        let mut ed = Editor::new();
        ed.add_history_entry("echo a\necho b");
        assert_eq!(ed.history(), &["echo a; echo b"]);
    }

    #[cfg(unix)]
    #[test]
    fn kill_ring_appends_consecutive_kills() {
        let mut ring = Vec::new();
        let mut st = state("one two three", 13);
        // C-w twice: "three" then "two " prepends onto the same entry.
        let (start, cur) = (word_back(&st.buffer, st.cursor), st.cursor);
        kill_span(&mut st, &mut ring, start, cur, false);
        st.prev_action = st.this_action;
        let (start, cur) = (word_back(&st.buffer, st.cursor), st.cursor);
        kill_span(&mut st, &mut ring, start, cur, false);
        assert_eq!(st.buffer, "one ");
        assert_eq!(ring, vec!["two three".to_string()]);
        // A yank restores both words at once.
        st.prev_action = Action::Other;
        yank(&mut st, &ring);
        assert_eq!(st.buffer, "one two three");
    }

    #[cfg(unix)]
    #[test]
    fn yank_pop_rotates_ring() {
        let mut ring = vec!["old".to_string(), "new".to_string()];
        let mut st = state("", 0);
        yank(&mut st, &ring);
        assert_eq!(st.buffer, "new");
        st.prev_action = st.this_action;
        yank_pop(&mut st, &ring);
        assert_eq!(st.buffer, "old");
        st.prev_action = st.this_action;
        yank_pop(&mut st, &ring);
        assert_eq!(st.buffer, "new");
        let _ = &mut ring;
    }

    #[cfg(unix)]
    #[test]
    fn transpose_words_swaps_around_cursor() {
        let mut st = state("echo one two", 12);
        transpose_words(&mut st);
        assert_eq!(st.buffer, "echo two one");
        assert_eq!(st.cursor, 12);
        // A single word: no-op.
        let mut st = state("word", 4);
        transpose_words(&mut st);
        assert_eq!(st.buffer, "word");
    }

    #[cfg(unix)]
    #[test]
    fn case_ops() {
        let mut st = state("echo word here", 5);
        case_word(&mut st, CaseOp::Upper);
        assert_eq!(st.buffer, "echo WORD here");
        assert_eq!(st.cursor, 9);
        let mut st = state("echo WORD", 5);
        case_word(&mut st, CaseOp::Lower);
        assert_eq!(st.buffer, "echo word");
        let mut st = state("echo wOrD", 5);
        case_word(&mut st, CaseOp::Capital);
        assert_eq!(st.buffer, "echo Word");
    }

    #[cfg(unix)]
    #[test]
    fn last_arg_inserts_and_cycles() {
        let history = vec!["echo first one".to_string(), "echo second two".to_string()];
        let mut st = state("echo ", 5);
        insert_last_arg(&mut st, &history);
        assert_eq!(st.buffer, "echo two");
        st.prev_action = st.this_action;
        insert_last_arg(&mut st, &history);
        assert_eq!(st.buffer, "echo one");
    }

    #[cfg(unix)]
    #[test]
    fn undo_restores_snapshots() {
        let mut st = state("hello", 5);
        st.undo.push(("hell".to_string(), 4));
        st.undo.push(("hello".to_string(), 5));
        undo_cmd(&mut st);
        assert_eq!(st.buffer, "hello");
        undo_cmd(&mut st);
        assert_eq!(st.buffer, "hell");
        assert_eq!(st.cursor, 4);
        undo_cmd(&mut st); // empty stack: no-op
        assert_eq!(st.buffer, "hell");
    }

    #[cfg(unix)]
    #[test]
    fn prefix_search_matches_start_only() {
        let history =
            vec!["git status".to_string(), "echo git".to_string(), "git push".to_string()];
        let mut st = state("git", 3);
        history_prefix_prev(&mut st, &history);
        assert_eq!(st.buffer, "git push");
        history_prefix_prev(&mut st, &history);
        assert_eq!(st.buffer, "git status"); // skipped "echo git"
        history_prefix_next(&mut st, &history);
        assert_eq!(st.buffer, "git push");
        history_prefix_next(&mut st, &history);
        assert_eq!(st.buffer, "git"); // back to the draft
    }
}
