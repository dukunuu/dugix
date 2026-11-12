//! Agent Skills on disk: a directory with a `SKILL.md` carrying YAML frontmatter.
//! Startup reads only the name and description; the body loads on demand.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub dir: PathBuf,
}

pub fn skills_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("skills")
}

/// The skill roots used by a normal application bootstrap, in precedence order:
/// an earlier root wins a duplicate name. Ad-hoc data directories deliberately
/// omit external roots, which keeps `App::new` test fixtures hermetic.
pub fn app_skill_roots(data_dir: &Path) -> Vec<PathBuf> {
    let local = skills_dir(data_dir);
    let is_default_data_dir =
        crate::config::project_dirs().is_ok_and(|dirs| dirs.data_dir() == data_dir);
    if is_default_data_dir {
        skill_search_paths(data_dir)
    } else {
        vec![local]
    }
}

/// The standard project and user Agent Skills roots: project roots from the current
/// directory towards the filesystem root, then user-level roots.
pub fn skill_search_paths(data_dir: &Path) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let cwd = std::env::current_dir().ok();
    let home = home_dir();
    add_project_roots(&mut roots, cwd.as_deref());
    // Nexus's managed root is explicit user state, so it outranks adapter roots
    // but stays below project-specific skills.
    push_skill_root(&mut roots, &skills_dir(data_dir));
    add_user_roots(&mut roots, home.as_deref());
    roots
}

const PROJECT_LAYOUTS: &[&str] = &[
    ".agents/skills",
    ".claude/skills",
    ".codex/skills",
    ".pi/agent/skills",
    ".opencode/skills",
];

fn add_project_roots(roots: &mut Vec<PathBuf>, cwd: Option<&Path>) {
    let Some(cwd) = cwd else { return };
    for ancestor in cwd.ancestors() {
        for layout in PROJECT_LAYOUTS {
            push_skill_root(roots, &ancestor.join(layout));
        }
    }
}

fn add_user_roots(roots: &mut Vec<PathBuf>, home: Option<&Path>) {
    // These are used by the corresponding agent ecosystems and are more
    // authoritative than their conventional defaults.
    for variable in ["AGENTS_HOME", "CODEX_HOME"] {
        if let Some(value) = std::env::var_os(variable) {
            push_skill_root(roots, &PathBuf::from(value).join("skills"));
        }
    }
    if let Some(value) = std::env::var_os("PI_AGENT_DIR") {
        push_skill_root(roots, &PathBuf::from(value).join("skills"));
    }
    if let Some(value) = std::env::var_os("NEXUS_SKILLS_DIR") {
        push_skill_root(roots, &PathBuf::from(value));
    }

    let Some(home) = home else { return };
    // `.agents/skills` is the portable location; the adapter roots let Nexus
    // consume skills installed by other agent runtimes too.
    for relative in [
        ".agents/skills",
        ".codex/skills",
        ".pi/agent/skills",
        ".claude/skills",
        ".config/agents/skills",
        ".config/opencode/skills",
    ] {
        push_skill_root(roots, &home.join(relative));
    }
    push_skill_root(roots, Path::new("/etc/agents/skills"));
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| directories::BaseDirs::new().map(|dirs| dirs.home_dir().to_path_buf()))
}

fn push_skill_root(roots: &mut Vec<PathBuf>, root: &Path) {
    push_unique(roots, root.to_path_buf());
    push_unique(roots, root.join(".system"));
}

fn push_unique(paths: &mut Vec<PathBuf>, path: PathBuf) {
    if !paths.iter().any(|existing| existing == &path) {
        paths.push(path);
    }
}

const FIND_SKILLS_SKILL: &str = include_str!("../assets/find-skills-SKILL.md");

/// Never overwrites: once installed they are normal files the user can edit or delete.
pub fn install_builtin(dir: &Path) {
    for (name, md) in [("find-skills", FIND_SKILLS_SKILL)] {
        let path = dir.join(name).join("SKILL.md");
        if path.exists() {
            continue;
        }
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(path, md);
    }
}

