//! Colouring code for the viewer: comments, strings, numbers, keywords, types
//! and calls, for most languages people keep in folders.
//!
//! Not a parser. Each language is described by its comment and string marks
//! and its keywords, and a line is scanned once, left to right. That is wrong
//! in corners a real grammar gets right — a regex literal in JavaScript, a
//! nested comment in Rust — and right everywhere that matters for reading, at
//! a cost that does not depend on the file's size: a line is coloured when it
//! is drawn, from the state the line before it ended in.

use std::ops::Range;

/// What a stretch of a line is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Token {
    Plain,
    Keyword,
    String,
    Comment,
    Number,
    /// A capitalised name: a type, in most languages.
    Type,
    /// A name followed by `(`: a call or a definition.
    Function,
}

/// Where a line begins: inside nothing, a block comment, or a string that ran
/// over the end of the line before.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Carry {
    #[default]
    None,
    Comment,
    /// Inside a string opened by this quote; `true` when it is a triple one.
    Str(char, bool),
}

/// How a language marks its comments and strings, and its keywords.
#[derive(Debug)]
pub struct Language {
    pub name: &'static str,
    line_comments: &'static [&'static str],
    block_comment: Option<(&'static str, &'static str)>,
    quotes: &'static [char],
    /// Whether `"""` and `'''` open strings that span lines.
    triple: bool,
    /// Whether a plain string may run past the end of its line (backticks
    /// in JavaScript, every string in a shell script).
    multiline_quotes: &'static [char],
    keywords: &'static [&'static str],
}

const C_LIKE: &[&str] = &[
    "if", "else", "for", "while", "do", "switch", "case", "default", "break", "continue", "return", "goto",
    "struct", "union", "enum", "typedef", "static", "const", "extern", "void", "int", "char", "float", "double",
    "long", "short", "unsigned", "signed", "sizeof", "inline", "volatile", "class", "public", "private",
    "protected", "virtual", "template", "typename", "namespace", "using", "new", "delete", "try", "catch",
    "throw", "true", "false", "nullptr", "NULL", "auto", "bool", "this", "operator", "override", "final",
    "import", "interface", "implements", "extends", "package", "abstract", "synchronized", "null", "instanceof",
    "self", "super", "func", "let", "var", "guard", "in", "is", "as", "where", "protocol", "extension",
    "internal", "fileprivate", "init", "deinit", "throws", "async", "await", "go", "defer", "chan", "map",
    "range", "select", "type", "fallthrough", "fun", "val", "when", "object", "companion", "data", "sealed",
    "#include", "#define", "#if", "#ifdef", "#ifndef", "#endif", "#else", "#pragma", "@interface", "@end",
    "@implementation", "@property",
];

const RUST: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern", "false",
    "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return",
    "self", "Self", "static", "struct", "super", "trait", "true", "type", "unsafe", "use", "where", "while",
    "Some", "None", "Ok", "Err",
];

const JS: &[&str] = &[
    "async", "await", "break", "case", "catch", "class", "const", "continue", "debugger", "default", "delete",
    "do", "else", "export", "extends", "false", "finally", "for", "from", "function", "if", "import", "in",
    "instanceof", "let", "new", "null", "of", "return", "static", "super", "switch", "this", "throw", "true",
    "try", "typeof", "undefined", "var", "void", "while", "with", "yield", "interface", "type", "enum",
    "implements", "private", "public", "protected", "readonly", "as", "declare", "namespace", "abstract",
    "keyof", "never", "unknown", "any", "string", "number", "boolean",
];

const PYTHON: &[&str] = &[
    "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class", "continue", "def",
    "del", "elif", "else", "except", "finally", "for", "from", "global", "if", "import", "in", "is", "lambda",
    "nonlocal", "not", "or", "pass", "raise", "return", "try", "while", "with", "yield", "match", "case", "self",
];

const SHELL: &[&str] = &[
    "if", "then", "else", "elif", "fi", "for", "while", "until", "do", "done", "case", "esac", "in", "function",
    "return", "local", "export", "readonly", "set", "unset", "echo", "exit", "source", "alias", "cd", "true",
    "false", "end", "begin", "and", "or", "not",
];

