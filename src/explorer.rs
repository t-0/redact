//! Left pane mode 1: a lazily loaded file tree with git status decorations.

use std::fs;
use std::path::{Path, PathBuf};

use crate::git::{FileStatus, GitState};

#[derive(Debug)]
pub struct Entry {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    pub expanded: bool,
    pub children: Option<Vec<Entry>>,
    /// True for files reported deleted by git that no longer exist on disk.
    pub phantom: bool,
    pub ignored: bool,
}

impl Entry {
    fn new(path: PathBuf, is_dir: bool) -> Entry {
        let name = path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned());
        Entry {
            path,
            name,
            is_dir,
            expanded: false,
            children: None,
            phantom: false,
            ignored: false,
        }
    }
}

/// One visible row in the flattened tree.
pub struct Row {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    pub expanded: bool,
    pub depth: usize,
    /// For each depth level, whether a vertical guide continues below this row.
    pub guides: Vec<bool>,
    pub last: bool,
    pub status: FileStatus,
    pub phantom: bool,
    pub ignored: bool,
}

pub struct Explorer {
    pub root: PathBuf,
    entries: Vec<Entry>,
    pub selected: usize,
    pub scroll: usize,
    rows: Vec<Row>,
}

fn read_dir(path: &Path, git: &GitState) -> Vec<Entry> {
    let mut out: Vec<Entry> = Vec::new();
    if let Ok(rd) = fs::read_dir(path) {
        for e in rd.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            if name == ".git" {
                continue;
            }
            let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false)
                || (p.is_dir() && fs::metadata(&p).map(|m| m.is_dir()).unwrap_or(false));
            let mut entry = Entry::new(p, is_dir);
            entry.ignored = git.is_ignored(&entry.path);
            out.push(entry);
        }
    }
    // Phantom entries for files deleted in the working tree but still known to git.
    for (p, st) in git.files.iter() {
        if *st == FileStatus::Deleted
            && p.parent() == Some(path)
            && !out.iter().any(|e| &e.path == p)
        {
            let mut e = Entry::new(p.clone(), false);
            e.phantom = true;
            out.push(e);
        }
    }
    out.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    out
}

impl Explorer {
    pub fn new(root: PathBuf, git: &GitState) -> Explorer {
        let mut ex = Explorer {
            root,
            entries: Vec::new(),
            selected: 0,
            scroll: 0,
            rows: Vec::new(),
        };
        ex.entries = read_dir(&ex.root, git);
        ex.rebuild(git);
        ex
    }

    /// Reload the tree from disk while keeping expansion state and selection.
    pub fn refresh(&mut self, git: &GitState) {
        let expanded: Vec<PathBuf> = self
            .rows
            .iter()
            .filter(|r| r.is_dir && r.expanded)
            .map(|r| r.path.clone())
            .collect();
        let selected_path = self.rows.get(self.selected).map(|r| r.path.clone());
        self.entries = read_dir(&self.root, git);
        for p in &expanded {
            self.expand_path(p, git);
        }
        self.rebuild(git);
        if let Some(sp) = selected_path
            && let Some(i) = self.rows.iter().position(|r| r.path == sp)
        {
            self.selected = i;
        }
        self.selected = self.selected.min(self.rows.len().saturating_sub(1));
    }

    fn expand_path(&mut self, target: &Path, git: &GitState) {
        fn walk(entries: &mut Vec<Entry>, target: &Path, git: &GitState) {
            for e in entries.iter_mut() {
                if e.is_dir && target.starts_with(&e.path) {
                    if e.children.is_none() {
                        e.children = Some(read_dir(&e.path, git));
                    }
                    if e.path == target {
                        e.expanded = true;
                    } else if let Some(ch) = e.children.as_mut() {
                        walk(ch, target, git);
                    }
                }
            }
        }
        walk(&mut self.entries, target, git);
    }

    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    pub fn rebuild(&mut self, git: &GitState) {
        fn walk(
            entries: &[Entry],
            depth: usize,
            guides: &mut Vec<bool>,
            git: &GitState,
            out: &mut Vec<Row>,
        ) {
            let n = entries.len();
            for (i, e) in entries.iter().enumerate() {
                let last = i + 1 == n;
                out.push(Row {
                    path: e.path.clone(),
                    name: e.name.clone(),
                    is_dir: e.is_dir,
                    expanded: e.expanded,
                    depth,
                    guides: guides.clone(),
                    last,
                    status: if e.phantom {
                        FileStatus::Deleted
                    } else {
                        git.status_of(&e.path, e.is_dir)
                    },
                    phantom: e.phantom,
                    ignored: e.ignored,
                });
                if e.is_dir
                    && e.expanded
                    && let Some(ch) = &e.children
                {
                    guides.push(!last);
                    walk(ch, depth + 1, guides, git, out);
                    guides.pop();
                }
            }
        }
        let mut rows = Vec::new();
        let mut guides = Vec::new();
        walk(&self.entries, 0, &mut guides, git, &mut rows);
        self.rows = rows;
        if self.selected >= self.rows.len() {
            self.selected = self.rows.len().saturating_sub(1);
        }
    }

