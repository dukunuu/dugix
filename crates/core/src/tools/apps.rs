//! Locally served web apps, their files, and the images they use.

use std::fmt::Write as _;
use std::path::PathBuf;

use super::hashline::{apply_hashline_edits, number_lines_with_hash, unified_diff};
use super::scripts::{format_output, run_cmd};
use super::{AppsCtx, ToolBox, valid_relative_path};

impl ToolBox {
    /// Resolve an app name or UUID to `(uuid, app_dir)`, assigning a new UUID to
    /// an unregistered name.
    pub(super) fn resolve_app(&self, name_or_uuid: &str) -> Result<(String, PathBuf), String> {
        let ctx = self.apps.as_ref().ok_or("apps are not available")?;
        let (uuid, app_name) = if looks_like_uuid(name_or_uuid) {
            let entry = ctx
                .registry
                .lookup(name_or_uuid)
                .ok_or_else(|| format!("unknown app uuid: {name_or_uuid}"))?;
            (name_or_uuid.to_string(), entry.name)
        } else if name_or_uuid.is_empty()
            || name_or_uuid.contains(['/', '\\'])
            || name_or_uuid == "."
            || name_or_uuid == ".."
        {
            return Err(format!("invalid app name: {name_or_uuid:?}"));
        } else {
            let uuid = match ctx.registry.resolve(&ctx.space_name, name_or_uuid) {
                Some(u) => u,
                None => ctx.registry.assign(&ctx.space_name, name_or_uuid),
            };
            (uuid, name_or_uuid.to_string())
        };
        let app_dir = ctx.dir.join(&app_name);
        Ok((uuid, app_dir))
    }

    /// The space's apps as `{name, uuid, url}`, sorted by name; an app with no
    /// assigned UUID has neither field.
    pub(super) fn list_apps(&self, ctx: &AppsCtx) -> Vec<serde_json::Value> {
        let mut names: Vec<String> = std::fs::read_dir(&ctx.dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|entry| entry.path().is_dir())
            .filter_map(|entry| entry.file_name().into_string().ok())
            .collect();
        names.sort();
        names
            .into_iter()
            .map(|name| match ctx.registry.resolve(&ctx.space_name, &name) {
                Some(uuid) => {
                    let link = self.app_link(&uuid);
                    let url = link.strip_prefix("live at ").unwrap_or(&link).to_string();
                    serde_json::json!({ "name": name, "uuid": uuid, "url": url })
                }
                None => serde_json::json!({ "name": name }),
            })
            .collect()
    }

    /// The live URL for an app (accepts a UUID).
    pub(super) fn app_link(&self, uuid: &str) -> String {
        match &self.apps {
            Some(ctx) => ctx.public_base.as_ref().map_or_else(
                || format!("live at http://127.0.0.1:{}/{}/", ctx.server_port, uuid),
                |base| format!("live at {}", crate::appserver::public_app_url(base, uuid)),
            ),
            None => String::new(),
        }
    }

