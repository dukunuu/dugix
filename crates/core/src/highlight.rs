//! Shared, theme-free syntax highlighting for code and Markdown.
//! Tree-sitter queries produce sorted, non-overlapping UTF-8 byte spans.
//! Frontends map each `Role` to their own colors or presentation classes.
//! Parsing is size-limited and cancelled when its time budget expires.

use std::{
    collections::HashMap,
    ops::Range,
    sync::{
        OnceLock,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use tree_sitter_highlight::{HighlightConfiguration, HighlightEvent, Highlighter};

const MAX_BYTES: usize = 512 * 1024;
const TIMEOUT: Duration = Duration::from_millis(100);

/// A syntax category independent of any frontend's theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    /// Language keywords and control flow.
    Keyword,
    /// String and character literals.
    String,
    /// Comments, including documentation comments.
    Comment,
    /// Functions, methods and constructors.
    Function,
    /// Types and built-in types.
    Type,
    /// Numeric literals.
    Number,
    /// Constants, booleans and built-in variables.
    Constant,
    /// Operators.
    Operator,
    /// Delimiters and brackets.
    Punctuation,
    /// Properties and fields.
    Property,
    /// Variables and parameters.
    Variable,
    /// Markup tags.
    Tag,
    /// Attributes and annotations.
    Attribute,
    /// Escape sequences.
    Escape,
    /// Markdown headings.
    Heading,
    /// Emphasized Markdown text.
    Emphasis,
    /// Strongly emphasized Markdown text.
    Strong,
    /// Link text, references and destinations.
    Link,
    /// Literal Markdown code.
    Code,
    /// Markdown quotations.
    Quote,
    /// Markdown list markers and special punctuation.
    ListMarker,
}

impl Role {
    /// Stable kebab-case name for HTML classes and IPC.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Keyword => "keyword",
            Self::String => "string",
            Self::Comment => "comment",
            Self::Function => "function",
            Self::Type => "type",
            Self::Number => "number",
            Self::Constant => "constant",
            Self::Operator => "operator",
            Self::Punctuation => "punctuation",
            Self::Property => "property",
            Self::Variable => "variable",
            Self::Tag => "tag",
            Self::Attribute => "attribute",
            Self::Escape => "escape",
            Self::Heading => "heading",
            Self::Emphasis => "emphasis",
            Self::Strong => "strong",
            Self::Link => "link",
            Self::Code => "code",
            Self::Quote => "quote",
            Self::ListMarker => "list-marker",
        }
    }
}

/// A highlighted, non-empty byte range on UTF-8 character boundaries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    /// Byte offsets within the original source.
    pub range: Range<usize>,
    /// Theme-free syntax category.
    pub role: Role,
}

/// Canonical language id for a fence/info-string name, or None when unsupported.
#[must_use]
pub fn language(name: &str) -> Option<&'static str> {
    let name = name
        .split_whitespace()
        .next()?
        .trim_start_matches(['{', '.'])
        .trim_end_matches('}')
        .to_ascii_lowercase();
    Some(match name.as_str() {
        "bash" | "sh" | "zsh" | "shell" | "console" => "bash",
        "c" | "h" => "c",
        "cpp" | "c++" | "cc" | "cxx" | "hpp" => "cpp",
        "css" => "css",
        "go" | "golang" => "go",
        "html" | "htm" | "xml" => "html",
        "java" => "java",
        "javascript" | "js" | "jsx" | "mjs" | "cjs" | "node" => "javascript",
        "json" | "jsonc" | "json5" => "json",
        "markdown" | "md" => "markdown",
        "python" | "py" | "python3" => "python",
        "ruby" | "rb" => "ruby",
        "rust" | "rs" => "rust",
        "toml" => "toml",
        "typescript" | "ts" | "mts" | "cts" => "typescript",
        "tsx" => "tsx",
        "yaml" | "yml" => "yaml",
        _ => return None,
    })
}

// More specific dotted names win over their general category in configure().
const CAPTURES: &[(&str, Role)] = &[
    ("comment", Role::Comment),
    ("string.escape", Role::Escape),
    ("escape", Role::Escape),
    ("string", Role::String),
    ("character", Role::String),
    ("number", Role::Number),
    ("float", Role::Number),
    ("constant.numeric", Role::Number),
    ("constant.builtin", Role::Constant),
    ("boolean", Role::Constant),
    ("constant", Role::Constant),
    ("keyword", Role::Keyword),
    ("conditional", Role::Keyword),
    ("repeat", Role::Keyword),
    ("include", Role::Keyword),
    ("exception", Role::Keyword),
    ("storageclass", Role::Keyword),
    ("function", Role::Function),
    ("method", Role::Function),
    ("constructor", Role::Function),
    ("type", Role::Type),
    ("operator", Role::Operator),
    ("punctuation", Role::Punctuation),
    ("property", Role::Property),
    ("field", Role::Property),
    ("variable.builtin", Role::Constant),
    ("variable", Role::Variable),
    ("parameter", Role::Variable),
    ("tag", Role::Tag),
    ("attribute", Role::Attribute),
    ("text.title", Role::Heading),
    ("markup.heading", Role::Heading),
    ("text.emphasis", Role::Emphasis),
    ("markup.italic", Role::Emphasis),
    ("text.strong", Role::Strong),
    ("markup.bold", Role::Strong),
    ("text.uri", Role::Link),
    ("text.reference", Role::Link),
    ("markup.link", Role::Link),
    ("text.literal", Role::Code),
    ("markup.raw", Role::Code),
    ("markup.quote", Role::Quote),
    ("punctuation.special", Role::ListMarker),
    ("markup.list", Role::ListMarker),
];

