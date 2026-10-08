//! FastOpen: default applications per MIME type, and launching them.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use noxfm_core::apps::{AppIndex, exec_argv, on_path, terminal_prefix};
use noxfm_proto::{AppRef, Entry};

/// Installed apps and mimeapps.list are re-read at most this often.
const RELOAD_AFTER: Duration = Duration::from_secs(30);

#[derive(Default)]
pub struct Apps {
    index: Mutex<Option<(Arc<AppIndex>, Instant)>>,
}

impl Apps {
    fn index(&self) -> Arc<AppIndex> {
        let mut g = self.index.lock().unwrap();
        match &*g {
            Some((idx, at)) if at.elapsed() < RELOAD_AFTER => idx.clone(),
            _ => {
                let idx = Arc::new(AppIndex::load());
                *g = Some((idx.clone(), Instant::now()));
                idx
            }
        }
    }

    pub fn apps_for(&self, mime: &str) -> Vec<AppRef> {
        self.index().apps_for(mime).into_iter().map(app_ref).collect()
    }

    pub fn all(&self) -> Vec<AppRef> {
        self.index().all().into_iter().filter(|a| !a.exec.is_empty()).map(app_ref).collect()
    }

    /// Makes `id` the default for `mime` in the user's mimeapps.list.
    pub fn set_default(&self, mime: &str, id: &str) -> anyhow::Result<()> {
        anyhow::ensure!(self.index().get(id).is_some(), "no application {id}");
        let path = noxfm_core::apps::user_mimeapps_list();
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&path, noxfm_core::apps::set_default(&text, mime, id))?;
        // Re-read on next use.
        *self.index.lock().unwrap() = None;
        Ok(())
    }

    /// Blocking (may scan the application directories).
    pub fn annotate(&self, entries: &mut [Entry]) {
        let idx = self.index();
        for e in entries {
            e.app = e.mime.as_deref().and_then(|m| idx.default_for(m)).map(app_ref);
        }
    }

    /// Opens `file` with `app_id`, or its default app. The app runs in its own
    /// process group so it outlives noxd.
    pub fn open(&self, file: &Path, mime: &str, app_id: Option<&str>) -> anyhow::Result<String> {
        let idx = self.index();
        let app = match app_id {
            Some(id) => idx.get(id).ok_or_else(|| anyhow::anyhow!("no application {id}"))?,
            None => idx.default_for(mime).ok_or_else(|| anyhow::anyhow!("no application opens {mime}"))?,
        };
        let mut argv = exec_argv(app, file).ok_or_else(|| anyhow::anyhow!("bad Exec line in {}", app.id))?;
        if app.terminal {
            let env = std::env::var("TERMINAL").ok();
            let mut prefix = terminal_prefix(on_path, env.as_deref())
                .ok_or_else(|| anyhow::anyhow!("{} needs a terminal, and none was found", app.name))?;
            prefix.append(&mut argv);
            argv = prefix;
        }
        let mut cmd = tokio::process::Command::new(&argv[0]);
        cmd.args(&argv[1..])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .process_group(0);
        if let Some(dir) = file.parent() {
            cmd.current_dir(dir);
        }
        let mut child = cmd.spawn().map_err(|e| anyhow::anyhow!("starting {}: {e}", app.name))?;
        // Reap it whenever it exits.
        tokio::spawn(async move {
            let _ = child.wait().await;
        });
        tracing::info!(app = %app.id, file = %file.display(), "opened");
        Ok(app.name.clone())
    }
}

fn app_ref(a: &noxfm_core::apps::App) -> AppRef {
    AppRef { id: a.id.clone(), name: a.name.clone(), icon: a.icon.clone() }
}