    /// The build tool a package.json declares: "astro" or "vite".
    #[must_use]
    pub(super) fn declared_build_tool(pkg: &serde_json::Value) -> Option<&'static str> {
        let mut deps: Vec<String> = Vec::new();
        for key in ["dependencies", "devDependencies"] {
            if let Some(obj) = pkg.get(key).and_then(serde_json::Value::as_object) {
                deps.extend(obj.keys().cloned());
            }
        }
        if deps.iter().any(|d| d == "astro") {
            Some("astro")
        } else if deps.iter().any(|d| d == "vite") {
            Some("vite")
        } else {
            None
        }
    }

    #[allow(clippy::too_many_lines)]
    pub(super) async fn run_apps(&self, name: &str, args: &str) -> (String, String) {
        match name {
            "copy_file_to_app" => {
                let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
                let file_name = v
                    .get("file_name")
                    .and_then(|x| x.as_str())
                    .unwrap_or_default()
                    .to_string();
                let app = v
                    .get("app")
                    .and_then(|x| x.as_str())
                    .unwrap_or_default()
                    .to_string();
                let status = format!("Copying {file_name} to {app}…");
                let result = match (&self.apps, &self.files) {
                    (None, _) => "apps not available".to_string(),
                    (_, None) => "files not available".to_string(),
                    (Some(ctx), Some(fc)) => {
                        let (uuid, app_dir) = match self.resolve_app(&app) {
                            Err(e) => return (e, status),
                            Ok(t) => t,
                        };
                        let conn = match crate::db::open_attached(&fc.db_path) {
                            Err(e) => return (format!("db error: {e}"), status),
                            Ok(c) => c,
                        };
                        let text = match crate::db::file_text(&conn, &fc.space_id, &file_name) {
                            Err(e) => return (format!("file read error: {e}"), status),
                            Ok(None) => return (format!("unknown file: {file_name}"), status),
                            Ok(Some(t)) => t,
                        };
                        let store_path = app_dir.join("_store.db");
                        let store = match rusqlite::Connection::open(&store_path) {
                            Err(e) => return (format!("store error: {e}"), status),
                            Ok(s) => s,
                        };
                        let _ = store.execute_batch(
                            "CREATE TABLE IF NOT EXISTS kv (key TEXT PRIMARY KEY, value TEXT)",
                        );
                        let key = format!("_file:{file_name}");
                        match store.execute(
                            "INSERT OR REPLACE INTO kv (key, value) VALUES (?1, ?2)",
                            rusqlite::params![key, text],
                        ) {
                            Ok(_) => {
                                let url = format!("http://127.0.0.1:{}/{uuid}/", ctx.server_port);
                                format!(
                                    "copied {file_name} into {app}'s KV — read it at {url}_api/kv/_file:{file_name}"
                                )
                            }
                            Err(e) => format!("kv write error: {e}"),
                        }
                    }
                };
                (result, status)
            }
            "list_images" => {
                let status = "Listing images…".to_string();
                let result = match &self.apps {
                    None => "apps not available".to_string(),
                    Some(ctx) => {
                        let conn = match rusqlite::Connection::open(&ctx.space_db_path) {
                            Err(e) => return (format!("db error: {e}"), status),
                            Ok(c) => c,
                        };
                        let mut images: Vec<serde_json::Value> = Vec::new();
                        let mut stmt = match conn.prepare(
                            "SELECT content FROM messages WHERE session_id = ?1 ORDER BY created_at ASC"
                        ) {
                            Ok(s) => s,
                            Err(e) => return (format!("query error: {e}"), status),
                        };
                        if let Ok(rows) =
                            stmt.query_map([&ctx.session_id], |r| r.get::<_, String>(0))
                        {
                            for row in rows.flatten() {
                                let mut rest = row.as_str();
                                while let Some(start) = rest.find("![") {
                                    if let Some(end) = rest[start..].find(')') {
                                        let inner = &rest[start + 2..start + end];
                                        if let Some((desc, file)) = inner.split_once("](") {
                                            let file = file.to_string();
                                            images.push(serde_json::json!({
                                                "id": file,
                                                "description": desc,
                                                "source": "conversation",
                                            }));
                                        }
                                        rest = &rest[start + end + 1..];
                                    } else {
                                        break;
                                    }
                                }
                            }
                        }
                        if let Ok(mut fstmt) = conn.prepare(
                            "SELECT f.name, f.status FROM files f
                             WHERE f.space_id = ?1
                             AND (f.name LIKE '%.jpg' OR f.name LIKE '%.jpeg'
                               OR f.name LIKE '%.png' OR f.name LIKE '%.gif'
                               OR f.name LIKE '%.webp' OR f.name LIKE '%.bmp')",
                        )
                            && let Ok(rows) = fstmt.query_map([&ctx.space_id], |r| {
                                let name: String = r.get(0)?;
                                let status: String = r.get(1)?;
                                Ok(serde_json::json!({"id": name, "description": null, "source": "space", "status": status}))
                            }) {
                                images.extend(rows.filter_map(std::result::Result::ok));
                            }
                        serde_json::json!({ "apps": self.list_apps(ctx), "images": images })
                            .to_string()
                    }
                };
                (result, status)
            }
            "copy_images_to_app" => {
                let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
                let image_ids: Vec<String> = v
                    .get("image_ids")
                    .and_then(|a| a.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                let app = v
                    .get("app")
                    .and_then(|x| x.as_str())
                    .unwrap_or_default()
                    .to_string();
                let status = format!("Copying {} images to {app}…", image_ids.len());
                let result = match self.apps.as_ref() {
                    None => "apps not available".to_string(),
                    Some(ctx) => {
                        let (uuid, app_dir) = match self.resolve_app(&app) {
                            Err(e) => return (e, status),
                            Ok(t) => t,
                        };
                        let images_dir = app_dir.join("_images");
                        if let Err(e) = std::fs::create_dir_all(&images_dir) {
                            return (format!("cannot create _images dir: {e}"), status);
                        }
                        let mut out: Vec<serde_json::Value> = Vec::new();
                        for img_id in &image_ids {
                            let src = ctx.files_dir.join(img_id);
                            let dst = images_dir.join(img_id);
                            if !valid_relative_path(img_id)
                                || !src.starts_with(&ctx.files_dir)
                                || !dst.starts_with(&images_dir)
                                || !src.exists()
                            {
                                out.push(serde_json::json!({"id": img_id, "error": "not found in space files"}));
                                continue;
                            }
                            match std::fs::copy(&src, &dst) {
                                Ok(_) => {
                                    out.push(serde_json::json!({
                                        "id": img_id,
                                        "url": format!("/{uuid}/_images/{img_id}"),
                                    }));
                                }
                                Err(e) => {
                                    out.push(
                                        serde_json::json!({"id": img_id, "error": format!("{e}")}),
                                    );
                                }
                            }
                        }
                        serde_json::to_string(&out).unwrap_or_else(|_| "[]".to_string())
                    }
                };
                (result, status)
            }
            "init_app" => {
                let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
                let app = v
                    .get("app")
                    .and_then(|x| x.as_str())
                    .unwrap_or_default()
                    .to_string();
                let framework = v
                    .get("framework")
                    .and_then(|x| x.as_str())
                    .unwrap_or("astro");
                let status = format!("Scaffolding {app} ({framework})…");
                let result = match self.resolve_app(&app) {
                    Err(e) => e,
                    Ok((uuid, app_dir)) => {
                        match crate::app_templates::scaffold(&app_dir, framework) {
                            Err(e) => e,
                            Ok(files) => format!(
                                "scaffolded {app} with the {framework} starter ({}) — edit with read/patch/write, then `app action=build` (installs deps + compiles to dist/). {}",
                                files.join(", "),
                                self.app_link(&uuid),
                            ),
                        }
                    }
                };
                (result, status)
            }
            "build_app" => {
                let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
                let app = v
                    .get("app")
                    .and_then(|x| x.as_str())
                    .unwrap_or_default()
                    .to_string();
                let status = format!("Building {app}…");
                let result = match self.resolve_app(&app) {
                    Err(e) => e,
                    Ok((uuid, app_dir)) => {
                        let pkg_json = app_dir.join("package.json");
                        let Some(pkg) = std::fs::read_to_string(&pkg_json)
                            .ok()
                            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
                        else {
                            return (
                                "no package.json — write one (or `app action=init` to scaffold a React/Astro or React/Vite app) before building".to_string(),
                                status,
                            );
                        };
                        let Some(tool) = Self::declared_build_tool(&pkg) else {
                            return (
                                "no build step detected (package.json declares neither astro nor vite) — the app is served as-is".to_string(),
                                status,
                            );
                        };
                        // Deps declared but not installed: install from package.json
                        // first, so init → build just works.
                        let mut npm_err: Option<String> = None;
                        let bin = app_dir.join("node_modules").join(".bin").join(tool);
                        if !bin.exists() {
                            let refs: Vec<&std::ffi::OsStr> =
                                ["install", "--no-audit", "--no-fund"]
                                    .iter()
                                    .map(std::ffi::OsStr::new)
                                    .collect();
                            match run_cmd("npm".as_ref(), &refs, &app_dir, 600).await {
                                Ok(out) if out.status.success() => {}
                                Ok(out) => npm_err = Some(format_output(&out)),
                                Err(e) => npm_err = Some(e),
                            }
                        }
                        if let Some(err) = npm_err {
                            return (
                                format!(
                                    "cannot build {app}: deps from package.json are missing and `npm install` failed:\n{err}"
                                ),
                                status,
                            );
                        }
                        // Absolute base: the app is served at /<uuid>/, and relative
                        // bases are unreliable across frameworks.
                        let base_arg = format!("/{uuid}/");
                        let build_args =
                            ["--no-install", tool, "build", "--base", base_arg.as_str()];
                        let refs: Vec<&std::ffi::OsStr> =
                            build_args.iter().map(std::ffi::OsStr::new).collect();
                        match run_cmd("npx".as_ref(), &refs, &app_dir, 600).await {
                            Ok(out) if out.status.success() => {
                                // Only serve dist/ when the build really produced
                                // it: a configured outDir exits 0 without it.
                                if !app_dir.join("dist").is_dir() {
                                    return (
                                        "build reported success but produced no dist/ — the \
framework's output directory is configured elsewhere (outDir/base); build output \
must land in dist/ for the app server to serve it"
                                            .to_string(),
                                        status,
                                    );
                                }
                                if let Some(ctx) = &self.apps {
                                    ctx.registry.set_served_from(&uuid, "dist");
                                }
                                format!("build ok — dist/ is live at {}", self.app_link(&uuid))
                            }
                            Ok(out) => format!("build failed:\n{}", format_output(&out)),
                            Err(e) => e,
                        }
                    }
                };
                (result, status)
            }
            "write_file" => {
                let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
                let field = |k: &str| {
                    v.get(k)
                        .and_then(|x| x.as_str())
                        .unwrap_or_default()
                        .to_string()
                };
                let (app, path, content) = (field("app"), field("path"), field("content"));
                let status = format!("Writing {app}/{path}…");
                let result = match self.resolve_app(&app) {
                    Err(e) => e,
                    Ok((uuid, app_dir)) => {
                        let file = app_dir.join(&path);
                        if path.is_empty() || path.starts_with('/') || path.contains("..") {
                            format!("invalid path: {path:?}")
                        } else {
                            let write = file
                                .parent()
                                .map_or(Ok(()), std::fs::create_dir_all)
                                .and_then(|()| std::fs::write(&file, &content));
                            match write {
                                Ok(()) => format!(
                                    "wrote {app}/{path} ({} bytes) — {}",
                                    content.len(),
                                    self.app_link(&uuid),
                                ),
                                Err(e) => format!("write failed: {e}"),
                            }
                        }
                    }
                };
                (result, status)
            }
            "edit_file" => {
                let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
                let field = |k: &str| {
                    v.get(k)
                        .and_then(|x| x.as_str())
                        .unwrap_or_default()
                        .to_string()
                };
                let (app, path) = (field("app"), field("path"));
                let edits: Vec<(String, Option<usize>, Option<String>)> = v
                    .get("edits")
                    .and_then(|e| e.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|e| {
                                let hash = e.get("hash")?.as_str()?.to_string();
                                let line = e
                                    .get("line")
                                    .and_then(serde_json::Value::as_u64)
                                    .and_then(|n| usize::try_from(n).ok());
                                let new = e.get("new").and_then(|n| n.as_str()).map(str::to_string);
                                Some((hash, line, new))
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let status = format!("Editing {app}/{path}…");
                let result = match self.resolve_app(&app) {
                    Err(e) => e,
                    Ok((uuid, app_dir)) => {
                        let file = app_dir.join(&path);
                        if path.is_empty() || path.starts_with('/') || path.contains("..") {
                            format!("invalid path: {path:?}")
                        } else if app_path_ignored(&app_dir, &file) {
                            format!("{app}/{path} is ignored by .gitignore")
                        } else {
                            match std::fs::read_to_string(&file) {
                                Err(e) => format!("cannot read {app}/{path}: {e}"),
                                Ok(text) => match apply_hashline_edits(&text, &edits) {
                                    Err(e) => e,
                                    Ok((new_text, diff)) => match std::fs::write(&file, new_text) {
                                        Ok(()) => {
                                            format!(
                                                "edited {app}/{path} — {}{diff}",
                                                self.app_link(&uuid)
                                            )
                                        }
                                        Err(e) => format!("write failed: {e}"),
                                    },
                                },
                            }
                        }
                    }
                };
                (result, status)
            }
            "diff_app" => {
                let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
                let field = |k: &str| {
                    v.get(k)
                        .and_then(|x| x.as_str())
                        .unwrap_or_default()
                        .to_string()
                };
                let (app, path, content) = (field("app"), field("path"), field("content"));
                let status = format!("Diffing {app}/{path}…");
                let result = match self.resolve_app(&app) {
                    Err(e) => e,
                    Ok((_uuid, app_dir)) => {
                        if path.is_empty() || path.starts_with('/') || path.contains("..") {
                            format!("invalid path: {path:?}")
                        } else {
                            let file = app_dir.join(&path);
                            if app_path_ignored(&app_dir, &file) {
                                format!("{app}/{path} is ignored by .gitignore")
                            } else {
                                match std::fs::read_to_string(&file) {
                                    Ok(current) => unified_diff(&app, &path, &current, &content),
                                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                                        unified_diff(&app, &path, "", &content)
                                    }
                                    Err(e) => format!("cannot read {app}/{path}: {e}"),
                                }
                            }
                        }
                    }
                };
                (result, status)
            }
            "grep_app" => {
                let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
                let field = |k: &str| {
                    v.get(k)
                        .and_then(|x| x.as_str())
                        .unwrap_or_default()
                        .to_string()
                };
                let (app, pattern) = (field("app"), field("pattern"));
                let compact = v
                    .get("compact")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(true);
                let status = format!("Searching {app}…");
                let result = match self.resolve_app(&app) {
                    Err(e) => e,
                    Ok((_uuid, app_dir)) => {
                        if !app_dir.is_dir() {
                            format!("unknown app: {app}")
                        } else if pattern.is_empty() {
                            "pattern must not be empty".to_string()
                        } else {
                            let mut hits = Vec::new();
                            grep_dir(&app_dir, &app_dir, &pattern.to_lowercase(), &mut hits);
                            if hits.is_empty() {
                                format!("no matches for {pattern:?} in {app}")
                            } else {
                                let n = hits.len();
                                hits.truncate(50);
                                let mut by_file = std::collections::BTreeMap::<
                                    String,
                                    Vec<(usize, String)>,
                                >::new();
                                for (path, line) in hits {
                                    let text = if compact {
                                        String::new()
                                    } else {
                                        std::fs::read_to_string(app_dir.join(&path))
                                            .ok()
                                            .and_then(|contents| {
                                                contents
                                                    .lines()
                                                    .nth(line.saturating_sub(1))
                                                    .map(str::to_string)
                                            })
                                            .unwrap_or_default()
                                    };
                                    by_file.entry(path).or_default().push((line, text));
                                }
                                let mut result = by_file
                                    .into_iter()
                                    .map(|(path, lines)| {
                                        if compact {
                                            let numbers = lines
                                                .into_iter()
                                                .map(|(line, _)| line.to_string())
                                                .collect::<Vec<_>>()
                                                .join(",");
                                            format!("{path}:{numbers}")
                                        } else {
                                            lines
                                                .into_iter()
                                                .map(|(line, text)| {
                                                    format!("{path}:{line}: {text}")
                                                })
                                                .collect::<Vec<_>>()
                                                .join("\n")
                                        }
                                    })
                                    .collect::<Vec<_>>()
                                    .join("\n");
                                if n > 50 {
                                    let _ = write!(result, "\n… ({} more matches)", n - 50);
                                }
                                result
                            }
                        }
                    }
                };
                (result, status)
            }
            "read_app_file" => {
                let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
                let field = |k: &str| {
                    v.get(k)
                        .and_then(|x| x.as_str())
                        .unwrap_or_default()
                        .to_string()
                };
                let (app, path) = (field("app"), field("path"));
                let offset = usize::try_from(
                    v.get("offset")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(1)
                        .max(1),
                )
                .unwrap_or(1);
                let limit = v
                    .get("limit")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(200)
                    .clamp(1, 200) as usize;
                let status = format!("Reading {app}/{path}…");
                let result = match self.resolve_app(&app) {
                    Err(e) => e,
                    Ok((_uuid, app_dir)) => {
                        let file = app_dir.join(&path);
                        if path.is_empty() || path.starts_with('/') || path.contains("..") {
                            format!("invalid path: {path:?}")
                        } else if app_path_ignored(&app_dir, &file) {
                            format!("{app}/{path} is ignored by .gitignore")
                        } else {
                            match std::fs::read_to_string(&file) {
                                Err(e) => format!("cannot read {app}/{path}: {e}"),
                                Ok(text) => {
                                    let lines: Vec<&str> = text.lines().collect();
                                    let total = lines.len();
                                    let start = (offset - 1).min(total);
                                    let slice = &lines[start..(start + limit).min(total)];
                                    if slice.is_empty() {
                                        format!(
                                            "{app}/{path}: offset {offset} is past the end ({total} lines)"
                                        )
                                    } else {
                                        format!(
                                            "{app}/{path} (lines {}-{} of {total}):\n{}",
                                            start + 1,
                                            start + slice.len(),
                                            number_lines_with_hash(slice, start),
                                        )
                                    }
                                }
                            }
                        }
                    }
                };
                (result, status)
            }
            other => (
                format!("unknown tool: {other}"),
                "Running tool…".to_string(),
            ),
        }
    }
}

fn looks_like_uuid(s: &str) -> bool {
    s.len() == 36 && s.chars().filter(|c| *c == '-').count() == 4
}

/// Apply the app root's `.gitignore` to reads and searches, without a dependency.
fn app_path_ignored(root: &std::path::Path, path: &std::path::Path) -> bool {
    if path.file_name().and_then(|n| n.to_str()) == Some(".gitignore") {
        return true;
    }
    let Ok(rel) = path.strip_prefix(root) else {
        return false;
    };
    let rel = rel
        .to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/");
    let Ok(rules) = std::fs::read_to_string(root.join(".gitignore")) else {
        return false;
    };
    let mut ignored = false;
    for raw in rules.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (negated, pattern) = match line.strip_prefix('!') {
            Some(pattern) => (true, pattern),
            None => (false, line),
        };
        let directory_rule = pattern.ends_with('/');
        let pattern = pattern.trim_start_matches('/').trim_end_matches('/');
        let matches = if directory_rule {
            let mut prefix = String::new();
            let parts: Vec<&str> = rel.split('/').collect();
            parts[..parts.len().saturating_sub(1)].iter().any(|part| {
                if !prefix.is_empty() {
                    prefix.push('/');
                }
                prefix.push_str(part);
                gitignore_pattern_matches(pattern, &prefix)
            })
        } else {
            gitignore_pattern_matches(pattern, &rel)
        };
        if matches {
            ignored = !negated;
        }
    }
    ignored
}

