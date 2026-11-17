//! The editor panel's pure logic: what has to survive a round trip through an
//! editor buffer, and what can be worked out from a file's text.

use crate::paths::base_name;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Eol {
    Lf,
    CrLf,
}

impl Eol {
    pub fn label(self) -> &'static str {
        match self {
            Eol::Lf => "LF",
            Eol::CrLf => "CRLF",
        }
    }
}

/// A file's text as the buffer holds it, plus what saving has to restore.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decoded {
    /// BOM stripped, line endings normalized to `\n`.
    pub text: String,
    pub eol: Eol,
    pub bom: bool,
}

const BOM: char = '\u{feff}';

/// Prepare file contents for the buffer. The buffer only ever holds `\n`, so a
/// CRLF file has to remember that it is one: saving it as LF would turn a
/// one-line edit into a whole-file diff.
pub fn decode_text(raw: &str) -> Decoded {
    let bom = raw.starts_with(BOM);
    let body = if bom { &raw[BOM.len_utf8()..] } else { raw };
    let eol = detect_eol(body);
    let text = if body.contains('\r') {
        body.replace("\r\n", "\n").replace('\r', "\n")
    } else {
        body.to_owned()
    };
    Decoded { text, eol, bom }
}

/// The inverse of [`decode_text`]. A mixed file comes back with its majority ending.
pub fn encode_text(text: &str, eol: Eol, bom: bool) -> String {
    let mut out = String::with_capacity(text.len() + 4);
    if bom {
        out.push(BOM);
    }
    match eol {
        Eol::Lf => out.push_str(text),
        Eol::CrLf => out.push_str(&text.replace('\n', "\r\n")),
    }
    out
}

fn detect_eol(text: &str) -> Eol {
    let crlf = text.matches("\r\n").count();
    let lf = text.matches('\n').count() - crlf;
    if crlf > lf { Eol::CrLf } else { Eol::Lf }
}

/// The file's indent unit: `\t` for a tab-indented file, otherwise the most
/// common step in leading spaces between one line and the next.
pub fn detect_indent(text: &str) -> String {
    let (mut tabs, mut spaced, mut previous) = (0, 0, 0usize);
    let mut steps = [0usize; 9];
    for line in text.split('\n').take(2000) {
        if line.trim().is_empty() {
            continue;
        }
        if line.starts_with('\t') {
            tabs += 1;
            continue;
        }
        let width = line.len() - line.trim_start().len();
        if width > 0 {
            spaced += 1;
        }
        let step = width.abs_diff(previous);
        if matches!(step, 2 | 3 | 4 | 8) {
            steps[step] += 1;
        }
        previous = width;
    }
    if tabs > spaced {
        return "\t".into();
    }
    let mut best = 0;
    for s in [2, 3, 4, 8] {
        if steps[s] > steps[best] {
            best = s;
        }
    }
    if best > 0 {
        " ".repeat(best)
    } else {
        "  ".into()
    }
}

/// The smallest single replacement turning `a` into `b`: `(from, to, insert)`
/// in byte offsets of `a`, or `None` if equal.
pub fn change_between<'b>(a: &str, b: &'b str) -> Option<(usize, usize, &'b str)> {
    if a == b {
        return None;
    }
    let (ab, bb) = (a.as_bytes(), b.as_bytes());
    let shortest = ab.len().min(bb.len());
    let mut start = 0;
    while start < shortest && ab[start] == bb[start] {
        start += 1;
    }
    while !a.is_char_boundary(start) || !b.is_char_boundary(start) {
        start -= 1;
    }
    let mut end = 0;
    while end < shortest - start && ab[ab.len() - 1 - end] == bb[bb.len() - 1 - end] {
        end += 1;
    }
    while !a.is_char_boundary(ab.len() - end) || !b.is_char_boundary(bb.len() - end) {
        end -= 1;
    }
    Some((start, ab.len() - end, &b[start..bb.len() - end]))
}

