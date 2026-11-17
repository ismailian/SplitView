//! A small, fast syntax highlighter for the editor panel: one linear pass over
//! the text, driven by a per-language table (comment markers, string quotes,
//! keywords). It is not a parser — it colours what a reader scans for (comments,
//! strings, numbers, keywords, types, calls) and nothing else, which keeps it
//! cheap enough to re-run on every edit of a large file.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Plain,
    Comment,
    String,
    Number,
    Keyword,
    Type,
    Function,
    /// Markup tags, headings, keys in config files.
    Tag,
}

#[derive(Debug, Clone, Copy)]
pub struct Lang {
    pub name: &'static str,
    line_comments: &'static [&'static str],
    block_comment: Option<(&'static str, &'static str)>,
    quotes: &'static [u8],
    keywords: &'static [&'static str],
    /// `key = value` / `key: value` files: the key is a Tag.
    keyed: bool,
    markup: bool,
}

const RUST_KW: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern",
    "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub",
    "ref", "return", "self", "Self", "static", "struct", "super", "trait", "true", "type",
    "unsafe", "use", "where", "while",
];
const JS_KW: &[&str] = &[
    "as",
    "async",
    "await",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "default",
    "delete",
    "do",
    "else",
    "enum",
    "export",
    "extends",
    "false",
    "finally",
    "for",
    "from",
    "function",
    "if",
    "implements",
    "import",
    "in",
    "instanceof",
    "interface",
    "let",
    "new",
    "null",
    "of",
    "private",
    "protected",
    "public",
    "readonly",
    "return",
    "static",
    "super",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "type",
    "typeof",
    "undefined",
    "var",
    "void",
    "while",
    "yield",
];
const PY_KW: &[&str] = &[
    "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class", "continue",
    "def", "del", "elif", "else", "except", "finally", "for", "from", "global", "if", "import",
    "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise", "return", "self", "try",
    "while", "with", "yield",
];
const GO_KW: &[&str] = &[
    "break",
    "case",
    "chan",
    "const",
    "continue",
    "default",
    "defer",
    "else",
    "fallthrough",
    "false",
    "for",
    "func",
    "go",
    "goto",
    "if",
    "import",
    "interface",
    "map",
    "nil",
    "package",
    "range",
    "return",
    "select",
    "struct",
    "switch",
    "true",
    "type",
    "var",
];
const C_KW: &[&str] = &[
    "auto",
    "bool",
    "break",
    "case",
    "catch",
    "char",
    "class",
    "const",
    "constexpr",
    "continue",
    "default",
    "delete",
    "do",
    "double",
    "else",
    "enum",
    "extern",
    "false",
    "float",
    "for",
    "if",
    "include",
    "define",
    "inline",
    "int",
    "long",
    "namespace",
    "new",
    "nullptr",
    "private",
    "protected",
    "public",
    "return",
    "short",
    "signed",
    "sizeof",
    "static",
    "struct",
    "switch",
    "template",
    "this",
    "throw",
    "true",
    "try",
    "typedef",
    "union",
    "unsigned",
    "using",
    "virtual",
    "void",
    "volatile",
    "while",
    "abstract",
    "extends",
    "final",
    "implements",
    "import",
    "interface",
    "package",
    "super",
    "fun",
    "val",
    "var",
    "when",
    "object",
    "override",
];
const SH_KW: &[&str] = &[
    "case", "do", "done", "elif", "else", "esac", "export", "fi", "for", "function", "if", "in",
    "local", "return", "then", "until", "while", "alias", "source", "echo", "cd", "exit", "param",
    "foreach", "begin", "process", "end", "set", "setlocal", "goto", "call",
];
const CSS_KW: &[&str] = &[
    "important",
    "media",
    "import",
    "keyframes",
    "root",
    "from",
    "to",
];
const SQL_KW: &[&str] = &[
    "select", "from", "where", "insert", "into", "values", "update", "set", "delete", "create",
    "table", "drop", "alter", "join", "left", "right", "inner", "outer", "on", "and", "or", "not",
    "null", "group", "by", "order", "limit", "as", "primary", "key", "index", "SELECT", "FROM",
    "WHERE", "INSERT", "INTO", "VALUES", "UPDATE", "SET", "DELETE", "CREATE", "TABLE", "JOIN",
    "ON", "AND", "OR", "NOT", "NULL", "GROUP", "BY", "ORDER", "LIMIT", "AS",
];

const fn lang(
    name: &'static str,
    line_comments: &'static [&'static str],
    block_comment: Option<(&'static str, &'static str)>,
    quotes: &'static [u8],
    keywords: &'static [&'static str],
) -> Lang {
    Lang {
        name,
        line_comments,
        block_comment,
        quotes,
        keywords,
        keyed: false,
        markup: false,
    }
}

pub const PLAIN: Lang = lang("Plain text", &[], None, &[], &[]);

