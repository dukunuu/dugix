//! The agent workspace's `fs` tool: list, glob, grep, read, and approved writes.

use std::fmt::Write as _;
use std::io::{Read as _, Write as _};
use std::path::Path;
use std::sync::Arc;

use super::hashline::{number_lines, unified_diff};
use super::{ToolBox, apply_hashline_edits, number_lines_with_hash};
use crate::workspace::{Access, PathError, Workspace};

impl ToolBox {
    pub(super) async fn run_fs(&self, name: &str, args: &str) -> (String, String) {
        match name {
            "fs_list" | "fs_glob" | "fs_grep" | "fs_read" | "fs_edit" | "fs_write" | "fs_move"
            | "fs_delete" => match &self.workspace {
                None => (
                    "fs needs an agent workspace · switch with /agent <path>".to_string(),
                    "blocked".to_string(),
                ),
                Some(workspace) => {
                    let workspace = Arc::clone(workspace);
                    let action = name.to_string();
                    let args = args.to_string();
                    let result =
                        tokio::task::spawn_blocking(move || fs_action(&workspace, &action, &args))
                            .await
                            .unwrap_or_else(|error| Err(PathError::Io(error.to_string())));
                    match result {
                        Ok(result) => (result, format!("Running {name}…")),
                        Err(error) => (error.to_string(), "blocked".to_string()),
                    }
                }
            },
            other => (
                format!("unknown tool: {other}"),
                "Running tool…".to_string(),
            ),
        }
    }
}

pub(super) fn fs_action(
    workspace: &Arc<Workspace>,
    name: &str,
    args: &str,
) -> Result<String, PathError> {
    let args: serde_json::Value = serde_json::from_str(args).unwrap_or_default();
    let rel = args
        .get("path")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(if matches!(name, "fs_list" | "fs_glob" | "fs_grep") {
            "."
        } else {
            ""
        });
    let access = if matches!(name, "fs_edit" | "fs_write" | "fs_move" | "fs_delete") {
        Access::Write
    } else {
        Access::Read
    };
    let path = workspace.resolve(rel, access)?;
    if access == Access::Write {
        return fs_mutate(workspace, &path, name, &args);
    }
    let number = |key: &str, default: usize, max: usize| {
        usize::try_from(
            args.get(key)
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(default as u64),
        )
        .unwrap_or(max)
        .min(max)
    };
    let pattern = args
        .get("pattern")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    match name {
        "fs_list" => {
            let mut entries = Vec::new();
            fs_list_dir(workspace, &path, number("depth", 2, 6), &mut entries)?;
            Ok(fs_capped_entries(entries, 400))
        }
        "fs_glob" => {
            let base = if path.is_dir() {
                &path
            } else {
                path.parent().unwrap_or(&path)
            };
            let mut builder = ignore::overrides::OverrideBuilder::new(base);
            builder
                .add(pattern)
                .map_err(|error| PathError::Invalid(error.to_string()))?;
            let matcher = builder
                .build()
                .map_err(|error| PathError::Invalid(error.to_string()))?;
            let mut entries = Vec::new();
            for entry in fs_walk(workspace, &path, None) {
                let entry = entry.map_err(|error| PathError::Io(error.to_string()))?;
                if matcher
                    .matched(
                        entry.path(),
                        entry.file_type().is_some_and(|kind| kind.is_dir()),
                    )
                    .is_whitelist()
                {
                    entries.push(fs_relative(workspace, entry.path()));
                }
            }
            entries.sort();
            Ok(fs_capped_entries(entries, 400))
        }
        "fs_grep" => fs_grep(workspace, &path, pattern, number("context", 0, 5)),
        "fs_read" => fs_read(
            workspace,
            &path,
            number("offset", 1, usize::MAX).max(1),
            number("limit", 400, 2000).max(1),
        ),
        _ => unreachable!("fs action dispatch"),
    }
}

