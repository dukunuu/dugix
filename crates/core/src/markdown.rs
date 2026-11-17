//! Plain-text markdown helpers: `to_plain` (markers stripped, for clipboard copy)
//! and the GFM pipe-table splitter shared with the TUI's styled renderer.

#[derive(Clone, Copy)]
pub enum TableAlign {
    Left,
    Center,
    Right,
}

pub enum TableSegment {
    Text(String),
    Table(Vec<Vec<String>>, Vec<TableAlign>),
}

/// A table starts at a header row followed by a valid delimiter row and continues
/// while lines still look like rows; fenced code blocks are skipped.
pub fn split_tables(content: &str) -> Vec<TableSegment> {
    let lines: Vec<&str> = content.lines().collect();
    let mut segments = Vec::new();
    let mut buf: Vec<&str> = Vec::new();
    let mut in_fence = false;
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            buf.push(line);
            i += 1;
            continue;
        }
        if !in_fence && is_delimiter_row(lines.get(i + 1)) && looks_like_row(line) {
            if !buf.is_empty() {
                segments.push(TableSegment::Text(std::mem::take(&mut buf).join("\n")));
            }
            let aligns = parse_aligns(lines[i + 1]);
            let mut rows = vec![split_cells(line)];
            let mut j = i + 2;
            while j < lines.len() && looks_like_row(lines[j]) {
                rows.push(split_cells(lines[j]));
                j += 1;
            }
            segments.push(TableSegment::Table(rows, aligns));
            i = j;
            continue;
        }
        buf.push(line);
        i += 1;
    }
    if !buf.is_empty() {
        segments.push(TableSegment::Text(buf.join("\n")));
    }
    segments
}

fn looks_like_row(line: &str) -> bool {
    let t = line.trim();
    !t.is_empty() && t.contains('|')
}

fn is_delimiter_row(line: Option<&&str>) -> bool {
    let Some(line) = line else { return false };
    if !looks_like_row(line) {
        return false;
    }
    let cells = split_cells(line);
    !cells.is_empty()
        && cells.iter().all(|c| {
            let c = c.trim_matches(':');
            !c.is_empty() && c.chars().all(|ch| ch == '-')
        })
}

fn parse_aligns(delim_line: &str) -> Vec<TableAlign> {
    split_cells(delim_line)
        .iter()
        .map(|c| {
            let c = c.trim();
            match (c.starts_with(':'), c.ends_with(':')) {
                (true, true) => TableAlign::Center,
                (false, true) => TableAlign::Right,
                _ => TableAlign::Left,
            }
        })
        .collect()
}

/// Doesn't handle escaped `\|`.
fn split_cells(line: &str) -> Vec<String> {
    let t = line.trim();
    let t = t.strip_prefix('|').unwrap_or(t);
    let t = t.strip_suffix('|').unwrap_or(t);
    t.split('|').map(|c| c.trim().to_string()).collect()
}

fn plain_table(rows: &[Vec<String>], aligns: &[TableAlign]) -> String {
    let Some(header) = rows.first() else {
        return String::new();
    };
    let mut out = vec![format!("| {} |", header.join(" | "))];
    let delim: Vec<&str> = aligns
        .iter()
        .map(|a| match a {
            TableAlign::Left => "---",
            TableAlign::Right => "---:",
            TableAlign::Center => ":---:",
        })
        .collect();
    out.push(format!("| {} |", delim.join(" | ")));
    for row in &rows[1..] {
        out.push(format!("| {} |", row.join(" | ")));
    }
    out.join("\n")
}

