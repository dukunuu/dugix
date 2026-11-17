//! `dugix sessions`, `spaces`, `export`, `memory`, `instructions`, `files`,
//! `backup` and `restore`.

use std::io::{IsTerminal as _, Write as _};
use std::path::Path;

use anyhow::{Context as _, Result, anyhow, bail};

use dugix_core::app;
use dugix_core::db;
use dugix_core::space;

use super::{
    SessionsCmd, fmt_ts, human_size, out, resolve_session, resolve_space_name, short_id, truncate,
};

pub(super) fn sessions(
    space_name: Option<&str>,
    json: bool,
    cmd: Option<SessionsCmd>,
) -> Result<()> {
    match cmd {
        Some(SessionsCmd::Rm { session }) => sessions_rm(&session),
        Some(SessionsCmd::Prune {
            keep,
            days,
            dry_run,
        }) => sessions_prune(keep, days, dry_run),
        None => sessions_list(space_name, json),
    }
}

fn sessions_list(space_name: Option<&str>, json: bool) -> Result<()> {
    let space = space::Space::open()?;
    let db = db::Db::open(&space.db_path())?;
    let space_id = match space_name {
        Some(name) => db
            .list_spaces()?
            .into_iter()
            .find(|s| s.name == name)
            .map(|s| s.id)
            .ok_or_else(|| anyhow!("no space named {name:?} — `dugix spaces` lists them"))?,
        None => db.default_space_id()?,
    };
    let sessions = db.list_sessions(&space_id)?;
    if sessions.is_empty() {
        out("no sessions yet — ask something in the TUI or with `dugix ask`");
        return Ok(());
    }
    if json {
        let v: Vec<serde_json::Value> = sessions
            .iter()
            .map(|s| {
                serde_json::json!({
                    "id": s.id, "title": s.title, "model": s.model,
                    "slug": s.slug, "kind": s.kind, "created_at": s.created_at,
                })
            })
            .collect();
        out(serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    for s in sessions {
        out(format!(
            "{:<8}  {:<40}  {:<32}  {:<8}  {}",
            short_id(&s.id),
            truncate(&s.title, 40),
            truncate(&s.model, 32),
            s.kind,
            fmt_ts(&s.created_at),
        ));
    }
    Ok(())
}

fn sessions_rm(reference: &str) -> Result<()> {
    let space = space::Space::open()?;
    let db = db::Db::open(&space.db_path())?;
    let (_, session) = resolve_session(&db, reference)?;
    db.delete_session(&session.id)?;
    out(format!(
        "deleted session \"{}\" ({})",
        session.title,
        short_id(&session.id)
    ));
    Ok(())
}

fn sessions_prune(keep: Option<u64>, days: Option<u64>, dry_run: bool) -> Result<()> {
    if keep.is_none() && days.is_none() {
        bail!("pass --keep N and/or --days N to choose what to prune");
    }
    let space = space::Space::open()?;
    let db = db::Db::open(&space.db_path())?;
    let now = chrono::Utc::now();
    let mut doomed: Vec<db::Session> = Vec::new();
    for sp in db.list_spaces()? {
        let sessions = db.list_sessions(&sp.id)?;
        doomed.extend(
            prune_targets(&sessions, keep, days, now)
                .into_iter()
                .cloned(),
        );
    }
    if doomed.is_empty() {
        out("nothing to prune");
        return Ok(());
    }
    for s in &doomed {
        out(format!(
            "{}{:<8}  {:<40}  {}",
            if dry_run {
                "would delete "
            } else {
                "deleted     "
            },
            short_id(&s.id),
            truncate(&s.title, 40),
            fmt_ts(&s.created_at),
        ));
        if !dry_run {
            db.delete_session(&s.id)?;
        }
    }
    out(format!(
        "{} {} session(s)",
        if dry_run { "would prune" } else { "pruned" },
        doomed.len()
    ));
    Ok(())
}

pub(super) fn spaces(json: bool) -> Result<()> {
    let space = space::Space::open()?;
    let db = db::Db::open(&space.db_path())?;
    let rows = db.list_spaces()?;
    if rows.is_empty() {
        out("no spaces yet");
        return Ok(());
    }
    if json {
        let v: Vec<serde_json::Value> = rows
            .iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.id, "name": r.name, "created_at": r.created_at,
                    "sessions": db.count_sessions(&r.id).unwrap_or(0),
                })
            })
            .collect();
        out(serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    let default_id = db.default_space_id().unwrap_or_default();
    for r in rows {
        let sessions = db.count_sessions(&r.id).unwrap_or(0);
        let marker = if r.id == default_id { " (default)" } else { "" };
        out(format!(
            "{:<8}  {:<24}  {:>4} sessions  {}{}",
            short_id(&r.id),
            truncate(&r.name, 24),
            sessions,
            fmt_ts(&r.created_at),
            marker,
        ));
    }
    Ok(())
}

