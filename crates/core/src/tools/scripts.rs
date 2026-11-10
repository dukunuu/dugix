//! Python scripts, packages, and command execution in the space.

use std::fmt::Write as _;
use std::path::PathBuf;

use super::hashline::{apply_hashline_edits, number_lines_with_hash};
use super::{ToolBox, confined_join, valid_relative_path};
use crate::skills::{resolve_skill_dir, resolve_skill_file};

impl ToolBox {
    /// The script tools: Python scripts and packages in the space.
    #[allow(clippy::too_many_lines)]
    pub(super) async fn run_scripts(&self, name: &str, args: &str) -> (String, String) {
        match name {
            "run_script" => {
                let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
                let field = |k: &str| {
                    v.get(k)
                        .and_then(|x| x.as_str())
                        .unwrap_or_default()
                        .to_string()
                };
                let is_space = v
                    .get("space")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                let (skill, script) = (field("skill"), field("path"));
                let extra: Vec<String> = v
                    .get("args")
                    .and_then(|a| a.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                let status = if is_space {
                    format!("Running space script {script}…")
                } else {
                    format!("Running {skill}/{script}…")
                };
                let result = if is_space {
                    let file = self.space_scripts_dir.join(&script);
                    if !valid_relative_path(&script) || !file.starts_with(&self.space_scripts_dir) {
                        format!("invalid path: {script}")
                    } else if !file.is_file() {
                        format!("no such script: {script}")
                    } else {
                        let dir = self.space_scripts_dir.clone();
                        let ext = file
                            .extension()
                            .and_then(|e| e.to_str())
                            .unwrap_or("")
                            .to_lowercase();
                        let run = async {
                            let mut cmd: Vec<std::ffi::OsString> = Vec::new();
                            let program: std::ffi::OsString = match ext.as_str() {
                                "py" => {
                                    let py = ensure_venv(&dir).await?;
                                    cmd.push(file.clone().into());
                                    py.into()
                                }
                                "sh" | "bash" => {
                                    cmd.push(file.clone().into());
                                    "bash".into()
                                }
                                "js" | "mjs" => {
                                    cmd.push(file.clone().into());
                                    "node".into()
                                }
                                _ => file.clone().into(),
                            };
                            cmd.extend(extra.iter().map(std::ffi::OsString::from));
                            let refs: Vec<&std::ffi::OsStr> =
                                cmd.iter().map(std::ffi::OsString::as_os_str).collect();
                            let files_dir = self.space_files_dir.to_string_lossy().to_string();
                            let apps_dir = self.space_apps_dir.to_string_lossy().to_string();
                            let scripts_dir = self.space_scripts_dir.to_string_lossy().to_string();
                            if ext == "py" {
                                let pp_dir = dir.join("scripts");
                                let pp = pp_dir.to_string_lossy().to_string();
                                run_cmd_env(
                                    &program,
                                    &refs,
                                    &dir,
                                    120,
                                    &[
                                        ("SPACE_FILES_DIR", files_dir.as_str()),
                                        ("SPACE_APPS_DIR", apps_dir.as_str()),
                                        ("SPACE_SCRIPTS_DIR", scripts_dir.as_str()),
                                        ("PYTHONPATH", pp.as_str()),
                                    ],
                                )
                                .await
                            } else {
                                run_cmd_env(
                                    &program,
                                    &refs,
                                    &dir,
                                    120,
                                    &[
                                        ("SPACE_FILES_DIR", files_dir.as_str()),
                                        ("SPACE_APPS_DIR", apps_dir.as_str()),
                                        ("SPACE_SCRIPTS_DIR", scripts_dir.as_str()),
                                    ],
                                )
                                .await
                            }
                        };
                        match run.await {
                            Ok(out) => format_output(&out),
                            Err(e) => e,
                        }
                    }
                } else {
                    match (
                        resolve_skill_dir(&self.skill_dirs, &skill),
                        resolve_skill_file(&self.skill_dirs, &skill, &script),
                    ) {
                        (Err(e), _) | (_, Err(e)) => e,
                        (Ok(_dir), Ok(file)) if !file.is_file() => {
                            format!("no such script: {skill}/{script}")
                        }
                        (Ok(dir), Ok(file)) => {
                            let ext = file
                                .extension()
                                .and_then(|e| e.to_str())
                                .unwrap_or("")
                                .to_lowercase();
                            let run = async {
                                let mut cmd: Vec<std::ffi::OsString> = Vec::new();
                                let program: std::ffi::OsString = match ext.as_str() {
                                    "py" => {
                                        let py = ensure_venv(&dir).await?;
                                        cmd.push(file.clone().into());
                                        py.into()
                                    }
                                    "sh" | "bash" => {
                                        cmd.push(file.clone().into());
                                        "bash".into()
                                    }
                                    "js" | "mjs" => {
                                        cmd.push(file.clone().into());
                                        "node".into()
                                    }
                                    _ => file.clone().into(),
                                };
                                cmd.extend(extra.iter().map(std::ffi::OsString::from));
                                let refs: Vec<&std::ffi::OsStr> =
                                    cmd.iter().map(std::ffi::OsString::as_os_str).collect();
                                run_cmd(&program, &refs, &dir, 120).await
                            };
                            match run.await {
                                Ok(out) => format_output(&out),
                                Err(e) => e,
                            }
                        }
                    }
                };
                (result, status)
            }
            "install_packages" => {
                let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
                let field = |k: &str| {
                    v.get(k)
                        .and_then(|x| x.as_str())
                        .unwrap_or_default()
                        .to_string()
                };
                let pkgs: Vec<String> = v
                    .get("packages")
                    .and_then(|a| a.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                let (skill, app) = (field("skill"), field("app"));
                let status = format!("Installing {}…", pkgs.join(" "));
                let result = match validate_packages(&pkgs) {
                    Err(e) => e,
                    Ok(()) if !skill.is_empty() && !app.is_empty() => {
                        "pass either skill or app, not both".to_string()
                    }
                    Ok(()) if !skill.is_empty() => {
                        match resolve_skill_dir(&self.skill_dirs, &skill) {
                            Err(_) => format!("unknown skill: {skill}"),
                            Ok(dir) if !dir.join("SKILL.md").is_file() => {
                                format!("unknown skill: {skill}")
                            }
                            Ok(dir) => {
                                let run = async {
                                    let py = ensure_venv(&dir).await?;
                                    let mut cmd: Vec<std::ffi::OsString> =
                                        vec!["-m".into(), "pip".into(), "install".into()];
                                    cmd.extend(pkgs.iter().map(std::ffi::OsString::from));
                                    let refs: Vec<&std::ffi::OsStr> =
                                        cmd.iter().map(std::ffi::OsString::as_os_str).collect();
                                    run_cmd(py.as_os_str(), &refs, &dir, 300).await
                                };
                                match run.await {
                                    Ok(out) if out.status.success() => {
                                        format!("installed {} into {skill}'s venv", pkgs.join(" "))
                                    }
                                    Ok(out) => {
                                        format!("pip install failed:\n{}", format_output(&out))
                                    }
                                    Err(e) => e,
                                }
                            }
                        }
                    }
                    Ok(()) if !app.is_empty() => match self.resolve_app(&app) {
                        Err(e) => e,
                        Ok((uuid, app_dir)) => {
                            let pkg_json = app_dir.join("package.json");
                            let prep = std::fs::create_dir_all(&app_dir).and_then(|()| {
                                if pkg_json.exists() {
                                    Ok(())
                                } else {
                                    // npm walks up looking for a package.json — pin
                                    // the install to this app dir with a minimal one.
                                    std::fs::write(
                                        &pkg_json,
                                        format!("{{\"name\":{app:?},\"private\":true}}"),
                                    )
                                }
                            });
                            if let Err(e) = prep {
                                format!("cannot prepare {app}: {e}")
                            } else {
                                let mut cmd: Vec<std::ffi::OsString> =
                                    vec!["install".into(), "--no-audit".into(), "--no-fund".into()];
                                cmd.extend(pkgs.iter().map(std::ffi::OsString::from));
                                let refs: Vec<&std::ffi::OsStr> =
                                    cmd.iter().map(std::ffi::OsString::as_os_str).collect();
                                match run_cmd("npm".as_ref(), &refs, &app_dir, 300).await {
                                    Ok(out) if out.status.success() => format!(
                                        "installed {} into {app}/node_modules — reference files as node_modules/<pkg>/… ; {}",
                                        pkgs.join(" "),
                                        self.app_link(&uuid),
                                    ),
                                    Ok(out) => {
                                        format!("npm install failed:\n{}", format_output(&out))
                                    }
                                    Err(e) => e,
                                }
                            }
                        }
                    },
                    Ok(()) => {
                        // No target: the space scripts dir venv (shared with run_python).
                        let dir = self.space_scripts_dir.clone();
                        let run = async {
                            std::fs::create_dir_all(&dir)
                                .map_err(|e| format!("cannot create scripts dir: {e}"))?;
                            let py = ensure_venv(&dir).await?;
                            let mut cmd: Vec<std::ffi::OsString> =
                                vec!["-m".into(), "pip".into(), "install".into()];
                            cmd.extend(pkgs.iter().map(std::ffi::OsString::from));
                            let refs: Vec<&std::ffi::OsStr> =
                                cmd.iter().map(std::ffi::OsString::as_os_str).collect();
                            run_cmd(py.as_os_str(), &refs, &dir, 300).await
                        };
                        match run.await {
                            Ok(out) if out.status.success() => {
                                format!("installed {} into the python scripts venv", pkgs.join(" "))
                            }
                            Ok(out) => format!("pip install failed:\n{}", format_output(&out)),
                            Err(e) => e,
                        }
                    }
                };
                (result, status)
            }
            "run_python" => {
                let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
                let code = v
                    .get("code")
                    .and_then(|x| x.as_str())
                    .unwrap_or_default()
                    .to_string();
                let name = v
                    .get("name")
                    .and_then(|n| n.as_str())
                    .filter(|n| !n.is_empty())
                    .map(str::to_string);
                let temporary = v
                    .get("temporary")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                let extra: Vec<String> = v
                    .get("args")
                    .and_then(|a| a.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                let result = match name {
                    None => "run_python requires a `name` parameter".to_string(),
                    Some(ref name) if !valid_relative_path(name) => {
                        format!("invalid script path: {name}")
                    }
                    Some(ref name) if !name.to_lowercase().ends_with(".py") => {
                        "run_python name must end with .py".to_string()
                    }
                    Some(ref name) => {
                        let (dir, file) = if temporary {
                            let tmp = std::env::temp_dir()
                                .join(format!("nexus-script-{}", uuid::Uuid::new_v4()));
                            let f = tmp.join(name);
                            (tmp, f)
                        } else {
                            let d = self.space_scripts_dir.clone();
                            let f = d.join(name);
                            (d, f)
                        };
                        let run = async {
                            if code.trim().is_empty() {
                                return Err("code must not be empty".to_string());
                            }
                            std::fs::create_dir_all(&dir)
                                .map_err(|e| format!("cannot create dir: {e}"))?;
                            std::fs::write(&file, &code)
                                .map_err(|e| format!("cannot write script: {e}"))?;
                            let py = ensure_venv(&dir).await?;
                            let mut cmd: Vec<std::ffi::OsString> = vec![file.into()];
                            cmd.extend(extra.iter().map(std::ffi::OsString::from));
                            let refs: Vec<&std::ffi::OsStr> =
                                cmd.iter().map(std::ffi::OsString::as_os_str).collect();
                            let files_dir = self.space_files_dir.to_string_lossy().to_string();
                            let apps_dir = self.space_apps_dir.to_string_lossy().to_string();
                            let scripts_dir = self.space_scripts_dir.to_string_lossy().to_string();
                            let envs = &[
                                ("SPACE_FILES_DIR", files_dir.as_str()),
                                ("SPACE_APPS_DIR", apps_dir.as_str()),
                                ("SPACE_SCRIPTS_DIR", scripts_dir.as_str()),
                            ];
                            run_cmd_env(py.as_os_str(), &refs, &dir, 120, envs).await
                        };
                        let output = run.await;
                        if temporary {
                            let _ = std::fs::remove_dir_all(&dir);
                        }
                        match output {
                            Ok(out) => format_output(&out),
                            Err(e) => e,
                        }
                    }
                };
                (result, "Running script…".to_string())
            }
            "list_scripts" => {
                let status = "Listing scripts…".to_string();
                let result = match std::fs::read_dir(&self.space_scripts_dir) {
                    Err(_) => "[]".to_string(),
                    Ok(entries) => {
                        let scripts: Vec<serde_json::Value> = entries
                            .flatten()
                            .filter(|e| e.path().is_file())
                            .filter_map(|e| {
                                let meta = e.metadata().ok()?;
                                let ext = e
                                    .path()
                                    .extension()
                                    .and_then(|x| x.to_str())
                                    .unwrap_or("")
                                    .to_string();
                                Some(serde_json::json!({
                                    "name": e.file_name().to_string_lossy(),
                                    "size": meta.len(),
                                    "ext": ext,
                                }))
                            })
                            .collect();
                        serde_json::to_string(&scripts).unwrap_or_else(|_| "[]".to_string())
                    }
                };
                (result, status)
            }
            "write_script" => {
                let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
                let path = v
                    .get("path")
                    .and_then(|x| x.as_str())
                    .unwrap_or_default()
                    .to_string();
                let content = v
                    .get("content")
                    .and_then(|x| x.as_str())
                    .unwrap_or_default()
                    .to_string();
                let status = format!("Writing {path}…");
                let result = match confined_join(&self.space_scripts_dir, &path) {
                    Err(e) => e,
                    Ok(file) => {
                        let write = file
                            .parent()
                            .map_or(Ok(()), std::fs::create_dir_all)
                            .and_then(|()| std::fs::write(&file, &content));
                        match write {
                            Ok(()) => format!("wrote {path} ({} bytes)", content.len()),
                            Err(e) => format!("write failed: {e}"),
                        }
                    }
                };
                (result, status)
            }
            "read_script" => {
                let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
                let path = v
                    .get("path")
                    .and_then(|x| x.as_str())
                    .unwrap_or_default()
                    .to_string();
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
                let status = format!("Reading {path}…");
                let result = match confined_join(&self.space_scripts_dir, &path) {
                    Err(e) => e,
                    Ok(file) => match std::fs::read_to_string(&file) {
                        Err(e) => format!("cannot read {path}: {e}"),
                        Ok(text) => {
                            let lines: Vec<&str> = text.lines().collect();
                            let total = lines.len();
                            let start = (offset - 1).min(total);
                            let slice = &lines[start..(start + limit).min(total)];
                            if slice.is_empty() {
                                format!("{path}: offset {offset} is past the end ({total} lines)")
                            } else {
                                format!(
                                    "{path} (lines {}-{} of {total}):\n{}",
                                    start + 1,
                                    start + slice.len(),
                                    number_lines_with_hash(slice, start),
                                )
                            }
                        }
                    },
                };
                (result, status)
            }
            "edit_script" => {
                let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
                let path = v
                    .get("path")
                    .and_then(|x| x.as_str())
                    .unwrap_or_default()
                    .to_string();
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
                let status = format!("Editing {path}…");
                let result = match confined_join(&self.space_scripts_dir, &path) {
                    Err(e) => e,
                    Ok(file) => match std::fs::read_to_string(&file) {
                        Err(e) => format!("cannot read {path}: {e}"),
                        Ok(text) => match apply_hashline_edits(&text, &edits) {
                            Err(e) => e,
                            Ok((new_text, diff)) => match std::fs::write(&file, new_text) {
                                Ok(()) => format!("edited {path} — {diff}"),
                                Err(e) => format!("write failed: {e}"),
                            },
                        },
                    },
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

/// Run a command with a timeout, kill-on-drop, and no shell. Returns the
/// raw output; spawn failures name the missing program.
pub(super) async fn run_cmd(
    program: &std::ffi::OsStr,
    args: &[&std::ffi::OsStr],
    dir: &std::path::Path,
    secs: u64,
) -> Result<std::process::Output, String> {
    run_cmd_env(program, args, dir, secs, &[]).await
}

/// Like `run_cmd` but with extra environment variables.
pub(super) async fn run_cmd_env(
    program: &std::ffi::OsStr,
    args: &[&std::ffi::OsStr],
    dir: &std::path::Path,
    secs: u64,
    envs: &[(&str, &str)],
) -> Result<std::process::Output, String> {
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args).current_dir(dir).kill_on_drop(true);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    crate::config::scrub_secret_env(cmd.as_std_mut());
    let fut = cmd.output();
    match tokio::time::timeout(std::time::Duration::from_secs(secs), fut).await {
        Err(_) => Err(format!(
            "{} timed out after {secs}s",
            program.to_string_lossy()
        )),
        Ok(Err(e)) => Err(format!("cannot run {}: {e}", program.to_string_lossy())),
        Ok(Ok(out)) => Ok(out),
    }
}

/// Command output as tool-result text: stdout, then stderr, then a non-zero
/// exit code — truncated so a chatty script can't flood the context.
pub(super) fn format_output(out: &std::process::Output) -> String {
    let mut s = String::from(String::from_utf8_lossy(&out.stdout).trim_end());
    let err = String::from_utf8_lossy(&out.stderr);
    if !err.trim().is_empty() {
        if !s.is_empty() {
            s.push('\n');
        }
        s.push_str("stderr:\n");
        s.push_str(err.trim_end());
    }
    let mut lines: Vec<&str> = s.lines().collect();
    if lines.len() > 200 {
        lines.truncate(200);
        lines.push("… (output truncated)");
    }
    let mut s = lines.join("\n");
    if s.chars().count() > 8000 {
        s = s.chars().take(8000).collect();
        s.push_str("\n… (output truncated)");
    }
    if !out.status.success() {
        let code = out
            .status
            .code()
            .map_or_else(|| "killed".to_string(), |c| c.to_string());
        if !s.is_empty() {
            s.push('\n');
        }
        let _ = write!(s, "exit code: {code}");
    }
    if s.is_empty() {
        s = "(no output)".to_string();
    }
    s
}

/// The python interpreter of a skill's own `.venv`, creating the venv (and
/// installing `requirements.txt` if the skill ships one) on first use.
/// Everything stays inside the skill's directory — nothing global.
async fn ensure_venv(skill_dir: &std::path::Path) -> Result<PathBuf, String> {
    let python = skill_dir.join(".venv/bin/python");
    if python.exists() {
        return Ok(python);
    }
    std::fs::create_dir_all(skill_dir)
        .map_err(|e| format!("cannot create {}: {e}", skill_dir.display()))?;
    // Corrupt venv from a system Python upgrade — nuke it and recreate.
    if skill_dir.join(".venv").exists() {
        std::fs::remove_dir_all(skill_dir.join(".venv"))
            .map_err(|e| format!("cannot remove corrupt venv: {e}"))?;
    }
    let out = run_cmd(
        "python3".as_ref(),
        &["-m".as_ref(), "venv".as_ref(), ".venv".as_ref()],
        skill_dir,
        120,
    )
    .await?;
    if !out.status.success() {
        return Err(format!("venv creation failed:\n{}", format_output(&out)));
    }
    if skill_dir.join("requirements.txt").exists() {
        let out = run_cmd(
            python.as_os_str(),
            &[
                "-m".as_ref(),
                "pip".as_ref(),
                "install".as_ref(),
                "-r".as_ref(),
                "requirements.txt".as_ref(),
            ],
            skill_dir,
            300,
        )
        .await?;
        if !out.status.success() {
            return Err(format!(
                "pip install -r requirements.txt failed:\n{}",
                format_output(&out)
            ));
        }
    }
    Ok(python)
}

/// Package names an installer may see: no flags, no whitespace — they land
/// in cmd directly, so a leading `-` would become an option injection.
fn validate_packages(pkgs: &[String]) -> Result<(), String> {
    if pkgs.is_empty() {
        return Err("no packages given".to_string());
    }
    for p in pkgs {
        if p.is_empty() || p.starts_with('-') || p.chars().any(char::is_whitespace) {
            return Err(format!("invalid package name: {p:?}"));
        }
    }
    Ok(())
}