/// The language for a file name, by extension (or well-known name).
pub fn lang_for(path: &str) -> Lang {
    let name = crate::paths::base_name(path).to_ascii_lowercase();
    let ext = name.rsplit_once('.').map_or("", |(_, e)| e);
    let c_like = Some(("/*", "*/"));
    match (name.as_str(), ext) {
        (_, "rs") => lang("Rust", &["//"], c_like, b"\"", RUST_KW),
        (_, "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs") => {
            lang("TypeScript", &["//"], c_like, b"\"'`", JS_KW)
        }
        (_, "py" | "pyw") => lang("Python", &["#"], None, b"\"'", PY_KW),
        (_, "go") => lang("Go", &["//"], c_like, b"\"'`", GO_KW),
        (
            _,
            "c" | "h" | "cc" | "cpp" | "hpp" | "cxx" | "cs" | "java" | "kt" | "kts" | "swift"
            | "dart" | "zig",
        ) => lang("C-like", &["//"], c_like, b"\"'", C_KW),
        ("makefile" | "dockerfile" | ".bashrc" | ".zshrc" | ".profile" | ".bash_profile", _)
        | (_, "sh" | "bash" | "zsh" | "fish") => lang("Shell", &["#"], None, b"\"'", SH_KW),
        (_, "ps1" | "psm1") => lang("PowerShell", &["#"], Some(("<#", "#>")), b"\"'", SH_KW),
        (_, "bat" | "cmd") => lang("Batch", &["::", "rem ", "REM "], None, b"\"", SH_KW),
        (
            _,
            "toml" | "ini" | "cfg" | "conf" | "env" | "properties" | "editorconfig" | "gitignore",
        ) => Lang {
            keyed: true,
            ..lang("Config", &["#", ";"], None, b"\"'", &["true", "false"])
        },
        (_, "yaml" | "yml") => Lang {
            keyed: true,
            ..lang("YAML", &["#"], None, b"\"'", &["true", "false", "null"])
        },
        (_, "json" | "jsonc") => Lang {
            keyed: true,
            ..lang("JSON", &["//"], None, b"\"", &["true", "false", "null"])
        },
        (_, "css" | "scss" | "less") => lang("CSS", &[], c_like, b"\"'", CSS_KW),
        (_, "sql") => lang("SQL", &["--"], c_like, b"'\"", SQL_KW),
        (_, "html" | "htm" | "xml" | "svg" | "vue" | "xaml" | "plist") => Lang {
            markup: true,
            ..lang("Markup", &[], Some(("<!--", "-->")), b"\"'", &[])
        },
        (_, "md" | "markdown") => Lang {
            markup: true,
            ..lang("Markdown", &[], None, b"`", &[])
        },
        (_, "lua") => lang(
            "Lua",
            &["--"],
            None,
            b"\"'",
            &[
                "function", "end", "local", "if", "then", "else", "return", "for", "do", "while",
                "nil", "true", "false",
            ],
        ),
        _ => PLAIN,
    }
}

fn is_ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
}

/// Tokenize `text` into contiguous `(start, end, kind)` byte spans covering it.
pub fn highlight(text: &str, lang: &Lang) -> Vec<(usize, usize, Kind)> {
    let b = text.as_bytes();
    let mut out: Vec<(usize, usize, Kind)> = Vec::with_capacity(b.len() / 6 + 1);
    let mut push = |s: usize, e: usize, k: Kind| {
        if s >= e {
            return;
        }
        match out.last_mut() {
            Some(last) if last.2 == k && last.1 == s => last.1 = e,
            _ => out.push((s, e, k)),
        }
    };
    if lang.name == PLAIN.name {
        push(0, b.len(), Kind::Plain);
        return out;
    }
    let mut i = 0;
    let mut line_start = true;
    while i < b.len() {
        let c = b[i];
        if c == b'\n' {
            push(i, i + 1, Kind::Plain);
            i += 1;
            line_start = true;
            continue;
        }
        let rest = &b[i..];
        if lang.markup && lang.name == "Markdown" && line_start && c == b'#' {
            let e = memchr_nl(b, i);
            push(i, e, Kind::Tag);
            i = e;
            continue;
        }
        if c == b' ' || c == b'\t' {
            push(i, i + 1, Kind::Plain);
            i += 1;
            continue;
        }
        let was_line_start = line_start;
        line_start = false;
        if let Some(lc) = lang
            .line_comments
            .iter()
            .find(|m| rest.starts_with(m.as_bytes()))
        {
            // `rem ` only counts at the start of a line; `#` in shells only after space.
            if !lc.ends_with(' ') || was_line_start {
                let e = memchr_nl(b, i);
                push(i, e, Kind::Comment);
                i = e;
                continue;
            }
        }
        if let Some((open, close)) = lang.block_comment
            && rest.starts_with(open.as_bytes())
        {
            let e = find(b, i + open.len(), close.as_bytes()).map_or(b.len(), |p| p + close.len());
            push(i, e, Kind::Comment);
            i = e;
            continue;
        }
        if lang.quotes.contains(&c) {
            let mut j = i + 1;
            while j < b.len() && b[j] != c {
                if b[j] == b'\\' && c != b'`' || b[j] == b'\\' && lang.name != "Markdown" {
                    j += 1;
                } else if b[j] == b'\n' && c != b'`' && c != b'"' {
                    break;
                }
                j += 1;
            }
            let e = (j + 1).min(b.len());
            // A keyed file's quoted key (JSON) is a key, not a value.
            let kind = if lang.keyed && next_non_space(b, e) == Some(b':') {
                Kind::Tag
            } else {
                Kind::String
            };
            push(i, e, kind);
            i = e;
            continue;
        }
        if lang.markup && c == b'<' && lang.name != "Markdown" {
            let mut j = i + 1;
            while j < b.len() && (is_ident(b[j]) || b"/?!-:".contains(&b[j])) {
                j += 1;
            }
            push(i, j, Kind::Tag);
            i = j;
            continue;
        }
        if c.is_ascii_digit() {
            let mut j = i + 1;
            while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'.' || b[j] == b'_') {
                j += 1;
            }
            push(i, j, Kind::Number);
            i = j;
            continue;
        }
        if is_ident(c) {
            let mut j = i + 1;
            while j < b.len() && (is_ident(b[j]) || (lang.keyed && b[j] == b'-')) {
                j += 1;
            }
            let word = &text[i..j];
            let after = next_non_space(b, j);
            let kind = if lang.keyed && was_line_start && matches!(after, Some(b'=' | b':')) {
                Kind::Tag
            } else if lang.keywords.contains(&word) {
                Kind::Keyword
            } else if lang.markup {
                Kind::Plain
            } else if after == Some(b'(') || (after == Some(b'!') && lang.name == "Rust") {
                Kind::Function
            } else if word.as_bytes()[0].is_ascii_uppercase() && word.len() > 1 {
                Kind::Type
            } else {
                Kind::Plain
            };
            push(i, j, kind);
            i = j;
            continue;
        }
        if lang.keyed && was_line_start && c == b'[' {
            let e = memchr_nl(b, i);
            push(i, e, Kind::Tag);
            i = e;
            continue;
        }
        // Any other byte: advance one whole char.
        let len = utf8_len(c);
        push(i, (i + len).min(b.len()), Kind::Plain);
        i += len;
    }
    out
}

