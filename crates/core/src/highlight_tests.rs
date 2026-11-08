use super::{MAX_BYTES, Role, Span, highlight, language};

fn checked_highlight(source: &str, lang: &str) -> Vec<Span> {
    let spans = highlight(source, lang);
    let mut previous: Option<&Span> = None;
    for span in &spans {
        assert!(span.range.start < span.range.end, "{lang}: {span:?}");
        assert!(span.range.end <= source.len(), "{lang}: {span:?}");
        assert!(source.is_char_boundary(span.range.start));
        assert!(source.is_char_boundary(span.range.end));
        if let Some(previous) = previous {
            assert!(previous.range.end <= span.range.start, "{lang}: {spans:?}");
            assert!(
                previous.range.end != span.range.start || previous.role != span.role,
                "adjacent spans of the same role were not merged: {spans:?}"
            );
        }
        previous = Some(span);
    }
    spans
}

fn assert_token(source: &str, spans: &[Span], role: Role, token: &str) {
    assert!(
        spans
            .iter()
            .any(|span| span.role == role && source[span.range.clone()].contains(token)),
        "missing {role:?} over {token:?} in {source:?}: {spans:?}"
    );
}

#[test]
fn language_aliases() {
    for (alias, canonical) in [
        ("RS", "rust"),
        ("py", "python"),
        ("yml", "yaml"),
        ("c++", "cpp"),
        ("{.sh}", "bash"),
        ("ts title=x", "typescript"),
        ("sh", "bash"),
        ("zsh", "bash"),
        ("shell", "bash"),
        ("console", "bash"),
        ("h", "c"),
        ("cc", "cpp"),
        ("cxx", "cpp"),
        ("hpp", "cpp"),
        ("golang", "go"),
        ("htm", "html"),
        ("xml", "html"),
        ("js", "javascript"),
        ("jsx", "javascript"),
        ("mjs", "javascript"),
        ("cjs", "javascript"),
        ("node", "javascript"),
        ("jsonc", "json"),
        ("json5", "json"),
        ("md", "markdown"),
        ("python3", "python"),
        ("rb", "ruby"),
        ("mts", "typescript"),
        ("cts", "typescript"),
    ] {
        assert_eq!(language(alias), Some(canonical), "{alias}");
        assert_eq!(language(canonical), Some(canonical));
    }
    assert_eq!(language("brainfuck"), None);
    assert_eq!(language(""), None);
    assert_eq!(language("  \n"), None);
}

#[test]
fn rust_tokens() {
    let source = "fn main() { let x = \"hi\"; // c\n}";
    let spans = checked_highlight(source, "rust");
    for (role, token) in [
        (Role::Keyword, "fn"),
        (Role::Keyword, "let"),
        (Role::Function, "main"),
        (Role::String, "\"hi\""),
        (Role::Comment, "// c"),
    ] {
        assert_token(source, &spans, role, token);
    }
}

#[test]
fn bundled_language_queries() {
    for (lang, source, role, token) in [
        (
            "python",
            "def greet():\n    return 'hi'\n",
            Role::Keyword,
            "def",
        ),
        ("json", "{\"key\": true, \"n\": 42}", Role::Number, "42"),
        ("bash", "echo \"hi\"\n", Role::Function, "echo"),
        ("yaml", "name: \"hi\"\n", Role::String, "hi"),
        ("toml", "name = \"hi\"\n", Role::String, "hi"),
        ("typescript", "const n: number = 1;", Role::Type, "number"),
        (
            "tsx",
            "const el: JSX.Element = <div title=\"hi\" />;",
            Role::Tag,
            "div",
        ),
        (
            "go",
            "package main\nfunc main() {}\n",
            Role::Keyword,
            "func",
        ),
        ("html", "<div class=\"x\">hi</div>", Role::Tag, "div"),
        ("css", "a { color: red; }\n", Role::Property, "color"),
        (
            "c",
            "int main(void) { return 0; }\n",
            Role::Keyword,
            "return",
        ),
        (
            "cpp",
            "class Thing { public: int value; };\n",
            Role::Keyword,
            "class",
        ),
        (
            "java",
            "class Main { int n = 1; }\n",
            Role::Keyword,
            "class",
        ),
        ("ruby", "def greet\n  'hi'\nend\n", Role::Keyword, "def"),
        (
            "javascript",
            "function greet() { return true; }\n",
            Role::Keyword,
            "function",
        ),
    ] {
        let spans = checked_highlight(source, lang);
        assert_token(source, &spans, role, token);
    }
}