const RUBY_LUA: &[&str] = &[
    "def", "end", "if", "elsif", "else", "unless", "while", "until", "for", "in", "do", "return", "class",
    "module", "self", "nil", "true", "false", "and", "or", "not", "then", "yield", "begin", "rescue", "ensure",
    "require", "local", "function", "elseif", "repeat",
];

const SQL: &[&str] = &[
    "select", "from", "where", "insert", "into", "values", "update", "set", "delete", "create", "table", "drop",
    "alter", "index", "join", "left", "right", "inner", "outer", "on", "group", "by", "order", "having", "limit",
    "and", "or", "not", "null", "as", "distinct", "union", "primary", "key", "foreign", "references", "SELECT",
    "FROM", "WHERE", "INSERT", "INTO", "VALUES", "UPDATE", "SET", "DELETE", "CREATE", "TABLE", "DROP", "ALTER",
    "JOIN", "LEFT", "INNER", "ON", "GROUP", "BY", "ORDER", "HAVING", "LIMIT", "AND", "OR", "NOT", "NULL", "AS",
];

const DATA: &[&str] = &["true", "false", "null", "yes", "no", "on", "off", "True", "False"];

macro_rules! lang {
    ($name:expr, $line:expr, $block:expr, $quotes:expr, $triple:expr, $multi:expr, $kw:expr) => {
        Language {
            name: $name,
            line_comments: $line,
            block_comment: $block,
            quotes: $quotes,
            triple: $triple,
            multiline_quotes: $multi,
            keywords: $kw,
        }
    };
}

static RUST_L: Language = lang!("Rust", &["//"], Some(("/*", "*/")), &['"'], false, &['"'], RUST);
static C_L: Language = lang!("C-family", &["//"], Some(("/*", "*/")), &['"', '\''], false, &[], C_LIKE);
static GO_L: Language = lang!("Go", &["//"], Some(("/*", "*/")), &['"', '\'', '`'], false, &['`'], C_LIKE);
static SWIFT_L: Language = lang!("Swift", &["//"], Some(("/*", "*/")), &['"'], true, &[], C_LIKE);
static JS_L: Language = lang!("JavaScript", &["//"], Some(("/*", "*/")), &['"', '\'', '`'], false, &['`'], JS);
static PY_L: Language = lang!("Python", &["#"], None, &['"', '\''], true, &[], PYTHON);
static SH_L: Language = lang!("Shell", &["#"], None, &['"', '\''], false, &['"', '\''], SHELL);
static RB_L: Language = lang!("Ruby", &["#"], None, &['"', '\''], false, &[], RUBY_LUA);
static LUA_L: Language = lang!("Lua", &["--"], None, &['"', '\''], false, &[], RUBY_LUA);
static SQL_L: Language = lang!("SQL", &["--"], Some(("/*", "*/")), &['\''], false, &[], SQL);
static CSS_L: Language = lang!("CSS", &[], Some(("/*", "*/")), &['"', '\''], false, &[], &[]);
static HTML_L: Language = lang!("Markup", &[], Some(("<!--", "-->")), &['"'], false, &[], &[]);
static CONF_L: Language = lang!("Config", &["#", ";"], None, &['"', '\''], false, &[], DATA);
static JSON_L: Language = lang!("JSON", &[], None, &['"'], false, &[], DATA);
static HASKELL_L: Language = lang!("Haskell", &["--"], Some(("{-", "-}")), &['"'], false, &[], &[]);
static LISP_L: Language = lang!("Lisp", &[";"], None, &['"'], false, &[], &[]);