/// Missing or unreadable directories are treated as empty.
pub fn load_skills(dir: &Path) -> Vec<Skill> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut skills: Vec<Skill> = entries
        .flatten()
        .filter_map(|entry| {
            if !entry.file_type().ok()?.is_dir() {
                return None;
            }
            let path = entry.path();
            let directory_name = path.file_name()?.to_str()?;
            let skill_file = path.join("SKILL.md");
            if !std::fs::symlink_metadata(&skill_file)
                .ok()?
                .file_type()
                .is_file()
            {
                return None;
            }
            let md = std::fs::read_to_string(skill_file).ok()?;
            let (name, description) = parse_frontmatter(&md)?;
            // The standard requires the metadata name to match the containing
            // directory; it also prevents ambiguous slash-command names.
            (name == directory_name).then_some(Skill {
                name,
                description,
                dir: path,
            })
        })
        .collect();
    skills.sort_unstable_by(|a, b| a.name.cmp(&b.name));
    skills
}

/// Keep the first valid definition of a duplicate name; `roots` must be ordered
/// from most-specific to least-specific.
pub fn load_skills_from_dirs(roots: &[PathBuf]) -> Vec<Skill> {
    let mut by_name = BTreeMap::new();
    for root in roots {
        for skill in load_skills(root) {
            by_name.entry(skill.name.clone()).or_insert(skill);
        }
    }
    by_name.into_values().collect()
}

#[must_use]
pub fn valid_skill_name(name: &str) -> bool {
    let length = name.chars().count();
    if !(1..=64).contains(&length) {
        return false;
    }
    let mut previous_hyphen = false;
    for (index, character) in name.chars().enumerate() {
        let is_hyphen = character == '-';
        if !(character.is_ascii_lowercase() || character.is_ascii_digit() || is_hyphen)
            || (index == 0 || index + 1 == length) && is_hyphen
            || previous_hyphen && is_hyphen
        {
            return false;
        }
        previous_hyphen = is_hyphen;
    }
    true
}

/// The opening and closing `---` lines are mandatory; unknown frontmatter fields
/// are accepted so skills from other runtimes remain portable.
pub fn parse_frontmatter(md: &str) -> Option<(String, String)> {
    let md = strip_bom(md);
    let mut lines = md.lines();
    if lines.next()?.trim() != "---" {
        return None;
    }
    let mut name = None;
    let mut description = None;
    let mut closed = false;
    for line in lines {
        let trimmed = line.trim();
        if trimmed == "---" {
            closed = true;
            break;
        }
        let Some((key, raw_value)) = trimmed.split_once(':') else {
            continue;
        };
        let key = key.trim();
        if key != "name" && key != "description" {
            continue;
        }
        let value = parse_scalar(raw_value.trim())?;
        match key {
            "name" if name.is_none() => name = Some(value),
            "description" if description.is_none() => description = Some(value),
            _ => return None,
        }
    }
    if !closed {
        return None;
    }
    let name = name?;
    let description = description?;
    (valid_skill_name(&name) && !description.is_empty() && description.chars().count() <= 1_024)
        .then_some((name, description))
}

fn parse_scalar(raw: &str) -> Option<String> {
    if raw.is_empty() {
        return None;
    }
    let first = raw.as_bytes().first().copied()?;
    if first == b'\'' || first == b'"' {
        let last = raw.as_bytes().last().copied()?;
        if last != first || raw.len() < 2 {
            return None;
        }
        let value = raw[1..raw.len() - 1].to_string();
        (!value.is_empty()).then_some(value)
    } else {
        Some(raw.to_string())
    }
}

fn strip_bom(md: &str) -> &str {
    md.strip_prefix('\u{feff}').unwrap_or(md)
}

fn frontmatter_body_start(md: &str) -> Option<usize> {
    let mut lines = md.split_inclusive('\n');
    let first = lines.next()?;
    if first.trim() != "---" {
        return None;
    }
    let mut offset = first.len();
    for line in lines {
        if line.trim().trim_end_matches('\r') == "---" {
            return Some(offset + line.len());
        }
        offset += line.len();
    }
    None
}

/// Everything after the closing `---` of valid frontmatter, trimmed; a file that
/// is not a valid skill document is returned whole.
pub fn skill_body(md: &str) -> &str {
    let md = strip_bom(md);
    if parse_frontmatter(md).is_some()
        && let Some(start) = frontmatter_body_start(md)
    {
        return md[start..].trim();
    }
    md.trim()
}

/// `owner/repo/path/to/skill` (or bare `owner/repo`) in three parts. Only safe
/// GitHub path components are accepted: the value lands in API URLs and local paths.
pub fn parse_gh_shorthand(s: &str) -> Option<(String, String, String)> {
    let value = s.trim();
    if value.is_empty() || value.starts_with('/') || value.ends_with('/') {
        return None;
    }
    let mut parts = value.split('/');
    let owner = parts.next()?.to_string();
    let repo = parts.next()?.to_string();
    if !valid_github_segment(&owner) || !valid_github_segment(&repo) {
        return None;
    }
    let path_parts: Vec<&str> = parts.collect();
    if path_parts
        .iter()
        .any(|part| !valid_github_segment(part) || *part == "." || *part == "..")
    {
        return None;
    }
    Some((owner, repo, path_parts.join("/")))
}