    fn entry_mut(&mut self, path: &Path) -> Option<&mut Entry> {
        fn walk<'a>(entries: &'a mut Vec<Entry>, path: &Path) -> Option<&'a mut Entry> {
            for e in entries.iter_mut() {
                if e.path == path {
                    return Some(e);
                }
                if e.is_dir
                    && path.starts_with(&e.path)
                    && let Some(ch) = e.children.as_mut()
                    && let Some(found) = walk(ch, path)
                {
                    return Some(found);
                }
            }
            None
        }
        walk(&mut self.entries, path)
    }

    pub fn selected_row(&self) -> Option<&Row> {
        self.rows.get(self.selected)
    }

    pub fn move_by(&mut self, delta: isize) {
        if self.rows.is_empty() {
            return;
        }
        let n = self.rows.len() as isize;
        self.selected = (self.selected as isize + delta).clamp(0, n - 1) as usize;
    }

    pub fn home(&mut self) {
        self.selected = 0;
    }

    pub fn end(&mut self) {
        self.selected = self.rows.len().saturating_sub(1);
    }

    /// Toggle expansion of the selected directory.
    pub fn toggle(&mut self, git: &GitState) {
        let Some(row) = self.rows.get(self.selected) else {
            return;
        };
        if !row.is_dir {
            return;
        }
        let path = row.path.clone();
        if let Some(e) = self.entry_mut(&path) {
            if e.children.is_none() {
                e.children = Some(read_dir(&e.path, git));
            }
            e.expanded = !e.expanded;
        }
        self.rebuild(git);
    }

    pub fn expand(&mut self, git: &GitState) {
        let Some(row) = self.rows.get(self.selected) else {
            return;
        };
        if row.is_dir && !row.expanded {
            self.toggle(git);
        }
    }

    /// Collapse the selected directory, or jump to the parent if already collapsed/file.
    pub fn collapse_or_parent(&mut self, git: &GitState) {
        let Some(row) = self.rows.get(self.selected) else {
            return;
        };
        if row.is_dir && row.expanded {
            self.toggle(git);
            return;
        }
        let depth = row.depth;
        if depth == 0 {
            return;
        }
        let mut i = self.selected;
        while i > 0 {
            i -= 1;
            if self.rows[i].depth < depth {
                self.selected = i;
                break;
            }
        }
    }

    /// Expand every directory (loading as needed) or collapse all, depending on
    /// whether anything is currently expanded.
    pub fn toggle_all(&mut self, git: &GitState) {
        let any_expanded = self.rows.iter().any(|r| r.is_dir && r.expanded);
        fn set_all(entries: &mut Vec<Entry>, expand: bool, git: &GitState) {
            for e in entries.iter_mut() {
                if !e.is_dir {
                    continue;
                }
                if expand {
                    if e.children.is_none() {
                        // Avoid descending into huge build directories.
                        if e.name == "target"
                            || e.name == "node_modules"
                            || e.name == ".venv"
                            || e.name == "__pycache__"
                        {
                            continue;
                        }
                        e.children = Some(read_dir(&e.path, git));
                    }
                    e.expanded = true;
                    if let Some(ch) = e.children.as_mut() {
                        set_all(ch, true, git);
                    }
                } else {
                    e.expanded = false;
                    if let Some(ch) = e.children.as_mut() {
                        set_all(ch, false, git);
                    }
                }
            }
        }
        set_all(&mut self.entries, !any_expanded, git);
        self.rebuild(git);
    }

    /// Reveal `path` in the tree (expanding parents) and select it.
    pub fn reveal(&mut self, path: &Path, git: &GitState) {
        if let Some(parent) = path.parent() {
            let mut cur = parent.to_path_buf();
            let mut chain = vec![];
            while cur.starts_with(&self.root) && cur != self.root {
                chain.push(cur.clone());
                match cur.parent() {
                    Some(p) => cur = p.to_path_buf(),
                    None => break,
                }
            }
            for p in chain.iter().rev() {
                self.expand_path(p, git);
            }
        }
        self.rebuild(git);
        if let Some(i) = self.rows.iter().position(|r| r.path == path) {
            self.selected = i;
        }
    }
}