/// Exports the latest reply with cited sources, or the full transcript.
pub(super) fn export(reference: &str, transcript: bool, output: Option<&Path>) -> Result<()> {
    let space = space::Space::open()?;
    let db = db::Db::open(&space.db_path())?;
    let (space_id, session) = resolve_session(&db, reference)?;
    let messages = db.load_messages(&session.id)?;
    let assembled = if transcript {
        transcript_markdown(&session, &messages)
    } else {
        let report = messages
            .iter()
            .rev()
            .find(|m| m.role == "assistant")
            .map(|m| m.content.clone())
            .ok_or_else(|| {
                anyhow!(
                    "session {} has no assistant reply yet",
                    short_id(&session.id)
                )
            })?;
        // Citations are space-wide; exporting all of them would leak other sessions' sources.
        let citations = db.search_citations(&space_id, None)?;
        let urls_in_report: std::collections::HashSet<String> =
            dugix_core::citations::parse_citations(&report)
                .into_iter()
                .map(|(_, url)| url)
                .collect();
        let cited: Vec<(String, String, String)> = citations
            .into_iter()
            .filter(|(_, url, _)| urls_in_report.contains(url))
            .collect();
        app::export::assemble_report(&report, &cited)
    };
    match output {
        Some(path) => {
            std::fs::write(path, &assembled)
                .with_context(|| format!("writing {}", path.display()))?;
            eprintln!("exported to {}", path.display());
        }
        None => out(assembled),
    }
    Ok(())
}

pub(super) fn transcript_markdown(session: &db::Session, messages: &[db::Message]) -> String {
    let mut out = format!("# {}\n\n", session.title);
    for m in messages {
        let _ = std::fmt::Write::write_fmt(&mut out, format_args!("## {}\n\n", m.role));
        if m.role == "tool_call" {
            out.push_str("```json\n");
            out.push_str(&m.content);
            out.push_str("\n```\n\n");
        } else {
            let _ =
                std::fmt::Write::write_fmt(&mut out, format_args!("{}\n\n", m.content.trim_end()));
        }
    }
    out
}

/// Selects sessions exceeding either retention limit from a newest-first list.
pub(super) fn prune_targets(
    sessions: &[db::Session],
    keep: Option<u64>,
    days: Option<u64>,
    now: chrono::DateTime<chrono::Utc>,
) -> Vec<&db::Session> {
    let cutoff = days.map(|d| now - chrono::Duration::days(d.cast_signed()));
    sessions
        .iter()
        .enumerate()
        .filter(|(i, s)| {
            let too_old = cutoff.is_some_and(|c| {
                chrono::DateTime::parse_from_rfc3339(&s.created_at).map_or(true, |t| t < c)
            });
            let beyond_keep = keep.is_some_and(|k| *i >= usize::try_from(k).unwrap_or(usize::MAX));
            too_old || beyond_keep
        })
        .map(|(_, s)| s)
        .collect()
}

