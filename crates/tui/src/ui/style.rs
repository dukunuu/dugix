//! The TUI's visual vocabulary. Every screen draws from here so one meaning
//! always looks one way: a single monochrome glyph per concept (no emoji —
//! their widths vary by terminal and they clash with the line-art chrome),
//! one color per role, one way to draw a section header, a separator, or a
//! line of metadata.
//!
//! Color roles:
//! - `accent`: you, and anything interactive or waiting on your input
//!   (your turns, the prompt, selections, titles, questions for you)
//! - `accent2`: the assistant and its agents (replies, tools, research)
//! - `fg_dim`: metadata (times, stats, sizes, hints)
//! - `border_dim`: chrome (frames, rules, rails, separators)
//! - `success` / `warning` / `error`: status, nothing else

use nexus_core::highlight::Role;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::theme::Theme;

/// One glyph per concept, each a single terminal column.
pub mod glyph {
    /// Your turns and the composer prompt.
    pub const YOU: &str = "❯";
    /// An assistant reply.
    pub const ASSISTANT: &str = "✦";
    /// A tool call.
    pub const TOOL: &str = "⚒";
    /// Research: stage rows and research sessions.
    pub const RESEARCH: &str = "◇";
    /// A question round waiting on your answer.
    pub const QUESTION: &str = "?";
    /// A plan waiting on your approval.
    pub const PLAN: &str = "☰";
    /// A compaction digest.
    pub const DIGEST: &str = "≡";
    /// A link to another session.
    pub const LINK: &str = "↪";
    /// An image or document placeholder.
    pub const MEDIA: &str = "▣";
    /// The active model.
    pub const MODEL: &str = "◆";
    /// The active space.
    pub const SPACE: &str = "⌂";
    /// Web answer mode.
    pub const WEB: &str = "◎";
    /// A call to a connected service (an MCP connector).
    pub const CONNECTOR: &str = "⇄";
    /// Incognito mode.
    pub const INCOGNITO: &str = "◌";
    /// Something running.
    pub const RUNNING: &str = "⟳";
    /// Finished while you weren't looking; a toggle that's on.
    pub const DOT: &str = "●";
    /// A toggle that's off; an inactive choice.
    pub const RING: &str = "○";
    /// Success.
    pub const OK: &str = "✓";
    /// Failure.
    pub const FAIL: &str = "✗";
    /// A warning (text form: one column in every terminal).
    pub const WARN: &str = "⚠";
    /// The selected row; a collapsed section.
    pub const SELECTED: &str = "▸";
    /// An expanded section.
    pub const EXPANDED: &str = "▾";
    /// Section-header bar.
    pub const SECTION: &str = "▍";
    /// A card's left rail.
    pub const RAIL: &str = "▎";
    /// A favorite.
    pub const FAVORITE: &str = "★";
    /// Accepts images.
    pub const VISION: &str = "⊡";
    /// Previous / next item in a viewer.
    pub const PREV: &str = "‹";
    pub const NEXT: &str = "›";
}

/// The one separator between inline fields.
pub const SEP: &str = " · ";

/// Theme colors for syntax roles (`nexus_core::highlight`). Code reuses the
/// existing roles rather than adding a palette: keywords and markup in
/// `accent`, calls in `accent2`, literals in the status colors, comments and
/// punctuation dim.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SyntaxColors {
    pub accent: Color,
    pub accent2: Color,
    pub warning: Color,
    pub success: Color,
    pub error: Color,
    pub dim: Color,
    pub fg: Color,
}

impl SyntaxColors {
    #[must_use]
    pub fn from_theme(theme: &Theme) -> Self {
        Self {
            accent: theme.accent,
            accent2: theme.accent2,
            warning: theme.warning,
            success: theme.success,
            error: theme.error,
            dim: theme.fg_dim,
            fg: theme.fg,
        }
    }
}

impl Default for SyntaxColors {
    fn default() -> Self {
        Self {
            accent: Color::Cyan,
            accent2: Color::Magenta,
            warning: Color::Yellow,
            success: Color::Green,
            error: Color::Red,
            dim: Color::DarkGray,
            fg: Color::Reset,
        }
    }
}

/// The one style for each syntax role.
#[must_use]
pub fn syntax_style(colors: SyntaxColors, role: Role) -> Style {
    let fg = |color: Color| Style::default().fg(color);
    match role {
        Role::Keyword | Role::Tag => fg(colors.accent),
        Role::Function => fg(colors.accent2),
        Role::Type | Role::Attribute => fg(colors.warning),
        Role::String | Role::Code => fg(colors.success),
        Role::Number | Role::Constant | Role::Escape => fg(colors.error),
        Role::Comment => fg(colors.dim).add_modifier(Modifier::ITALIC),
        Role::Punctuation | Role::Quote | Role::ListMarker => fg(colors.dim),
        Role::Operator | Role::Property | Role::Variable => fg(colors.fg),
        Role::Heading => fg(colors.accent).add_modifier(Modifier::BOLD),
        Role::Emphasis => Style::default().add_modifier(Modifier::ITALIC),
        Role::Strong => Style::default().add_modifier(Modifier::BOLD),
        Role::Link => fg(colors.accent).add_modifier(Modifier::UNDERLINED),
    }
}

