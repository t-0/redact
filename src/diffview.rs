//! Side-by-side (left = old, right = new) diff of a file against HEAD.

use std::path::PathBuf;

use similar::{ChangeTag, TextDiff};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RowKind {
    Equal,
    Insert,
    Delete,
    Replace,
}

#[derive(Clone, Debug)]
pub struct DiffRow {
    pub left_no: Option<usize>,
    pub left: String,
    pub right_no: Option<usize>,
    pub right: String,
    pub kind: RowKind,
}

pub struct DiffTab {
    pub path: PathBuf,
    pub title: String,
    pub rows: Vec<DiffRow>,
    pub scroll: usize,
    pub selected: usize,
    pub hscroll: usize,
    pub stats: (usize, usize),
}

impl DiffTab {
    pub fn new(path: PathBuf, old: &str, new: &str) -> DiffTab {
        let diff = TextDiff::from_lines(old, new);
        let mut rows: Vec<DiffRow> = Vec::new();
        let mut adds = 0;
        let mut dels = 0;
        for group in diff.grouped_ops(3) {
            for op in group {
                let mut pending_del: Vec<DiffRow> = Vec::new();
                for change in diff.iter_changes(&op) {
                    let text = change.value().trim_end_matches('\n').to_string();
                    match change.tag() {
                        ChangeTag::Equal => {
                            rows.append(&mut pending_del);
                            rows.push(DiffRow {
                                left_no: change.old_index().map(|i| i + 1),
                                left: text.clone(),
                                right_no: change.new_index().map(|i| i + 1),
                                right: text,
                                kind: RowKind::Equal,
                            });
                        }
                        ChangeTag::Delete => {
                            dels += 1;
                            pending_del.push(DiffRow {
                                left_no: change.old_index().map(|i| i + 1),
                                left: text,
                                right_no: None,
                                right: String::new(),
                                kind: RowKind::Delete,
                            });
                        }
                        ChangeTag::Insert => {
                            adds += 1;
                            // pair with a pending deletion to make a "replace" row
                            if let Some(mut d) = pending_del.first().cloned() {
                                pending_del.remove(0);
                                d.right_no = change.new_index().map(|i| i + 1);
                                d.right = text;
                                d.kind = RowKind::Replace;
                                rows.push(d);
                            } else {
                                rows.push(DiffRow {
                                    left_no: None,
                                    left: String::new(),
                                    right_no: change.new_index().map(|i| i + 1),
                                    right: text,
                                    kind: RowKind::Insert,
                                });
                            }
                        }
                    }
                }
                rows.append(&mut pending_del);
            }
            // separator between hunks
            rows.push(DiffRow {
                left_no: None,
                left: "···".into(),
                right_no: None,
                right: "···".into(),
                kind: RowKind::Equal,
            });
        }
        if rows.last().map(|r| r.left == "···").unwrap_or(false) {
            rows.pop();
        }
        let title = format!(
            "diff: {}",
            path.file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default()
        );
        DiffTab {
            path,
            title,
            rows,
            scroll: 0,
            selected: 0,
            hscroll: 0,
            stats: (adds, dels),
        }
    }

    /// Recompute the diff from new contents, keeping the selection in range.
    pub fn refresh(&mut self, old: &str, new: &str) {
        let fresh = DiffTab::new(self.path.clone(), old, new);
        self.rows = fresh.rows;
        self.stats = fresh.stats;
        self.selected = self.selected.min(self.rows.len().saturating_sub(1));
        self.scroll = self.scroll.min(self.rows.len().saturating_sub(1));
    }

    pub fn move_by(&mut self, delta: isize) {
        if self.rows.is_empty() {
            return;
        }
        self.selected =
            (self.selected as isize + delta).clamp(0, self.rows.len() as isize - 1) as usize;
    }

    /// Jump to the next/previous changed row.
    pub fn next_change(&mut self, forward: bool) {
        let n = self.rows.len();
        if n == 0 {
            return;
        }
        let mut i = self.selected;
        // skip the current hunk
        while i < n && self.rows[i].kind != RowKind::Equal {
            if forward {
                i += 1;
            } else if i == 0 {
                return;
            } else {
                i -= 1;
            }
        }
        loop {
            if forward {
                i += 1;
                if i >= n {
                    return;
                }
            } else {
                if i == 0 {
                    return;
                }
                i -= 1;
            }
            if self.rows[i].kind != RowKind::Equal {
                // go to the start of this hunk when moving backwards
                if !forward {
                    while i > 0 && self.rows[i - 1].kind != RowKind::Equal {
                        i -= 1;
                    }
                }
                self.selected = i;
                return;
            }
        }
    }

    pub fn scroll_to_selected(&mut self, rows: usize) {
        if rows == 0 {
            return;
        }
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + rows {
            self.scroll = self.selected + 1 - rows;
        }
    }
}