pub(super) fn memory(
    space_name: Option<&str>,
    edit: bool,
    command: Option<super::MemoryCommand>,
) -> Result<()> {
    use dugix_core::memory::{self, book};
    let space = space::Space::open()?;
    let db = db::Db::open(&space.db_path())?;
    let name = resolve_space_name(&db, space_name)?;
    let space_id = db
        .list_spaces()?
        .into_iter()
        .find(|s| s.name == name)
        .map(|s| s.id)
        .with_context(|| format!("no space named {name:?}"))?;
    let conn = db::open_conn(&space.db_path())?;
    if let Some(command) = command {
        match command {
            super::MemoryCommand::Shares => out(memory::sharing_list(&conn, &space_id)?),
            super::MemoryCommand::Share {
                chapter,
                with,
                all,
                none: _,
            } => {
                let target = if all {
                    memory::ShareTarget::All
                } else if with.is_empty() {
                    memory::ShareTarget::None
                } else {
                    memory::ShareTarget::Spaces(with)
                };
                let chapter = memory::find_chapter(&conn, &space_id, &chapter)?
                    .with_context(|| format!("no chapter titled {chapter:?} in this space"))?;
                let chapter = memory::share_chapter(&conn, &chapter.id, target.resolve(&conn)?)?;
                if chapter.shared_with == memory::Share::Private {
                    out(format!("{:?} is private", chapter.title));
                } else {
                    let labels =
                        memory::share_labels(&conn, &space_id, std::slice::from_ref(&chapter))?;
                    out(format!(
                        "{:?} {}",
                        chapter.title,
                        labels[&chapter.id]
                            .trim()
                            .trim_start_matches('(')
                            .trim_end_matches(')')
                    ));
                }
            }
        }
        return Ok(());
    }
    let text = book::render_edit(
        &memory::chapters(&conn, &space_id)?,
        &memory::facts(&conn, &space_id)?,
    );
    if !edit {
        if memory::chapters(&conn, &space_id)?.is_empty() {
            out("(no memory yet — run with --edit to add some)");
        } else {
            out(text.trim_end());
        }
        return Ok(());
    }
    space.ensure_space_dir(&name)?;
    let path = space.memory_path(&name).with_file_name("memory.edit.md");
    std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
    open_editor(&path)?;
    let edited = std::fs::read_to_string(&path)?;
    let summary = book::apply_edit(
        &conn,
        &space_id,
        &book::parse_edit(&edited),
        book::opened_at(&edited),
    )?;
    std::fs::remove_file(&path)?;
    out(summary.to_string());
    Ok(())
}

pub(super) fn instructions(space_name: Option<&str>, edit: bool) -> Result<()> {
    let space = space::Space::open()?;
    let db = db::Db::open(&space.db_path())?;
    let name = resolve_space_name(&db, space_name)?;
    show_or_edit(&space.instructions_path(&name), "instructions", edit)
}

fn show_or_edit(path: &Path, label: &str, edit: bool) -> Result<()> {
    if edit {
        if !path.exists() {
            std::fs::write(
                path,
                format!(
                    "<!-- {label} for this space — everything here is visible to the model -->\n"
                ),
            )
            .with_context(|| format!("writing {}", path.display()))?;
        }
        open_editor(path)?;
        return Ok(());
    }
    match std::fs::read_to_string(path) {
        Ok(text) => out(text.trim_end()),
        Err(_) => out(format!("(no {label} yet — run with --edit to create it)")),
    }
    Ok(())
}

fn open_editor(path: &Path) -> Result<()> {
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());
    let status = std::process::Command::new(&editor)
        .arg(path)
        .status()
        .with_context(|| format!("running $EDITOR ({editor})"))?;
    if !status.success() {
        bail!("editor exited with {status}");
    }
    Ok(())
}

pub(super) fn files(space_name: Option<&str>) -> Result<()> {
    let space = space::Space::open()?;
    let db = db::Db::open(&space.db_path())?;
    let name = resolve_space_name(&db, space_name)?;
    let space_id = db
        .list_spaces()?
        .into_iter()
        .find(|s| s.name == name)
        .map(|s| s.id)
        .context("space vanished")?;
    let rows = db.list_files(&space_id)?;
    if rows.is_empty() {
        out(format!(
            "no files imported in \"{name}\" yet — drop files into {}",
            space.files_dir(&name).display()
        ));
        return Ok(());
    }
    for f in rows {
        out(format!(
            "{:<40} {:>10}  {}",
            truncate(&f.name, 40),
            human_size(f.size.cast_unsigned()),
            f.status,
        ));
    }
    Ok(())
}