/// `source` split on `\n` into one styled line per source line, colored by
/// tree-sitter spans for `lang`; text outside any span keeps `base`.
#[must_use]
pub fn highlighted_lines(
    source: &str,
    lang: &str,
    colors: SyntaxColors,
    base: Style,
) -> Vec<Line<'static>> {
    let spans = nexus_core::highlight::highlight(source, lang);
    let mut spans = spans.iter().peekable();
    let mut lines = Vec::new();
    let mut start = 0;
    for text in source.split('\n') {
        let end = start + text.len();
        let mut parts: Vec<Span<'static>> = Vec::new();
        let mut at = start;
        while let Some(span) = spans.peek() {
            if span.range.start >= end {
                break;
            }
            let from = span.range.start.max(at);
            let to = span.range.end.min(end);
            if from > at {
                parts.push(Span::styled(source[at..from].to_string(), base));
            }
            if to > from {
                parts.push(Span::styled(
                    source[from..to].to_string(),
                    base.patch(syntax_style(colors, span.role)),
                ));
                at = to;
            }
            if span.range.end <= end {
                spans.next();
            } else {
                break;
            }
        }
        if end > at || parts.is_empty() {
            parts.push(Span::styled(source[at..end].to_string(), base));
        }
        lines.push(Line::from(parts));
        start = end + 1;
    }
    lines
}

/// A popup or panel title: the name in bold accent.
pub fn title(theme: &Theme, name: impl Into<String>) -> Span<'static> {
    Span::styled(
        format!(" {} ", name.into()),
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD),
    )
}

/// `▍title ─────` — the one section header, ruled out to `width`.
pub fn section(theme: &Theme, name: &str, width: usize) -> Line<'static> {
    let head = format!("{} {name} ", glyph::SECTION);
    let rule = width.saturating_sub(crate::ui::style::width(&head));
    Line::from(vec![
        Span::styled(
            head,
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("─".repeat(rule), Style::default().fg(theme.border_dim)),
    ])
}

/// Metadata text: times, stats, sizes, hints.
pub fn meta(theme: &Theme, text: impl Into<String>) -> Span<'static> {
    Span::styled(text.into(), Style::default().fg(theme.fg_dim))
}

/// Display columns of `s`: CJK and most emoji take two, so layout math
/// measures with this, never `chars().count()`.
pub fn width(s: &str) -> usize {
    unicode_width::UnicodeWidthStr::width(s)
}

/// The inline field separator, in the chrome color.
pub fn sep(theme: &Theme) -> Span<'static> {
    Span::styled(SEP, Style::default().fg(theme.border_dim))
}

/// A transcript event's first line: role-colored glyph, then its title.
pub fn event_head(
    glyph: &str,
    glyph_style: Style,
    title: String,
    title_style: Style,
) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{glyph} "), glyph_style),
        Span::styled(title, title_style),
    ])
}

/// A transcript event's body line, indented under its title.
pub fn event_body(text: &str, style: Style) -> Line<'static> {
    Line::from(Span::styled(format!("  {text}"), style))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The vocabulary is monochrome: an emoji in any UI string (outside
    /// tests) would reintroduce varying widths and a second visual language.
    #[test]
    fn ui_strings_use_the_glyph_vocabulary_not_emoji() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/ui");
        let mut stack = vec![root];
        let mut offenders = Vec::new();
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.file_name().is_some_and(|n| n == "tests.rs") {
                    continue;
                }
                let src = std::fs::read_to_string(&path).unwrap();
                let code = src.split("#[cfg(test)]").next().unwrap_or("");
                for (n, line) in code.lines().enumerate() {
                    let is_comment = line.trim_start().starts_with("//");
                    if !is_comment
                        && line
                            .chars()
                            .any(|c| ('\u{1F000}'..='\u{1FFFF}').contains(&c))
                    {
                        offenders.push(format!("{}:{}: {}", path.display(), n + 1, line.trim()));
                    }
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "emoji in UI strings:\n{}",
            offenders.join("\n")
        );
    }

    #[test]
    fn highlighted_lines_split_multiline_spans_and_keep_text() {
        let colors = SyntaxColors::default();
        let source = "/* a\nb */ fn x() {}\n\nlet é = 1;";
        let lines = highlighted_lines(source, "rust", colors, Style::default());
        let text: Vec<String> = lines
            .iter()
            .map(|line| line.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        assert_eq!(text, ["/* a", "b */ fn x() {}", "", "let é = 1;"]);
        let comment = syntax_style(colors, Role::Comment);
        assert_eq!(lines[0].spans[0].style, comment);
        assert_eq!(lines[1].spans[0].content, "b */");
        assert_eq!(lines[1].spans[0].style, comment);
        let plain = highlighted_lines("a\nb", "nope", colors, Style::default());
        assert_eq!(plain.len(), 2);
        assert_eq!(plain[1].spans[0].content, "b");
    }
}
