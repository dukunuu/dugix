//! Skill picker interactions and editor handoffs.

use dugix_core::app::{Popup, SkillsMode};

use crate::app_view::AppView;

impl AppView {
    pub fn open_skills_popup(&mut self) {
        self.skills_mode = SkillsMode::Browse;
        self.skills_filter.clear();
        self.skills_selected = self
            .skills_selected
            .min(self.core.skills.len().saturating_sub(1));
        self.popup = Popup::Skills;
    }

    pub fn reload_skills(&mut self) {
        self.core.reload_skills();
        let len = self.core.skills.len();
        self.skills_selected = self.skills_selected.min(len.saturating_sub(1));
    }

    /// Indices of the skills the typed filter keeps.
    pub fn skills_rows(&self) -> Vec<usize> {
        crate::ui::popups::list_filter::matching(
            &self.skills_filter,
            self.core.skills.iter().map(|skill| skill.name.as_str()),
        )
    }

    pub fn move_skills_selection(&mut self, delta: i32) {
        self.skills_selected =
            crate::ui::popups::list_filter::step(&self.skills_rows(), self.skills_selected, delta);
    }

    pub fn start_skill_install(&mut self) {
        self.skills_edit.clear();
        self.skills_mode = SkillsMode::Install;
    }

    pub fn start_skill_remove(&mut self) {
        let Some(skill) = self.core.skills.get(self.skills_selected) else {
            return;
        };
        let name = skill.name.clone();
        let managed = self.core.skill_is_app_managed(skill);
        if managed {
            self.skills_mode = SkillsMode::ConfirmRemove;
        } else {
            self.push_status(format!(
                "cannot remove global skill '{name}' here — edit it with Ctrl+E"
            ));
        }
    }

    pub fn confirm_skill_install(&mut self) {
        let spec = self.skills_edit.clone();
        self.skills_mode = SkillsMode::Browse;
        self.core.start_skill_install(&spec);
    }

    pub fn confirm_skill_remove(&mut self) {
        let selected = self.core.skills.get(self.skills_selected).map(|skill| {
            (
                skill.name.clone(),
                skill.dir.clone(),
                self.core.skill_is_app_managed(skill),
            )
        });
        if let Some((name, dir, managed)) = selected {
            if managed {
                let _ = std::fs::remove_dir_all(dir);
                self.reload_skills();
                self.push_status(format!("removed skill: {name}"));
            } else {
                self.push_status(format!("cannot remove global skill: {name}"));
            }
        }
        self.skills_mode = SkillsMode::Browse;
    }

    pub fn skill_edit_path_for_selected(&self) -> Option<std::path::PathBuf> {
        self.core
            .skills
            .get(self.skills_selected)
            .map(|s| s.dir.join("SKILL.md"))
    }
}
