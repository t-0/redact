//! Text buffer with multi-cursor editing, selections, undo/redo and find.

pub mod highlight;

use std::path::{Path, PathBuf};

use highlight::{Lang, Run, Syntax};
use std::time::Instant;

use crate::diagnostics::{Diagnostic, Severity};
use tree_sitter::{InputEdit, Point};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Pos {
    pub line: usize,
    pub col: usize,
}

impl Pos {
    pub fn new(line: usize, col: usize) -> Pos {
        Pos { line, col }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Cursor {
    pub pos: Pos,
    /// Selection anchor; the selection is between `anchor` and `pos`.
    pub anchor: Option<Pos>,
    /// Remembered column for vertical movement.
    pub sticky: Option<usize>,
}

impl Cursor {
    pub fn at(pos: Pos) -> Cursor {
        Cursor {
            pos,
            anchor: None,
            sticky: None,
        }
    }

    pub fn selection(&self) -> Option<(Pos, Pos)> {
        let a = self.anchor?;
        if a == self.pos {
            return None;
        }
        Some((a.min(self.pos), a.max(self.pos)))
    }
}

/// One primitive edit; the undo log stores these instead of buffer copies so
/// memory stays proportional to what changed, not to the file size.
#[derive(Clone, Debug)]
enum Op {
    Insert { at: Pos, text: String },
    Delete { at: Pos, text: String },
}

#[derive(Clone, Debug)]
struct Transaction {
    ops: Vec<Op>,
    cursors_before: Vec<Cursor>,
    cursors_after: Vec<Cursor>,
    kind: EditKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EditKind {
    None,
    Typing,
    Deleting,
}

pub struct Editor {
    pub path: Option<PathBuf>,
    pub lines: Vec<String>,
    pub cursors: Vec<Cursor>,
    pub scroll_line: usize,
    pub scroll_col: usize,
    pub modified: bool,
    /// Modification time of the file when it was last read or written.
    pub disk_mtime: Option<std::time::SystemTime>,
    /// The file changed on disk while this buffer had unsaved edits.
    pub conflict: bool,
    pub lang: Lang,
    undo: Vec<Transaction>,
    redo: Vec<Transaction>,
    /// True while the top of `undo` may still receive coalesced edits.
    open_txn: bool,
    recording: bool,
    syntax: Option<Syntax>,
    pub search: Option<String>,
    /// When the buffer last changed; syntax errors are shown once typing pauses.
    pub last_edit: Instant,
    /// Bumped on every change; lets views cache derived renderings.
    pub revision: u64,
}

pub const TAB_WIDTH: usize = 4;

fn mtime(path: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

fn char_len(s: &str) -> usize {
    s.chars().count()
}

fn byte_idx(s: &str, col: usize) -> usize {
    s.char_indices().nth(col).map(|(i, _)| i).unwrap_or(s.len())
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

impl Editor {
    pub fn new() -> Editor {
        Editor {
            path: None,
            lines: vec![String::new()],
            cursors: vec![Cursor::default()],
            scroll_line: 0,
            scroll_col: 0,
            modified: false,
            disk_mtime: None,
            conflict: false,
            lang: Lang::Plain,
            undo: Vec::new(),
            redo: Vec::new(),
            open_txn: false,
            recording: true,
            syntax: None,
            search: None,
            last_edit: Instant::now(),
            revision: 0,
        }
    }

    pub fn from_text(text: &str, lang: Lang) -> Editor {
        let mut ed = Editor::new();
        ed.set_text(text);
        ed.set_lang(lang);
        ed
    }

    pub fn set_lang(&mut self, lang: Lang) {
        self.lang = lang;
        self.syntax = Syntax::new(lang);
    }

    pub fn open(path: &Path) -> anyhow::Result<Editor> {
        let text = std::fs::read_to_string(path)?;
        let mut ed = Editor::from_text(&text, Lang::detect(path, &text));
        ed.path = Some(path.to_path_buf());
        ed.disk_mtime = mtime(path);
        Ok(ed)
    }

    /// True when the file on disk is newer than what this buffer last read or wrote.
    pub fn disk_changed(&self) -> bool {
        match &self.path {
            Some(p) => mtime(p) != self.disk_mtime,
            None => false,
        }
    }

    /// Re-read the file, keeping cursors and scroll where possible. Drops undo history.
    pub fn reload(&mut self) -> anyhow::Result<()> {
        let Some(path) = self.path.clone() else {
            anyhow::bail!("no file name")
        };
        let text = std::fs::read_to_string(&path)?;
        let cursors = self.cursors.clone();
        let scroll = (self.scroll_line, self.scroll_col);
        self.set_text(&text);
        self.cursors = cursors
            .into_iter()
            .map(|c| Cursor {
                pos: self.clamp(c.pos),
                anchor: c.anchor.map(|a| self.clamp(a)),
                sticky: c.sticky,
            })
            .collect();
        self.dedupe_cursors();
        self.scroll_line = scroll.0.min(self.lines.len() - 1);
        self.scroll_col = scroll.1;
        self.undo.clear();
        self.redo.clear();
        self.open_txn = false;
        self.modified = false;
        self.conflict = false;
        self.disk_mtime = mtime(&path);
        Ok(())
    }

    pub fn set_text(&mut self, text: &str) {
        let mut lines: Vec<String> = text
            .split('\n')
            .map(|l| l.trim_end_matches('\r').to_string())
            .collect();
        if lines.is_empty() {
            lines.push(String::new());
        }
        self.lines = lines;
        self.cursors = vec![Cursor::default()];
        if let Some(s) = self.syntax.as_mut() {
            s.invalidate();
        }
    }

    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    pub fn save(&mut self) -> anyhow::Result<()> {
        let Some(path) = &self.path else {
            anyhow::bail!("no file name")
        };
        std::fs::write(path, self.text())?;
        self.modified = false;
        self.conflict = false;
        self.disk_mtime = mtime(path);
        Ok(())
    }

    pub fn save_as(&mut self, path: PathBuf) -> anyhow::Result<()> {
        let first = self.lines.first().cloned().unwrap_or_default();
        self.set_lang(Lang::detect(&path, &first));
        self.path = Some(path);
        self.save()
    }

    pub fn title(&self) -> String {
        match &self.path {
            Some(p) => p
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default(),
            None => "untitled".to_string(),
        }
    }

    pub fn primary(&self) -> &Cursor {
        self.cursors.last().expect("at least one cursor")
    }

    pub fn line_len(&self, line: usize) -> usize {
        self.lines.get(line).map(|l| char_len(l)).unwrap_or(0)
    }

    fn clamp(&self, p: Pos) -> Pos {
        let line = p.line.min(self.lines.len() - 1);
        Pos {
            line,
            col: p.col.min(self.line_len(line)),
        }
    }

    /// Highlight runs for the `count` lines starting at `first`.
    pub fn runs(&mut self, first: usize, count: usize) -> Vec<Vec<Run>> {
        match self.syntax.as_mut() {
            Some(s) => s.runs(&self.lines, first, count),
            None => vec![Vec::new(); count],
        }
    }

    /// Tree-sitter parse errors as diagnostics (char columns), once typing has paused.
    pub fn syntax_diagnostics(&mut self) -> Vec<Diagnostic> {
        if self.last_edit.elapsed() < std::time::Duration::from_millis(600) {
            return Vec::new();
        }
        // Data files get one precise message from a real parser on open/save; the
        // grammar's recovery guesses are only useful while there are unsaved edits.
        if matches!(self.lang, Lang::Json | Lang::Toml) && !self.modified {
            return Vec::new();
        }
        let Some(sy) = self.syntax.as_mut() else {
            return Vec::new();
        };
        let lines = &self.lines;
        sy.errors(lines)
            .iter()
            .map(|e| {
                let line = e.row.min(lines.len() - 1);
                let text = &lines[line];
                let col = char_len(&text[..e.col.min(text.len())]);
                // clamp multi-line errors to the first line
                let end_col = if e.end_row == e.row {
                    char_len(&text[..e.end_col.min(text.len())])
                } else {
                    char_len(text)
                };
                Diagnostic {
                    line,
                    col,
                    end_line: line,
                    end_col: end_col.max(col + 1),
                    severity: Severity::Error,
                    message: e.message.clone(),
                }
            })
            .collect()
    }

    fn byte_offset(&self, p: Pos) -> usize {
        let mut b = 0;
        for l in &self.lines[..p.line] {
            b += l.len() + 1;
        }
        b + byte_idx(&self.lines[p.line], p.col)
    }

    fn point(&self, p: Pos) -> Point {
        Point::new(p.line, byte_idx(&self.lines[p.line], p.col))
    }

    /// Visual column (accounting for tabs) of a char column on a line.
    pub fn display_col(&self, line: usize, col: usize) -> usize {
        let mut x = 0;
        for (i, c) in self.lines[line].chars().enumerate() {
            if i >= col {
                break;
            }
            x += if c == '\t' {
                TAB_WIDTH - (x % TAB_WIDTH)
            } else {
                unicode_width::UnicodeWidthChar::width(c).unwrap_or(1)
            };
        }
        x
    }

    // ---------- undo ----------

    /// Start (or continue, when coalescing typing) an undo transaction.
    fn snapshot(&mut self, kind: EditKind) {
        if self.open_txn
            && let Some(top) = self.undo.last()
            && kind != EditKind::None
            && top.kind == kind
        {
            return;
        }
        self.close_txn();
        self.undo.push(Transaction {
            ops: Vec::new(),
            cursors_before: self.cursors.clone(),
            cursors_after: Vec::new(),
            kind,
        });
        if self.undo.len() > 1000 {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.open_txn = true;
    }

    fn close_txn(&mut self) {
        if self.open_txn {
            if let Some(top) = self.undo.last_mut() {
                top.cursors_after = self.cursors.clone();
            }
            self.open_txn = false;
        }
    }

    fn record(&mut self, op: Op) {
        if !self.recording {
            return;
        }
        if let Some(top) = self.undo.last_mut() {
            top.ops.push(op);
        }
    }

    fn end_of(at: Pos, text: &str) -> Pos {
        let newlines = text.matches('\n').count();
        if newlines == 0 {
            Pos::new(at.line, at.col + char_len(text))
        } else {
            Pos::new(
                at.line + newlines,
                char_len(text.rsplit('\n').next().unwrap_or("")),
            )
        }
    }

    pub fn undo(&mut self) {
        self.close_txn();
        let Some(txn) = self.undo.pop() else { return };
        self.recording = false;
        for op in txn.ops.iter().rev() {
            match op {
                Op::Insert { at, text } => self.delete_range(*at, Self::end_of(*at, text)),
                Op::Delete { at, text } => {
                    self.insert_at(*at, text);
                }
            }
        }
        self.recording = true;
        self.cursors = txn.cursors_before.clone();
        self.redo.push(txn);
        self.modified = true;
    }

    pub fn redo(&mut self) {
        self.close_txn();
        let Some(txn) = self.redo.pop() else { return };
        self.recording = false;
        for op in txn.ops.iter() {
            match op {
                Op::Insert { at, text } => {
                    self.insert_at(*at, text);
                }
                Op::Delete { at, text } => self.delete_range(*at, Self::end_of(*at, text)),
            }
        }
        self.recording = true;
        self.cursors = txn.cursors_after.clone();
        self.undo.push(txn);
        self.modified = true;
    }

    pub fn end_edit_group(&mut self) {
        self.close_txn();
    }

    // ---------- primitive edits (keep all cursors consistent) ----------

    fn shift_for_insert(p: Pos, at: Pos, newlines: usize, last_len: usize, inclusive: bool) -> Pos {
        if p < at || (p == at && !inclusive) {
            return p;
        }
        if p.line == at.line {
            if newlines == 0 {
                Pos {
                    line: p.line,
                    col: p.col + last_len,
                }
            } else {
                Pos {
                    line: p.line + newlines,
                    col: p.col - at.col + last_len,
                }
            }
        } else {
            Pos {
                line: p.line + newlines,
                col: p.col,
            }
        }
    }

    fn shift_for_delete(p: Pos, s: Pos, e: Pos) -> Pos {
        if p <= s {
            return p;
        }
        if p < e {
            return s;
        }
        if p.line == e.line {
            Pos {
                line: s.line,
                col: s.col + (p.col - e.col),
            }
        } else {
            Pos {
                line: p.line - (e.line - s.line),
                col: p.col,
            }
        }
    }

    /// Insert `text` at `at`, returning the position just after it.
    fn insert_at(&mut self, at: Pos, text: &str) -> Pos {
        let at = self.clamp(at);
        if text.is_empty() {
            return at;
        }
        let parts: Vec<&str> = text.split('\n').collect();
        let newlines = parts.len() - 1;
        let last_len = char_len(parts[newlines]);
        let start_byte = self.byte_offset(at);
        let start_point = self.point(at);
        let line = &self.lines[at.line];
        let bi = byte_idx(line, at.col);
        let tail = line[bi..].to_string();
        let head = line[..bi].to_string();
        if newlines == 0 {
            self.lines[at.line] = format!("{}{}{}", head, parts[0], tail);
        } else {
            self.lines[at.line] = format!("{}{}", head, parts[0]);
            let mut new_lines: Vec<String> = parts[1..].iter().map(|s| s.to_string()).collect();
            let last = new_lines.last_mut().unwrap();
            last.push_str(&tail);
            let idx = at.line + 1;
            self.lines.splice(idx..idx, new_lines);
        }
        for c in self.cursors.iter_mut() {
            c.pos = Self::shift_for_insert(c.pos, at, newlines, last_len, true);
            if let Some(a) = c.anchor {
                c.anchor = Some(Self::shift_for_insert(a, at, newlines, last_len, true));
            }
        }
        self.modified = true;
        self.last_edit = Instant::now();
        self.revision += 1;
        let end = if newlines == 0 {
            Pos {
                line: at.line,
                col: at.col + last_len,
            }
        } else {
            Pos {
                line: at.line + newlines,
                col: last_len,
            }
        };
        if let Some(s) = self.syntax.as_mut() {
            s.edit(&InputEdit {
                start_byte,
                old_end_byte: start_byte,
                new_end_byte: start_byte + text.len(),
                start_position: start_point,
                old_end_position: start_point,
                new_end_position: Point::new(end.line, byte_idx(&self.lines[end.line], end.col)),
            });
        }
        self.record(Op::Insert {
            at,
            text: text.to_string(),
        });
        end
    }

    fn delete_range(&mut self, s: Pos, e: Pos) {
        let s = self.clamp(s);
        let e = self.clamp(e);
        if s >= e {
            return;
        }
        let removed = if self.recording {
            self.text_in(s, e)
        } else {
            String::new()
        };
        let start_byte = self.byte_offset(s);
        let old_end_byte = self.byte_offset(e);
        let start_point = self.point(s);
        let old_end_point = self.point(e);
        let head = {
            let l = &self.lines[s.line];
            l[..byte_idx(l, s.col)].to_string()
        };
        let tail = {
            let l = &self.lines[e.line];
            l[byte_idx(l, e.col)..].to_string()
        };
        self.lines[s.line] = head + &tail;
        if e.line > s.line {
            self.lines.drain(s.line + 1..=e.line);
        }
        for c in self.cursors.iter_mut() {
            c.pos = Self::shift_for_delete(c.pos, s, e);
            if let Some(a) = c.anchor {
                c.anchor = Some(Self::shift_for_delete(a, s, e));
            }
        }
        self.modified = true;
        self.last_edit = Instant::now();
        self.revision += 1;
        if let Some(sy) = self.syntax.as_mut() {
            sy.edit(&InputEdit {
                start_byte,
                old_end_byte,
                new_end_byte: start_byte,
                start_position: start_point,
                old_end_position: old_end_point,
                new_end_position: start_point,
            });
        }
        self.record(Op::Delete {
            at: s,
            text: removed,
        });
    }

    pub fn text_in(&self, s: Pos, e: Pos) -> String {
        if s.line == e.line {
            let l = &self.lines[s.line];
            return l[byte_idx(l, s.col)..byte_idx(l, e.col)].to_string();
        }
        let mut out = String::new();
        let l = &self.lines[s.line];
        out.push_str(&l[byte_idx(l, s.col)..]);
        for i in s.line + 1..e.line {
            out.push('\n');
            out.push_str(&self.lines[i]);
        }
        out.push('\n');
        let l = &self.lines[e.line];
        out.push_str(&l[..byte_idx(l, e.col)]);
        out
    }

    /// Remove duplicate cursors (same position) keeping the last one.
    fn dedupe_cursors(&mut self) {
        let mut seen: Vec<Pos> = Vec::new();
        let mut keep: Vec<Cursor> = Vec::new();
        for c in self.cursors.iter().rev() {
            if !seen.contains(&c.pos) {
                seen.push(c.pos);
                keep.push(*c);
            }
        }
        keep.reverse();
        self.cursors = keep;
    }

    /// Cursor indices ordered from the last position in the buffer to the first.
    fn order_desc(&self) -> Vec<usize> {
        let mut idx: Vec<usize> = (0..self.cursors.len()).collect();
        idx.sort_by(|a, b| self.cursors[*b].pos.cmp(&self.cursors[*a].pos));
        idx
    }

    fn delete_selection_of(&mut self, i: usize) -> bool {
        if let Some((s, e)) = self.cursors[i].selection() {
            self.delete_range(s, e);
            self.cursors[i].anchor = None;
            self.cursors[i].pos = s;
            true
        } else {
            false
        }
    }

    // ---------- editing commands ----------

    pub fn insert_char(&mut self, ch: char) {
        self.snapshot(EditKind::Typing);
        let mut buf = [0u8; 4];
        let s = ch.encode_utf8(&mut buf).to_string();
        for i in self.order_desc() {
            self.delete_selection_of(i);
            let p = self.cursors[i].pos;
            let end = self.insert_at(p, &s);
            self.cursors[i].pos = end;
            self.cursors[i].sticky = None;
        }
        self.dedupe_cursors();
    }

    pub fn insert_str(&mut self, text: &str) {
        self.snapshot(EditKind::None);
        for i in self.order_desc() {
            self.delete_selection_of(i);
            let p = self.cursors[i].pos;
            let end = self.insert_at(p, text);
            self.cursors[i].pos = end;
            self.cursors[i].sticky = None;
        }
        self.dedupe_cursors();
    }

    pub fn insert_newline(&mut self) {
        self.snapshot(EditKind::None);
        for i in self.order_desc() {
            self.delete_selection_of(i);
            let p = self.cursors[i].pos;
            let indent: String = self.lines[p.line]
                .chars()
                .take_while(|c| *c == ' ' || *c == '\t')
                .collect();
            let mut indent: String = indent.chars().take(p.col).collect();
            // open a new block after `{` (rust) or `:` (python)
            let before: String = self.lines[p.line].chars().take(p.col).collect();
            let opener = match self.lang {
                Lang::Rust => before.trim_end().ends_with('{') || before.trim_end().ends_with('('),
                Lang::Python => before.trim_end().ends_with(':'),
                _ => false,
            };
            if opener {
                indent.push_str(&" ".repeat(TAB_WIDTH));
            }
            let text = format!("\n{}", indent);
            let end = self.insert_at(p, &text);
            self.cursors[i].pos = end;
            self.cursors[i].sticky = None;
        }
        self.dedupe_cursors();
    }

    pub fn insert_tab(&mut self) {
        // Indent whole lines when a multi-line selection exists, otherwise insert spaces.
        let any_multiline = self.cursors.iter().any(|c| {
            c.selection()
                .map(|(s, e)| s.line != e.line)
                .unwrap_or(false)
        });
        if any_multiline {
            self.indent_selection(true);
            return;
        }
        self.snapshot(EditKind::Typing);
        for i in self.order_desc() {
            self.delete_selection_of(i);
            let p = self.cursors[i].pos;
            let dc = self.display_col(p.line, p.col);
            let n = TAB_WIDTH - (dc % TAB_WIDTH);
            let end = self.insert_at(p, &" ".repeat(n));
            self.cursors[i].pos = end;
        }
        self.dedupe_cursors();
    }

    pub fn indent_selection(&mut self, indent: bool) {
        self.snapshot(EditKind::None);
        let mut lines: Vec<usize> = Vec::new();
        for c in &self.cursors {
            match c.selection() {
                Some((s, e)) => {
                    let last = if e.col == 0 && e.line > s.line {
                        e.line - 1
                    } else {
                        e.line
                    };
                    for l in s.line..=last {
                        lines.push(l);
                    }
                }
                None => lines.push(c.pos.line),
            }
        }
        lines.sort_unstable();
        lines.dedup();
        for l in lines.into_iter().rev() {
            if indent {
                self.insert_at(Pos::new(l, 0), &" ".repeat(TAB_WIDTH));
            } else {
                let n = self.lines[l]
                    .chars()
                    .take(TAB_WIDTH)
                    .take_while(|c| *c == ' ')
                    .count();
                let n = if n == 0 && self.lines[l].starts_with('\t') {
                    1
                } else {
                    n
                };
                self.delete_range(Pos::new(l, 0), Pos::new(l, n));
            }
        }
    }

    pub fn backspace(&mut self) {
        self.snapshot(EditKind::Deleting);
        for i in self.order_desc() {
            if self.delete_selection_of(i) {
                continue;
            }
            let p = self.cursors[i].pos;
            if p.col > 0 {
                // delete a full indent step when only whitespace precedes the cursor
                let line = &self.lines[p.line];
                let prefix: String = line.chars().take(p.col).collect();
                let n = if !prefix.is_empty() && prefix.chars().all(|c| c == ' ') {
                    let dc = self.display_col(p.line, p.col);
                    let back = dc % TAB_WIDTH;
                    if back == 0 { TAB_WIDTH } else { back }
                } else {
                    1
                };
                self.delete_range(Pos::new(p.line, p.col - n), p);
            } else if p.line > 0 {
                let prev_len = self.line_len(p.line - 1);
                self.delete_range(Pos::new(p.line - 1, prev_len), p);
            }
            self.cursors[i].sticky = None;
        }
        self.dedupe_cursors();
    }

    pub fn delete_forward(&mut self) {
        self.snapshot(EditKind::Deleting);
        for i in self.order_desc() {
            if self.delete_selection_of(i) {
                continue;
            }
            let p = self.cursors[i].pos;
            if p.col < self.line_len(p.line) {
                self.delete_range(p, Pos::new(p.line, p.col + 1));
            } else if p.line + 1 < self.lines.len() {
                self.delete_range(p, Pos::new(p.line + 1, 0));
            }
        }
        self.dedupe_cursors();
    }

    pub fn delete_word_back(&mut self) {
        self.snapshot(EditKind::None);
        for i in self.order_desc() {
            if self.delete_selection_of(i) {
                continue;
            }
            let p = self.cursors[i].pos;
            let target = self.word_left(p);
            self.delete_range(target, p);
        }
        self.dedupe_cursors();
    }

    /// Delete the whole line under every cursor.
    pub fn delete_lines(&mut self) {
        self.snapshot(EditKind::None);
        let mut lines: Vec<usize> = self.cursors.iter().map(|c| c.pos.line).collect();
        lines.sort_unstable();
        lines.dedup();
        for l in lines.into_iter().rev() {
            if self.lines.len() == 1 {
                self.delete_range(Pos::new(0, 0), Pos::new(0, self.line_len(0)));
            } else if l + 1 < self.lines.len() {
                self.delete_range(Pos::new(l, 0), Pos::new(l + 1, 0));
            } else {
                self.delete_range(
                    Pos::new(l - 1, self.line_len(l - 1)),
                    Pos::new(l, self.line_len(l)),
                );
            }
        }
        for c in self.cursors.iter_mut() {
            c.anchor = None;
        }
        self.dedupe_cursors();
    }

    /// Text that Ctrl+C would copy: one chunk per cursor.
    pub fn copy_chunks(&self) -> Vec<String> {
        let mut idx: Vec<usize> = (0..self.cursors.len()).collect();
        idx.sort_by_key(|i| self.cursors[*i].pos);
        idx.iter()
            .map(|&i| {
                let c = &self.cursors[i];
                match c.selection() {
                    Some((s, e)) => self.text_in(s, e),
                    None => format!("{}\n", self.lines[c.pos.line]),
                }
            })
            .collect()
    }

    pub fn cut(&mut self) -> Vec<String> {
        let chunks = self.copy_chunks();
        self.snapshot(EditKind::None);
        let any_sel = self.cursors.iter().any(|c| c.selection().is_some());
        if any_sel {
            for i in self.order_desc() {
                self.delete_selection_of(i);
            }
            self.dedupe_cursors();
        } else {
            self.delete_lines();
        }
        chunks
    }

    pub fn paste(&mut self, chunks: &[String]) {
        if chunks.is_empty() {
            return;
        }
        self.snapshot(EditKind::None);
        let mut idx: Vec<usize> = (0..self.cursors.len()).collect();
        idx.sort_by_key(|i| self.cursors[*i].pos);
        let per_cursor = chunks.len() == self.cursors.len() && chunks.len() > 1;
        let whole = chunks.concat();
        for (k, i) in idx.iter().enumerate().rev() {
            let i = *i;
            let text = if per_cursor {
                chunks[k].clone()
            } else {
                whole.clone()
            };
            // Whole-line paste when the chunk is a full line and there is no selection.
            let line_paste = text.ends_with('\n')
                && !text[..text.len() - 1].contains('\n')
                && self.cursors[i].selection().is_none();
            if line_paste {
                let p = self.cursors[i].pos;
                self.insert_at(Pos::new(p.line, 0), &text);
                self.cursors[i].pos = Pos::new(p.line + 1, p.col);
            } else {
                self.delete_selection_of(i);
                let p = self.cursors[i].pos;
                let end = self.insert_at(p, &text);
                self.cursors[i].pos = end;
            }
        }
        self.dedupe_cursors();
    }

    /// Move the lines spanned by every cursor/selection up or down by one.
    pub fn move_lines(&mut self, down: bool) {
        // Collect inclusive line ranges, merged.
        let mut ranges: Vec<(usize, usize)> = self
            .cursors
            .iter()
            .map(|c| match c.selection() {
                Some((s, e)) => {
                    let last = if e.col == 0 && e.line > s.line {
                        e.line - 1
                    } else {
                        e.line
                    };
                    (s.line, last)
                }
                None => (c.pos.line, c.pos.line),
            })
            .collect();
        ranges.sort_unstable();
        let mut merged: Vec<(usize, usize)> = Vec::new();
        for r in ranges {
            if let Some(last) = merged.last_mut()
                && r.0 <= last.1 + 1
            {
                last.1 = last.1.max(r.1);
                continue;
            }
            merged.push(r);
        }
        let n = self.lines.len();
        if down {
            if merged.last().map(|r| r.1 + 1 >= n).unwrap_or(true) {
                return;
            }
        } else if merged.first().map(|r| r.0 == 0).unwrap_or(true) {
            return;
        }
        self.snapshot(EditKind::None);
        let saved = self.cursors.clone();
        if down {
            for r in merged.iter().rev() {
                // pull the line below the range up to the top of the range
                let moved = self.lines[r.1 + 1].clone();
                self.delete_range(
                    Pos::new(r.1, self.line_len(r.1)),
                    Pos::new(r.1 + 1, self.line_len(r.1 + 1)),
                );
                self.insert_at(Pos::new(r.0, 0), &format!("{}\n", moved));
            }
        } else {
            for r in merged.iter() {
                // push the line above the range below it
                let moved = self.lines[r.0 - 1].clone();
                self.delete_range(Pos::new(r.0 - 1, 0), Pos::new(r.0, 0));
                let last = r.1 - 1;
                self.insert_at(Pos::new(last, self.line_len(last)), &format!("\n{}", moved));
            }
        }
        // Every cursor's whole selection (including an exclusive end sitting at
        // column 0 of the next line) travels with the lines that moved.
        let n = self.lines.len();
        let shift = |p: &mut Pos| {
            if down {
                p.line += 1;
                if p.line >= n {
                    *p = Pos::new(n - 1, usize::MAX);
                }
            } else {
                p.line = p.line.saturating_sub(1);
            }
        };
        let mut cursors = saved;
        for c in cursors.iter_mut() {
            shift(&mut c.pos);
            if let Some(a) = c.anchor.as_mut() {
                shift(a);
            }
            c.pos = self.clamp(c.pos);
            c.anchor = c.anchor.map(|a| self.clamp(a));
        }
        self.cursors = cursors;
        self.modified = true;
    }

    // ---------- movement ----------

    fn word_left(&self, p: Pos) -> Pos {
        if p.col == 0 {
            return if p.line > 0 {
                Pos::new(p.line - 1, self.line_len(p.line - 1))
            } else {
                p
            };
        }
        let chars: Vec<char> = self.lines[p.line].chars().collect();
        let mut i = p.col;
        while i > 0 && chars[i - 1].is_whitespace() {
            i -= 1;
        }
        if i > 0 {
            let w = is_word(chars[i - 1]);
            while i > 0 && !chars[i - 1].is_whitespace() && is_word(chars[i - 1]) == w {
                i -= 1;
            }
        }
        Pos::new(p.line, i)
    }

    fn word_right(&self, p: Pos) -> Pos {
        let len = self.line_len(p.line);
        if p.col >= len {
            return if p.line + 1 < self.lines.len() {
                Pos::new(p.line + 1, 0)
            } else {
                p
            };
        }
        let chars: Vec<char> = self.lines[p.line].chars().collect();
        let mut i = p.col;
        let w = is_word(chars[i]);
        while i < len && !chars[i].is_whitespace() && is_word(chars[i]) == w {
            i += 1;
        }
        while i < len && chars[i].is_whitespace() {
            i += 1;
        }
        Pos::new(p.line, i)
    }

    fn apply_move<F: Fn(&Editor, &Cursor) -> Pos>(
        &mut self,
        extend: bool,
        keep_sticky: bool,
        f: F,
    ) {
        let n = self.cursors.len();
        for i in 0..n {
            let c = self.cursors[i];
            let np = f(self, &c);
            let c = &mut self.cursors[i];
            if extend {
                if c.anchor.is_none() {
                    c.anchor = Some(c.pos);
                }
            } else {
                c.anchor = None;
            }
            c.pos = np;
            if !keep_sticky {
                c.sticky = None;
            }
        }
        self.end_edit_group();
        self.dedupe_cursors();
    }

    pub fn move_left(&mut self, extend: bool) {
        self.apply_move(extend, false, |ed, c| {
            if !extend && let Some((s, _)) = c.selection() {
                return s;
            }
            if c.pos.col > 0 {
                Pos::new(c.pos.line, c.pos.col - 1)
            } else if c.pos.line > 0 {
                Pos::new(c.pos.line - 1, ed.line_len(c.pos.line - 1))
            } else {
                c.pos
            }
        });
    }

    pub fn move_right(&mut self, extend: bool) {
        self.apply_move(extend, false, |ed, c| {
            if !extend && let Some((_, e)) = c.selection() {
                return e;
            }
            if c.pos.col < ed.line_len(c.pos.line) {
                Pos::new(c.pos.line, c.pos.col + 1)
            } else if c.pos.line + 1 < ed.lines.len() {
                Pos::new(c.pos.line + 1, 0)
            } else {
                c.pos
            }
        });
    }

    pub fn move_word_left(&mut self, extend: bool) {
        self.apply_move(extend, false, |ed, c| ed.word_left(c.pos));
    }

    pub fn move_word_right(&mut self, extend: bool) {
        self.apply_move(extend, false, |ed, c| ed.word_right(c.pos));
    }

    fn vertical(&mut self, delta: isize, extend: bool) {
        let n = self.cursors.len();
        for i in 0..n {
            let c = self.cursors[i];
            let sticky = c
                .sticky
                .unwrap_or_else(|| self.display_col(c.pos.line, c.pos.col));
            let target =
                (c.pos.line as isize + delta).clamp(0, self.lines.len() as isize - 1) as usize;
            let col = self.col_for_display(target, sticky);
            let c = &mut self.cursors[i];
            if extend {
                if c.anchor.is_none() {
                    c.anchor = Some(c.pos);
                }
            } else {
                c.anchor = None;
            }
            c.pos = Pos::new(target, col);
            c.sticky = Some(sticky);
        }
        self.end_edit_group();
        self.dedupe_cursors();
    }

    /// Char column on `line` closest to visual column `x`.
    pub fn col_for_display(&self, line: usize, x: usize) -> usize {
        let mut acc = 0;
        for (i, c) in self.lines[line].chars().enumerate() {
            let w = if c == '\t' {
                TAB_WIDTH - (acc % TAB_WIDTH)
            } else {
                unicode_width::UnicodeWidthChar::width(c).unwrap_or(1)
            };
            if acc + w > x {
                return i;
            }
            acc += w;
        }
        self.line_len(line)
    }

    pub fn move_up(&mut self, extend: bool) {
        self.vertical(-1, extend);
    }

    pub fn move_down(&mut self, extend: bool) {
        self.vertical(1, extend);
    }

    pub fn page(&mut self, rows: usize, down: bool, extend: bool) {
        let d = rows.max(1) as isize;
        self.vertical(if down { d } else { -d }, extend);
        if down {
            self.scroll_line = (self.scroll_line + rows).min(self.lines.len().saturating_sub(1));
        } else {
            self.scroll_line = self.scroll_line.saturating_sub(rows);
        }
    }

    pub fn home(&mut self, extend: bool) {
        self.apply_move(extend, false, |ed, c| {
            let first_non_ws = ed.lines[c.pos.line]
                .chars()
                .take_while(|ch| ch.is_whitespace())
                .count();
            if c.pos.col == first_non_ws || first_non_ws == ed.line_len(c.pos.line) {
                Pos::new(c.pos.line, 0)
            } else {
                Pos::new(c.pos.line, first_non_ws)
            }
        });
    }

    pub fn end(&mut self, extend: bool) {
        self.apply_move(extend, false, |ed, c| {
            Pos::new(c.pos.line, ed.line_len(c.pos.line))
        });
    }

    pub fn doc_start(&mut self, extend: bool) {
        self.apply_move(extend, false, |_, _| Pos::new(0, 0));
    }

    pub fn doc_end(&mut self, extend: bool) {
        self.apply_move(extend, false, |ed, _| {
            Pos::new(ed.lines.len() - 1, ed.line_len(ed.lines.len() - 1))
        });
    }

    pub fn goto_line(&mut self, line: usize) {
        let line = line.saturating_sub(1).min(self.lines.len() - 1);
        self.cursors = vec![Cursor::at(Pos::new(line, 0))];
    }

    pub fn select_all(&mut self) {
        let end = Pos::new(self.lines.len() - 1, self.line_len(self.lines.len() - 1));
        self.cursors = vec![Cursor {
            pos: end,
            anchor: Some(Pos::new(0, 0)),
            sticky: None,
        }];
    }

    /// Select the word under the primary cursor, or with an existing selection add
    /// a cursor at the next occurrence of the selected text (Ctrl+D style).
    pub fn select_next_match(&mut self) {
        let primary = *self.primary();
        match primary.selection() {
            None => {
                let chars: Vec<char> = self.lines[primary.pos.line].chars().collect();
                let mut s = primary.pos.col;
                let mut e = primary.pos.col;
                while s > 0 && is_word(chars[s - 1]) {
                    s -= 1;
                }
                while e < chars.len() && is_word(chars[e]) {
                    e += 1;
                }
                if s < e {
                    let last = self.cursors.len() - 1;
                    self.cursors[last].anchor = Some(Pos::new(primary.pos.line, s));
                    self.cursors[last].pos = Pos::new(primary.pos.line, e);
                }
            }
            Some((s, e)) => {
                if s.line != e.line {
                    return;
                }
                let needle = self.text_in(s, e);
                if let Some((ms, me)) = self.find_from(&needle, e, true)
                    && !self.cursors.iter().any(|c| c.pos == me)
                {
                    self.cursors.push(Cursor {
                        pos: me,
                        anchor: Some(ms),
                        sticky: None,
                    });
                }
            }
        }
    }

    pub fn add_cursor_vertical(&mut self, down: bool) {
        let p = *self.primary();
        let line = if down {
            if p.pos.line + 1 >= self.lines.len() {
                return;
            }
            p.pos.line + 1
        } else {
            if p.pos.line == 0 {
                return;
            }
            p.pos.line - 1
        };
        let sticky = p
            .sticky
            .unwrap_or_else(|| self.display_col(p.pos.line, p.pos.col));
        let col = self.col_for_display(line, sticky);
        self.cursors.push(Cursor {
            pos: Pos::new(line, col),
            anchor: None,
            sticky: Some(sticky),
        });
        self.dedupe_cursors();
    }

    /// Place a cursor at the end of every line of the selection (or every line of
    /// the document when nothing is selected).
    pub fn cursors_at_line_ends(&mut self) {
        let p = *self.primary();
        match p.selection() {
            // Split the selection into one selection per line, each ending at the
            // line end, so typing or deleting acts on every selected line.
            Some((s, e)) => {
                let last = if e.col == 0 && e.line > s.line {
                    e.line - 1
                } else {
                    e.line
                };
                self.cursors = (s.line..=last)
                    .map(|l| {
                        let start = if l == s.line { s.col } else { 0 };
                        Cursor {
                            pos: Pos::new(l, self.line_len(l)),
                            anchor: Some(Pos::new(l, start)),
                            sticky: None,
                        }
                    })
                    .collect();
            }
            None => {
                self.cursors = (0..self.lines.len())
                    .map(|l| Cursor::at(Pos::new(l, self.line_len(l))))
                    .collect();
            }
        }
    }

    pub fn collapse_cursors(&mut self) {
        let p = *self.primary();
        self.cursors = vec![Cursor {
            pos: p.pos,
            anchor: None,
            sticky: p.sticky,
        }];
    }

    // ---------- search ----------

    /// Find `needle` starting at `from` (wrapping), returning its span.
    pub fn find_from(&self, needle: &str, from: Pos, wrap: bool) -> Option<(Pos, Pos)> {
        if needle.is_empty() {
            return None;
        }
        let nlen = char_len(needle);
        let n = self.lines.len();
        for k in 0..=n {
            let line = (from.line + k) % n;
            if k == n && !wrap {
                break;
            }
            let text = &self.lines[line];
            let start_col = if k == 0 { from.col } else { 0 };
            let start_b = byte_idx(text, start_col);
            if let Some(off) = text[start_b..].find(needle) {
                let col = char_len(&text[..start_b + off]);
                return Some((Pos::new(line, col), Pos::new(line, col + nlen)));
            }
            if k == n {
                // wrapped back to the starting line: search its head too
                let head = &text[..start_b];
                if let Some(off) = head.find(needle) {
                    let col = char_len(&text[..off]);
                    return Some((Pos::new(line, col), Pos::new(line, col + nlen)));
                }
            }
        }
        None
    }

    pub fn find_next(&mut self, needle: &str) -> bool {
        let from = self.primary().pos;
        if let Some((s, e)) = self.find_from(needle, from, true) {
            self.cursors = vec![Cursor {
                pos: e,
                anchor: Some(s),
                sticky: None,
            }];
            true
        } else {
            false
        }
    }

    /// Keep the primary cursor visible inside a viewport of `rows` x `cols`.
    pub fn scroll_to_cursor(&mut self, rows: usize, cols: usize) {
        let p = self.primary().pos;
        if rows == 0 || cols == 0 {
            return;
        }
        if p.line < self.scroll_line {
            self.scroll_line = p.line;
        } else if p.line >= self.scroll_line + rows {
            self.scroll_line = p.line + 1 - rows;
        }
        let x = self.display_col(p.line, p.col);
        if x < self.scroll_col {
            self.scroll_col = x;
        } else if x >= self.scroll_col + cols {
            self.scroll_col = x + 1 - cols;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ed(text: &str) -> Editor {
        Editor::from_text(text, Lang::Plain)
    }

    #[test]
    fn insert_and_newline() {
        let mut e = ed("ab");
        e.move_right(false);
        e.insert_char('x');
        assert_eq!(e.text(), "axb");
        e.insert_newline();
        assert_eq!(e.text(), "ax\nb");
        assert_eq!(e.primary().pos, Pos::new(1, 0));
    }

    #[test]
    fn multi_cursor_typing_shifts_later_cursors() {
        let mut e = ed("aa\nbb");
        e.add_cursor_vertical(true);
        e.insert_char('_');
        assert_eq!(e.text(), "_aa\n_bb");
        assert_eq!(e.cursors.len(), 2);
        assert!(e.cursors.iter().all(|c| c.pos.col == 1));
    }

    #[test]
    fn two_cursors_same_line() {
        let mut e = ed("abcd");
        e.cursors = vec![Cursor::at(Pos::new(0, 1)), Cursor::at(Pos::new(0, 3))];
        e.insert_char('-');
        assert_eq!(e.text(), "a-bc-d");
        assert_eq!(e.cursors[0].pos.col, 2);
        assert_eq!(e.cursors[1].pos.col, 5);
        e.backspace();
        assert_eq!(e.text(), "abcd");
    }

    #[test]
    fn move_lines() {
        let mut e = ed("1\n2\n3");
        e.move_down(false);
        e.move_lines(true);
        assert_eq!(e.text(), "1\n3\n2");
        assert_eq!(e.primary().pos.line, 2);
        e.move_lines(false);
        e.move_lines(false);
        assert_eq!(e.text(), "2\n1\n3");
    }

    #[test]
    fn move_selection_down_keeps_whole_selection() {
        let mut e = ed("a\nb\nc\nd\ne");
        // select lines 1-2 the way shift+down does: anchor (0,0), pos (2,0)
        e.cursors = vec![Cursor {
            pos: Pos::new(2, 0),
            anchor: Some(Pos::new(0, 0)),
            sticky: None,
        }];
        e.move_lines(true);
        assert_eq!(e.text(), "c\na\nb\nd\ne");
        assert_eq!(
            e.primary().selection(),
            Some((Pos::new(1, 0), Pos::new(3, 0)))
        );
        e.move_lines(true);
        assert_eq!(e.text(), "c\nd\na\nb\ne");
        assert_eq!(
            e.primary().selection(),
            Some((Pos::new(2, 0), Pos::new(4, 0)))
        );
        e.move_lines(true);
        assert_eq!(e.text(), "c\nd\ne\na\nb");
        // end clamps to the end of the last line
        assert_eq!(
            e.primary().selection(),
            Some((Pos::new(3, 0), Pos::new(4, 1)))
        );
        e.move_lines(true);
        assert_eq!(e.text(), "c\nd\ne\na\nb");
        e.move_lines(false);
        assert_eq!(e.text(), "c\nd\na\nb\ne");
    }

    #[test]
    fn line_end_cursors_select_lines() {
        let mut e = ed("abc\ndef\nghi");
        e.cursors = vec![Cursor {
            pos: Pos::new(2, 0),
            anchor: Some(Pos::new(0, 1)),
            sticky: None,
        }];
        e.cursors_at_line_ends();
        assert_eq!(e.cursors.len(), 2);
        assert_eq!(
            e.cursors[0].selection(),
            Some((Pos::new(0, 1), Pos::new(0, 3)))
        );
        assert_eq!(
            e.cursors[1].selection(),
            Some((Pos::new(1, 0), Pos::new(1, 3)))
        );
        e.insert_char('x');
        assert_eq!(e.text(), "ax\nx\nghi");
    }

    #[test]
    fn line_end_cursors() {
        let mut e = ed("a\nbb\nccc");
        e.cursors_at_line_ends();
        assert_eq!(e.cursors.len(), 3);
        e.insert_char(';');
        assert_eq!(e.text(), "a;\nbb;\nccc;");
    }

    #[test]
    fn undo_redo() {
        let mut e = ed("x");
        e.insert_char('y');
        e.end_edit_group();
        e.insert_char('z');
        assert_eq!(e.text(), "yzx");
        e.undo();
        assert_eq!(e.text(), "yx");
        e.undo();
        assert_eq!(e.text(), "x");
        e.redo();
        assert_eq!(e.text(), "yx");
    }

    #[test]
    fn word_motion() {
        let mut e = ed("foo bar_baz  qux");
        e.move_word_right(false);
        assert_eq!(e.primary().pos.col, 4);
        e.move_word_right(false);
        assert_eq!(e.primary().pos.col, 13);
        e.move_word_left(false);
        assert_eq!(e.primary().pos.col, 4);
    }

    #[test]
    fn find_wraps() {
        let e = ed("one\ntwo\nthree");
        let f = e.find_from("one", Pos::new(1, 0), true).unwrap();
        assert_eq!(f.0, Pos::new(0, 0));
    }
}

impl Editor {
    /// Toggle a line comment on every line touched by a cursor or selection.
    pub fn toggle_comment(&mut self) {
        let marker = match self.lang {
            Lang::Rust => "// ",
            Lang::Python | Lang::Toml | Lang::Shell | Lang::Plain => "# ",
            Lang::Ini => "; ",
            Lang::Json | Lang::Xml | Lang::Markdown => return,
        };
        let mut lines: Vec<usize> = Vec::new();
        for c in &self.cursors {
            match c.selection() {
                Some((s, e)) => {
                    let last = if e.col == 0 && e.line > s.line {
                        e.line - 1
                    } else {
                        e.line
                    };
                    lines.extend(s.line..=last);
                }
                None => lines.push(c.pos.line),
            }
        }
        lines.sort_unstable();
        lines.dedup();
        let all_commented = lines
            .iter()
            .all(|&l| self.lines[l].trim_start().starts_with(marker.trim_end()));
        let indent = lines
            .iter()
            .filter(|&&l| !self.lines[l].trim().is_empty())
            .map(|&l| {
                self.lines[l]
                    .chars()
                    .take_while(|c| c.is_whitespace())
                    .count()
            })
            .min()
            .unwrap_or(0);
        self.snapshot(EditKind::None);
        for l in lines.into_iter().rev() {
            if all_commented {
                let ws = self.lines[l]
                    .chars()
                    .take_while(|c| c.is_whitespace())
                    .count();
                let rest: String = self.lines[l].chars().skip(ws).collect();
                let n = if rest.starts_with(marker) {
                    marker.chars().count()
                } else {
                    marker.trim_end().chars().count()
                };
                self.delete_range(Pos::new(l, ws), Pos::new(l, ws + n));
            } else if !self.lines[l].trim().is_empty() {
                self.insert_at(Pos::new(l, indent), marker);
            }
        }
    }
}

impl Editor {
    /// Replace the buffer with `new`, applying only the changed line ranges so
    /// cursors in untouched regions stay put and the syntax tree edits incrementally.
    /// Everything lands in a single undo step. Returns the number of hunks applied.
    pub fn apply_text(&mut self, new: &str) -> usize {
        let old = self.text();
        if old == new {
            return 0;
        }
        let diff = similar::TextDiff::from_lines(old.as_str(), new);
        let old_lines: Vec<&str> = diff.old_slices().to_vec();
        let new_lines: Vec<&str> = diff.new_slices().to_vec();
        // (old line start, old line end, replacement text)
        let mut hunks: Vec<(usize, usize, String)> = Vec::new();
        for op in diff.ops() {
            use similar::DiffTag;
            match op.tag() {
                DiffTag::Equal => {}
                DiffTag::Delete => {
                    hunks.push((op.old_range().start, op.old_range().end, String::new()))
                }
                DiffTag::Insert | DiffTag::Replace => {
                    let text: String = new_lines[op.new_range()].concat();
                    hunks.push((op.old_range().start, op.old_range().end, text));
                }
            }
        }
        let _ = old_lines;
        self.snapshot(EditKind::None);
        let n = hunks.len();
        for (s, e, text) in hunks.into_iter().rev() {
            let start = self.line_start_pos(s);
            let end = self.line_start_pos(e);
            if start < end {
                self.delete_range(start, end);
            }
            if !text.is_empty() {
                self.insert_at(start, &text);
            }
        }
        self.end_edit_group();
        self.dedupe_cursors();
        n
    }

    /// Start of line `i`, or the end of the text when `i` is past the last line.
    fn line_start_pos(&self, i: usize) -> Pos {
        if i < self.lines.len() {
            Pos::new(i, 0)
        } else {
            let last = self.lines.len() - 1;
            Pos::new(last, self.line_len(last))
        }
    }
}

#[cfg(test)]
mod apply_tests {
    use super::*;

    #[test]
    fn apply_text_keeps_cursor_and_is_one_undo() {
        let mut e = Editor::from_text("a\nb\nc\nd\n", Lang::Plain);
        e.cursors = vec![Cursor::at(Pos::new(3, 1))];
        let n = e.apply_text("a\nB\nc\nd\n");
        assert_eq!(n, 1);
        assert_eq!(e.text(), "a\nB\nc\nd\n");
        assert_eq!(e.primary().pos, Pos::new(3, 1));
        e.undo();
        assert_eq!(e.text(), "a\nb\nc\nd\n");
    }

    #[test]
    fn apply_text_handles_missing_trailing_newline() {
        let mut e = Editor::from_text("x\ny", Lang::Plain);
        e.apply_text("x\ny\nz\n");
        assert_eq!(e.text(), "x\ny\nz\n");
        e.apply_text("q");
        assert_eq!(e.text(), "q");
    }
}

/// What a save-time cleanup changed.
#[derive(Default, Debug, PartialEq, Eq)]
pub struct Cleanup {
    pub trimmed_lines: usize,
    pub added_newline: bool,
}

impl Editor {
    /// Apply the save-time cleanups as one undo step. Trailing whitespace is left
    /// alone on rows inside multi-line strings (per the syntax tree), and in
    /// Markdown where two trailing spaces mean a line break. Unknown file types
    /// are not touched at all.
    pub fn clean_for_save(&mut self, trim: bool, final_newline: bool) -> Cleanup {
        let mut result = Cleanup::default();
        if self.lang == Lang::Plain {
            return result;
        }
        let trim = trim && self.lang != Lang::Markdown;
        let protected = if trim {
            match self.syntax.as_mut() {
                Some(sy) => sy.protected_rows(&self.lines),
                None => Default::default(),
            }
        } else {
            Default::default()
        };
        // find the edits first so the snapshot is only taken when something changes
        let mut trims: Vec<(usize, usize, usize)> = Vec::new(); // (line, from col, to col)
        if trim {
            for (i, line) in self.lines.iter().enumerate() {
                if protected.contains(&i) {
                    continue;
                }
                let len = char_len(line);
                let kept = char_len(line.trim_end_matches([' ', '\t']));
                if kept < len {
                    trims.push((i, kept, len));
                }
            }
        }
        let needs_newline =
            final_newline && !self.lines.last().map(|l| l.is_empty()).unwrap_or(true);
        if trims.is_empty() && !needs_newline {
            return result;
        }
        self.snapshot(EditKind::None);
        for (line, from, to) in trims.iter().rev() {
            self.delete_range(Pos::new(*line, *from), Pos::new(*line, *to));
        }
        result.trimmed_lines = trims.len();
        if needs_newline {
            let last = self.lines.len() - 1;
            let end = Pos::new(last, self.line_len(last));
            self.insert_at(end, "\n");
            result.added_newline = true;
        }
        self.end_edit_group();
        self.dedupe_cursors();
        result
    }
}

#[cfg(test)]
mod cleanup_tests {
    use super::*;

    #[test]
    fn trims_and_adds_newline_but_not_inside_strings() {
        let src = "fn main() {   \n    let s = \"line one   \n  line two  \";  \n    let r = r#\"raw   \n  keep \"#;\n}";
        let mut e = Editor::from_text(src, Lang::Rust);
        let c = e.clean_for_save(true, true);
        assert!(c.added_newline);
        let lines: Vec<&str> = e.lines.iter().map(|s| s.as_str()).collect();
        assert_eq!(lines[0], "fn main() {");
        // first line of the string keeps its trailing spaces (they are content)
        assert_eq!(lines[1], "    let s = \"line one   ");
        // closing line of the string is protected too
        assert_eq!(lines[2], "  line two  \";  ");
        assert_eq!(lines[3], "    let r = r#\"raw   ");
        assert_eq!(lines[4], "  keep \"#;");
        assert_eq!(lines[5], "}");
        assert_eq!(lines[6], "");
        assert_eq!(c.trimmed_lines, 1);
        // single undo step
        e.undo();
        assert_eq!(e.text(), src);
    }

    #[test]
    fn python_triple_quotes_and_markdown_breaks_are_safe() {
        let src = "x = 1   \ns = \"\"\"a   \nb  \n\"\"\"\n";
        let mut e = Editor::from_text(src, Lang::Python);
        let c = e.clean_for_save(true, true);
        assert_eq!(c.trimmed_lines, 1);
        assert_eq!(e.text(), "x = 1\ns = \"\"\"a   \nb  \n\"\"\"\n");
        let md = "para  \nnext\n";
        let mut m = Editor::from_text(md, Lang::Markdown);
        assert_eq!(m.clean_for_save(true, true), Cleanup::default());
        assert_eq!(m.text(), md);
        let mut p = Editor::from_text("plain   ", Lang::Plain);
        assert_eq!(p.clean_for_save(true, true), Cleanup::default());
    }

    #[test]
    fn settings_off_means_no_change() {
        let mut e = Editor::from_text("a   \nb", Lang::Rust);
        assert_eq!(e.clean_for_save(false, false), Cleanup::default());
        assert_eq!(e.text(), "a   \nb");
    }
}