fn valid_github_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment != "."
        && segment != ".."
        && segment
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "-_.".contains(character))
}

#[derive(serde::Deserialize)]
struct GhEntry {
    name: String,
    #[serde(rename = "type")]
    kind: String,
    download_url: Option<String>,
}

pub fn resolve_skill_dir(roots: &[PathBuf], name: &str) -> Result<PathBuf, String> {
    if !valid_skill_name(name) {
        return Err(format!("invalid skill name: {name:?}"));
    }
    load_skills_from_dirs(roots)
        .into_iter()
        .find(|skill| skill.name == name)
        .map(|skill| skill.dir)
        .ok_or_else(|| format!("unknown skill: {name}"))
}

/// Reject traversal and symlinks that point outside the skill directory; a missing
/// final file is returned so the caller can produce a useful `no such file` message.
pub fn resolve_skill_file(roots: &[PathBuf], name: &str, file: &str) -> Result<PathBuf, String> {
    let dir = resolve_skill_dir(roots, name)?;
    if !valid_relative_path(file) {
        return Err(format!("invalid path: {file:?}"));
    }
    let path = file.split('/').fold(dir.clone(), |mut path, segment| {
        path.push(segment);
        path
    });
    if path.exists() {
        let canonical_dir = std::fs::canonicalize(&dir)
            .map_err(|e| format!("cannot resolve skill directory: {e}"))?;
        let canonical_path =
            std::fs::canonicalize(&path).map_err(|e| format!("cannot resolve skill file: {e}"))?;
        if !canonical_path.starts_with(&canonical_dir) {
            return Err(format!("skill path escapes skill directory: {file:?}"));
        }
    }
    Ok(path)
}

fn valid_relative_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && Path::new(path)
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
}

/// Only app-managed skills may be removed by the `/skills` UI; external agent roots
/// stay readable but are never deleted by Nexus.
#[must_use]
pub fn is_app_managed(skill: &Skill, data_dir: &Path) -> bool {
    skill
        .dir
        .parent()
        .is_some_and(|parent| parent == skills_dir(data_dir))
}

/// Fetch `SKILL.md` and sibling files (one directory deep) into
/// `dest_root/<skill-name>/`, cleaning up a partial download unless a valid SKILL.md
/// is found.
pub async fn install_from_github(
    client: &reqwest::Client,
    owner: &str,
    repo: &str,
    path: &str,
    dest_root: &Path,
) -> anyhow::Result<String> {
    anyhow::ensure!(
        valid_github_segment(owner)
            && valid_github_segment(repo)
            && (path.is_empty() || path.split('/').all(valid_github_segment)),
        "invalid GitHub skill source: {owner}/{repo}/{path}"
    );
    let entries = fetch_gh_contents(client, owner, repo, path).await?;
    let Some(skill_entry) = entries
        .iter()
        .find(|entry| entry.name == "SKILL.md" && entry.kind == "file")
    else {
        anyhow::bail!("no SKILL.md at {owner}/{repo}/{path}");
    };
    let Some(skill_url) = &skill_entry.download_url else {
        anyhow::bail!("SKILL.md at {owner}/{repo}/{path} has no download URL");
    };
    let skill_md = client
        .get(skill_url)
        .header("User-Agent", "nexus-chat")
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    let Some((name, _)) = parse_frontmatter(&skill_md) else {
        anyhow::bail!(
            "SKILL.md at {owner}/{repo}/{path} is missing valid Agent Skills frontmatter"
        );
    };
    let dest = dest_root.join(&name);
    let result = download_gh_entries(client, &entries, &dest, owner, repo, path, 1).await;
    if let Err(error) = result {
        let _ = std::fs::remove_dir_all(&dest);
        return Err(error);
    }
    // Validate before declaring success: a SKILL.md with bad frontmatter or a
    // mismatched directory name must not become invisible to the loader.
    let md = match std::fs::read_to_string(dest.join("SKILL.md")) {
        Ok(md) => md,
        Err(error) => {
            let _ = std::fs::remove_dir_all(&dest);
            return Err(error.into());
        }
    };
    let Some((skill_name, _)) = parse_frontmatter(&md) else {
        let _ = std::fs::remove_dir_all(&dest);
        anyhow::bail!(
            "SKILL.md at {owner}/{repo}/{path} is missing valid Agent Skills frontmatter"
        );
    };
    if skill_name != name {
        let _ = std::fs::remove_dir_all(&dest);
        anyhow::bail!("SKILL.md name '{skill_name}' does not match skill directory '{name}'");
    }
    Ok(name)
}