fn fs_mutate(
    workspace: &Workspace,
    path: &Path,
    name: &str,
    args: &serde_json::Value,
) -> Result<String, PathError> {
    let rel = fs_relative(workspace, path);
    if name == "fs_write" && !path.try_exists()? {
        return fs_replace(workspace, path, name, args, "", None);
    }
    let Some(previous) = workspace.last_read(path) else {
        return Ok(format!(
            "fs.{} needs a fresh read · call fs with action=read on {rel} first",
            name.trim_start_matches("fs_")
        ));
    };
    let mut file = std::fs::File::open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(PathError::Invalid(format!("{rel} is not a file")));
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let current_stamp = Workspace::stamp_bytes(&bytes, &metadata)?;
    let current = String::from_utf8_lossy(&bytes);
    if previous.len != current_stamp.len || previous.sha256 != current_stamp.sha256 {
        let mut result = format!("{rel} changed since your last read · re-read it and re-apply");
        if let Some(text) = previous.text {
            let _ = write!(result, "\n{}", fs_diff(&rel, &text, &current, 60).0);
        }
        return Ok(result);
    }
    if name == "fs_edit" && (bytes.contains(&0) || std::str::from_utf8(&bytes).is_err()) {
        return Ok(format!(
            "fs.edit needs a UTF-8 text file · {rel} is binary or not UTF-8; use write to replace it whole"
        ));
    }
    match name {
        "fs_edit" | "fs_write" => fs_replace(
            workspace,
            path,
            name,
            args,
            &current,
            Some(metadata.permissions()),
        ),
        "fs_move" => {
            let to = args
                .get("to")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let target = workspace.resolve(to, Access::Write)?;
            if target.try_exists()? {
                return Ok(format!("fs.move target exists: {to} · choose a new path"));
            }
            std::fs::create_dir_all(target.parent().expect("workspace file parent"))?;
            workspace.before_change(path)?;
            workspace.before_change(&target)?;
            // A hard link refuses a target created after the existence check.
            #[cfg(unix)]
            match std::fs::hard_link(path, &target) {
                Ok(()) => std::fs::remove_file(path)?,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    return Ok(format!("fs.move target exists: {to} · choose a new path"));
                }
                Err(_) => std::fs::rename(path, &target)?,
            }
            #[cfg(not(unix))]
            std::fs::rename(path, &target)?;
            workspace.after_change(path);
            workspace.after_change(&target);
            workspace.forget_read(path);
            workspace.record_read(&target, current_stamp);
            Ok(format!(
                "moved {rel} -> {}",
                fs_relative(workspace, &target)
            ))
        }
        "fs_delete" => {
            let (diff, _, _) = fs_diff(&rel, &current, "", 200);
            workspace.before_change(path)?;
            std::fs::remove_file(path)?;
            workspace.after_change(path);
            workspace.forget_read(path);
            Ok(format!(
                "deleted {rel} (+0 −{})\n{diff}",
                current.lines().count()
            ))
        }
        _ => unreachable!("fs mutation dispatch"),
    }
}

fn fs_replace(
    workspace: &Workspace,
    path: &Path,
    name: &str,
    args: &serde_json::Value,
    current: &str,
    permissions: Option<std::fs::Permissions>,
) -> Result<String, PathError> {
    let content = match fs_replacement(name, args, current) {
        Ok(content) => content,
        Err(error) => return Ok(error),
    };
    let rel = fs_relative(workspace, path);
    let (diff, added, deleted) = fs_diff(&rel, current, &content, 200);
    workspace.before_change(path)?;
    let stamp = fs_atomic_write(path, &content, permissions)?;
    workspace.after_change(path);
    workspace.record_read(path, stamp);
    let verb = if name == "fs_edit" { "edited" } else { "wrote" };
    Ok(format!("{verb} {rel} (+{added} −{deleted})\n{diff}"))
}

pub(super) fn fs_replacement(
    name: &str,
    args: &serde_json::Value,
    current: &str,
) -> Result<String, String> {
    let mut content = if name == "fs_edit" {
        let edits: Vec<_> = args
            .get("edits")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|edit| {
                Some((
                    edit.get("hash")?.as_str()?.to_string(),
                    edit.get("line")
                        .and_then(serde_json::Value::as_u64)
                        .and_then(|line| usize::try_from(line).ok()),
                    edit.get("new")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string),
                ))
            })
            .collect();
        apply_hashline_edits(current, &edits)?.0
    } else {
        args.get("content")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    if current.contains("\r\n") && !content.contains("\r\n") {
        content = content.replace('\n', "\r\n");
    }
    Ok(content)
}

