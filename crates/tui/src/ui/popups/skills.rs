use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{ListItem, ListState};

use crate::app_view::AppView;

use super::chrome::{self, PopupFrame, ctrl, hint, keys};
use super::list_filter;

#[allow(clippy::too_many_lines)] // rows + browse / install / confirm chrome
pub fn render(f: &mut Frame, app: &AppView) {
    use dugix_core::app::SkillsMode;
    let area = chrome::popup_area(app, f.area(), "skills", chrome::STANDARD);

    let width = area.width.saturating_sub(5 + 2 * chrome::PAD) as usize;
    let rows = app.skills_rows();
    let filtering = !app.skills_filter.is_empty();
    let shown = list_filter::shown(&rows, app.skills_selected);
    let items: Vec<ListItem> = rows
        .iter()
        .map(|&index| &app.skills[index])
        .map(|s| {
            // App-managed skills are removable; an agent tool's are read-only.
            let source = if app.skill_is_app_managed(s) {
                "dugix".to_string()
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
        list_filter::count(rows.len(), app.skills.len(), filtering, "skill")
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
                hint(keys::ESC, if filtering { "clear" } else { "close" }),
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
    if app.skills_mode == SkillsMode::Browse {
        inner = chrome::render_field(
            f,
            inner,
            "⌕",
            &app.skills_filter,
            "Type to filter",
            false,
            &app.theme,
        );
    } else if app.skills_mode == SkillsMode::Install {
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
    let desc = shown
        .and_then(|_| app.skills.get(app.skills_selected))
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
    state.select(shown);
    chrome::render_list(f, list, &mut state, list_area, rows.len(), 1, app);
    chrome::render_detail(f, detail_area, &desc, &app.theme);
    if rows.is_empty() {
        let message = if filtering {
            format!(
                "No skill matches “{}”. Esc clears the filter.",
                app.skills_filter
            )
        } else {
            "No skills installed. ^N installs one.".to_string()
        };
        chrome::render_empty(f, list_area, &message, &app.theme);
    }
}

pub fn handle_key(app: &mut AppView, key: KeyEvent) {
    use super::{
        ConfirmDeleteAction, EditAction, classify_browse_key, classify_confirm_delete_key,
        classify_edit_key,
    };
    use dugix_core::app::SkillsMode;
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
        // `/<skill-name>`; letters filter, so every other action is a Ctrl key.
        SkillsMode::Browse if key.code == KeyCode::Enter => {
            if list_filter::shown(&app.skills_rows(), app.skills_selected).is_none() {
                return;
            }
            if let Some(name) = app.skills.get(app.skills_selected).map(|s| s.name.clone()) {
                app.popup = dugix_core::app::Popup::None;
                if let Err(e) =
                    app.execute(dugix_core::app::AppCommand::ArmSkill { name, rest: None })
                {
                    app.push_status(format!("error: {e:#}"));
                }
            }
        }
        SkillsMode::Browse => match classify_browse_key(key, true, false) {
            Some(super::BrowseAction::Close) if !app.skills_filter.is_empty() => {
                app.skills_filter.clear();
            }
            Some(super::BrowseAction::Close) => app.popup = dugix_core::app::Popup::None,
            Some(super::BrowseAction::Filter(c)) => {
                app.skills_filter.push(c);
                app.skills_selected = list_filter::first(&app.skills_rows(), app.skills_selected);
            }
            Some(super::BrowseAction::Backspace) => {
                app.skills_filter.pop();
                app.skills_selected = list_filter::first(&app.skills_rows(), app.skills_selected);
            }
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
            Some(super::BrowseAction::ConfirmDelete)
                if list_filter::shown(&app.skills_rows(), app.skills_selected).is_some() =>
            {
                app.start_skill_remove();
            }
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
