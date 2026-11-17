//! Terminal emulation for SplitView (the same design as SecureWire's `sw_term`).
//!
//! * [`Terminal`] wraps `alacritty_terminal::Term` + the VT parser behind a fair
//!   mutex. The PTY reader thread calls [`Terminal::advance`] with output bytes;
//!   the UI thread locks it briefly per frame to draw.
//! * [`view::TerminalView`] is the egui widget: renderer, keyboard/mouse input,
//!   selection, clipboard. It never does I/O — it returns the bytes to write and
//!   the new size, and the caller forwards them to the PTY.
//!
//! Scrollback lives here, in the emulator; the PTY side is a dumb pipe.

pub mod input;
pub mod snapshot;
pub mod view;

use std::sync::Arc;

use alacritty_terminal::event::{Event as AtEvent, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::{Processor, StdSyncHandler};

pub use view::{Palette, TerminalView, ViewOutput};

/// Events the emulator raises that the owner must act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TermEvent {
    /// Bytes the terminal must send back to the PTY (replies to device-attribute
    /// or cursor-position queries).
    PtyWrite(Vec<u8>),
    Title(String),
    ResetTitle,
    Bell,
}

pub type EventSink = Arc<dyn Fn(TermEvent) + Send + Sync>;

#[derive(Clone)]
pub struct Proxy(EventSink);

impl EventListener for Proxy {
    fn send_event(&self, ev: AtEvent) {
        let out = match ev {
            AtEvent::PtyWrite(s) => TermEvent::PtyWrite(s.into_bytes()),
            AtEvent::Title(t) => TermEvent::Title(t),
            AtEvent::ResetTitle => TermEvent::ResetTitle,
            AtEvent::Bell => TermEvent::Bell,
            // Clipboard access by the program (OSC 52) and color queries are
            // deliberately not honoured.
            _ => return,
        };
        (self.0)(out);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Size {
    pub cols: u16,
    pub rows: u16,
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.rows as usize
    }
    fn screen_lines(&self) -> usize {
        self.rows as usize
    }
    fn columns(&self) -> usize {
        self.cols as usize
    }
}

pub struct Inner {
    pub term: Term<Proxy>,
    parser: Processor<StdSyncHandler>,
}

/// Shared terminal state. Cheap to clone.
#[derive(Clone)]
pub struct Terminal {
    inner: Arc<FairMutex<Inner>>,
}

impl std::fmt::Debug for Terminal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Terminal")
            .field("size", &self.size())
            .finish()
    }
}

impl Terminal {
    pub fn new(size: Size, scrollback: usize, sink: EventSink) -> Self {
        let config = Config {
            scrolling_history: scrollback,
            ..Config::default()
        };
        let term = Term::new(config, &size, Proxy(sink));
        Self {
            inner: Arc::new(FairMutex::new(Inner {
                term,
                parser: Processor::new(),
            })),
        }
    }

    /// Feed bytes read from the PTY.
    pub fn advance(&self, bytes: &[u8]) {
        let mut g = self.inner.lock();
        let Inner { term, parser } = &mut *g;
        parser.advance(term, bytes);
    }

    pub fn size(&self) -> Size {
        let g = self.inner.lock();
        Size {
            cols: g.term.columns() as u16,
            rows: g.term.screen_lines() as u16,
        }
    }

    pub fn resize(&self, size: Size) {
        self.inner.lock().term.resize(size);
    }

