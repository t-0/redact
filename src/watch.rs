//! Filesystem watcher: feeds changed paths to the app so panes and buffers
//! follow what happens outside the editor (git, builds, other editors).

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};

use notify::{RecursiveMode, Watcher, recommended_watcher};

pub struct FsWatcher {
    _watcher: Box<dyn Watcher + Send>,
    pub rx: Receiver<PathBuf>,
}

/// Directories whose churn we never care about (build output, caches).
fn noisy(path: &Path) -> bool {
    path.components().any(|c| {
        matches!(
            c.as_os_str().to_str().unwrap_or(""),
            "target"
                | "node_modules"
                | ".venv"
                | "venv"
                | "__pycache__"
                | ".pytest_cache"
                | ".mypy_cache"
                | ".ruff_cache"
        )
    })
}

impl FsWatcher {
    pub fn new(root: &Path) -> anyhow::Result<FsWatcher> {
        let (tx, rx) = channel();
        let mut watcher = recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(ev) = res {
                // Reads produce Access events too (inotify IN_OPEN / IN_CLOSE_NOWRITE).
                // Our own cargo check, test discovery and git status read the whole
                // tree, so forwarding those would retrigger them forever.
                if ev.kind.is_access() {
                    return;
                }
                for p in ev.paths {
                    if !noisy(&p) {
                        let _ = tx.send(p);
                    }
                }
            }
        })?;
        watcher.watch(root, RecursiveMode::Recursive)?;
        Ok(FsWatcher {
            _watcher: Box::new(watcher),
            rx,
        })
    }
}
