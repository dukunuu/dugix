use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{ListItem, ListState};

use crate::app_view::AppView;

use super::chrome::{self, PopupFrame, ctrl, hint, keys};

#[allow(clippy::too_many_lines)] // rows + browse / install / confirm chrome
pub fn render(f: &mut Frame, app: &AppView) {
    use nexus_core::app::SkillsMode;
    let area = chrome::popup_area(app, f.area(), "skills", chrome::STANDARD);

    let width = area.width.saturating_sub(5 + 2 * chrome::PAD) as usize;
    let items: Vec<ListItem> = app
        .skills
        .iter()
        .map(|s| {
            // App-managed skills are removable; an agent tool's are read-only.
            let source = if app.skill_is_app_managed(s) {
                "nexus".to_string()
            } else {
                skill_source(&s.dir)
            };
            let name = chrome::truncate(
                &s.name,
                width.saturating_sub(crate::ui::style::width(&source) + 2),
            );
            let gap = width
                .saturating_sub(crate::ui::style::width(&name) + crate::ui::style::width(&source));
            ListItem::new(Line::from(vec![
                Span::styled(name, Style::default().fg(app.theme.fg)),
                Span::raw(" ".repeat(gap)),
                Span::styled(source, Style::default().fg(app.theme.fg_dim)),
            ]))
        })
        .collect();

    let count = if app.skills_mode == SkillsMode::Browse {
        format!(
            "{} skill{}",
            app.skills.len(),
            if app.skills.len() == 1 { "" } else { "s" }
        )
    } else {
        String::new()
    };
    let (title, hints) = match app.skills_mode {
        SkillsMode::Install => (
            "Install skill",
            vec![hint(keys::ENTER, "install"), hint(keys::ESC, "cancel")],
        ),
        SkillsMode::ConfirmRemove => (
            "Remove skill?",
            vec![hint(ctrl('d'), "remove"), hint(keys::ESC, "cancel")],
        ),
        SkillsMode::Browse => (
            "Skills",
            vec![
                hint(keys::MOVE, ""),
                hint(keys::ENTER, "arm"),
                hint(ctrl('n'), "install"),
                hint(ctrl('d'), "remove"),
                hint(keys::ESC, "close"),
            ],
        ),
    };
    let tone = if app.skills_mode == SkillsMode::ConfirmRemove {
        chrome::Tone::Danger
    } else {
        chrome::Tone::Normal
    };
    let mut inner = chrome::render_popup(
        f,
        area,
        &PopupFrame {
            title,
            count: &count,
            hints: &hints,
            tone,
            focused: true,
        },
        app,
    );
    if app.skills_mode == SkillsMode::Install {
        inner = chrome::render_field(
            f,
            inner,
            "✎",
            &app.skills_edit,
            "owner/repo/path",
            false,
            &app.theme,
        );
    }
    let desc = app
        .skills
        .get(app.skills_selected)
        .map_or_else(String::new, |skill| {
            if app.skills_mode == SkillsMode::ConfirmRemove {
                format!("“{}” will be removed.", skill.name)
            } else {
                skill.description.clone()
            }
        });
    let (list_area, detail_area) = chrome::split_with_detail(inner, &desc);

    let list = chrome::standard_list(items, &app.theme);
    let mut state = ListState::default();
    if !app.skills.is_empty() {
        state.select(Some(app.skills_selected.min(app.skills.len() - 1)));
    }
    chrome::render_list(f, list, &mut state, list_area, app.skills.len(), 1, app);
    chrome::render_detail(f, detail_area, &desc, &app.theme);
    if app.skills.is_empty() {
        chrome::render_empty(
            f,
            list_area,
            "No skills installed. ^N installs one.",
            &app.theme,
        );
    }
}

pub fn handle_key(app: &mut AppView, key: KeyEvent) {
    use super::{
        ConfirmDeleteAction, EditAction, classify_browse_key, classify_confirm_delete_key,
        classify_edit_key,
    };
    use nexus_core::app::SkillsMode;
    match app.skills_mode {
        SkillsMode::Install => match classify_edit_key(key) {
            Some(EditAction::Cancel) => app.skills_mode = SkillsMode::Browse,
            Some(EditAction::Save) => app.confirm_skill_install(),
            Some(EditAction::Backspace) => {
                app.skills_edit.pop();
            }
            Some(EditAction::Push(c)) => app.skills_edit.push(c),
            None => {}
        },
        SkillsMode::ConfirmRemove => match classify_confirm_delete_key(key) {
            Some(ConfirmDeleteAction::Yes) => app.confirm_skill_remove(),
            Some(ConfirmDeleteAction::No) => app.skills_mode = SkillsMode::Browse,
            None => {}
        },
        // Enter arms the highlighted skill for the next message, like
        // `/<skill-name>`; there is no rename and no text filter.
        SkillsMode::Browse if key.code == crossterm::event::KeyCode::Enter => {
            if let Some(name) = app.skills.get(app.skills_selected).map(|s| s.name.clone()) {
                app.popup = nexus_core::app::Popup::None;
                if let Err(e) =
                    app.execute(nexus_core::app::AppCommand::ArmSkill { name, rest: None })
                {
                    app.push_status(format!("error: {e:#}"));
                }
            }
        }
        SkillsMode::Browse if !super::is_chord(key) && key.code == KeyCode::Char('k') => {
            app.move_skills_selection(-1);
        }
        SkillsMode::Browse if !super::is_chord(key) && key.code == KeyCode::Char('j') => {
            app.move_skills_selection(1);
        }
        SkillsMode::Browse => match classify_browse_key(key, true, false) {
            Some(super::BrowseAction::Close) => app.popup = nexus_core::app::Popup::None,
            Some(super::BrowseAction::MoveUp) => app.move_skills_selection(-1),
            Some(super::BrowseAction::MoveDown) => app.move_skills_selection(1),
            Some(
                a @ (super::BrowseAction::PageUp
                | super::BrowseAction::PageDown
                | super::BrowseAction::Top
                | super::BrowseAction::Bottom),
            ) => {
                super::apply_page(|d| app.move_skills_selection(d), a, 10);
            }
            Some(super::BrowseAction::Create) => app.start_skill_install(),
            Some(super::BrowseAction::ConfirmDelete) => app.start_skill_remove(),
            _ => {}
        },
    }
}

/// The agent tool a non-app skill came from: the directory holding its `skills`
/// root, looking past a `.system` namespace.
fn skill_source(dir: &std::path::Path) -> String {
    let parts: Vec<&str> = dir
        .iter()
        .filter_map(|p| p.to_str())
        .filter(|p| *p != ".system")
        .collect();
    parts
        .iter()
        .rposition(|p| *p == "skills")
        .and_then(|i| i.checked_sub(1))
        .map_or_else(|| "external".to_string(), |i| parts[i].to_string())
}

#[cfg(test)]
mod tests {
    #[test]
    fn skill_source_names_the_tool_root() {
        let src = |p: &str| super::skill_source(std::path::Path::new(p));
        assert_eq!(src("/home/u/.claude/skills/grill-me"), ".claude");
        assert_eq!(src("/repo/.agents/skills/tdd"), ".agents");
        assert_eq!(src("/home/u/.codex/skills/.system/imagegen"), ".codex");
        assert_eq!(src("/somewhere/else/tdd"), "external");
    }
}