#[test]
fn inherited_queries() {
    for (lang, source, role, token) in [
        ("typescript", "const n: number = 1;", Role::Keyword, "const"),
        ("tsx", "const el = <div />;", Role::Keyword, "const"),
        ("cpp", "int main() { return 0; }", Role::Keyword, "return"),
    ] {
        let spans = checked_highlight(source, lang);
        assert_token(source, &spans, role, token);
    }
}

#[test]
fn markdown_inline_and_fenced_injections() {
    let source = "# Title\n\nSome *em* and `code`.\n\n```rust\nfn x() {}\n```\n";
    let spans = checked_highlight(source, "markdown");
    for (role, token) in [
        (Role::Heading, "Title"),
        (Role::Emphasis, "em"),
        (Role::Code, "code"),
        (Role::Keyword, "fn"),
    ] {
        assert_token(source, &spans, role, token);
    }
}

#[test]
fn markdown_fence_alias() {
    let source = "```RS title=x\nfn x() {}\n```\n";
    let spans = checked_highlight(source, "md");
    assert_token(source, &spans, Role::Keyword, "fn");
}

#[test]
fn unknown_languages_are_unstyled() {
    let source = "fn main() { let x = \"hi\"; }";
    assert!(checked_highlight(source, "brainfuck").is_empty());
    assert!(checked_highlight(source, "").is_empty());
    for fence_language in ["brainfuck", ""] {
        let source = format!("# Title\n\n```{fence_language}\nfn x() {{}}\n```\n");
        let spans = checked_highlight(&source, "markdown");
        assert_token(&source, &spans, Role::Heading, "Title");
        let start = source.find("fn x()").unwrap();
        let end = start + "fn x() {}\n".len();
        assert!(
            spans
                .iter()
                .all(|span| span.range.end <= start || span.range.start >= end),
            "unknown fence body was highlighted: {spans:?}"
        );
    }
}

#[test]
fn nested_roles_and_multibyte_boundaries() {
    let source = "let s = \"héllo 🙂\";";
    let spans = checked_highlight(source, "rust");
    assert_token(source, &spans, Role::String, "\"héllo 🙂\"");

    let source = "let s = \"héllo\\n🙂\";";
    let spans = checked_highlight(source, "rust");
    assert_token(source, &spans, Role::String, "héllo");
    assert_token(source, &spans, Role::Escape, "\\n");
    assert_token(source, &spans, Role::String, "🙂");
}

#[test]
fn oversize_input_is_unstyled() {
    let source = "x".repeat(MAX_BYTES + 1);
    assert!(checked_highlight(&source, "rust").is_empty());
}

#[test]
fn cancelled_highlight_discards_spans() {
    let config = super::configuration("rust").unwrap();
    let cancelled = std::sync::atomic::AtomicUsize::new(1);
    // tree-sitter-highlight polls the flag every hundred or so events.
    let source = "fn main() { let x = 1; }\n".repeat(200);
    assert!(super::collect_spans(&source, config, &cancelled).is_none());
}

#[test]
fn stable_role_names() {
    for (role, name) in [
        (Role::Keyword, "keyword"),
        (Role::String, "string"),
        (Role::Comment, "comment"),
        (Role::Function, "function"),
        (Role::Type, "type"),
        (Role::Number, "number"),
        (Role::Constant, "constant"),
        (Role::Operator, "operator"),
        (Role::Punctuation, "punctuation"),
        (Role::Property, "property"),
        (Role::Variable, "variable"),
        (Role::Tag, "tag"),
        (Role::Attribute, "attribute"),
        (Role::Escape, "escape"),
        (Role::Heading, "heading"),
        (Role::Emphasis, "emphasis"),
        (Role::Strong, "strong"),
        (Role::Link, "link"),
        (Role::Code, "code"),
        (Role::Quote, "quote"),
        (Role::ListMarker, "list-marker"),
    ] {
        assert_eq!(role.name(), name);
    }
}

#[test]
fn segments_cover_the_source_and_roles_map_to_design_colors() {
    let source = "let x = \"é\"; // c";
    let pieces = super::segments(source, "rust");
    let joined: String = pieces.iter().map(|(r, _)| &source[r.clone()]).collect();
    assert_eq!(joined, source);
    assert!(pieces.windows(2).all(|w| w[0].0.end == w[1].0.start));
    assert!(pieces.iter().any(|(_, role)| *role == Some(Role::Keyword)));
    assert_eq!(super::segments("plain", "nope"), vec![(0..5, None)]);
    assert!(super::segments("", "rust").is_empty());
    assert_eq!(Role::Keyword.color_key(), "accent");
    assert_eq!(Role::String.color_key(), "accent2");
    assert_eq!(Role::Number.color_key(), "accent3");
    assert_eq!(Role::Comment.color_key(), "muted");
}