async fn fetch_gh_contents(
    client: &reqwest::Client,
    owner: &str,
    repo: &str,
    path: &str,
) -> anyhow::Result<Vec<GhEntry>> {
    let url = format!("https://api.github.com/repos/{owner}/{repo}/contents/{path}");
    let resp = client
        .get(&url)
        .header("User-Agent", "nexus-chat")
        .send()
        .await?
        .error_for_status()?
        .json::<Vec<GhEntry>>()
        .await?;
    Ok(resp)
}

fn valid_entry_name(name: &str) -> bool {
    !name.is_empty()
        && !name.contains(['/', '\\'])
        && !name.chars().any(char::is_control)
        && name != "."
        && name != ".."
}

/// Download a GitHub contents listing into `dest`, recursing up to `depth_left`
/// levels: skill resource folders are small and shallow.
fn download_gh_entries<'a>(
    client: &'a reqwest::Client,
    entries: &'a [GhEntry],
    dest: &'a Path,
    owner: &'a str,
    repo: &'a str,
    repo_path: &'a str,
    depth_left: u8,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>> + Send + 'a>> {
    Box::pin(async move {
        std::fs::create_dir_all(dest)?;
        for entry in entries {
            anyhow::ensure!(
                valid_entry_name(&entry.name),
                "unsafe GitHub entry name: {}",
                entry.name
            );
            match entry.kind.as_str() {
                "file" => {
                    let Some(url) = &entry.download_url else {
                        continue;
                    };
                    let bytes = client
                        .get(url)
                        .header("User-Agent", "nexus-chat")
                        .send()
                        .await?
                        .error_for_status()?
                        .bytes()
                        .await?;
                    std::fs::write(dest.join(&entry.name), bytes)?;
                }
                "dir" if depth_left > 0 => {
                    let sub_repo_path = if repo_path.is_empty() {
                        entry.name.clone()
                    } else {
                        format!("{repo_path}/{}", entry.name)
                    };
                    let subentries = fetch_gh_contents(client, owner, repo, &sub_repo_path).await?;
                    download_gh_entries(
                        client,
                        &subentries,
                        &dest.join(&entry.name),
                        owner,
                        repo,
                        &sub_repo_path,
                        depth_left - 1,
                    )
                    .await?;
                }
                _ => {}
            }
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(prefix: &str) -> PathBuf {
        std::env::temp_dir().join(format!("{prefix}-{}", uuid::Uuid::new_v4()))
    }

    fn write_skill(root: &Path, dir: &str, name: &str, description: &str, body: &str) {
        let skill_dir = root.join(dir);
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {description}\n---\n{body}"),
        )
        .unwrap();
    }

    #[test]
    fn install_builtin_writes_find_skills_without_overwriting() {
        let dir = temp_dir("nexus-skills");
        install_builtin(&dir);
        let names: Vec<String> = load_skills(&dir).iter().map(|s| s.name.clone()).collect();
        assert_eq!(names, vec!["find-skills"]);

        std::fs::write(
            dir.join("find-skills/SKILL.md"),
            "---\nname: mine\ndescription: d\n---\nx",
        )
        .unwrap();
        install_builtin(&dir);
        assert!(
            std::fs::read_to_string(dir.join("find-skills/SKILL.md"))
                .unwrap()
                .contains("mine")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parses_frontmatter_and_body() {
        let md = "---\nname: web-search\ndescription: Search the web\n---\nBody text here.";
        let (name, desc) = parse_frontmatter(md).unwrap();
        assert_eq!(name, "web-search");
        assert_eq!(desc, "Search the web");
        assert_eq!(skill_body(md), "Body text here.");
    }

    #[test]
    fn quoted_values_are_unquoted() {
        let md = "---\nname: \"a\"\ndescription: 'b'\n---\nx";
        assert_eq!(
            parse_frontmatter(md),
            Some(("a".to_string(), "b".to_string()))
        );
    }

    #[test]
    fn strict_frontmatter_requires_closing_delimiter_and_valid_name() {
        assert_eq!(
            parse_frontmatter("---\nname: X\ndescription: d\n---\nx"),
            None
        );
        assert_eq!(
            parse_frontmatter("---\nname: a--b\ndescription: d\n---\nx"),
            None
        );
        assert_eq!(parse_frontmatter("---\nname: x\ndescription: d"), None);
        assert_eq!(
            skill_body("---\nname: x\ndescription: d"),
            "---\nname: x\ndescription: d"
        );
    }

    #[test]
    fn missing_frontmatter_is_none_but_body_is_whole_file() {
        let md = "just a plain file, no frontmatter";
        assert_eq!(parse_frontmatter(md), None);
        assert_eq!(skill_body(md), md);
    }

    #[test]
    fn missing_description_is_none() {
        let md = "---\nname: x\n---\nbody";
        assert_eq!(parse_frontmatter(md), None);
    }

    #[test]
    fn crlf_line_endings_still_parse() {
        let md = "---\r\nname: x\r\ndescription: y\r\n---\r\nbody\r\n";
        assert_eq!(
            parse_frontmatter(md),
            Some(("x".to_string(), "y".to_string()))
        );
        assert_eq!(skill_body(md), "body");
    }

    #[test]
    fn mismatched_directory_names_are_not_loaded() {
        let root = temp_dir("nexus-skill-strict");
        write_skill(&root, "folder", "different", "d", "body");
        assert!(load_skills(&root).is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn roots_are_merged_with_specific_root_precedence() {
        let first = temp_dir("nexus-skills-first");
        let second = temp_dir("nexus-skills-second");
        write_skill(&first, "shared", "shared", "first", "one");
        write_skill(&second, "shared", "shared", "second", "two");
        write_skill(&second, "other", "other", "other", "three");
        let skills = load_skills_from_dirs(&[first.clone(), second.clone()]);
        assert_eq!(
            skills.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            ["other", "shared"]
        );
        let shared = skills.iter().find(|s| s.name == "shared").unwrap();
        assert_eq!(shared.description, "first");
        assert_eq!(
            skill_body(&std::fs::read_to_string(shared.dir.join("SKILL.md")).unwrap()),
            "one"
        );
        let _ = std::fs::remove_dir_all(first);
        let _ = std::fs::remove_dir_all(second);
    }

    #[test]
    fn parses_owner_repo_path_shorthand() {
        assert_eq!(
            parse_gh_shorthand("anthropics/skills/pdf"),
            Some((
                "anthropics".to_string(),
                "skills".to_string(),
                "pdf".to_string()
            ))
        );
    }

    #[test]
    fn parses_bare_owner_repo_as_root_path() {
        assert_eq!(
            parse_gh_shorthand("anthropics/skills"),
            Some((
                "anthropics".to_string(),
                "skills".to_string(),
                String::new()
            ))
        );
    }

    #[test]
    fn rejects_unsafe_shorthand() {
        assert_eq!(parse_gh_shorthand("anthropics"), None);
        assert_eq!(parse_gh_shorthand(""), None);
        assert_eq!(parse_gh_shorthand("a/b/../skill"), None);
        assert_eq!(parse_gh_shorthand("a/b/skill\\x"), None);
    }

    #[test]
    fn multi_segment_path_survives_split() {
        assert_eq!(
            parse_gh_shorthand("a/b/nested/path/skill"),
            Some((
                "a".to_string(),
                "b".to_string(),
                "nested/path/skill".to_string()
            ))
        );
    }

    #[test]
    fn confined_skill_files_support_external_roots() {
        let root = temp_dir("nexus-skills-confined");
        write_skill(&root, "t", "t", "d", "x");
        std::fs::write(root.join("t/readme.md"), "resource").unwrap();
        let roots = vec![root.clone()];
        assert_eq!(
            std::fs::read_to_string(resolve_skill_file(&roots, "t", "readme.md").unwrap()).unwrap(),
            "resource"
        );
        assert!(resolve_skill_file(&roots, "t", "../outside").is_err());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn gh_contents_json_parses_into_entries() {
        let json = r#"[
            {"name": "SKILL.md", "type": "file", "download_url": "https://raw/SKILL.md"},
            {"name": "helpers", "type": "dir", "download_url": null}
        ]"#;
        let entries: Vec<GhEntry> = serde_json::from_str(json).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "SKILL.md");
        assert_eq!(entries[0].kind, "file");
        assert_eq!(entries[1].kind, "dir");
    }
}