pub(super) fn fs_approval_summary(
    workspace: &Workspace,
    name: &str,
    args: &serde_json::Value,
) -> Result<String, String> {
    let path = workspace
        .resolve(
            args["path"].as_str().expect("validated path"),
            Access::Write,
        )
        .map_err(|error| error.to_string())?;
    let rel = fs_relative(workspace, &path);
    let exists = path.try_exists().map_err(|error| error.to_string())?;
    if name == "fs_write" && !exists {
        return Ok(format!("write {rel}"));
    }
    let previous = workspace.last_read(&path).ok_or_else(|| {
        format!(
            "fs.{} needs a fresh read · call fs with action=read on {rel} first",
            name.trim_start_matches("fs_")
        )
    })?;
    let bytes = std::fs::read(&path).map_err(|error| error.to_string())?;
    let metadata = std::fs::metadata(&path).map_err(|error| error.to_string())?;
    let stamp = Workspace::stamp_bytes(&bytes, &metadata).map_err(|error| error.to_string())?;
    let current = String::from_utf8_lossy(&bytes);
    if previous.len != stamp.len || previous.sha256 != stamp.sha256 {
        let mut result = format!("{rel} changed since your last read · re-read it and re-apply");
        if let Some(text) = previous.text {
            let _ = write!(result, "\n{}", fs_diff(&rel, &text, &current, 60).0);
        }
        return Err(result);
    }
    match name {
        "fs_write" | "fs_edit" => {
            if name == "fs_edit" && (bytes.contains(&0) || std::str::from_utf8(&bytes).is_err()) {
                return Err(format!(
                    "fs.edit needs a UTF-8 text file · {rel} is binary or not UTF-8; use write to replace it whole"
                ));
            }
            let content = fs_replacement(name, args, &current)?;
            Ok(format!(
                "{} {rel}\n{}",
                name.trim_start_matches("fs_"),
                fs_diff(&rel, &current, &content, 40).0
            ))
        }
        "fs_move" => {
            let to = args["to"].as_str().expect("validated destination");
            let target = workspace
                .resolve(to, Access::Write)
                .map_err(|error| error.to_string())?;
            if target.try_exists().map_err(|error| error.to_string())? {
                return Err(format!("fs.move target exists: {to} · choose a new path"));
            }
            Ok(format!("move {rel} -> {}", fs_relative(workspace, &target)))
        }
        "fs_delete" => Ok(format!("delete {rel} ({} lines)", current.lines().count())),
        _ => unreachable!("fs approval action"),
    }
}

