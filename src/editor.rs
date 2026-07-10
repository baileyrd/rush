//! Hand-rolled line editor — rush's own replacement for `rustyline` (C71's
//! unblocking, and a deliberate roll-our-own decision in the spirit of the
//! hand-rolled lexer/parser/glob matcher).
//!
//! Layers, bottom to top:
//!   * raw terminal mode (termios) behind an RAII guard, so every exit path
//!     — including panics — restores the terminal;
//!   * key decoding: UTF-8 assembly plus escape-sequence parsing (CSI/SS3,
//!     Alt- chords), with a short poll to tell a lone ESC from a sequence;
//!   * a render engine that repaints the whole edit region per keystroke:
//!     display-width math (via `unicode_width`, ANSI-aware), soft-wrap row
//!     accounting, forced wraps at exact column boundaries (avoiding the
//!     delayed-wrap ambiguity), syntax highlighting (C68), the dimmed
//!     history hint (C33), and — newly possible now that rush owns this
//!     layer — the right-side prompt `$RPS1` (C71), shown while the first
//!     row has room for it;
//!   * keymaps: the emacs set by default, plus a documented vi-mode subset
//!     when `set -o vi` (C73) is active — checked live on every
//!     `read_line`, so switching needs no editor rebuild at all;
//!   * history: in-memory with consecutive-dedup, file persistence
//!     (tolerating rustyline's old `#V2` header on load), Up/Down
//!     navigation with draft preservation, and Ctrl-R incremental reverse
//!     search;
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
        Editor { history: Vec::new() }
    }

    pub fn history(&self) -> &[String] {
        &self.history
    }

    /// Append to history, skipping a consecutive duplicate.
    pub fn add_history_entry(&mut self, line: &str) {
        if self.history.last().map(String::as_str) != Some(line) {
            self.history.push(line.to_string());
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

/// One decoded key press.
#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Key {
    Char(char),
    Ctrl(char), // Ctrl('a') for ^A …
    Alt(char),
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
    Esc,
    Other,
}

/// Raw-mode RAII guard: restores the saved termios on drop, whatever the
/// exit path.
#[cfg(unix)]
struct RawMode {
    saved: libc::termios,
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
            // Input: no Ctrl-S/Q flow control, no CR→NL mangling. Local:
            // no canonical buffering, no echo, no signal generation (^C
            // becomes a key we handle), no ^V. Output stays cooked so
            // ordinary `println!` keeps working for lists and job notices.
            t.c_iflag &= !(libc::IXON | libc::ICRNL | libc::INLCR);
            t.c_lflag &= !(libc::ICANON | libc::ECHO | libc::ISIG | libc::IEXTEN);
            t.c_cc[libc::VMIN] = 1;
            t.c_cc[libc::VTIME] = 0;
            if libc::tcsetattr(0, libc::TCSADRAIN, &t) != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(RawMode { saved })
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
/// pure, so the quirk table is unit-testable.
#[cfg(unix)]
fn csi_key(params: &str, final_byte: u8) -> Key {
    match (params, final_byte) {
        (_, b'A') => Key::Up,
        (_, b'B') => Key::Down,
        (_, b'C') => Key::Right,
        (_, b'D') => Key::Left,
        (_, b'H') => Key::Home,
        (_, b'F') => Key::End,
        ("1", b'~') | ("7", b'~') => Key::Home,
        ("4", b'~') | ("8", b'~') => Key::End,
        ("3", b'~') => Key::Delete,
        _ => Key::Other,
    }
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
                            Some(final_byte) => return Ok(Some(csi_key(&params, final_byte))),
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
                Some(c) if c.is_ascii_graphic() => Key::Alt(c as char),
                _ => Key::Other,
            }
        }
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
    /// History navigation: index into `history` (None = live line), and
    /// the draft stashed when navigation started.
    hist_index: Option<usize>,
    draft: String,
    /// vi mode (C73): true when in normal mode; `pending` holds a `d`
    /// awaiting its motion.
    vi: bool,
    vi_normal: bool,
    vi_pending: Option<char>,
    /// Ctrl-R incremental search, when active.
    search: Option<SearchState>,
}

#[cfg(unix)]
struct SearchState {
    query: String,
    /// Index into history of the current match, if any.
    hit: Option<usize>,
}