/// The language a file is written in, by its name; `None` for plain text.
pub fn language_for(name: &str) -> Option<&'static Language> {
    let lower = name.to_ascii_lowercase();
    match lower.as_str() {
        "gemfile" | "rakefile" | "podfile" => return Some(&RB_L),
        "makefile" | "dockerfile" | ".gitignore" | ".env" | ".zshrc" | ".bashrc" | ".bash_profile" | ".profile" => {
            return Some(&SH_L);
        }
        _ => {}
    }
    let ext = lower.rsplit_once('.').map(|(_, e)| e)?;
    Some(match ext {
        "rs" => &RUST_L,
        "c" | "h" | "cc" | "cpp" | "cxx" | "hpp" | "hh" | "m" | "mm" | "java" | "cs" | "kt" | "kts" | "scala"
        | "dart" | "zig" | "v" | "glsl" | "hlsl" | "metal" | "proto" | "groovy" | "gradle" => &C_L,
        "go" => &GO_L,
        "swift" => &SWIFT_L,
        "js" | "jsx" | "ts" | "tsx" | "mjs" | "cjs" | "vue" | "svelte" => &JS_L,
        "py" | "pyi" | "pyw" => &PY_L,
        "sh" | "bash" | "zsh" | "fish" | "command" => &SH_L,
        "rb" | "rake" | "gemspec" => &RB_L,
        "lua" => &LUA_L,
        "sql" => &SQL_L,
        "css" | "scss" | "sass" | "less" => &CSS_L,
        "html" | "htm" | "xml" | "svg" | "plist" | "xhtml" => &HTML_L,
        "toml" | "yaml" | "yml" | "ini" | "cfg" | "conf" | "properties" | "env" | "mk" => &CONF_L,
        "json" | "jsonc" | "json5" | "geojson" | "lock" => &JSON_L,
        "hs" | "elm" => &HASKELL_L,
        "lisp" | "el" | "clj" | "cljs" | "scm" | "rkt" => &LISP_L,
        _ => return None,
    })
}

/// Colours one line, starting in `carry`; returns the stretches and the state
/// the line ends in.
pub fn line(lang: &Language, text: &str, carry: Carry) -> (Vec<(Range<usize>, Token)>, Carry) {
    let mut out: Vec<(Range<usize>, Token)> = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    let mut state = carry;
    let push = |out: &mut Vec<(Range<usize>, Token)>, range: Range<usize>, token: Token| {
        if range.is_empty() {
            return;
        }
        match out.last_mut() {
            Some((last, t)) if *t == token && last.end == range.start => last.end = range.end,
            _ => out.push((range, token)),
        }
    };

    while i < bytes.len() {
        match state {
            Carry::Comment => {
                let close = lang.block_comment.map_or("", |(_, c)| c);
                match text[i..].find(close).filter(|_| !close.is_empty()) {
                    Some(at) => {
                        push(&mut out, i..i + at + close.len(), Token::Comment);
                        i += at + close.len();
                        state = Carry::None;
                    }
                    None => {
                        push(&mut out, i..bytes.len(), Token::Comment);
                        return (out, Carry::Comment);
                    }
                }
            }
            Carry::Str(quote, triple) => {
                let end = string_end(text, i, quote, triple);
                match end {
                    Some(end) => {
                        push(&mut out, i..end, Token::String);
                        i = end;
                        state = Carry::None;
                    }
                    None => {
                        push(&mut out, i..bytes.len(), Token::String);
                        let runs_on = triple || lang.multiline_quotes.contains(&quote);
                        return (out, if runs_on { Carry::Str(quote, triple) } else { Carry::None });
                    }
                }
            }
            Carry::None => {
                let rest = &text[i..];
                if let Some(mark) = lang.line_comments.iter().find(|m| rest.starts_with(**m)) {
                    // `#` opens a comment only at a word's edge, so `a#b` and
                    // `#include` in a config are not cut off.
                    if *mark != "#" || i == 0 || bytes[i - 1].is_ascii_whitespace() {
                        push(&mut out, i..bytes.len(), Token::Comment);
                        return (out, Carry::None);
                    }
                }
                if let Some((open, _)) = lang.block_comment {
                    if rest.starts_with(open) {
                        push(&mut out, i..i + open.len(), Token::Comment);
                        i += open.len();
                        state = Carry::Comment;
                        continue;
                    }
                }
                let c = rest.chars().next().unwrap_or(' ');
                if lang.quotes.contains(&c) {
                    let triple = lang.triple && rest.starts_with(&c.to_string().repeat(3));
                    let open = if triple { 3 } else { 1 };
                    push(&mut out, i..i + open, Token::String);
                    i += open;
                    state = Carry::Str(c, triple);
                    continue;
                }
                if c.is_ascii_digit() {
                    let end = i + rest.find(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '.' || ch == '_')).unwrap_or(rest.len());
                    push(&mut out, i..end, Token::Number);
                    i = end;
                    continue;
                }
                if c.is_alphabetic() || c == '_' || c == '#' || c == '@' {
                    let len = rest[c.len_utf8()..]
                        .find(|ch: char| !(ch.is_alphanumeric() || ch == '_'))
                        .map_or(rest.len(), |n| n + c.len_utf8());
                    let word = &rest[..len];
                    let token = if lang.keywords.contains(&word) {
                        Token::Keyword
                    } else if rest[len..].starts_with('(') || rest[len..].starts_with("!(") {
                        Token::Function
                    } else if word.chars().next().is_some_and(char::is_uppercase) && word.len() > 1 {
                        Token::Type
                    } else {
                        Token::Plain
                    };
                    push(&mut out, i..i + len, token);
                    i += len;
                    continue;
                }
                push(&mut out, i..i + c.len_utf8(), Token::Plain);
                i += c.len_utf8();
            }
        }
    }
    let ends = match state {
        Carry::Str(q, triple) if !(triple || lang.multiline_quotes.contains(&q)) => Carry::None,
        other => other,
    };
    (out, ends)
}