fn gitignore_pattern_matches(pattern: &str, relative_path: &str) -> bool {
    if pattern.contains('/') {
        wildcard_match(pattern, relative_path)
    } else {
        relative_path
            .split('/')
            .any(|part| wildcard_match(pattern, part))
    }
}

fn wildcard_match(pattern: &str, text: &str) -> bool {
    let Some(p) = pattern.chars().next() else {
        return text.is_empty();
    };
    let Some(t) = text.chars().next() else {
        return p == '*';
    };
    match p {
        '*' => {
            wildcard_match(&pattern[p.len_utf8()..], text)
                || wildcard_match(pattern, &text[t.len_utf8()..])
        }
        '?' => wildcard_match(&pattern[p.len_utf8()..], &text[t.len_utf8()..]),
        _ if p == t => wildcard_match(&pattern[p.len_utf8()..], &text[t.len_utf8()..]),
        _ => false,
    }
}

/// Collect `(relpath, line)` matches for a lowercase substring pattern.
fn grep_dir(
    root: &std::path::Path,
    dir: &std::path::Path,
    pattern: &str,
    out: &mut Vec<(String, usize)>,
) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = rd.filter_map(std::result::Result::ok).collect();
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        if app_path_ignored(root, &path) {
            continue;
        }
        let name = entry.file_name();
        if path.is_dir() {
            // Dependencies and derived build output are never searched.
            if name != "node_modules" && name != ".venv" && name != ".git" && name != "dist" {
                grep_dir(root, &path, pattern, out);
            }
        } else if let Ok(text) = std::fs::read_to_string(&path) {
            let rel = path.strip_prefix(root).unwrap_or(&path).display();
            for (i, line) in text.lines().enumerate() {
                if line.to_lowercase().contains(pattern) {
                    out.push((rel.to_string(), i + 1));
                }
            }
        }
    }
}