#[cfg(unix)]
fn read_line_raw(ed: &mut Editor, prompt: &str, rprompt: &str) -> io::Result<ReadResult> {
    let _raw = RawMode::enable()?;
    let mut st = LineState {
        buffer: String::new(),
        cursor: 0,
        prompt,
        rprompt,
        painted_rows: 1,
        painted_cursor_row: 0,
        hist_index: None,
        draft: String::new(),
        vi: crate::vars::edit_mode_vi(),
        vi_normal: false,
        vi_pending: None,
        search: None,
    };
    render(&mut st, &ed.history)?;

    loop {
        let Some(key) = read_key()? else {
            // EOF on stdin itself.
            finish_line(&mut st)?;
            return Ok(ReadResult::Eof);
        };

        // Ctrl-R search intercepts everything while active.
        if st.search.is_some() {
            match handle_search_key(&mut st, key, &ed.history)? {
                SearchOutcome::Continue => {
                    render(&mut st, &ed.history)?;
                    continue;
                }
                SearchOutcome::Accept => {
                    finish_line(&mut st)?;
                    return Ok(ReadResult::Line(st.buffer));
                }
                SearchOutcome::Exit => {
                    render(&mut st, &ed.history)?;
                    continue;
                }
            }
        }

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
                st.search = Some(SearchState { query: String::new(), hit: None });
            }
            Key::Ctrl('l') => {
                print!("\x1b[2J\x1b[H");
                st.painted_rows = 1;
                st.painted_cursor_row = 0;
            }
            Key::Tab => {
                complete_at_cursor(&mut st)?;
            }
            key if st.vi && st.vi_normal => handle_vi_normal(&mut st, key, &ed.history),
            key => handle_insert(&mut st, key, &ed.history),
        }
        render(&mut st, &ed.history)?;
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
fn handle_insert(st: &mut LineState, key: Key, history: &[String]) {
    match key {
        Key::Esc if st.vi => {
            st.vi_normal = true;
        }
        Key::Char(' ') => {
            // Abbreviations (C70): a space after a defined abbreviation in
            // command position rewrites it in place first.
            if let Some((start, expansion)) = crate::completion::abbr_expansion(&st.buffer, st.cursor) {
                st.buffer.replace_range(start..st.cursor, &expansion);
                st.cursor = start + expansion.len();
            }
            insert_char(st, ' ');
        }
        Key::Char(c) => insert_char(st, c),
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
        Key::End | Key::Ctrl('e') => st.cursor = st.buffer.len(),
        Key::Ctrl('k') => st.buffer.truncate(st.cursor),
        Key::Ctrl('u') => {
            st.buffer.replace_range(..st.cursor, "");
            st.cursor = 0;
        }
        Key::Ctrl('w') => {
            let start = word_back(&st.buffer, st.cursor);
            st.buffer.replace_range(start..st.cursor, "");
            st.cursor = start;
        }
        Key::Ctrl('t') => transpose(st),
        Key::Alt('b') => st.cursor = word_back(&st.buffer, st.cursor),
        Key::Alt('f') => st.cursor = word_forward(&st.buffer, st.cursor),
        Key::Up | Key::Ctrl('p') => history_prev(st, history),
        Key::Down | Key::Ctrl('n') => history_next(st, history),
        _ => {}
    }
}