/// Where a string that is open at `from` ends — just past its closing quote —
/// if it ends on this line. A backslash escapes the next character.
fn string_end(text: &str, from: usize, quote: char, triple: bool) -> Option<usize> {
    let close = if triple { quote.to_string().repeat(3) } else { quote.to_string() };
    let mut chars = text[from..].char_indices().peekable();
    while let Some((at, c)) = chars.next() {
        if c == '\\' {
            chars.next();
            continue;
        }
        if text[from + at..].starts_with(&close) {
            return Some(from + at + close.len());
        }
    }
    None
}

/// The state each line of `lines` begins in, for colouring any of them on its
/// own later. One pass over the text.
pub fn carries<'a>(lang: &Language, lines: impl Iterator<Item = &'a str>) -> Vec<Carry> {
    let mut out = Vec::new();
    let mut carry = Carry::None;
    for text in lines {
        out.push(carry);
        carry = line(lang, text, carry).1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(lang: &Language, text: &str, carry: Carry) -> Vec<(&'static str, Token)> {
        let (spans, _) = line(lang, text, carry);
        spans
            .into_iter()
            .filter(|(_, t)| *t != Token::Plain)
            .map(|(r, t)| (&*Box::leak(text[r].to_owned().into_boxed_str()), t))
            .collect()
    }

    #[test]
    fn rust_keywords_strings_numbers_calls_and_comments() {
        let rust = language_for("main.rs").unwrap();
        assert_eq!(
            tokens(rust, r#"let s = String::from("a\"b"); // done"#, Carry::None),
            vec![
                ("let", Token::Keyword),
                ("String", Token::Type),
                ("from", Token::Function),
                (r#""a\"b""#, Token::String),
                ("// done", Token::Comment),
            ]
        );
        assert_eq!(tokens(rust, "x = 0x1F + 2.5;", Carry::None), vec![("0x1F", Token::Number), ("2.5", Token::Number)]);
    }

    #[test]
    fn a_block_comment_and_a_triple_string_carry_to_the_next_line() {
        let rust = language_for("lib.rs").unwrap();
        let (_, carry) = line(rust, "a /* begins", Carry::None);
        assert_eq!(carry, Carry::Comment);
        assert_eq!(tokens(rust, "ends */ fn", Carry::Comment), vec![("ends */", Token::Comment), ("fn", Token::Keyword)]);

        let py = language_for("x.py").unwrap();
        let states = carries(py, ["doc = \"\"\"start", "middle", "end\"\"\" # c"].into_iter());
        assert_eq!(states, vec![Carry::None, Carry::Str('"', true), Carry::Str('"', true)]);
        // An ordinary string does not run on past its line.
        assert_eq!(line(py, "x = 'open", Carry::None).1, Carry::None);
    }

    #[test]
    fn files_are_recognised_by_their_names() {
        assert_eq!(language_for("App.tsx").unwrap().name, "JavaScript");
        assert_eq!(language_for("Makefile").unwrap().name, "Shell");
        assert_eq!(language_for(".profile").unwrap().name, "Shell");
        assert_eq!(language_for("Gemfile").unwrap().name, "Ruby");
        assert_eq!(language_for("config.yaml").unwrap().name, "Config");
        assert!(language_for("notes.txt").is_none());
        assert!(language_for("README").is_none());
    }
}