fn configuration(lang: &str) -> Option<&'static HighlightConfiguration> {
    static CONFIGURATIONS: OnceLock<
        HashMap<&'static str, OnceLock<Option<HighlightConfiguration>>>,
    > = OnceLock::new();
    CONFIGURATIONS
        .get_or_init(|| {
            [
                "bash",
                "c",
                "cpp",
                "css",
                "go",
                "html",
                "java",
                "javascript",
                "json",
                "markdown",
                "markdown_inline",
                "python",
                "ruby",
                "rust",
                "toml",
                "typescript",
                "tsx",
                "yaml",
            ]
            .into_iter()
            .map(|lang| (lang, OnceLock::new()))
            .collect()
        })
        .get(lang)?
        .get_or_init(|| build_configuration(lang))
        .as_ref()
}

fn build_configuration(lang: &str) -> Option<HighlightConfiguration> {
    let (grammar, query) = match lang {
        "bash" => (
            tree_sitter_bash::LANGUAGE.into(),
            tree_sitter_bash::HIGHLIGHT_QUERY.to_owned(),
        ),
        "c" => (
            tree_sitter_c::LANGUAGE.into(),
            tree_sitter_c::HIGHLIGHT_QUERY.to_owned(),
        ),
        "cpp" => (
            tree_sitter_cpp::LANGUAGE.into(),
            [
                tree_sitter_c::HIGHLIGHT_QUERY,
                tree_sitter_cpp::HIGHLIGHT_QUERY,
            ]
            .join("\n"),
        ),
        "css" => (
            tree_sitter_css::LANGUAGE.into(),
            tree_sitter_css::HIGHLIGHTS_QUERY.to_owned(),
        ),
        "go" => (
            tree_sitter_go::LANGUAGE.into(),
            tree_sitter_go::HIGHLIGHTS_QUERY.to_owned(),
        ),
        "html" => (
            tree_sitter_html::LANGUAGE.into(),
            tree_sitter_html::HIGHLIGHTS_QUERY.to_owned(),
        ),
        "java" => (
            tree_sitter_java::LANGUAGE.into(),
            tree_sitter_java::HIGHLIGHTS_QUERY.to_owned(),
        ),
        "javascript" => (
            tree_sitter_javascript::LANGUAGE.into(),
            [
                tree_sitter_javascript::HIGHLIGHT_QUERY,
                tree_sitter_javascript::JSX_HIGHLIGHT_QUERY,
            ]
            .join("\n"),
        ),
        "json" => (
            tree_sitter_json::LANGUAGE.into(),
            tree_sitter_json::HIGHLIGHTS_QUERY.to_owned(),
        ),
        "markdown" => (
            tree_sitter_md::LANGUAGE.into(),
            // @none does not mask an enclosing capture in tree-sitter-highlight.
            // Leave fence bodies to injections, including unknown, unstyled ones.
            tree_sitter_md::HIGHLIGHT_QUERY_BLOCK.replace("  (fenced_code_block)\n", ""),
        ),
        "markdown_inline" => (
            tree_sitter_md::INLINE_LANGUAGE.into(),
            tree_sitter_md::HIGHLIGHT_QUERY_INLINE.to_owned(),
        ),
        "python" => (
            tree_sitter_python::LANGUAGE.into(),
            tree_sitter_python::HIGHLIGHTS_QUERY.to_owned(),
        ),
        "ruby" => (
            tree_sitter_ruby::LANGUAGE.into(),
            tree_sitter_ruby::HIGHLIGHTS_QUERY.to_owned(),
        ),
        "rust" => (
            tree_sitter_rust::LANGUAGE.into(),
            tree_sitter_rust::HIGHLIGHTS_QUERY.to_owned(),
        ),
        "toml" => (
            tree_sitter_toml_ng::LANGUAGE.into(),
            tree_sitter_toml_ng::HIGHLIGHTS_QUERY.to_owned(),
        ),
        "typescript" | "tsx" => (
            if lang == "tsx" {
                tree_sitter_typescript::LANGUAGE_TSX.into()
            } else {
                tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
            },
            [
                tree_sitter_javascript::HIGHLIGHT_QUERY,
                if lang == "tsx" {
                    tree_sitter_javascript::JSX_HIGHLIGHT_QUERY
                } else {
                    ""
                },
                tree_sitter_typescript::HIGHLIGHTS_QUERY,
            ]
            .join("\n"),
        ),
        "yaml" => (
            tree_sitter_yaml::LANGUAGE.into(),
            tree_sitter_yaml::HIGHLIGHTS_QUERY.to_owned(),
        ),
        _ => return None,
    };
    configure(lang, grammar, &query)
}