/// A label per open path: the file name, plus its parent directory where two
/// tabs would otherwise read the same.
pub fn tab_labels(paths: &[&str]) -> Vec<String> {
    let names: Vec<&str> = paths.iter().map(|p| base_name(p)).collect();
    paths
        .iter()
        .enumerate()
        .map(|(i, p)| {
            if names.iter().filter(|n| **n == names[i]).count() < 2 {
                names[i].to_owned()
            } else {
                let parts: Vec<&str> = p.split(['/', '\\']).filter(|s| !s.is_empty()).collect();
                parts[parts.len().saturating_sub(2)..].join("/")
            }
        })
        .collect()
}

/// Every match of `needle` in `hay` (byte ranges), optionally case-insensitive
/// (ASCII folding, so offsets stay valid).
pub fn find_all(hay: &str, needle: &str, case_sensitive: bool) -> Vec<(usize, usize)> {
    if needle.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    if case_sensitive {
        let mut from = 0;
        while let Some(p) = hay[from..].find(needle) {
            out.push((from + p, from + p + needle.len()));
            from += p + needle.len();
        }
    } else {
        let h = hay.as_bytes();
        let n = needle.as_bytes();
        let mut i = 0;
        while i + n.len() <= h.len() {
            if h[i..i + n.len()].eq_ignore_ascii_case(n)
                && hay.is_char_boundary(i)
                && hay.is_char_boundary(i + n.len())
            {
                out.push((i, i + n.len()));
                i += n.len();
            } else {
                i += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eol_and_bom_round_trip() {
        let raw = "\u{feff}a\r\nb\r\nc";
        let d = decode_text(raw);
        assert_eq!(d.text, "a\nb\nc");
        assert_eq!(d.eol, Eol::CrLf);
        assert!(d.bom);
        assert_eq!(encode_text(&d.text, d.eol, d.bom), raw);
        let lf = decode_text("x\ny\n");
        assert_eq!((lf.eol, lf.bom), (Eol::Lf, false));
        assert_eq!(encode_text(&lf.text, lf.eol, lf.bom), "x\ny\n");
        // Mixed: the majority wins.
        assert_eq!(decode_text("a\r\nb\r\nc\n").eol, Eol::CrLf);
        assert_eq!(decode_text("").eol, Eol::Lf);
    }

    #[test]
    fn indent_detection() {
        assert_eq!(
            detect_indent("fn a() {\n    x;\n    if y {\n        z;\n    }\n}"),
            "    "
        );
        assert_eq!(detect_indent("a:\n  b:\n    c: 1\n"), "  ");
        assert_eq!(detect_indent("a\n\tb\n\tc\n"), "\t");
        assert_eq!(detect_indent("/*\n * x\n */"), "  ");
    }

    #[test]
    fn minimal_change() {
        assert_eq!(change_between("abc", "abc"), None);
        assert_eq!(
            change_between("hello world", "hello brave world"),
            Some((6, 6, "brave "))
        );
        assert_eq!(change_between("abc", "ac"), Some((1, 2, "")));
        assert_eq!(change_between("é1", "é2"), Some((2, 3, "2")));
        assert_eq!(change_between("aé", "aè"), Some((1, 3, "è")));
    }

    #[test]
    fn labels_disambiguate() {
        assert_eq!(
            tab_labels(&["/x/editor/model.ts", "/x/sidebar/model.ts", "/x/main.ts"]),
            vec!["editor/model.ts", "sidebar/model.ts", "main.ts"]
        );
    }

    #[test]
    fn finds_matches() {
        assert_eq!(find_all("aXbxc", "x", false), vec![(1, 2), (3, 4)]);
        assert_eq!(find_all("aXbxc", "x", true), vec![(3, 4)]);
        assert_eq!(find_all("aaa", "aa", true), vec![(0, 2)]);
        assert!(find_all("abc", "", true).is_empty());
    }
}