fn utf8_len(c: u8) -> usize {
    match c {
        0xf0.. => 4,
        0xe0.. => 3,
        0xc0.. => 2,
        _ => 1,
    }
}

fn memchr_nl(b: &[u8], from: usize) -> usize {
    b[from..]
        .iter()
        .position(|&c| c == b'\n')
        .map_or(b.len(), |p| from + p)
}

fn find(b: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if from > b.len() {
        return None;
    }
    b[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| from + p)
}

fn next_non_space(b: &[u8], from: usize) -> Option<u8> {
    b[from.min(b.len())..]
        .iter()
        .copied()
        .find(|&c| c != b' ' && c != b'\t')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds<'a>(text: &'a str, path: &str) -> Vec<(&'a str, Kind)> {
        highlight(text, &lang_for(path))
            .into_iter()
            .filter(|s| s.2 != Kind::Plain)
            .map(|(s, e, k)| (&text[s..e], k))
            .collect()
    }

    #[test]
    fn rust() {
        let k = kinds("fn main() { let s = \"hi\"; // c\n Vec::new(42) }", "a.rs");
        assert!(k.contains(&("fn", Kind::Keyword)));
        assert!(k.contains(&("main", Kind::Function)));
        assert!(k.contains(&("\"hi\"", Kind::String)));
        assert!(k.contains(&("// c", Kind::Comment)));
        assert!(k.contains(&("Vec", Kind::Type)));
        assert!(k.contains(&("42", Kind::Number)));
    }

    #[test]
    fn spans_cover_everything_contiguously() {
        let text = "x = 'é' /* multi\nline */ ñ 0x1F\n";
        let spans = highlight(text, &lang_for("a.ts"));
        let mut at = 0;
        for (s, e, _) in &spans {
            assert_eq!(*s, at);
            assert!(text.is_char_boundary(*e));
            at = *e;
        }
        assert_eq!(at, text.len());
    }

    #[test]
    fn config_keys_and_json_keys() {
        let k = kinds("[server]\nport = 80 # c\n", "a.toml");
        assert!(k.contains(&("[server]", Kind::Tag)));
        assert!(k.contains(&("port", Kind::Tag)));
        assert!(k.contains(&("# c", Kind::Comment)));
        let j = kinds("{\"a\": \"b\", \"n\": true}", "x.json");
        assert!(j.contains(&("\"a\"", Kind::Tag)));
        assert!(j.contains(&("\"b\"", Kind::String)));
        assert!(j.contains(&("true", Kind::Keyword)));
    }

    #[test]
    fn unterminated_things_do_not_panic() {
        for p in ["a.rs", "a.py", "a.html", "a.md", "a.json", "a.sh", "a.bat"] {
            let _ = highlight("\"abc\\", &lang_for(p));
            let _ = highlight("/* abc", &lang_for(p));
            let _ = highlight("<a", &lang_for(p));
            let _ = highlight("€", &lang_for(p));
        }
        assert_eq!(lang_for("README").name, "Plain text");
        assert_eq!(lang_for("Makefile").name, "Shell");
    }
}