    pub fn lock(&self) -> parking_lot::MutexGuard<'_, Inner> {
        self.inner.lock()
    }

    /// Plain text of the visible screen (tests, previews).
    pub fn screen_text(&self) -> String {
        let g = self.inner.lock();
        let mut out = String::new();
        let mut line = None;
        for cell in g.term.grid().display_iter() {
            if line.is_some_and(|l| l != cell.point.line) {
                out.truncate(out.trim_end_matches(' ').len());
                out.push('\n');
            }
            line = Some(cell.point.line);
            out.push(cell.c);
        }
        out.truncate(out.trim_end_matches(' ').len());
        out
    }

    /// The last non-empty line at or above the cursor, skipping the injected
    /// two-line prompt's frame (`┌─…`, `└──╼`): a backgrounded dev server should
    /// be identified by what it printed, not by the prompt.
    pub fn preview_line(&self) -> String {
        let g = self.inner.lock();
        let grid = g.term.grid();
        let cols = grid.columns();
        let top = grid.topmost_line().0;
        let line_text = |y: i32| -> String {
            let row = &grid[Line(y)];
            (0..cols)
                .map(|x| row[Column(x)].c)
                .collect::<String>()
                .trim()
                .to_owned()
        };
        let cursor = grid.cursor.point.line.0;
        // If the cursor sits in the injected two-line prompt, start above its
        // top line (`┌─…`), which may have wrapped onto several rows.
        let mut y = cursor;
        for back in 0..6 {
            let at = cursor - back;
            if at < top {
                break;
            }
            if line_text(at).starts_with('┌') {
                y = at - 1;
                break;
            }
        }
        let mut seen = 0;
        while y >= top && seen < 200 {
            let t = line_text(y);
            if !t.is_empty() && !t.starts_with('┌') && !t.starts_with('└') {
                return t.chars().take(200).collect();
            }
            y -= 1;
            seen += 1;
        }
        String::new()
    }

    /// The text of the line under a visible row/column, and the column — for
    /// finding a URL under the mouse.
    pub fn line_at(&self, row: usize) -> String {
        let g = self.inner.lock();
        let grid = g.term.grid();
        let offset = grid.display_offset() as i32;
        let line = Line(row as i32 - offset);
        if line < grid.topmost_line() || line > grid.bottommost_line() {
            return String::new();
        }
        let r = &grid[line];
        (0..grid.columns()).map(|x| r[Column(x)].c).collect()
    }
}

/// The `http(s)://…` URL covering `col` in `line`, if any.
pub fn url_at(line: &str, col: usize) -> Option<String> {
    let chars: Vec<char> = line.chars().collect();
    let text: String = chars.iter().collect();
    for scheme in ["https://", "http://"] {
        let mut from = 0;
        while let Some(p) = text[from..].find(scheme) {
            let start_b = from + p;
            let start = text[..start_b].chars().count();
            let mut end = start;
            while end < chars.len()
                && !chars[end].is_whitespace()
                && !matches!(chars[end], '"' | '\'' | '<' | '>' | '`')
            {
                end += 1;
            }
            // Trailing punctuation is almost never part of the URL.
            while end > start && matches!(chars[end - 1], '.' | ',' | ')' | ';' | ':' | ']') {
                end -= 1;
            }
            if (start..end).contains(&col) {
                return Some(chars[start..end].iter().collect());
            }
            from = start_b + scheme.len();
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn term() -> (Terminal, Arc<Mutex<Vec<TermEvent>>>) {
        let events = Arc::new(Mutex::new(Vec::new()));
        let e2 = events.clone();
        let t = Terminal::new(
            Size { cols: 20, rows: 4 },
            100,
            Arc::new(move |e| e2.lock().unwrap().push(e)),
        );
        (t, events)
    }

    #[test]
    fn prints_and_wraps() {
        let (t, _) = term();
        t.advance(b"hello\r\nworld \x1b[31mred\x1b[0m");
        assert_eq!(
            t.screen_text().lines().take(2).collect::<Vec<_>>(),
            ["hello", "world red"]
        );
    }

    #[test]
    fn answers_device_attributes_and_sets_title() {
        let (t, ev) = term();
        t.advance(b"\x1b]0;my title\x07\x1b[c");
        let ev = ev.lock().unwrap();
        assert!(ev.contains(&TermEvent::Title("my title".into())));
        assert!(
            ev.iter()
                .any(|e| matches!(e, TermEvent::PtyWrite(b) if b.starts_with(b"\x1b[")))
        );
    }

    #[test]
    fn resize_changes_size() {
        let (t, _) = term();
        t.resize(Size { cols: 80, rows: 24 });
        assert_eq!(t.size(), Size { cols: 80, rows: 24 });
    }

    #[test]
    fn preview_skips_prompt_frame() {
        let (t, _) = term();
        t.advance("listening on :3000\r\n┌─[me@box]─[~]\r\n└──╼ $ ".as_bytes());
        assert_eq!(t.preview_line(), "listening on :3000");
        // A prompt whose first line wrapped (narrow pane) is skipped whole.
        let (t, _) = term();
        t.advance("ready\r\n┌─[me@box]─[/a/very/long/path/x]\r\n└──╼ $ ".as_bytes());
        assert_eq!(t.preview_line(), "ready");
    }

    #[test]
    fn finds_urls() {
        let l = "see https://example.com/a?b=1. ok";
        assert_eq!(url_at(l, 6).as_deref(), Some("https://example.com/a?b=1"));
        assert_eq!(url_at(l, 2), None);
        assert_eq!(url_at("x http://a.b", 11).as_deref(), Some("http://a.b"));
    }
}
