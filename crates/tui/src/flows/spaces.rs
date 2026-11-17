use anyhow::Result;

use dugix_core::app::{Popup, SpaceMode};

use crate::app_view::AppView;

impl AppView {
    pub fn open_space_picker(&mut self) -> Result<()> {
        self.spaces_cache = self.core.db.list_spaces()?;
        self.space_selected = self
            .spaces_cache
            .iter()
            .position(|s| s.id == self.core.active_space.id)
            .unwrap_or(0);
        self.space_filter.clear();
        self.space_mode = SpaceMode::Browse;
        self.popup = Popup::Space;
        Ok(())
    }

    pub fn filtered_spaces(&self) -> Vec<&dugix_core::db::Space> {
        let needle = self.space_filter.trim();
        if needle.is_empty() {
            return self.spaces_cache.iter().collect();
        }
        dugix_core::app::fuzzy_filter_sorted(&self.spaces_cache, |s| {
            dugix_core::app::fuzzy_score(&s.name, needle)
        })
    }

    pub fn selected_space(&self) -> Option<dugix_core::db::Space> {
        self.filtered_spaces()
            .get(self.space_selected)
            .map(|s| (*s).clone())
    }

    pub fn move_space_selection(&mut self, delta: i32) {
        self.space_selected =
            dugix_core::app::clamp_cursor(self.space_selected, self.filtered_spaces().len(), delta);
    }

    pub fn space_filter_push(&mut self, c: char) {
        self.space_filter.insert_char(c);
        self.space_selected = 0;
    }

    pub fn space_filter_pop(&mut self) {
        self.space_filter.backspace();
        self.space_selected = 0;
    }

    pub fn start_space_create(&mut self) {
        self.space_edit.clear();
        self.space_mode = SpaceMode::Create;
    }

    pub fn start_space_rename(&mut self) {
        if let Some(s) = self
            .selected_space()
            .filter(|s| s.name != dugix_core::db::DEFAULT_SPACE)
        {
            self.space_edit = s.name;
            self.space_mode = SpaceMode::Rename;
        }
    }

    pub fn confirm_space_create(&mut self) -> Result<()> {
        let name = self.space_edit.trim().to_string();
        if !name.is_empty() && !self.spaces_cache.iter().any(|s| s.name == name) {
            let s = self.core.db.create_space(&name)?;
            self.core.space.ensure_space_dir(&name)?;
            self.spaces_cache.push(s);
            self.space_selected = self.spaces_cache.len() - 1;
        }
        self.space_mode = SpaceMode::Browse;
        Ok(())
    }

    pub fn confirm_space_rename(&mut self) -> Result<()> {
        let name = self.space_edit.trim().to_string();
        if let (false, Some(s)) = (name.is_empty(), self.selected_space()) {
            self.core.db.rename_space(&s.id, &name)?;
            self.core.space.rename_space_dir(&s.name, &name)?;
            if let Some(reg) = self
                .core
                .app_server
                .as_ref()
                .map(dugix_core::appserver::AppServer::registry)
            {
                reg.rename_space(&s.name, &name);
            }
            if let Some(cached) = self.spaces_cache.iter_mut().find(|c| c.id == s.id) {
                cached.name.clone_from(&name);
            }
            if self.core.active_space.id == s.id {
                self.core.active_space.name = name;
                self.core.refresh_toolbox();
            }
        }
        self.space_mode = SpaceMode::Browse;
        Ok(())
    }

    pub fn confirm_space_delete(&mut self) -> Result<()> {
        if let Some(s) = self
            .selected_space()
            .filter(|s| s.name != dugix_core::db::DEFAULT_SPACE)
        {
            self.core.db.delete_space(&s.id)?;
            self.core.space.remove_space_dir(&s.name)?;
            self.spaces_cache.retain(|c| c.id != s.id);
            if self.core.active_space.id == s.id {
                self.core.switch_to_default_space()?;
            }
            self.push_status(format!("deleted space: {}", s.name));
        }
        self.space_mode = SpaceMode::Browse;
        let len = self.filtered_spaces().len();
        self.space_selected = self.space_selected.min(len.saturating_sub(1));
        Ok(())
    }

    pub fn confirm_space(&mut self) {
        if let Some(s) = self.selected_space() {
            self.core.set_active_space(s);
        }
        self.popup = Popup::None;
    }

    pub fn space_instructions_target(&self) -> Option<std::path::PathBuf> {
        self.selected_space()
            .and_then(|s| self.core.instructions_path_for_space(&s.name))
    }

    pub fn selected_space_name(&self) -> Option<String> {
        self.selected_space().map(|s| s.name.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dugix_core::app::App;
    use dugix_core::app::AppCommand;
    use dugix_core::db::Db;
    use dugix_core::space::Space;

    fn test_space() -> Space {
        Space {
            root: std::env::temp_dir().join(format!("dugix-flow-test-{}", uuid::Uuid::new_v4())),
        }
    }

    fn test_app() -> AppView {
        AppView::new(App::new(
            Db::open_in_memory().unwrap(),
            Some("sk-or-test-key"),
            test_space(),
        ))
    }

    #[tokio::test]
    async fn switching_space_clears_open_conversation() {
        let mut a = test_app();
        a.core.current_model = Some("a/one".into());
        a.core
            .execute(AppCommand::Send {
                text: "hello".to_string(),
            })
            .unwrap();
        assert!(a.core.session.is_some());

        let other = a.core.db.create_space("other").unwrap();
        a.spaces_cache = vec![other.clone()];
        a.space_selected = 0;
        a.confirm_space();

        assert_eq!(a.core.active_space.id, other.id);
        assert!(a.core.session.is_none());
        assert!(a.core.messages.is_empty());
    }

    #[test]
    fn space_crud_via_app_methods() {
        let mut a = test_app();
        a.spaces_cache = a.core.db.list_spaces().unwrap();
        a.space_edit = "work".into();
        a.confirm_space_create().unwrap();
        assert!(a.spaces_cache.iter().any(|s| s.name == "work"));

        a.space_selected = a
            .spaces_cache
            .iter()
            .position(|s| s.name == "work")
            .unwrap();
        a.space_edit = "work-2".into();
        a.confirm_space_rename().unwrap();
        assert!(a.spaces_cache.iter().any(|s| s.name == "work-2"));

        a.confirm_space_delete().unwrap(); // "work-2" still selected
        assert!(!a.spaces_cache.iter().any(|s| s.name == "work-2"));
        assert_eq!(a.core.active_space.name, dugix_core::db::DEFAULT_SPACE); // untouched, wasn't active
    }
}
