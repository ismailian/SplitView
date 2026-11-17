//! Picks SplitView's own OSC sequences out of a PTY byte stream.
//!
//! The emulator (alacritty_terminal) does not report OSC 7 (the shell's working
//! directory) or our private OSC 5170 (the injected `splitview` shell command)
//! to its listener, so the PTY reader thread runs every chunk through this
//! scanner before feeding the emulator. It is a small state machine that keeps
//! its state between chunks, because a sequence can be split across two reads.
//! The bytes themselves are not modified: the emulator still sees them and
//! ignores the OSCs it doesn't know.

/// What the shell reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Osc {
    /// OSC 7 payload, e.g. `file://host/home/x`.
    Cwd(String),
    /// OSC 5170 payload: the arguments of `splitview …`.
    AppCommand(String),
}

/// Longest payload kept; anything longer is dropped (a path is never this long,
/// and a hostile program must not make us buffer without bound).
const MAX_PAYLOAD: usize = 4096;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum State {
    #[default]
    Ground,
    /// Saw ESC.
    Esc,
    /// Inside `ESC ]`, reading the numeric command.
    Num,
    /// Reading the payload of a command we care about.
    Data,
    /// Inside a payload we don't care about: skip to its terminator.
    Skip,
    /// Saw ESC inside a payload (possible ST).
    DataEsc,
    SkipEsc,
}

#[derive(Debug, Default)]
pub struct OscScanner {
    state: State,
    num: u32,
    num_digits: u8,
    buf: Vec<u8>,
}

impl OscScanner {
    pub fn new() -> Self {
        Self::default()
    }

    /// Scan one chunk, appending any complete sequences to `out`.
    pub fn scan(&mut self, bytes: &[u8], out: &mut Vec<Osc>) {
        let mut i = 0;
        while i < bytes.len() {
            if self.state == State::Ground {
                // Fast path: jump to the next ESC.
                match bytes[i..].iter().position(|&b| b == 0x1b) {
                    Some(p) => {
                        i += p + 1;
                        self.state = State::Esc;
                    }
                    None => return,
                }
                continue;
            }
            let b = bytes[i];
            i += 1;
            self.state = match self.state {
                State::Ground => State::Ground,
                State::Esc => {
                    if b == b']' {
                        self.num = 0;
                        self.num_digits = 0;
                        State::Num
                    } else if b == 0x1b {
                        State::Esc
                    } else {
                        State::Ground
                    }
                }
                State::Num => match b {
                    b'0'..=b'9' if self.num_digits < 6 => {
                        self.num = self.num * 10 + (b - b'0') as u32;
                        self.num_digits += 1;
                        State::Num
                    }
                    b';' if self.num == 7 || self.num == 5170 => {
                        self.buf.clear();
                        State::Data
                    }
                    0x07 => State::Ground,
                    0x1b => State::SkipEsc,
                    _ => State::Skip,
                },
                State::Data => match b {
                    0x07 => {
                        self.finish(out);
                        State::Ground
                    }
                    0x1b => State::DataEsc,
                    0x18 | 0x1a => State::Ground,
                    _ => {
                        if self.buf.len() < MAX_PAYLOAD {
                            self.buf.push(b);
                            State::Data
                        } else {
                            State::Skip
                        }
                    }
                },
                State::DataEsc => {
                    if b == b'\\' {
                        self.finish(out);
                        State::Ground
                    } else if b == b']' {
                        // An ESC aborted this one and started another OSC.
                        self.num = 0;
                        self.num_digits = 0;
                        State::Num
                    } else {
                        State::Ground
                    }
                }
                State::Skip => match b {
                    0x07 | 0x18 | 0x1a => State::Ground,
                    0x1b => State::SkipEsc,
                    _ => State::Skip,
                },
                State::SkipEsc => {
                    if b == b']' {
                        self.num = 0;
                        self.num_digits = 0;
                        State::Num
                    } else {
                        State::Ground
                    }
                }
            };
        }
    }

    fn finish(&mut self, out: &mut Vec<Osc>) {
        let text = String::from_utf8_lossy(&self.buf).into_owned();
        self.buf.clear();
        out.push(match self.num {
            7 => Osc::Cwd(text),
            _ => Osc::AppCommand(text.trim().to_owned()),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan_all(chunks: &[&[u8]]) -> Vec<Osc> {
        let mut s = OscScanner::new();
        let mut out = Vec::new();
        for c in chunks {
            s.scan(c, &mut out);
        }
        out
    }

    #[test]
    fn finds_cwd_with_bel_and_st() {
        assert_eq!(
            scan_all(&[b"abc\x1b]7;file://h/tmp\x07def"]),
            vec![Osc::Cwd("file://h/tmp".into())]
        );
        assert_eq!(
            scan_all(&[b"\x1b]7;file://h/a\x1b\\"]),
            vec![Osc::Cwd("file://h/a".into())]
        );
    }

    #[test]
    fn handles_sequences_split_across_chunks() {
        assert_eq!(
            scan_all(&[b"x\x1b", b"]51", b"70;edit ", b"foo.txt \x1b", b"\\"]),
            vec![Osc::AppCommand("edit foo.txt".into())]
        );
    }

    #[test]
    fn ignores_other_oscs_and_csi() {
        assert_eq!(
            scan_all(&[b"\x1b]0;title\x07\x1b[31mred\x1b]2;t\x1b\\\x1b]7;file://h/b\x07"]),
            vec![Osc::Cwd("file://h/b".into())]
        );
        assert_eq!(scan_all(&[b"\x1b]77;nope\x07"]), vec![]);
    }

    #[test]
    fn bounds_payload_and_recovers() {
        let mut long = b"\x1b]7;".to_vec();
        long.extend(std::iter::repeat_n(b'a', MAX_PAYLOAD + 10));
        long.extend(b"\x07\x1b]7;ok\x07");
        assert_eq!(scan_all(&[&long]), vec![Osc::Cwd("ok".into())]);
    }

    #[test]
    fn several_in_one_chunk() {
        assert_eq!(
            scan_all(&[b"\x1b]7;a\x07\x1b]5170;settings\x07"]),
            vec![Osc::Cwd("a".into()), Osc::AppCommand("settings".into())]
        );
    }
}