fn fs_atomic_write(
    path: &Path,
    content: &str,
    permissions: Option<std::fs::Permissions>,
) -> Result<crate::workspace::FileStamp, PathError> {
    let parent = path.parent().expect("workspace file parent");
    std::fs::create_dir_all(parent)?;
    let temp = parent.join(format!(".nexus-{}.tmp", uuid::Uuid::new_v4()));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)?;
    let result = (|| {
        file.write_all(content.as_bytes())?;
        if let Some(permissions) = permissions {
            file.set_permissions(permissions)?;
        }
        file.sync_all()?;
        let stamp = Workspace::stamp_bytes(content.as_bytes(), &file.metadata()?)?;
        std::fs::rename(&temp, path)?;
        Ok(stamp)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

/// Diff lines a large write's approval preview may carry (the popup scrolls).
pub(super) const APPROVAL_DETAIL_LINES: usize = 4_000;

pub(super) fn fs_diff(
    path: &str,
    current: &str,
    candidate: &str,
    limit: usize,
) -> (String, usize, usize) {
    let diff = unified_diff("workspace", path, current, candidate);
    let added = diff
        .lines()
        .skip_while(|line| !line.starts_with("@@"))
        .filter(|line| line.starts_with('+'))
        .count();
    let deleted = diff
        .lines()
        .skip_while(|line| !line.starts_with("@@"))
        .filter(|line| line.starts_with('-'))
        .count();
    let mut capped = diff.lines().take(limit).collect::<Vec<_>>().join("\n");
    let more = diff.lines().count().saturating_sub(limit);
    if more > 0 {
        let _ = write!(capped, "\n… {more} more lines");
    }
    (capped, added, deleted)
}

// Never follow directory symlinks, and apply the workspace's hard exclusions
// before entering directories or reading files.
fn fs_walk(workspace: &Arc<Workspace>, path: &Path, depth: Option<usize>) -> ignore::Walk {
    let workspace = Arc::clone(workspace);
    ignore::WalkBuilder::new(path)
        .hidden(false)
        .require_git(false)
        .git_global(false)
        .follow_links(false)
        .max_depth(depth)
        .sort_by_file_name(std::ffi::OsStr::cmp)
        .filter_entry(move |entry| {
            entry.file_name() != ".git"
                && workspace
                    .resolve(&entry.path().to_string_lossy(), Access::Read)
                    .is_ok()
        })
        .build()
}

pub(super) fn fs_relative(workspace: &Workspace, path: &Path) -> String {
    let rel = path
        .strip_prefix(workspace.root())
        .expect("confined workspace path");
    if rel.as_os_str().is_empty() {
        ".".to_string()
    } else {
        rel.to_string_lossy().into_owned()
    }
}

fn fs_capped_entries(mut entries: Vec<String>, max: usize) -> String {
    let more = entries.len().saturating_sub(max);
    entries.truncate(max);
    if more > 0 {
        entries.push(format!("… {more} more"));
    }
    entries.join("\n")
}

fn fs_list_dir(
    workspace: &Arc<Workspace>,
    path: &Path,
    depth: usize,
    out: &mut Vec<String>,
) -> Result<(), PathError> {
    let label = fs_relative(workspace, path);
    if !path.is_dir() {
        std::fs::metadata(path)?;
        out.push(label);
        return Ok(());
    }
    let mut entries = Vec::new();
    if depth > 0 {
        for entry in fs_walk(workspace, path, Some(1)).skip(1) {
            entries.push(entry.map_err(|error| PathError::Io(error.to_string()))?);
        }
    }
    if entries.len() > 100 {
        out.push(format!("{label}/ ({} entries)", entries.len()));
    } else {
        out.push(format!("{label}/"));
        for entry in entries {
            if entry.file_type().is_some_and(|kind| kind.is_dir()) {
                fs_list_dir(workspace, entry.path(), depth - 1, out)?;
            } else {
                out.push(fs_relative(workspace, entry.path()));
            }
        }
    }
    Ok(())
}

/// A file that is not UTF-8 text, read through the file index's converter so the
/// model sees text, never bytes. Converted lines carry no edit anchors.
fn fs_read_converted(
    workspace: &Workspace,
    path: &Path,
    bytes: &[u8],
    offset: usize,
    limit: usize,
) -> String {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    let ex = crate::extract::to_text_bytes(bytes, name, &crate::extract::ExtractConfig::none());
    if ex.text.trim().is_empty() {
        let kind = ex.kind;
        return match (&ex.outcome, kind) {
            (crate::extract::Outcome::Visual, crate::extract::Kind::Pdf) => format!(
                "pdf ({} bytes) has no text layer · import it into the space files and view pages with files(action=pdf_page)",
                bytes.len()
            ),
            (crate::extract::Outcome::Visual, _) => format!(
                "{} image ({} bytes) · not text · import it into the space files to view or search it",
                kind.label(),
                bytes.len()
            ),
            (_, crate::extract::Kind::Binary("unknown")) => {
                format!("binary file ({} bytes) · not shown", bytes.len())
            }
            _ => format!(
                "binary file ({} bytes, {}) · not shown",
                bytes.len(),
                kind.label()
            ),
        };
    }
    let lines: Vec<_> = ex.text.lines().collect();
    let start = offset.saturating_sub(1).min(lines.len());
    let end = start.saturating_add(limit).min(lines.len());
    let (first, last) = if start == end {
        (0, 0)
    } else {
        (start + 1, end)
    };
    let mut result = format!(
        "{} (converted from {}, lines {first}-{last} of {}):\n{}",
        fs_relative(workspace, path),
        ex.kind.label(),
        lines.len(),
        number_lines(&lines[start..end], start)
    );
    if end < lines.len() {
        let _ = write!(result, "\n… continue with offset={}", end + 1);
    }
    result
}

fn fs_read(
    workspace: &Workspace,
    path: &Path,
    offset: usize,
    limit: usize,
) -> Result<String, PathError> {
    let mut file = std::fs::File::open(path)?;
    let metadata = file.metadata()?;
    if metadata.len() > 16 * 1024 * 1024 {
        return Ok(format!(
            "file too large ({} bytes) · use fs grep or shell (sed -n, head) for a slice",
            metadata.len()
        ));
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    // Judged by content: an uncompressed PDF is valid UTF-8, and still not
    // something to show line by line.
    let is_text = !bytes.contains(&0)
        && crate::extract::detect_head(
            &bytes[..bytes.len().min(64 * 1024)],
            path.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default(),
        )
        .is_text();
    let mut result = if let Ok(text) = std::str::from_utf8(&bytes)
        && is_text
    {
        let lines: Vec<_> = text.lines().collect();
        let start = offset.saturating_sub(1).min(lines.len());
        let end = start.saturating_add(limit).min(lines.len());
        let (first, last) = if start == end {
            (0, 0)
        } else {
            (start + 1, end)
        };
        let mut result = format!(
            "{} (lines {first}-{last} of {}):\n{}",
            fs_relative(workspace, path),
            lines.len(),
            number_lines_with_hash(&lines[start..end], start)
        );
        if end < lines.len() {
            let _ = write!(result, "\n… continue with offset={}", end + 1);
        }
        result
    } else {
        fs_read_converted(workspace, path, &bytes, offset, limit)
    };
    workspace.record_read(path, Workspace::stamp_bytes(&bytes, &metadata)?);
    // Empty files still have a header, but no empty hash-line.
    if result.ends_with('\n') {
        result.pop();
    }
    Ok(result)
}

pub(super) fn fs_grep(
    workspace: &Arc<Workspace>,
    path: &Path,
    pattern: &str,
    context: usize,
) -> Result<String, PathError> {
    let mut result = String::new();
    let mut total = 0;
    let mut shown = 0;
    let mut skipped = 0;
    for entry in fs_walk(workspace, path, None) {
        let entry = entry.map_err(|error| PathError::Io(error.to_string()))?;
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        let file = workspace.resolve(&entry.path().to_string_lossy(), Access::Read)?;
        if std::fs::metadata(&file)?.len() > 4 * 1024 * 1024 {
            skipped += 1;
            continue;
        }
        let bytes = std::fs::read(&file)?;
        if bytes.iter().take(8192).any(|byte| *byte == 0) {
            continue;
        }
        let text = String::from_utf8_lossy(&bytes);
        let lines: Vec<_> = text.lines().collect();
        let matches: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter_map(|(index, line)| line.contains(pattern).then_some(index))
            .collect();
        total += matches.len();
        let keep = matches.len().min(200 - shown);
        if keep == 0 {
            continue;
        }
        let _ = writeln!(result, "{}:", fs_relative(workspace, &file));
        let mut selected = std::collections::BTreeSet::new();
        let end = matches.get(keep).copied().unwrap_or(lines.len());
        for index in &matches[..keep] {
            selected
                .extend(index.saturating_sub(context)..index.saturating_add(context + 1).min(end));
        }
        for index in selected {
            let _ = writeln!(result, "  {}: {}", index + 1, lines[index]);
        }
        shown += keep;
    }
    if total > shown {
        let _ = writeln!(result, "… {} more", total - shown);
    }
    if result.is_empty() {
        result.push_str("no matches\n");
    }
    if skipped > 0 {
        let _ = writeln!(
            result,
            "… skipped {skipped} files over 4 MiB · use shell for those"
        );
    }
    Ok(result.trim_end().to_string())
}
