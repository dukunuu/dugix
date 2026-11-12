use super::App;

impl App {
    pub fn app_url(&self, name: &str) -> Option<String> {
        let s = self.app_server.as_ref()?;
        let uuid = s.registry().resolve(&self.active_space.name, name)?;
        Some(s.app_url(&uuid))
    }

    /// Count app files recursively, excluding dependencies.
    pub fn app_file_count(&self, name: &str) -> usize {
        fn count(dir: &std::path::Path) -> usize {
            let Ok(rd) = std::fs::read_dir(dir) else {
                return 0;
            };
            rd.flatten()
                .map(|e| {
                    let p = e.path();
                    if p.is_dir() {
                        if e.file_name() == "node_modules" {
                            0
                        } else {
                            count(&p)
                        }
                    } else {
                        1
                    }
                })
                .sum()
        }
        count(&self.space.apps_dir(&self.active_space.name).join(name))
    }
}
