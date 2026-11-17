//! Serialize the terminal (scrollback + screen) to an ANSI byte stream that a
//! fresh terminal of the same size can `write()` to reproduce it — what a remote
//! client receives when it attaches to a pane (it used to come from xterm.js's
//! SerializeAddon).

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::TermMode;
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::vte::ansi::{Color, NamedColor};

use crate::Terminal;

#[derive(Clone, Copy, PartialEq)]
struct Pen {
    fg: Color,
    bg: Color,
    flags: Flags,
}

const STYLE_FLAGS: Flags = Flags::BOLD
    .union(Flags::ITALIC)
    .union(Flags::DIM)
    .union(Flags::INVERSE)
    .union(Flags::HIDDEN)
    .union(Flags::STRIKEOUT)
    .union(Flags::ALL_UNDERLINES);

impl Pen {
    const DEFAULT: Pen = Pen {
        fg: Color::Named(NamedColor::Foreground),
        bg: Color::Named(NamedColor::Background),
        flags: Flags::empty(),
    };

    fn of(cell: &Cell) -> Pen {
        Pen {
            fg: cell.fg,
            bg: cell.bg,
            flags: cell.flags & STYLE_FLAGS,
        }
    }

    fn sgr(&self, out: &mut String) {
        out.push_str("\x1b[0");
        for (f, code) in [
            (Flags::BOLD, ";1"),
            (Flags::DIM, ";2"),
            (Flags::ITALIC, ";3"),
            (Flags::INVERSE, ";7"),
            (Flags::HIDDEN, ";8"),
            (Flags::STRIKEOUT, ";9"),
        ] {
            if self.flags.contains(f) {
                out.push_str(code);
            }
        }
        if self.flags.intersects(Flags::ALL_UNDERLINES) {
            out.push_str(";4");
        }
        color(out, self.fg, true);
        color(out, self.bg, false);
        out.push('m');
    }
}

fn color(out: &mut String, c: Color, fg: bool) {
    use std::fmt::Write;
    let base = if fg { 30 } else { 40 };
    match c {
        Color::Named(n) => {
            let i = n as usize;
            if i < 8 {
                let _ = write!(out, ";{}", base + i);
            } else if i < 16 {
                let _ = write!(out, ";{}", base + 60 + i - 8);
            } else if (NamedColor::DimBlack as usize..=NamedColor::DimWhite as usize).contains(&i) {
                let _ = write!(out, ";{}", base + i - NamedColor::DimBlack as usize);
            }
            // Foreground/Background/Cursor: the default (reset by `0`).
        }
        Color::Indexed(i) => {
            let _ = write!(out, ";{};5;{i}", base + 8);
        }
        Color::Spec(rgb) => {
            let _ = write!(out, ";{};2;{};{};{}", base + 8, rgb.r, rgb.g, rgb.b);
        }
    }
}

impl Terminal {
    /// ANSI dump of history + screen, ending with the cursor where it is.
    pub fn snapshot_ansi(&self) -> String {
        let g = self.lock();
        let t = &g.term;
        let grid = t.grid();
        let cols = grid.columns();
        let mode = *t.mode();
        let mut out = String::with_capacity(4096);
        let alt = mode.contains(TermMode::ALT_SCREEN);
        if alt {
            out.push_str("\x1b[?1049h\x1b[H\x1b[2J");
        }
        let top = if alt { Line(0) } else { grid.topmost_line() };
        let bottom = grid.bottommost_line();
        let mut pen = Pen::DEFAULT;
        let mut y = top;
        while y <= bottom {
            let row = &grid[y];
            let wrapped = row[Column(cols - 1)].flags.contains(Flags::WRAPLINE);
            // Trailing default blanks are dropped (unless the line wraps on).
            let mut end = cols;
            if !wrapped {
                while end > 0 {
                    let c = &row[Column(end - 1)];
                    if (c.c == ' ' || c.c == '\0') && Pen::of(c) == Pen::DEFAULT {
                        end -= 1;
                    } else {
                        break;
                    }
                }
            }
            for x in 0..end {
                let cell = &row[Column(x)];
                if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                    continue;
                }
                let p = Pen::of(cell);
                if p != pen {
                    p.sgr(&mut out);
                    pen = p;
                }
                out.push(if cell.c == '\0' { ' ' } else { cell.c });
                if let Some(zw) = cell.zerowidth() {
                    out.extend(zw.iter());
                }
            }
            if y < bottom && !wrapped {
                if pen != Pen::DEFAULT {
                    out.push_str("\x1b[0m");
                    pen = Pen::DEFAULT;
                }
                out.push_str("\r\n");
            }
            y += 1;
        }
        if pen != Pen::DEFAULT {
            out.push_str("\x1b[0m");
        }
        let c = grid.cursor.point;
        out.push_str(&format!("\x1b[{};{}H", c.line.0 + 1, c.column.0 + 1));
        if !mode.contains(TermMode::SHOW_CURSOR) {
            out.push_str("\x1b[?25l");
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use crate::{Size, Terminal};
    use std::sync::Arc;

    fn term(cols: u16, rows: u16) -> Terminal {
        Terminal::new(Size { cols, rows }, 100, Arc::new(|_| {}))
    }

    #[test]
    fn round_trips_text_colors_and_cursor() {
        let a = term(20, 4);
        a.advance(b"one\r\n\x1b[31mred\x1b[0m two\r\nthree\r\nfour\r\nfive");
        let dump = a.snapshot_ansi();
        let b = term(20, 4);
        b.advance(dump.as_bytes());
        assert_eq!(a.screen_text(), b.screen_text());
        assert_eq!(
            a.lock().term.grid().cursor.point,
            b.lock().term.grid().cursor.point
        );
        assert!(dump.contains("\x1b[0;31"));
        // History came along too.
        assert!(dump.starts_with("one"));
    }

    #[test]
    fn wrapped_lines_stay_joined() {
        let a = term(5, 3);
        a.advance(b"abcdefgh");
        let b = term(5, 3);
        b.advance(a.snapshot_ansi().as_bytes());
        assert_eq!(a.screen_text(), b.screen_text());
    }
}
