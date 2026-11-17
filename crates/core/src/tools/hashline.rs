//! Hash-anchored line numbering and edits, plus the unified diff shown for edits.

use std::fmt::Write as _;

use sha2::{Digest, Sha256};

/// `cat -n`-style numbering for ranged reads.
pub(super) fn number_lines(slice: &[&str], start: usize) -> String {
    slice
        .iter()
        .enumerate()
        .map(|(i, l)| format!("{:>5}\t{l}", start + i + 1))
        .collect::<Vec<_>>()
        .join("\n")
}

/// sha256(content) truncated to 8 hex chars, so an edit elsewhere keeps it valid.
pub(super) fn line_hash(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content);
    hasher
        .finalize()
        .iter()
        .take(4)
        .fold(String::new(), |mut h, b| {
            let _ = write!(h, "{b:02x}");
            h
        })
}

/// Hash format used by older reads; still accepted for a running session.
fn legacy_line_hash(line_no: usize, content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(format!("{line_no}:{content}"));
    hasher
        .finalize()
        .iter()
        .take(4)
        .fold(String::new(), |mut h, b| {
            let _ = write!(h, "{b:02x}");
            h
        })
}

/// `N:HASH<tab>content` per line, so `edit_file` can target it without substring
/// matching.
pub(crate) fn number_lines_with_hash(slice: &[&str], start: usize) -> String {
    slice
        .iter()
        .enumerate()
        .map(|(i, l)| {
            let n = start + i + 1;
            format!("{:>5}:{}\t{l}", n, line_hash(l))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Apply hashline edits to `text`, returning the new content and a
/// git-diff-style summary of what changed.
pub(crate) fn apply_hashline_edits(
    text: &str,
    edits: &[(String, Option<usize>, Option<String>)],
) -> Result<(String, String), String> {
    if edits.is_empty() {
        return Err("edits must not be empty".to_string());
    }
    let lines: Vec<&str> = text.lines().collect();
    let mut resolved: Vec<(usize, Option<String>)> = Vec::new();
    let mut seen: std::collections::HashSet<usize> = std::collections::HashSet::new();
    for (hash, line_hint, new) in edits {
        let matches: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(i, l)| line_hash(l) == *hash || legacy_line_hash(i + 1, l) == *hash)
            .map(|(i, _)| i)
            .collect();
        let idx = match matches.as_slice() {
            [] => {
                return Err(format!(
                    "hash {hash} not found — read the file again, it may have changed"
                ));
            }
            [idx] => *idx,
            _ => line_hint
                .and_then(|line| line.checked_sub(1))
                .filter(|idx| matches.contains(idx))
                .ok_or_else(|| {
                    format!(
                        "hash {hash} matches multiple lines — include the 1-based line from the latest read"
                    )
                })?,
        };
        if !seen.insert(idx) {
            return Err(format!(
                "hash {hash} targets a line already edited by another entry in this call"
            ));
        }
        resolved.push((idx, new.clone()));
    }

    let mut diff = String::new();
    for (idx, new) in &resolved {
        let _ = write!(diff, "\n- {}", lines[*idx]);
        if let Some(t) = new {
            for l in t.lines() {
                let _ = write!(diff, "\n+ {l}");
            }
        }
    }

    // Apply highest index first so earlier (lower) indices, still unprocessed,
    // stay valid regardless of how many lines an edit adds or removes.
    let mut apply_order = resolved;
    apply_order.sort_by_key(|b| std::cmp::Reverse(b.0));
    let mut out: Vec<String> = lines.iter().map(std::string::ToString::to_string).collect();
    for (idx, new) in apply_order {
        match new {
            None => {
                out.remove(idx);
            }
            Some(t) => {
                let replacement: Vec<String> = t.lines().map(str::to_string).collect();
                out.splice(idx..=idx, replacement);
            }
        }
    }
    let mut new_text = out.join("\n");
    if text.ends_with('\n') && !out.is_empty() {
        new_text.push('\n');
    }
    Ok((new_text, diff))
}

/// Compare two file contents with git's unified-diff renderer; `--no-index` keeps
/// the app files out of any repository.
pub(super) fn unified_diff(app: &str, path: &str, current: &str, candidate: &str) -> String {
    if current == candidate {
        return "no changes".to_string();
    }
    let dir = std::env::temp_dir().join(format!("dugix-diff-{}", uuid::Uuid::new_v4()));
    let before = dir.join("before");
    let after = dir.join("after");
    let result = (|| {
        std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create diff workspace: {e}"))?;
        std::fs::write(&before, current)
            .map_err(|e| format!("cannot write current snapshot: {e}"))?;
        std::fs::write(&after, candidate)
            .map_err(|e| format!("cannot write candidate snapshot: {e}"))?;
        let output = std::process::Command::new("git")
            .arg("diff")
            .arg("--no-index")
            .arg("--no-ext-diff")
            .arg("--unified=3")
            .arg(&before)
            .arg(&after)
            .output()
            .map_err(|e| format!("cannot run git diff: {e}"))?;
        match output.status.code() {
            Some(0) => Ok("no changes".to_string()),
            Some(1) => {
                let old_label = format!("a/{app}/{path}");
                let new_label = format!("b/{app}/{path}");
                let before_path = before.to_string_lossy();
                let after_path = after.to_string_lossy();
                let diff = String::from_utf8_lossy(&output.stdout)
                    .replace(before_path.as_ref(), &old_label)
                    .replace(after_path.as_ref(), &new_label);
                Ok(diff)
            }
            _ => Err(String::from_utf8_lossy(&output.stderr).trim().to_string()),
        }
    })();
    let _ = std::fs::remove_dir_all(&dir);
    result.unwrap_or_else(|e| format!("diff failed: {e}"))
}