/// `content` with markdown markers stripped, for clipboard copy.
pub fn to_plain(content: &str) -> String {
    let content = ui_fences_to_plain(content);
    split_tables(&content)
        .into_iter()
        .map(|seg| match seg {
            TableSegment::Table(rows, aligns) => plain_table(&rows, &aligns),
            TableSegment::Text(text) => plain_text_segment(&text),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Replace each valid `dugix-ui` block with its readable text, so copying a
/// designed reply gives words rather than JSON; invalid or unclosed blocks are kept.
fn ui_fences_to_plain(content: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut lines = content.lines();
    while let Some(line) = lines.next() {
        if !line
            .trim_start()
            .strip_prefix("```")
            .map(str::trim)
            .is_some_and(crate::interactive::is_ui_fence)
        {
            out.push(line.to_string());
            continue;
        }
        let mut body = Vec::new();
        let mut closed = false;
        for inner in lines.by_ref() {
            if inner.trim_start().starts_with("```") {
                closed = true;
                break;
            }
            body.push(inner);
        }
        match crate::interactive::Document::parse(&body.join("\n")) {
            Ok(document) if closed => out.push(document.plain_text()),
            _ => {
                out.push(line.to_string());
                out.extend(body.iter().map(|line| (*line).to_string()));
                if closed {
                    out.push("```".to_string());
                }
            }
        }
    }
    out.join("\n")
}

fn plain_text_segment(content: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut in_fence = false;
    for line in content.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
            continue; // fence lines themselves are dropped
        }
        if in_fence {
            out.push(line.to_string()); // code content verbatim
            continue;
        }
        let plain = strip_inline(line);
        match classify_line(&plain) {
            Block::Drop => {}
            Block::Header(rest) | Block::List(rest) => out.push(rest),
            Block::Plain => out.push(plain),
        }
    }
    out.join("\n")
}

enum Block {
    Drop,
    Header(String),
    List(String),
    Plain,
}

/// Block markers (`#`, `-`) are stripped, fence lines dropped, everything else kept.
fn classify_line(plain: &str) -> Block {
    let trimmed = plain.trim_start();
    if trimmed.starts_with("```") {
        return Block::Drop;
    }
    if let Some(rest) = header_rest(trimmed) {
        return Block::Header(rest);
    }
    if let Some(rest) = list_rest(plain) {
        return Block::List(rest);
    }
    Block::Plain
}

fn header_rest(trimmed: &str) -> Option<String> {
    let hashes = trimmed.chars().take_while(|&c| c == '#').count();
    if (1..=6).contains(&hashes) && trimmed[hashes..].starts_with(' ') {
        Some(trimmed[hashes..].trim_start().to_string())
    } else {
        None
    }
}

fn list_rest(plain: &str) -> Option<String> {
    let indent_len = plain.len() - plain.trim_start().len();
    let (indent, s) = plain.split_at(indent_len);
    for marker in ["- ", "* ", "+ "] {
        if let Some(rest) = s.strip_prefix(marker) {
            return Some(format!("{indent}• {rest}"));
        }
    }
    None
}

/// Unmatched markers are left as literal text, as `tui_markdown` does.
fn strip_inline(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '!'
            && chars.get(i + 1) == Some(&'[')
            && let Some((alt, rest)) = take_link(&chars, i + 1)
        {
            out.push_str(&alt);
            i = rest;
            continue;
        }
        if c == '['
            && let Some((link_text, rest)) = take_link(&chars, i)
        {
            out.push_str(&link_text);
            i = rest;
            continue;
        }
        // A run of backticks closes a run of the same length, per CommonMark.
        if c == '`' {
            let run = chars[i..].iter().take_while(|&&x| x == '`').count();
            if let Some(close) = find_run(&chars, i + run, run, '`') {
                let code: String = chars[i + run..close].iter().collect();
                out.push_str(code.trim());
                i = close + run;
                continue;
            }
        }
        if c == '~'
            && chars.get(i + 1) == Some(&'~')
            && let Some(close) = find_run(&chars, i + 2, 2, '~')
        {
            out.push_str(&chars[i + 2..close].iter().collect::<String>());
            i = close + 2;
            continue;
        }
        if (c == '*' || c == '_')
            && let Some(run) = emphasis_run(&chars, i)
            && let Some(close) = find_run(&chars, i + run, run, c)
        {
            out.push_str(&chars[i + run..close].iter().collect::<String>());
            i = close + run;
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

/// A link starting at `open` (the `[`): `(text, index after ')')`.
fn take_link(chars: &[char], open: usize) -> Option<(String, usize)> {
    let close = chars[open..].iter().position(|&c| c == ']')? + open;
    if chars.get(close + 1) != Some(&'(') {
        return None;
    }
    let end = chars[close + 2..].iter().position(|&c| c == ')')? + close + 2;
    Some((chars[open + 1..close].iter().collect(), end + 1))
}

fn find_run(chars: &[char], from: usize, run: usize, marker: char) -> Option<usize> {
    if run == 0 {
        return None;
    }
    let start = chars[from..].iter().position(|&x| x == marker)? + from;
    let len = chars[start..].iter().take_while(|&&x| x == marker).count();
    (len >= run).then_some(start)
}

/// Capped at 2: a run of 3+ is left alone (`CommonMark` treats it as strong+em).
fn emphasis_run(chars: &[char], i: usize) -> Option<usize> {
    let c = chars[i];
    let run = chars[i..].iter().take_while(|&&x| x == c).count();
    (run == 1 || run == 2).then_some(run)
}

#[cfg(test)]
mod to_plain_tests {
    #[test]
    fn copying_a_designed_reply_gives_its_text_not_its_json() {
        let reply = "```dugix-ui\n{\"version\":1,\"title\":\"Rust vs Go\",\"view\":[{\"type\":\"callout\",\"tone\":\"tip\",\"text\":\"Go ships faster.\"}]}\n```\nBoth are **fine**.";
        let plain = to_plain(reply);
        assert!(plain.contains("Rust vs Go"), "{plain}");
        assert!(plain.contains("Go ships faster."), "{plain}");
        assert!(plain.contains("Both are fine."), "{plain}");
        assert!(!plain.contains("\"version\""), "{plain}");
        let broken = "```dugix-ui\n{not json}\n```";
        assert!(to_plain(broken).contains("{not json}"));
        assert!(to_plain("```dugix-ui\n{\"version\":1").contains("\"version\":1"));
    }

    use super::*;

    const TABLE: &str = "| Name | Age |\n| --- | ---: |\n| Alice | 30 |\n| Bob | 7 |";

    #[test]
    fn to_plain_reconstructs_clean_markdown_table() {
        let plain = to_plain(TABLE);
        assert_eq!(
            plain,
            "| Name | Age |\n| --- | ---: |\n| Alice | 30 |\n| Bob | 7 |"
        );
    }

    #[test]
    fn detects_table_and_leaves_surrounding_text_alone() {
        let content = format!("before\n\n{TABLE}\n\nafter");
        let segs = split_tables(&content);
        assert_eq!(segs.len(), 3);
        assert!(matches!(&segs[0], TableSegment::Text(t) if t.trim() == "before"));
        match &segs[1] {
            TableSegment::Table(rows, aligns) => {
                assert_eq!(
                    rows,
                    &[
                        vec!["Name".to_string(), "Age".to_string()],
                        vec!["Alice".to_string(), "30".to_string()],
                        vec!["Bob".to_string(), "7".to_string()],
                    ]
                );
                assert!(matches!(aligns[0], TableAlign::Left));
                assert!(matches!(aligns[1], TableAlign::Right));
            }
            TableSegment::Text(_) => panic!("expected a table segment"),
        }
        assert!(matches!(&segs[2], TableSegment::Text(t) if t.trim() == "after"));
    }

    #[test]
    fn pipes_inside_a_fenced_code_block_are_not_a_table() {
        let content = "```\n| not | a | table |\n| --- | --- |\n```";
        let segs = split_tables(content);
        assert_eq!(segs.len(), 1);
        assert!(matches!(&segs[0], TableSegment::Text(_)));
    }

    #[test]
    fn strips_headers_lists_and_inline_markers() {
        let plain = to_plain("# Big\n\n- one\n- two\n\nrun `cargo test` and **see** *it* work");
        assert_eq!(
            plain,
            "Big\n\n• one\n• two\n\nrun cargo test and see it work"
        );
    }

    #[test]
    fn links_become_their_text_and_images_their_alt() {
        let plain = to_plain("see [the docs](https://example.com) or ![diagram](x.png)");
        assert_eq!(plain, "see the docs or diagram");
    }

    #[test]
    fn unmatched_markers_stay_literal() {
        assert_eq!(to_plain("a * lone star"), "a * lone star");
        assert_eq!(to_plain("2 * 3 = 6"), "2 * 3 = 6");
    }

    #[test]
    fn fenced_code_keeps_content_verbatim_and_drops_fences() {
        let md = "```rust\nfn main() { println!(\"hi\"); }\n```\nafter";
        assert_eq!(to_plain(md), "fn main() { println!(\"hi\"); }\nafter");
    }
}