pub(super) fn backup(output: Option<&Path>) -> Result<()> {
    let space = space::Space::open()?;
    let default = space.root.parent().unwrap_or(&space.root).join(format!(
        "dugix-backup-{}.zip",
        chrono::Utc::now().format("%Y-%m-%d")
    ));
    let path = output.unwrap_or(&default);
    // Recent SQLite commits can live in the WAL; checkpoint them before archiving the database.
    checkpoint_wal(&space.db_path());
    zip_dir(&space.root, path)?;
    out(format!("backed up to {}", path.display()));
    Ok(())
}

pub(super) fn restore(file: &Path, yes: bool) -> Result<()> {
    let space = space::Space::open()?;
    if !yes {
        if !std::io::stdin().is_terminal() {
            bail!("restore overwrites your local data — pass --yes to confirm");
        }
        eprint!("restore {} over current data? [y/N] ", file.display());
        let _ = std::io::stderr().flush();
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        if !matches!(line.trim().to_lowercase().as_str(), "y" | "yes") {
            bail!("aborted");
        }
    }
    // Invalidate derived caches before and after extraction, since backups can contain stale caches.
    let cache_path = space.root.join("cache.db");
    let _ = std::fs::remove_file(&cache_path);
    // Remove only pre-restore WAL sidecars to prevent replay over the restored database.
    for suffix in [
        "dugix.db-wal",
        "dugix.db-shm",
        "cache.db-wal",
        "cache.db-shm",
    ] {
        let _ = std::fs::remove_file(space.root.join(suffix));
    }
    unzip_into(file, &space.root)?;
    let _ = std::fs::remove_file(&cache_path);
    out("restored");
    Ok(())
}

// If checkpointing fails, the archived WAL sidecar still preserves recent commits.
fn checkpoint_wal(db_path: &Path) {
    if let Ok(conn) = dugix_core::db::open_conn(db_path) {
        let _ = conn.pragma_update(None, "wal_checkpoint", "TRUNCATE");
    }
}

pub(super) fn zip_dir(src: &Path, dst: &Path) -> Result<()> {
    let file = std::fs::File::create(dst).with_context(|| format!("creating {}", dst.display()))?;
    let mut zip = zip::ZipWriter::new(file);
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    zip_entries(&mut zip, src, src, opts)?;
    zip.finish()?;
    Ok(())
}

fn zip_entries(
    zip: &mut zip::ZipWriter<std::fs::File>,
    root: &Path,
    dir: &Path,
    opts: zip::write::SimpleFileOptions,
) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let rel = path
            .strip_prefix(root)
            .with_context(|| format!("path outside root: {}", path.display()))?;
        // Derived caches rebuild on demand and do not belong in backups.
        if rel == std::path::Path::new("cache.db") {
            continue;
        }
        if entry.file_type()?.is_dir() {
            zip.add_directory(format!("{}/", rel.display()), opts)?;
            zip_entries(zip, root, &path, opts)?;
        } else {
            zip.start_file(rel.display().to_string(), opts)?;
            let mut f = std::fs::File::open(&path)?;
            std::io::copy(&mut f, zip)?;
        }
    }
    Ok(())
}

/// Extracts a backup while rejecting paths that escape the destination.
pub(super) fn unzip_into(archive_path: &Path, dest_root: &Path) -> Result<()> {
    let file = std::fs::File::open(archive_path)
        .with_context(|| format!("opening {}", archive_path.display()))?;
    let mut archive = zip::ZipArchive::new(file)
        .with_context(|| format!("reading {}", archive_path.display()))?;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let name = entry.name()?.into_owned();
        let rel = Path::new(&name);
        if rel.is_absolute()
            || rel.components().any(|c| {
                matches!(
                    c,
                    std::path::Component::ParentDir
                        | std::path::Component::RootDir
                        | std::path::Component::Prefix(_)
                )
            })
        {
            bail!("refusing unsafe path in archive: {name}");
        }
        let dest = dest_root.join(rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&dest)?;
        } else {
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut f = std::fs::File::create(&dest)?;
            std::io::copy(&mut entry, &mut f)?;
        }
    }
    Ok(())
}