/// The vi normal-mode subset (C73): motions `h l 0 $ b w e`, edits
/// `x X D dd dw db d$ d0`, inserts `i I a A`, history `k j`. A documented
/// subset, not the full modal language.
#[cfg(unix)]
fn handle_vi_normal(st: &mut LineState, key: Key, history: &[String]) {
    if let Some('d') = st.vi_pending {
        st.vi_pending = None;
        match key {
            Key::Char('d') => {
                st.buffer.clear();
                st.cursor = 0;
            }
            Key::Char('w') => {
                let end = word_forward(&st.buffer, st.cursor);
                st.buffer.replace_range(st.cursor..end, "");
            }
            Key::Char('b') => {
                let start = word_back(&st.buffer, st.cursor);
                st.buffer.replace_range(start..st.cursor, "");
                st.cursor = start;
            }
            Key::Char('$') => st.buffer.truncate(st.cursor),
            Key::Char('0') => {
                st.buffer.replace_range(..st.cursor, "");
                st.cursor = 0;
            }
            _ => {}
        }
        return;
    }
    match key {
        Key::Char('h') | Key::Left => {
            if let Some(prev) = prev_char_start(&st.buffer, st.cursor) {
                st.cursor = prev;
            }
        }
        Key::Char('l') | Key::Right => {
            if let Some(next) = next_char_end(&st.buffer, st.cursor) {
                st.cursor = next;
            }
        }
        Key::Char('0') | Key::Home => st.cursor = 0,
        Key::Char('$') | Key::End => st.cursor = st.buffer.len(),
        Key::Char('b') => st.cursor = word_back(&st.buffer, st.cursor),
        Key::Char('w') | Key::Char('e') => st.cursor = word_forward(&st.buffer, st.cursor),
        Key::Char('x') => {
            if let Some(next) = next_char_end(&st.buffer, st.cursor) {
                st.buffer.replace_range(st.cursor..next, "");
            }
        }
        Key::Char('X') => {
            if let Some(prev) = prev_char_start(&st.buffer, st.cursor) {
                st.buffer.replace_range(prev..st.cursor, "");
                st.cursor = prev;
            }
        }
        Key::Char('D') => st.buffer.truncate(st.cursor),
        Key::Char('d') => st.vi_pending = Some('d'),
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
        _ => {}
    }
}

#[cfg(unix)]
enum SearchOutcome {
    Continue,
    Accept,
    Exit,
}

/// Ctrl-R incremental reverse search (bash's `(reverse-i-search)`).
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
            let below = search.hit.unwrap_or(history.len());
            search.hit = find_match(history, &search.query, below).or(search.hit);
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

/// Start of the word before `pos` (whitespace-delimited).
fn word_back(s: &str, pos: usize) -> usize {
    let before = &s[..pos];
    let trimmed = before.trim_end();
    match trimmed.rfind(char::is_whitespace) {
        Some(i) => i + 1,
        None => 0,
    }
}

/// End of the word after `pos` (whitespace-delimited).
fn word_forward(s: &str, pos: usize) -> usize {
    let rest = &s[pos..];
    let skipped = rest.len() - rest.trim_start().len();
    match rest[skipped..].find(char::is_whitespace) {
        Some(i) => pos + skipped + i,
        None => s.len(),
    }
}

#[cfg(unix)]
fn transpose(st: &mut LineState) {
    if let Some(prev) = prev_char_start(&st.buffer, st.cursor)
        && let Some(next) = next_char_end(&st.buffer, st.cursor)
    {
        let a: String = st.buffer[prev..st.cursor].to_string();
        let b: String = st.buffer[st.cursor..next].to_string();
        st.buffer.replace_range(prev..next, &format!("{b}{a}"));
        st.cursor = prev + b.len() + a.len();
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
/// wide-character-aware). When a painted row ends exactly at the terminal
/// width, a newline is emitted to force the wrap immediately — sidestepping
/// terminals' delayed-wrap state, which would otherwise break the relative
/// cursor movements the next repaint starts with.
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
        let line = format!("(reverse-i-search)`{}': {}", search.query, shown);
        out.push_str(&line);
        let w = display_width(&line);
        st.painted_rows = w / cols + 1;
        st.painted_cursor_row = w / cols;
        print!("{out}");
        return io::stdout().flush();
    }

    let highlighted = crate::completion::highlight_line(&st.buffer);
    let hint = if st.cursor == st.buffer.len() {
        crate::completion::hint(&st.buffer, history).unwrap_or_default()
    } else {
        String::new()
    };

    let wp = display_width(st.prompt);
    let wb = display_width(&st.buffer);
    let wh = display_width(&hint);
    let wcursor = wp + display_width(&st.buffer[..st.cursor]);
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
        assert_eq!(word_forward("echo hello", 0), 4);
        assert_eq!(word_forward("echo hello", 4), 10);
    }

    #[cfg(unix)]
    #[test]
    fn csi_sequences() {
        assert_eq!(csi_key("", b'A'), Key::Up);
        assert_eq!(csi_key("", b'D'), Key::Left);
        assert_eq!(csi_key("3", b'~'), Key::Delete);
        assert_eq!(csi_key("1", b'~'), Key::Home);
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
}
