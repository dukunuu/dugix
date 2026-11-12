use super::App;
use tokio::sync::mpsc;

impl App {
    pub fn reload_skills(&mut self) {
        let skills =
            crate::skills::load_skills_from_dirs(&crate::skills::app_skill_roots(&self.space.root));
        if skills != self.skills {
            self.skills = skills;
            self.bump_cache_epoch();
        }
    }

    /// External Agent Skills roots cannot be modified by the remove action.
    pub fn skill_is_app_managed(&self, skill: &crate::skills::Skill) -> bool {
        crate::skills::is_app_managed(skill, &self.space.root)
    }

    pub fn start_skill_install(&mut self, spec: &str) {
        let spec = spec.trim().to_string();
        let Some((owner, repo, path)) = crate::skills::parse_gh_shorthand(&spec) else {
            self.push_status(format!("expected owner/repo/path, got: {spec}"));
            return;
        };
        let dest = crate::skills::skills_dir(&self.space.root);
        let (tx, rx) = mpsc::unbounded_channel();
        self.skills_rx = Some(rx);
        self.push_status(format!("installing {spec}…"));
        tokio::spawn(async move {
            let client = reqwest::Client::new();
            let result = crate::skills::install_from_github(&client, &owner, &repo, &path, &dest)
                .await
                .map_err(|e| e.to_string());
            let _ = tx.send(result);
        });
    }

    pub fn on_skill_install_result(&mut self, result: Option<Result<String, String>>) {
        self.skills_rx = None;
        match result {
            Some(Ok(name)) => {
                self.reload_skills();
                self.push_status(format!("installed skill: {name}"));
            }
            Some(Err(e)) => self.push_status(format!("skill install failed: {e}")),
            None => {}
        }
    }
}