fn configure(
    lang: &str,
    grammar: tree_sitter::Language,
    query: &str,
) -> Option<HighlightConfiguration> {
    // Inline nodes hold their text as child ranges; without include-children
    // the injected inline parse sees nothing.
    let markdown_injections = tree_sitter_md::INJECTION_QUERY_BLOCK.replace(
        "(#set! injection.language \"markdown_inline\"))",
        "(#set! injection.language \"markdown_inline\")\n  (#set! injection.include-children))",
    );
    let injections: &str = match lang {
        "markdown" => &markdown_injections,
        "markdown_inline" => tree_sitter_md::INJECTION_QUERY_INLINE,
        _ => "",
    };
    let mut config = HighlightConfiguration::new(grammar, lang, query, injections, "").ok()?;
    let names: Vec<_> = CAPTURES.iter().map(|(name, _)| *name).collect();
    config.configure(&names);
    Some(config)
}

/// Highlight spans for `source`; empty for unknown languages, oversize input, timeouts or parse failure.
#[must_use]
pub fn highlight(source: &str, lang: &str) -> Vec<Span> {
    if source.len() > MAX_BYTES {
        return Vec::new();
    }
    let Some(config) = language(lang).and_then(configuration) else {
        return Vec::new();
    };
    if config.language_name == "markdown" {
        warm_markdown_injections(source);
    }
    let cancelled = AtomicUsize::new(0);
    let started = Instant::now();
    let spans = std::thread::scope(|scope| {
        let (done, finished) = mpsc::channel::<()>();
        let flag = &cancelled;
        // Wakes immediately on completion rather than waiting out the budget.
        scope.spawn(move || {
            if matches!(
                finished.recv_timeout(TIMEOUT.saturating_sub(started.elapsed())),
                Err(mpsc::RecvTimeoutError::Timeout)
            ) {
                flag.store(1, Ordering::Relaxed);
            }
        });
        let spans = collect_spans(source, config, &cancelled);
        drop(done);
        spans
    });
    if cancelled.load(Ordering::Relaxed) != 0 || started.elapsed() >= TIMEOUT {
        Vec::new()
    } else {
        spans.unwrap_or_default()
    }
}

/// Build the configurations a Markdown source will inject before the time
/// budget starts, so a first-time query compile never blanks the result.
fn warm_markdown_injections(source: &str) {
    configuration("markdown_inline");
    for line in source.lines() {
        let line = line.trim_start_matches([' ', '>']);
        let Some(fence) = line.chars().next().filter(|c| matches!(c, '`' | '~')) else {
            continue;
        };
        let info = line.trim_start_matches(fence);
        if line.len() - info.len() >= 3
            && let Some(lang) = language(info)
        {
            configuration(lang);
        }
    }
    if source.contains('<') {
        configuration("html");
    }
    if source.starts_with("---") {
        configuration("yaml");
    }
    if source.starts_with("+++") {
        configuration("toml");
    }
}

fn collect_spans(
    source: &str,
    config: &HighlightConfiguration,
    cancelled: &AtomicUsize,
) -> Option<Vec<Span>> {
    let mut highlighter = Highlighter::new();
    let events = highlighter
        .highlight(config, source.as_bytes(), None, Some(cancelled), |name| {
            if name == "markdown_inline" {
                configuration(name)
            } else {
                language(name).and_then(configuration)
            }
        })
        .ok()?;
    let mut active = Vec::new();
    let mut spans: Vec<Span> = Vec::new();
    for event in events {
        match event.ok()? {
            HighlightEvent::HighlightStart(index) => active.push(CAPTURES.get(index.0)?.1),
            HighlightEvent::HighlightEnd => {
                active.pop()?;
            }
            HighlightEvent::Source { start, end } => {
                if start == end {
                    continue;
                }
                if !source.is_char_boundary(start) || !source.is_char_boundary(end) {
                    return None;
                }
                if let Some(&role) = active.last() {
                    if let Some(last) = spans.last_mut()
                        && last.range.end == start
                        && last.role == role
                    {
                        last.range.end = end;
                    } else {
                        spans.push(Span {
                            range: start..end,
                            role,
                        });
                    }
                }
            }
        }
    }
    Some(spans)
}

#[cfg(test)]
#[path = "highlight_tests.rs"]
mod tests;
