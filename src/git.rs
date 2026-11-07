//! Thin wrappers around libgit2 for status, branch and file contents at HEAD.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use git2::{Repository, Status, StatusOptions};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileStatus {
    Normal,
    Added,
    Modified,
    Deleted,
    Untracked,
    Conflicted,
}

impl FileStatus {
    pub fn letter(self) -> char {
        match self {
            FileStatus::Normal => '-',
            FileStatus::Added => 'A',
            FileStatus::Modified => 'M',
            FileStatus::Deleted => 'D',
            FileStatus::Untracked => 'U',
            FileStatus::Conflicted => 'C',
        }
    }

    pub fn color(self) -> ratatui::style::Color {
        use crate::theme;
        match self {
            FileStatus::Normal => theme::git_normal(),
            FileStatus::Added => theme::git_added(),
            FileStatus::Modified => theme::git_modified(),
            FileStatus::Deleted => theme::git_deleted(),
            FileStatus::Untracked => theme::git_untracked(),
            FileStatus::Conflicted => theme::git_deleted(),
        }
    }

    /// Priority used when rolling file states up to a directory.
    fn rank(self) -> u8 {
        match self {
            FileStatus::Normal => 0,
            FileStatus::Untracked => 1,
            FileStatus::Added => 2,
            FileStatus::Modified => 3,
            FileStatus::Deleted => 4,
            FileStatus::Conflicted => 5,
        }
    }
}

fn classify(s: Status) -> FileStatus {
    if s.is_conflicted() {
        FileStatus::Conflicted
    } else if s.is_wt_new() {
        FileStatus::Untracked
    } else if s.is_index_new() {
        FileStatus::Added
    } else if s.is_wt_deleted() || s.is_index_deleted() {
        FileStatus::Deleted
    } else if s.is_wt_modified()
        || s.is_index_modified()
        || s.is_wt_renamed()
        || s.is_index_renamed()
        || s.is_wt_typechange()
        || s.is_index_typechange()
    {
        FileStatus::Modified
    } else {
        FileStatus::Normal
    }
}

/// Snapshot of the repository state used by the explorer and source control pane.
#[derive(Default)]
pub struct GitState {
    pub branch: Option<String>,
    /// Absolute path -> status for every non-clean file.
    pub files: HashMap<PathBuf, FileStatus>,
    /// Absolute directory path -> rolled-up status.
    pub dirs: HashMap<PathBuf, FileStatus>,
    /// Repository working directory, if any.
    pub workdir: Option<PathBuf>,
    repo: Option<Repository>,
}

impl GitState {
    pub fn load(root: &Path) -> GitState {
        let repo = match Repository::discover(root) {
            Ok(r) => r,
            Err(_) => return GitState::default(),
        };
        let workdir = repo.workdir().map(|p| p.to_path_buf());
        let branch = match repo.head() {
            Ok(h) if h.is_branch() => h.shorthand().map(|s| s.to_string()),
            Ok(h) => h.target().map(|oid| format!("{:.7}", oid)),
            // unborn branch (no commits yet): HEAD is still a symbolic ref to it
            Err(_) => repo.find_reference("HEAD").ok().and_then(|r| {
                r.symbolic_target()
                    .map(|t| t.trim_start_matches("refs/heads/").to_string())
            }),
        };

        let mut opts = StatusOptions::new();
        opts.include_untracked(true)
            .recurse_untracked_dirs(true)
            .include_ignored(false)
            .exclude_submodules(true);
        let mut files = HashMap::new();
        let mut dirs: HashMap<PathBuf, FileStatus> = HashMap::new();
        if let (Ok(statuses), Some(wd)) = (repo.statuses(Some(&mut opts)), &workdir) {
            for entry in statuses.iter() {
                let Some(rel) = entry.path() else { continue };
                let st = classify(entry.status());
                if st == FileStatus::Normal {
                    continue;
                }
                let abs = wd.join(rel);
                // roll up to every parent directory inside the workdir
                let mut parent = abs.parent();
                while let Some(p) = parent {
                    if !p.starts_with(wd) {
                        break;
                    }
                    let e = dirs.entry(p.to_path_buf()).or_insert(FileStatus::Normal);
                    if st.rank() > e.rank() {
                        *e = st;
                    }
                    if p == wd {
                        break;
                    }
                    parent = p.parent();
                }
                files.insert(abs, st);
            }
        }
        GitState {
            branch,
            files,
            dirs,
            workdir,
            repo: Some(repo),
        }
    }

    pub fn is_ignored(&self, path: &Path) -> bool {
        self.repo
            .as_ref()
            .map(|r| r.status_should_ignore(path).unwrap_or(false))
            .unwrap_or(false)
    }

    pub fn status_of(&self, path: &Path, is_dir: bool) -> FileStatus {
        if is_dir {
            // A directory that is itself untracked: all contents are untracked.
            self.dirs.get(path).copied().unwrap_or(FileStatus::Normal)
        } else {
            self.files.get(path).copied().unwrap_or(FileStatus::Normal)
        }
    }

    /// Changed files relative to the workdir, sorted.
    pub fn changes(&self) -> Vec<(PathBuf, FileStatus)> {
        let mut v: Vec<(PathBuf, FileStatus)> =
            self.files.iter().map(|(p, s)| (p.clone(), *s)).collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    }
}

/// Contents of `path` as of HEAD, or None if it is not tracked there.
pub fn head_contents(root: &Path, path: &Path) -> Option<String> {
    let repo = Repository::discover(root).ok()?;
    let wd = repo.workdir()?;
    let rel = path.strip_prefix(wd).ok()?;
    let head = repo.head().ok()?.peel_to_tree().ok()?;
    let entry = head.get_path(rel).ok()?;
    let blob = repo.find_blob(entry.id()).ok()?;
    Some(String::from_utf8_lossy(blob.content()).into_owned())
}
