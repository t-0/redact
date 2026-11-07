//! Application state and key handling.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;

use crate::debug::{Debugger, Row as DebugRow};
use crate::deps::{DepsService, Health};
use crate::diagnostics::{Diagnostic, Diagnostics};
use crate::diffview::DiffTab;
use crate::editor::Editor;
use crate::explorer::Explorer;
use crate::git::{FileStatus, GitState};
use crate::keymap::{Action, Key, Keymap, Lookup, fuzzy_match};
use crate::settings::Settings;
use crate::terminal::TerminalTab;
use crate::tests::{TestEvent, TestsPane};
use crate::watch::FsWatcher;

/// First few hundred bytes of a file, for shebang detection.
fn read_head(path: &Path) -> String {
    use std::io::Read;
    let mut buf = [0u8; 256];
    let n = std::fs::File::open(path)
        .and_then(|mut f| f.read(&mut buf))
        .unwrap_or(0);
    String::from_utf8_lossy(&buf[..n]).into_owned()
}
use std::collections::HashSet;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LeftMode {
    Explorer,
    Scm,
    Tests,
    Debug,
    Problems,
}

impl LeftMode {
    pub fn title(self) -> &'static str {
        match self {
            LeftMode::Explorer => "explorer",
            LeftMode::Scm => "source control",
            LeftMode::Tests => "tests",
            LeftMode::Debug => "debug",
            LeftMode::Problems => "problems",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Focus {
    Left,
    Right,
}

pub struct OutputTab {
    pub title: String,
    pub lines: Vec<String>,
    pub scroll: usize,
    pub follow: bool,
}

pub enum TabKind {
    Editor(Editor),
    Terminal(TerminalTab),
    Diff(DiffTab),
    Output(OutputTab),
}

pub struct Tab {
    pub kind: TabKind,
    pub pinned: bool,
    /// Show a rendered preview instead of the editor (document formats only).
    pub preview: bool,
    pub preview_scroll: usize,
    /// (editor revision, width, rendered lines) so the preview is rebuilt only on change.
    pub preview_cache: Option<(u64, usize, Vec<ratatui::text::Line<'static>>)>,
}

impl Tab {
    pub fn new(kind: TabKind, pinned: bool) -> Tab {
        Tab {
            kind,
            pinned,
            preview: false,
            preview_scroll: 0,
            preview_cache: None,
        }
    }
}

impl Tab {
    pub fn title(&self) -> String {
        match &self.kind {
            TabKind::Editor(e) => e.title(),
            TabKind::Terminal(t) => t.title.clone(),
            TabKind::Diff(d) => d.title.clone(),
            TabKind::Output(o) => o.title.clone(),
        }
    }

    pub fn modified(&self) -> bool {
        matches!(&self.kind, TabKind::Editor(e) if e.modified)
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum PromptKind {
    Open,
    SaveAs,
    Find,
    Goto,
    ConfirmClose(usize),
    ConfirmQuit,
    /// Save over a file that changed on disk since it was loaded.
    ConfirmOverwrite,
}

pub struct Prompt {
    pub kind: PromptKind,
    pub label: String,
    pub input: String,
}

/// One row of the problems pane.
#[derive(Clone, Debug)]
pub enum ProblemRow {
    File(PathBuf, usize),
    Item(PathBuf, Diagnostic),
}

pub struct ProblemsPane {
    pub rows: Vec<ProblemRow>,
    pub selected: usize,
    pub scroll: usize,
}

impl ProblemsPane {
    pub fn move_by(&mut self, delta: isize) {
        if self.rows.is_empty() {
            return;
        }
        let n = self.rows.len() as isize;
        let mut i = (self.selected as isize + delta).clamp(0, n - 1);
        while matches!(self.rows[i as usize], ProblemRow::File(..)) {
            let next = i + delta.signum();
            if next < 0 || next >= n {
                break;
            }
            i = next;
        }
        self.selected = i as usize;
    }
}

/// The command palette: a filter box over every action.
pub struct Palette {
    pub query: String,
    pub selected: usize,
    pub scroll: usize,
    /// Actions matching the query, best first, with matched character positions in the label.
    pub items: Vec<(Action, Vec<usize>)>,
}

impl Palette {
    pub fn label(a: Action) -> String {
        format!("{}: {}", a.section(), a.describe())
    }

    fn refilter(&mut self) {
        let mut scored: Vec<(usize, Action, Vec<usize>)> = Action::ALL
            .iter()
            .filter_map(|a| {
                let label = Palette::label(*a);
                // match against the label first, then fall back to the config name
                fuzzy_match(&self.query, &label)
                    .map(|(s, p)| (s, *a, p))
                    .or_else(|| {
                        fuzzy_match(&self.query, a.name()).map(|(s, _)| (s + 30, *a, Vec::new()))
                    })
            })
            .collect();
        scored.sort_by(|x, y| {
            x.0.cmp(&y.0)
                .then_with(|| x.1.describe().cmp(y.1.describe()))
        });
        self.items = scored.into_iter().map(|(_, a, p)| (a, p)).collect();
        self.selected = self.selected.min(self.items.len().saturating_sub(1));
    }
}

pub struct Scm {
    pub items: Vec<(PathBuf, FileStatus)>,
    pub selected: usize,
    pub scroll: usize,
}

pub struct App {
    pub root: PathBuf,
    pub git: GitState,
    pub explorer: Explorer,
    pub scm: Scm,
    pub tests: TestsPane,
    pub debugger: Debugger,
    pub diags: Diagnostics,
    /// Diagnostic under the primary cursor, refreshed each frame for the status bar.
    pub cursor_diag: Option<Diagnostic>,
    /// Dependency-health note under the cursor (Cargo.toml), for the status bar.
    pub cursor_annotation: Option<(String, ratatui::style::Color)>,
    pub deps: DepsService,
    /// (path, editor revision, deps generation) -> rendered annotations.
    dep_cache: Option<(
        (PathBuf, u64, u64),
        Vec<(usize, String, ratatui::style::Color)>,
    )>,
    pub problems: ProblemsPane,
    pub palette: Option<Palette>,
    pub left_mode: LeftMode,
    pub focus: Focus,
    pub tabs: Vec<Tab>,
    pub active: usize,
    pub prompt: Option<Prompt>,
    pub message: Option<(String, Instant)>,
    pub clipboard: Vec<String>,
    pub should_quit: bool,
    pub show_help: bool,
    pub right_content: Rect,
    pub left_rows: usize,
    term_count: usize,
    last_git_refresh: Instant,
    pub left_visible: bool,
    /// Keys pressed so far of a multi-key binding.
    pub pending: Vec<Key>,
    pub keymap: Keymap,
    pub left_width: u16,
    pub trim_trailing_whitespace: bool,
    pub final_newline: bool,
    pub show_whitespace: bool,
    watcher: Option<FsWatcher>,
    /// Paths reported by the watcher, waiting for the debounce window to close.
    fs_pending: HashSet<PathBuf>,
    fs_last: Option<Instant>,
}

const GIT_REFRESH: Duration = Duration::from_secs(3);

impl App {
    pub fn new(root: PathBuf, mut settings: Settings) -> App {
        let git = GitState::load(&root);
        let explorer = Explorer::new(root.clone(), &git);
        let scm = Scm {
            items: git.changes(),
            selected: 0,
            scroll: 0,
        };
        let mut tests = TestsPane::new();
        tests.discover(&root);
        let watcher = FsWatcher::new(&root).ok();
        let mut startup_msg: Option<String> = None;
        if settings.created {
            startup_msg = Some(format!(
                "wrote default settings to {}",
                settings.path.display()
            ));
        }
        let settings_problems: Vec<crate::settings::Problem> = std::mem::take(&mut settings.errors);
        if !settings_problems.is_empty() {
            let n = settings_problems.len();
            startup_msg = Some(format!(
                "{} problem{} in the settings file (ctrl+k e)",
                n,
                if n == 1 { "" } else { "s" }
            ));
        }
        let settings_file = settings.path.clone();
        let mut diags = Diagnostics::new();
        diags.check_cargo(&root);
        let mut app = App {
            root,
            git,
            explorer,
            scm,
            tests,
            debugger: Debugger::new(),
            diags,
            cursor_diag: None,
            cursor_annotation: None,
            deps: DepsService::new(),
            dep_cache: None,
            palette: None,
            problems: ProblemsPane {
                rows: Vec::new(),
                selected: 0,
                scroll: 0,
            },
            left_mode: LeftMode::Explorer,
            focus: Focus::Left,
            tabs: Vec::new(),
            active: 0,
            prompt: None,
            message: None,
            clipboard: Vec::new(),
            should_quit: false,
            show_help: false,
            right_content: Rect::default(),
            left_rows: 0,
            term_count: 0,
            last_git_refresh: Instant::now(),
            left_visible: true,
            pending: Vec::new(),
            keymap: settings.keymap,
            left_width: settings.left_width,
            trim_trailing_whitespace: settings.trim_trailing_whitespace,
            final_newline: settings.final_newline,
            show_whitespace: settings.show_whitespace,
            watcher,
            fs_pending: HashSet::new(),
            fs_last: None,
        };
        app.report_settings_problems(&settings_file, settings_problems);
        if let Some(m) = startup_msg {
            app.set_message(m);
        }
        app
    }

    pub fn set_message(&mut self, msg: impl Into<String>) {
        self.message = Some((msg.into(), Instant::now()));
    }

    pub fn active_tab(&self) -> Option<&Tab> {
        self.tabs.get(self.active)
    }

    pub fn active_tab_mut(&mut self) -> Option<&mut Tab> {
        self.tabs.get_mut(self.active)
    }

    pub fn active_editor_mut(&mut self) -> Option<&mut Editor> {
        match self.active_tab_mut().map(|t| &mut t.kind) {
            Some(TabKind::Editor(e)) => Some(e),
            _ => None,
        }
    }

    /// What the status bar reports as the current activity.
    pub fn activity(&self) -> String {
        if self.tests.running {
            return "running tests".into();
        }
        if let Some(sess) = &self.debugger.session {
            return format!(
                "debug {}",
                match sess.state {
                    crate::debug::State::Launching => "launching",
                    crate::debug::State::Running => "running",
                    crate::debug::State::Stopped => "stopped",
                    crate::debug::State::Exited => "exited",
                }
            );
        }
        if self.prompt.is_some() {
            return "prompt".into();
        }
        match self.focus {
            Focus::Left => self.left_mode.title().to_string(),
            Focus::Right => match self.active_tab().map(|t| &t.kind) {
                Some(TabKind::Editor(e)) => {
                    let preview = self.active_tab().map(|t| t.preview).unwrap_or(false);
                    format!(
                        "{} {}",
                        if preview { "previewing" } else { "editing" },
                        e.lang.name()
                    )
                }
                Some(TabKind::Terminal(_)) => "terminal".into(),
                Some(TabKind::Diff(_)) => "diff".into(),
                Some(TabKind::Output(_)) => "output".into(),
                None => "idle".into(),
            },
        }
    }

    // ---------- periodic work ----------

    pub fn tick(&mut self) {
        if let Some((_, at)) = &self.message
            && at.elapsed() > Duration::from_secs(5)
        {
            self.message = None;
        }
        if self.last_git_refresh.elapsed() > GIT_REFRESH {
            self.refresh_git();
            self.check_open_files();
        }
        self.drain_fs_events();
        self.pump_debugger();
        if self.deps.pump() {
            self.dep_cache = None;
            let ann = self.active_annotations();
            if !ann.is_empty() {
                let outdated = ann.iter().filter(|a| a.1.starts_with('⬆')).count();
                let yanked = ann.iter().filter(|a| a.1.starts_with('⚠')).count();
                let fetching = ann.iter().filter(|a| a.1 == "…").count();
                if fetching == 0 {
                    let mut parts = vec![format!("{} dependencies", ann.len())];
                    if outdated > 0 {
                        parts.push(format!("{} outdated", outdated));
                    }
                    if yanked > 0 {
                        parts.push(format!("{} yanked", yanked));
                    }
                    if outdated == 0 && yanked == 0 {
                        parts.push("all current".into());
                    }
                    self.set_message(parts.join(", "));
                }
            }
        }
        if self.left_mode == LeftMode::Problems {
            self.rebuild_problems();
        }
        let root = self.root.clone();
        if self.diags.pump(&root) {
            let (e, w) = self.diags.counts();
            if e + w > 0 {
                self.set_message(format!(
                    "check: {} error{}, {} warning{}",
                    e,
                    if e == 1 { "" } else { "s" },
                    w,
                    if w == 1 { "" } else { "s" }
                ));
            }
        }
        self.drain_test_events();
        // drop terminals whose shell exited
        let mut i = 0;
        while i < self.tabs.len() {
            if matches!(&self.tabs[i].kind, TabKind::Terminal(t) if t.exited()) {
                self.tabs.remove(i);
                if self.active >= self.tabs.len() {
                    self.active = self.tabs.len().saturating_sub(1);
                }
            } else {
                i += 1;
            }
        }
    }

    /// Collect watcher events and act once they have been quiet for a moment.
    fn drain_fs_events(&mut self) {
        if let Some(w) = &self.watcher {
            while let Ok(p) = w.rx.try_recv() {
                self.fs_pending.insert(p);
                self.fs_last = Some(Instant::now());
            }
        }
        let quiet = self
            .fs_last
            .map(|t| t.elapsed() > Duration::from_millis(150))
            .unwrap_or(false);
        if self.fs_pending.is_empty() || !quiet {
            return;
        }
        let changed: Vec<PathBuf> = self.fs_pending.drain().collect();
        self.fs_last = None;
        self.handle_fs_changes(&changed);
    }

    fn handle_fs_changes(&mut self, changed: &[PathBuf]) {
        // git state and the tree: anything at all may have changed status
        self.refresh_git();
        self.check_open_files();
        // diffs against HEAD depend on both the file and the repository
        for i in 0..self.tabs.len() {
            if let TabKind::Diff(d) = &mut self.tabs[i].kind {
                let old = crate::git::head_contents(&self.root, &d.path).unwrap_or_default();
                let new = std::fs::read_to_string(&d.path).unwrap_or_default();
                d.refresh(&old, &new);
            }
        }
        // tests: rediscover when source changed
        let source_changed = changed.iter().any(|p| {
            matches!(
                p.extension().and_then(|e| e.to_str()),
                Some("rs") | Some("py")
            )
        });
        if source_changed && !self.tests.running {
            self.tests.discover(&self.root);
        }
        if changed
            .iter()
            .any(|p| p.extension().map(|e| e == "rs").unwrap_or(false))
        {
            let root = self.root.clone();
            self.diags.check_cargo(&root);
        }
        for p in changed
            .iter()
            .filter(|p| p.extension().map(|e| e == "py").unwrap_or(false) && p.exists())
        {
            let root = self.root.clone();
            self.diags.check_python(&root, p);
        }
        for p in changed
            .iter()
            .filter(|p| p.exists() && crate::diagnostics::is_data_file(p))
        {
            self.diags.check_data(p);
        }
        for p in changed.iter().filter(|p| p.is_file()) {
            let head = read_head(p);
            if crate::editor::highlight::Lang::detect(p, &head)
                == crate::editor::highlight::Lang::Shell
            {
                let root = self.root.clone();
                self.diags.check_shell(&root, p);
            }
        }
    }

    /// Reload open buffers whose file changed on disk; flag the ones with unsaved edits.
    pub fn check_open_files(&mut self) {
        let mut messages: Vec<String> = Vec::new();
        for tab in self.tabs.iter_mut() {
            let TabKind::Editor(ed) = &mut tab.kind else {
                continue;
            };
            if !ed.disk_changed() {
                continue;
            }
            let Some(path) = ed.path.clone() else {
                continue;
            };
            if !path.exists() {
                if !ed.conflict {
                    ed.conflict = true;
                    ed.modified = true;
                    messages.push(format!("{} was deleted on disk", ed.title()));
                }
                continue;
            }
            if ed.modified {
                if !ed.conflict {
                    ed.conflict = true;
                    messages.push(format!(
                        "{} changed on disk; ctrl+k r reloads, ctrl+s overwrites",
                        ed.title()
                    ));
                }
            } else {
                match ed.reload() {
                    Ok(()) => messages.push(format!("reloaded {}", ed.title())),
                    Err(e) => messages.push(format!("reload failed: {}", e)),
                }
            }
        }
        if let Some(m) = messages.pop() {
            self.set_message(m);
        }
    }

    fn pump_debugger(&mut self) {
        let root = self.root.clone();
        self.debugger.pump(&root);
        if !self.debugger.output.is_empty() {
            let lines: Vec<String> = std::mem::take(&mut self.debugger.output);
            let out = self.output_tab_mut("debug");
            out.lines.extend(lines);
        }
        if let Some(msg) = self.debugger.status.take() {
            self.set_message(msg);
        }
        if let Some((path, line)) = self.debugger.jump.take() {
            let keep_focus = self.focus;
            self.open_file_at(&path, line);
            let rows = self.right_content.height as usize;
            if let Some(ed) = self.active_editor_mut() {
                // centre the stop line
                ed.scroll_line = line.saturating_sub(1).saturating_sub(rows / 2);
            }
            self.focus = keep_focus;
        }
    }

    /// F5: continue when stopped, otherwise launch the selected/last target.
    fn debug_start_or_continue(&mut self) {
        if let Some(sess) = &self.debugger.session {
            match sess.state {
                crate::debug::State::Stopped => self.debugger.continue_(),
                crate::debug::State::Running => {
                    self.set_message("running; ctrl+k x stops, F8 pauses")
                }
                _ => {}
            }
            return;
        }
        if self.left_mode != LeftMode::Debug {
            self.show_left(LeftMode::Debug);
        }
        let target = match self.debugger.selected_row() {
            Some(DebugRow::Target(i)) => self.debugger.targets.get(*i).cloned(),
            _ => None,
        };
        let root = self.root.clone();
        let msg = self.debugger.launch(&root, target);
        self.set_message(msg);
    }

    fn toggle_breakpoint_here(&mut self) {
        let loc = match self.active_tab().map(|t| &t.kind) {
            Some(TabKind::Editor(e)) if self.focus == Focus::Right => {
                e.path.clone().map(|p| (p, e.primary().pos.line + 1))
            }
            _ => None,
        };
        match loc {
            Some((p, l)) => self.debugger.toggle_breakpoint(&p, l),
            None => {
                if let Some(DebugRow::Breakpoint(p, l)) = self.debugger.selected_row().cloned() {
                    self.debugger.toggle_breakpoint(&p, l);
                } else {
                    self.set_message("open a file to set a breakpoint");
                }
            }
        }
    }

    /// Format the active buffer with the formatter for its language.
    fn format_active(&mut self) {
        let root = self.root.clone();
        let Some(ed) = self.active_editor_mut() else {
            self.set_message("format works on editor tabs");
            return;
        };
        let lang = ed.lang;
        let text = ed.text();
        let path = ed.path.clone();
        let outcome = crate::format::format(lang, &text, &root, path.as_deref());
        match outcome {
            crate::format::Outcome::Formatted(new) => {
                let n = ed.apply_text(&new);
                self.set_message(format!(
                    "formatted {} ({} change{})",
                    lang.name(),
                    n,
                    if n == 1 { "" } else { "s" }
                ));
            }
            crate::format::Outcome::Unchanged => self.set_message("already formatted"),
            crate::format::Outcome::Unavailable(tool) => self.set_message(format!(
                "no formatter for {}: install {}",
                lang.name(),
                tool
            )),
            crate::format::Outcome::Failed(msg) => self.set_message(msg),
        }
    }

    /// Put settings problems in the problems pane, where they stay visible.
    fn report_settings_problems(&mut self, path: &Path, problems: Vec<crate::settings::Problem>) {
        let diags: Vec<Diagnostic> = problems
            .into_iter()
            .map(|p| {
                let line = p.line.unwrap_or(0);
                Diagnostic {
                    line,
                    col: 0,
                    end_line: line,
                    end_col: 200,
                    severity: crate::diagnostics::Severity::Error,
                    message: p.message,
                }
            })
            .collect();
        self.diags.set_settings(path, diags);
    }

    /// Re-read the settings file and put it into effect.
    fn apply_settings(&mut self) {
        let s = crate::settings::load();
        self.keymap = s.keymap;
        self.left_width = s.left_width;
        self.trim_trailing_whitespace = s.trim_trailing_whitespace;
        self.final_newline = s.final_newline;
        self.show_whitespace = s.show_whitespace;
        self.pending.clear();
        let n = s.errors.len();
        let path = s.path.clone();
        self.report_settings_problems(&path, s.errors);
        if n == 0 {
            self.set_message("settings applied");
        } else {
            self.set_message(format!(
                "settings applied; {} problem{} (ctrl+k e)",
                n,
                if n == 1 { "" } else { "s" }
            ));
        }
    }

    /// Run the checker that applies to a file, given its detected language.
    fn check_file(&mut self, path: Option<&Path>, lang: crate::editor::highlight::Lang) {
        use crate::editor::highlight::Lang;
        let Some(path) = path else { return };
        let root = self.root.clone();
        if crate::diagnostics::is_data_file(path) {
            self.diags.check_data(path);
            return;
        }
        match lang {
            Lang::Rust => self.diags.check_cargo(&root),
            Lang::Python => self.diags.check_python(&root, path),
            Lang::Shell => self.diags.check_shell(&root, path),
            _ => {}
        }
    }

    /// Inline notes for a Cargo.toml buffer: one per dependency line.
    pub fn active_annotations(&mut self) -> Vec<(usize, String, ratatui::style::Color)> {
        let (path, revision, text) = match self.active_tab().map(|t| &t.kind) {
            Some(TabKind::Editor(e))
                if e.path
                    .as_ref()
                    .and_then(|p| p.file_name())
                    .map(|n| n == "Cargo.toml")
                    .unwrap_or(false) =>
            {
                (e.path.clone().unwrap(), e.revision, e.text())
            }
            _ => return Vec::new(),
        };
        let key = (path.clone(), revision, self.deps.generation);
        if let Some((k, v)) = &self.dep_cache
            && *k == key
        {
            return v.clone();
        }
        let deps = crate::deps::parse_cargo_toml(&text);
        let names: Vec<String> = deps
            .iter()
            .filter(|d| d.source == crate::deps::Source::Registry)
            .map(|d| d.name.clone())
            .collect();
        let started = self.deps.request(&names);
        if started > 0 {
            self.set_message(format!(
                "checking crates.io for {} crate{}…",
                started,
                if started == 1 { "" } else { "s" }
            ));
        }
        // the lock file next to this manifest, else the project's
        let lock_path = path
            .parent()
            .map(|p| p.join("Cargo.lock"))
            .filter(|p| p.exists())
            .unwrap_or_else(|| self.root.join("Cargo.lock"));
        let locked = std::fs::read_to_string(&lock_path)
            .map(|t| crate::deps::parse_lock(&t))
            .unwrap_or_default();
        let ann: Vec<(usize, String, ratatui::style::Color)> = deps
            .iter()
            .map(|d| {
                let h: Health = self.deps.health(d, &locked);
                (d.line, h.text(), h.color())
            })
            .collect();
        self.dep_cache = Some((key, ann.clone()));
        ann
    }

    /// Diagnostics for the active editor: compiler results plus live syntax errors.
    pub fn active_diagnostics(&mut self) -> Vec<Diagnostic> {
        let path = match self.active_tab().map(|t| &t.kind) {
            Some(TabKind::Editor(e)) => e.path.clone(),
            _ => return Vec::new(),
        };
        let mut v = path
            .as_ref()
            .map(|p| self.diags.for_file(p))
            .unwrap_or_default();
        if let Some(ed) = self.active_editor_mut() {
            v.extend(ed.syntax_diagnostics());
        }
        v.sort_by_key(|d| (d.line, d.col));
        v
    }

    /// Jump to the next (or previous) diagnostic in the active file, wrapping around.
    fn goto_diagnostic(&mut self, forward: bool) {
        let diags = self.active_diagnostics();
        let Some(ed) = self.active_editor_mut() else {
            return;
        };
        if diags.is_empty() {
            self.set_message("no problems in this file");
            return;
        }
        let cur = ed.primary().pos;
        let target = if forward {
            diags
                .iter()
                .find(|d| (d.line, d.col) > (cur.line, cur.col))
                .or(diags.first())
        } else {
            diags
                .iter()
                .rev()
                .find(|d| (d.line, d.col) < (cur.line, cur.col))
                .or(diags.last())
        };
        if let Some(d) = target.cloned() {
            ed.goto_line(d.line + 1);
            ed.cursors[0].pos.col = d.col.min(ed.line_len(d.line));
            self.set_message(d.message);
        }
    }

    /// Every diagnostic in the project plus live syntax errors from open buffers, by file.
    fn all_problems(&mut self) -> Vec<(PathBuf, Diagnostic)> {
        let mut all = self.diags.all();
        for tab in self.tabs.iter_mut() {
            if let TabKind::Editor(ed) = &mut tab.kind
                && let Some(p) = ed.path.clone()
            {
                for d in ed.syntax_diagnostics() {
                    all.push((p.clone(), d));
                }
            }
        }
        all.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then(a.1.line.cmp(&b.1.line))
                .then(a.1.col.cmp(&b.1.col))
        });
        all.dedup();
        all
    }

    fn rebuild_problems(&mut self) {
        let all = self.all_problems();
        let mut rows: Vec<ProblemRow> = Vec::new();
        let mut i = 0;
        while i < all.len() {
            let file = all[i].0.clone();
            let mut j = i;
            while j < all.len() && all[j].0 == file {
                j += 1;
            }
            rows.push(ProblemRow::File(file.clone(), j - i));
            for (p, d) in &all[i..j] {
                rows.push(ProblemRow::Item(p.clone(), d.clone()));
            }
            i = j;
        }
        // keep the selection on the same diagnostic when the list changes
        let prev = self.problems.rows.get(self.problems.selected).cloned();
        self.problems.rows = rows;
        let n = self.problems.rows.len();
        if let Some(ProblemRow::Item(pp, pd)) = prev
            && let Some(k) = self.problems.rows.iter().position(|r| matches!(r, ProblemRow::Item(p, d) if *p == pp && d.line == pd.line && d.message == pd.message)) {
                self.problems.selected = k;
            }
        self.problems.selected = self.problems.selected.min(n.saturating_sub(1));
        if n > 0
            && matches!(
                self.problems.rows[self.problems.selected],
                ProblemRow::File(..)
            )
        {
            self.problems.move_by(1);
        }
    }

    fn open_problem(&mut self) {
        if let Some(ProblemRow::Item(p, d)) =
            self.problems.rows.get(self.problems.selected).cloned()
        {
            self.open_file_at(&p, d.line + 1);
            if let Some(ed) = self.active_editor_mut() {
                ed.cursors[0].pos.col = d.col.min(ed.line_len(d.line));
            }
            self.focus = Focus::Left;
            self.set_message(d.message);
        }
    }

    /// Open the output tab with the compiler's full rendered messages.
    fn show_problems(&mut self) {
        let all = self.diags.all();
        let root = self.root.clone();
        let mut lines: Vec<String> = Vec::new();
        for (p, d) in &all {
            let rel = p.strip_prefix(&root).unwrap_or(p).display();
            let lvl = match d.severity {
                crate::diagnostics::Severity::Error => "error",
                crate::diagnostics::Severity::Warning => "warning",
            };
            lines.push(format!(
                "{}:{}:{}: {}: {}",
                rel,
                d.line + 1,
                d.col + 1,
                lvl,
                d.message
            ));
        }
        if lines.is_empty() {
            lines.push("no problems".into());
        }
        if !self.diags.rendered.is_empty() {
            lines.push(String::new());
            lines.extend(self.diags.rendered.iter().cloned());
        }
        let out = self.output_tab_mut("problems");
        out.lines = lines;
        out.scroll = 0;
        out.follow = false;
        let idx = self
            .tabs
            .iter()
            .position(|t| matches!(&t.kind, TabKind::Output(o) if o.title == "problems"))
            .unwrap();
        self.active = idx;
        self.focus = Focus::Right;
    }

    pub fn refresh_git(&mut self) {
        self.git = GitState::load(&self.root);
        self.explorer.refresh(&self.git);
        self.scm.items = self.git.changes();
        self.scm.selected = self
            .scm
            .selected
            .min(self.scm.items.len().saturating_sub(1));
        self.last_git_refresh = Instant::now();
    }

    fn output_tab_mut(&mut self, title: &str) -> &mut OutputTab {
        let idx = self
            .tabs
            .iter()
            .position(|t| matches!(&t.kind, TabKind::Output(o) if o.title == title));
        let idx = match idx {
            Some(i) => i,
            None => {
                self.tabs.push(Tab::new(
                    TabKind::Output(OutputTab {
                        title: title.to_string(),
                        lines: Vec::new(),
                        scroll: 0,
                        follow: true,
                    }),
                    false,
                ));
                self.tabs.len() - 1
            }
        };
        match &mut self.tabs[idx].kind {
            TabKind::Output(o) => o,
            _ => unreachable!(),
        }
    }

    fn drain_test_events(&mut self) {
        let mut events = Vec::new();
        if let Some(rx) = &self.tests.rx {
            while let Ok(ev) = rx.try_recv() {
                events.push(ev);
            }
        }
        for ev in events {
            match ev {
                TestEvent::Line(l) => self.output_tab_mut("tests").lines.push(l),
                TestEvent::Result { filter, passed } => self.tests.apply_result(&filter, passed),
                TestEvent::Done(code) => {
                    self.tests.finish();
                    self.tests.rx = None;
                    let out = self.output_tab_mut("tests");
                    out.lines.push(format!("[exit {}]", code));
                    out.lines.push(String::new());
                    self.set_message(if code == 0 {
                        "tests passed"
                    } else {
                        "tests failed"
                    });
                }
            }
        }
    }

    // ---------- tabs ----------

    pub fn open_file(&mut self, path: &Path) {
        if let Some(i) = self
            .tabs
            .iter()
            .position(|t| matches!(&t.kind, TabKind::Editor(e) if e.path.as_deref() == Some(path)))
        {
            self.active = i;
            self.focus = Focus::Right;
            return;
        }
        match Editor::open(path) {
            Ok(ed) => {
                self.tabs.push(Tab::new(TabKind::Editor(ed), false));
                self.active = self.tabs.len() - 1;
                self.focus = Focus::Right;
                let lang = match &self.tabs[self.active].kind {
                    TabKind::Editor(e) => e.lang,
                    _ => crate::editor::highlight::Lang::Plain,
                };
                if !matches!(lang, crate::editor::highlight::Lang::Rust) {
                    self.check_file(Some(path), lang);
                }
            }
            Err(e) => self.set_message(format!("open failed: {}", e)),
        }
    }

    pub fn open_file_at(&mut self, path: &Path, line: usize) {
        self.open_file(path);
        if let Some(ed) = self.active_editor_mut() {
            ed.goto_line(line);
        }
    }

    pub fn new_file(&mut self) {
        self.tabs
            .push(Tab::new(TabKind::Editor(Editor::new()), false));
        self.active = self.tabs.len() - 1;
        self.focus = Focus::Right;
    }

    pub fn open_terminal(&mut self) {
        self.term_count += 1;
        let title = format!("term {}", self.term_count);
        let rows = self.right_content.height.max(2);
        let cols = self.right_content.width.max(10);
        match TerminalTab::spawn(&self.root, title, rows, cols) {
            Ok(t) => {
                self.tabs.push(Tab::new(TabKind::Terminal(t), true));
                self.active = self.tabs.len() - 1;
                self.focus = Focus::Right;
            }
            Err(e) => self.set_message(format!("terminal failed: {}", e)),
        }
    }

    pub fn open_diff(&mut self, path: &Path) {
        if let Some(i) = self
            .tabs
            .iter()
            .position(|t| matches!(&t.kind, TabKind::Diff(d) if d.path == path))
        {
            self.tabs.remove(i);
            if self.active > i {
                self.active -= 1;
            }
        }
        let old = crate::git::head_contents(&self.root, path).unwrap_or_default();
        let new = std::fs::read_to_string(path).unwrap_or_default();
        self.tabs.push(Tab::new(
            TabKind::Diff(DiffTab::new(path.to_path_buf(), &old, &new)),
            false,
        ));
        self.active = self.tabs.len() - 1;
        self.focus = Focus::Right;
    }

    pub fn next_tab(&mut self, delta: isize) {
        if self.tabs.is_empty() {
            return;
        }
        let n = self.tabs.len() as isize;
        self.active = ((self.active as isize + delta).rem_euclid(n)) as usize;
    }

    /// Alt+w: close the active tab, or just move on if it is pinned.
    pub fn close_tab(&mut self) {
        let Some(tab) = self.tabs.get(self.active) else {
            return;
        };
        if tab.pinned {
            self.next_tab(1);
            return;
        }
        if tab.modified() {
            self.prompt = Some(Prompt {
                kind: PromptKind::ConfirmClose(self.active),
                label: "discard unsaved changes? (y/n)".into(),
                input: String::new(),
            });
            return;
        }
        self.force_close(self.active);
    }

    fn force_close(&mut self, i: usize) {
        if i >= self.tabs.len() {
            return;
        }
        self.tabs.remove(i);
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len().saturating_sub(1);
        }
        if self.tabs.is_empty() {
            self.focus = Focus::Left;
        }
    }

    pub fn toggle_pin(&mut self) {
        if let Some(t) = self.active_tab_mut() {
            t.pinned = !t.pinned;
        }
    }

    pub fn save(&mut self) {
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return;
        };
        if let TabKind::Editor(ed) = &mut tab.kind {
            if ed.path.is_none() {
                self.prompt = Some(Prompt {
                    kind: PromptKind::SaveAs,
                    label: "save as".into(),
                    input: self.root.to_string_lossy().into_owned() + "/",
                });
                return;
            }
            if ed.disk_changed() && ed.path.as_ref().map(|p| p.exists()).unwrap_or(false) {
                self.prompt = Some(Prompt {
                    kind: PromptKind::ConfirmOverwrite,
                    label: "file changed on disk; overwrite? (y/n)".into(),
                    input: String::new(),
                });
                return;
            }
            self.save_force();
        }
    }

    fn save_force(&mut self) {
        let trim = self.trim_trailing_whitespace;
        let final_newline = self.final_newline;
        let Some(ed) = self.active_editor_mut() else {
            return;
        };
        let cleanup = ed.clean_for_save(trim, final_newline);
        match ed.save() {
            Ok(()) => {
                let mut t = ed.title();
                let mut notes = Vec::new();
                if cleanup.trimmed_lines > 0 {
                    notes.push(format!(
                        "trimmed {} line{}",
                        cleanup.trimmed_lines,
                        if cleanup.trimmed_lines == 1 { "" } else { "s" }
                    ));
                }
                if cleanup.added_newline {
                    notes.push("added final newline".to_string());
                }
                if !notes.is_empty() {
                    t = format!("{} ({})", t, notes.join(", "));
                }
                let path = ed.path.clone();
                let lang = ed.lang;
                self.set_message(format!("saved {}", t));
                self.refresh_git();
                self.tests.discover(&self.root);
                self.check_file(path.as_deref(), lang);
                if path.as_deref() == Some(crate::settings::settings_path().as_path()) {
                    self.apply_settings();
                }
            }
            Err(e) => self.set_message(format!("save failed: {}", e)),
        }
    }

    fn reload_active(&mut self) {
        let Some(ed) = self.active_editor_mut() else {
            return;
        };
        match ed.reload() {
            Ok(()) => {
                let t = ed.title();
                self.set_message(format!("reloaded {} from disk", t));
            }
            Err(e) => self.set_message(format!("reload failed: {}", e)),
        }
    }

    pub fn request_quit(&mut self) {
        if self.tabs.iter().any(|t| t.modified()) {
            self.prompt = Some(Prompt {
                kind: PromptKind::ConfirmQuit,
                label: "unsaved changes; quit anyway? (y/n)".into(),
                input: String::new(),
            });
        } else {
            self.should_quit = true;
        }
    }

    // ---------- key handling ----------

    pub fn handle_key(&mut self, ev: KeyEvent) {
        if self.prompt.is_some() {
            self.handle_prompt_key(ev);
            return;
        }
        if self.show_help {
            self.show_help = false;
            return;
        }
        if self.palette.is_some() {
            self.handle_palette_key(ev);
            return;
        }
        let key = Key::from_event(ev);
        let in_terminal = matches!(
            self.active_tab().map(|t| &t.kind),
            Some(TabKind::Terminal(_))
        ) && self.focus == Focus::Right;

        // second key of a chord
        if !self.pending.is_empty() {
            let pending = std::mem::take(&mut self.pending);
            match self.keymap.lookup(&pending, key) {
                Lookup::Action(a) => self.perform(a, false),
                Lookup::Prefix => {
                    self.pending = pending;
                    self.pending.push(key);
                }
                Lookup::None => {
                    let seq = pending
                        .iter()
                        .chain(std::iter::once(&key))
                        .map(|k| k.label())
                        .collect::<Vec<_>>()
                        .join(" ");
                    self.set_message(format!("{} is not bound; F1 for keys", seq));
                }
            }
            return;
        }

        // Inside a terminal only single-key, terminal-safe bindings are intercepted;
        // everything else (including chord prefixes like ctrl+k) goes to the shell.
        match self.keymap.lookup(&[], key) {
            Lookup::Action(a) if !in_terminal || a.terminal_safe() => {
                self.perform(a, false);
                return;
            }
            Lookup::Prefix if !in_terminal => {
                self.pending.push(key);
                return;
            }
            _ => {}
        }
        // shift + a movement binding extends the selection
        if key.shift()
            && !in_terminal
            && let Lookup::Action(a) = self.keymap.lookup(&[], key.without_shift())
            && a.is_movement()
        {
            self.perform(a, true);
            return;
        }

        let alt = ev.modifiers.contains(KeyModifiers::ALT);
        let shift = ev.modifiers.contains(KeyModifiers::SHIFT);
        match self.focus {
            Focus::Left => self.handle_left_key(ev, alt, shift),
            Focus::Right => self.handle_right_key(ev),
        }
    }

    /// Execute a named action. `extend` is set for shift+movement in the editor.
    fn perform(&mut self, action: Action, extend: bool) {
        match action {
            Action::Help => self.show_help = true,
            Action::Palette => {
                let mut p = Palette {
                    query: String::new(),
                    selected: 0,
                    scroll: 0,
                    items: Vec::new(),
                };
                p.refilter();
                self.palette = Some(p);
            }
            Action::Explorer => self.show_left(LeftMode::Explorer),
            Action::Scm => self.show_left(LeftMode::Scm),
            Action::Tests => self.show_left(LeftMode::Tests),
            Action::Debug => self.show_left(LeftMode::Debug),
            Action::Problems => self.show_left(LeftMode::Problems),
            Action::FocusToggle => self.toggle_focus(),
            Action::LeftToggle => self.toggle_left_visible(),
            Action::Terminal => self.open_terminal(),
            Action::CloseTab => self.close_tab(),
            Action::PinTab => self.toggle_pin(),
            Action::PrevTab => {
                self.next_tab(-1);
                self.focus = Focus::Right;
            }
            Action::NextTab => {
                self.next_tab(1);
                self.focus = Focus::Right;
            }
            Action::Open => {
                self.prompt = Some(Prompt {
                    kind: PromptKind::Open,
                    label: "open".into(),
                    input: self.root.to_string_lossy().into_owned() + "/",
                });
            }
            Action::NewFile => self.new_file(),
            Action::Save => self.save(),
            Action::SaveAs => {
                let initial = match self.active_tab().map(|t| &t.kind) {
                    Some(TabKind::Editor(e)) => {
                        e.path.as_ref().map(|p| p.to_string_lossy().into_owned())
                    }
                    _ => None,
                };
                let initial =
                    initial.unwrap_or_else(|| self.root.to_string_lossy().into_owned() + "/");
                self.prompt = Some(Prompt {
                    kind: PromptKind::SaveAs,
                    label: "save as".into(),
                    input: initial,
                });
            }
            Action::Reload => self.reload_active(),
            Action::Format => self.format_active(),
            Action::ToggleWhitespace => {
                self.show_whitespace = !self.show_whitespace;
                self.set_message(if self.show_whitespace {
                    "showing whitespace (set show_whitespace in settings to keep)"
                } else {
                    "hiding whitespace"
                });
            }
            Action::RunTests => self.run_tests(true),
            Action::ExpandAll => match self.left_mode {
                LeftMode::Explorer => self.explorer.toggle_all(&self.git),
                LeftMode::Tests => self.tests.toggle_all(),
                _ => {}
            },
            Action::Preview => {
                let Some(tab) = self.tabs.get_mut(self.active) else {
                    return;
                };
                let lang = match &tab.kind {
                    TabKind::Editor(e) => e.lang,
                    _ => {
                        self.set_message("preview works on editor tabs");
                        return;
                    }
                };
                if !crate::preview::supported(lang) {
                    self.set_message("no preview for this file type (markdown, json, toml, xml)");
                    return;
                }
                tab.preview = !tab.preview;
                self.focus = Focus::Right;
                let on = tab.preview;
                self.set_message(if on {
                    "preview; ctrl+k v returns to the editor"
                } else {
                    "editing"
                });
            }
            Action::Settings => {
                let path = crate::settings::settings_path();
                if !path.exists()
                    && let Err(e) = std::fs::write(&path, crate::settings::default_text())
                {
                    self.set_message(format!("could not create {}: {}", path.display(), e));
                    return;
                }
                self.open_file(&path);
                self.set_message("edit, then ctrl+s to apply");
            }
            Action::Quit => self.request_quit(),
            Action::DebugContinue => self.debug_start_or_continue(),
            Action::StepOver => self.debugger.step_over(),
            Action::StepIn => self.debugger.step_in(),
            Action::StepOut => self.debugger.step_out(),
            Action::DebugStop => self.debugger.stop(),
            Action::Breakpoint => self.toggle_breakpoint_here(),
            Action::NextProblem => self.goto_diagnostic(true),
            Action::PrevProblem => self.goto_diagnostic(false),
            Action::ProblemsOutput => self.show_problems(),
            Action::CargoCheck => {
                let root = self.root.clone();
                self.diags.check_cargo(&root);
                self.set_message("cargo check started");
            }
            // editor actions
            Action::Copy => {
                if let Some(ed) = self.active_editor_mut() {
                    let chunks = ed.copy_chunks();
                    let n = chunks.len();
                    self.clipboard = chunks;
                    self.set_message(format!(
                        "copied {} selection{}",
                        n,
                        if n == 1 { "" } else { "s" }
                    ));
                }
            }
            Action::Cut => {
                if let Some(ed) = self.active_editor_mut() {
                    self.clipboard = ed.cut();
                }
            }
            Action::Paste => {
                let clip = self.clipboard.clone();
                if let Some(ed) = self.active_editor_mut() {
                    ed.paste(&clip);
                }
            }
            Action::Find => {
                let initial = self
                    .active_editor_mut()
                    .and_then(|e| e.search.clone())
                    .unwrap_or_default();
                if self.active_editor_mut().is_some() {
                    self.prompt = Some(Prompt {
                        kind: PromptKind::Find,
                        label: "find".into(),
                        input: initial,
                    });
                }
            }
            Action::FindNext => {
                let search = self.active_editor_mut().and_then(|e| e.search.clone());
                match search {
                    Some(s) => {
                        if let Some(ed) = self.active_editor_mut()
                            && !ed.find_next(&s)
                        {
                            self.set_message("not found");
                        }
                    }
                    None => self.perform(Action::Find, false),
                }
            }
            Action::GotoLine => {
                if self.active_editor_mut().is_some() {
                    self.prompt = Some(Prompt {
                        kind: PromptKind::Goto,
                        label: "go to line".into(),
                        input: String::new(),
                    });
                }
            }
            _ => {
                let Some(ed) = self.active_editor_mut() else {
                    return;
                };
                match action {
                    Action::WordLeft => ed.move_word_left(extend),
                    Action::WordRight => ed.move_word_right(extend),
                    Action::DocStart => ed.doc_start(extend),
                    Action::DocEnd => ed.doc_end(extend),
                    Action::MoveLineUp => ed.move_lines(false),
                    Action::MoveLineDown => ed.move_lines(true),
                    Action::AddCursorUp => ed.add_cursor_vertical(false),
                    Action::AddCursorDown => ed.add_cursor_vertical(true),
                    Action::LineEnds => ed.cursors_at_line_ends(),
                    Action::SelectNext => ed.select_next_match(),
                    Action::SelectAll => ed.select_all(),
                    Action::Undo => ed.undo(),
                    Action::Redo => ed.redo(),
                    Action::ToggleComment => ed.toggle_comment(),
                    Action::DeleteLine => ed.delete_lines(),
                    Action::DeleteWordBack => ed.delete_word_back(),
                    _ => {}
                }
            }
        }
    }

    fn show_left(&mut self, mode: LeftMode) {
        self.left_mode = mode;
        self.left_visible = true;
        self.focus = Focus::Left;
        match mode {
            LeftMode::Explorer => {
                if let Some(TabKind::Editor(e)) = self.active_tab().map(|t| &t.kind)
                    && let Some(p) = e.path.clone()
                {
                    self.explorer.reveal(&p, &self.git);
                }
            }
            LeftMode::Scm => self.refresh_git(),
            LeftMode::Tests => self.tests.discover(&self.root),
            LeftMode::Problems => self.rebuild_problems(),
            LeftMode::Debug => {
                let active = match self.active_tab().map(|t| &t.kind) {
                    Some(TabKind::Editor(e)) => e.path.clone(),
                    _ => None,
                };
                let root = self.root.clone();
                self.debugger.discover(&root, active.as_deref());
            }
        }
    }

    fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            Focus::Left if !self.tabs.is_empty() => Focus::Right,
            _ => {
                self.left_visible = true;
                Focus::Left
            }
        };
    }

    fn toggle_left_visible(&mut self) {
        self.left_visible = !self.left_visible;
        if !self.left_visible && !self.tabs.is_empty() {
            self.focus = Focus::Right;
        }
    }

    fn handle_left_key(&mut self, key: KeyEvent, alt: bool, shift: bool) {
        let page = self.left_rows.max(1) as isize;
        match self.left_mode {
            LeftMode::Explorer => match key.code {
                KeyCode::Up | KeyCode::Char('k') => self.explorer.move_by(-1),
                KeyCode::Down | KeyCode::Char('j') => self.explorer.move_by(1),
                KeyCode::PageUp => self.explorer.move_by(-page),
                KeyCode::PageDown => self.explorer.move_by(page),
                KeyCode::Home => self.explorer.home(),
                KeyCode::End => self.explorer.end(),
                KeyCode::Left => self.explorer.collapse_or_parent(&self.git),
                KeyCode::Right => self.explorer.expand(&self.git),
                KeyCode::Char('E') | KeyCode::Char('*') => self.explorer.toggle_all(&self.git),
                KeyCode::Char('e') if alt && shift => self.explorer.toggle_all(&self.git),
                KeyCode::Char('e') => self.explorer.toggle(&self.git),
                KeyCode::Char('r') => self.refresh_git(),
                KeyCode::Enter | KeyCode::Char(' ') => {
                    if let Some(row) = self.explorer.selected_row() {
                        if row.is_dir {
                            self.explorer.toggle(&self.git);
                        } else if row.phantom {
                            let p = row.path.clone();
                            self.open_diff(&p);
                        } else {
                            let p = row.path.clone();
                            self.open_file(&p);
                        }
                    }
                }
                KeyCode::Char('d') => {
                    if let Some(row) = self.explorer.selected_row()
                        && !row.is_dir
                    {
                        let p = row.path.clone();
                        self.open_diff(&p);
                    }
                }
                KeyCode::Esc if !self.tabs.is_empty() => {
                    self.focus = Focus::Right;
                }
                _ => {}
            },
            LeftMode::Scm => match key.code {
                KeyCode::Up | KeyCode::Char('k') => {
                    self.scm.selected = self.scm.selected.saturating_sub(1)
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    if self.scm.selected + 1 < self.scm.items.len() {
                        self.scm.selected += 1;
                    }
                }
                KeyCode::Home => self.scm.selected = 0,
                KeyCode::End => self.scm.selected = self.scm.items.len().saturating_sub(1),
                KeyCode::Char('r') => self.refresh_git(),
                KeyCode::Enter | KeyCode::Char(' ') => {
                    if let Some((p, _)) = self.scm.items.get(self.scm.selected).cloned() {
                        self.open_diff(&p);
                    }
                }
                KeyCode::Char('o') => {
                    if let Some((p, st)) = self.scm.items.get(self.scm.selected).cloned()
                        && st != FileStatus::Deleted
                    {
                        self.open_file(&p);
                    }
                }
                KeyCode::Esc if !self.tabs.is_empty() => {
                    self.focus = Focus::Right;
                }
                _ => {}
            },
            LeftMode::Problems => match key.code {
                KeyCode::Up | KeyCode::Char('k') => self.problems.move_by(-1),
                KeyCode::Down | KeyCode::Char('j') => self.problems.move_by(1),
                KeyCode::PageUp => self.problems.move_by(-page),
                KeyCode::PageDown => self.problems.move_by(page),
                KeyCode::Home => {
                    self.problems.selected = 0;
                    self.problems.move_by(1);
                }
                KeyCode::End => self.problems.selected = self.problems.rows.len().saturating_sub(1),
                KeyCode::Enter | KeyCode::Char(' ') => self.open_problem(),
                KeyCode::Char('o') => self.show_problems(),
                KeyCode::Char('r') => {
                    let root = self.root.clone();
                    self.diags.check_cargo(&root);
                    self.set_message("cargo check started");
                }
                KeyCode::Esc if !self.tabs.is_empty() => {
                    self.focus = Focus::Right;
                }
                _ => {}
            },
            LeftMode::Debug => match key.code {
                KeyCode::Up | KeyCode::Char('k') => self.debugger.move_by(-1),
                KeyCode::Down | KeyCode::Char('j') => self.debugger.move_by(1),
                KeyCode::PageUp => self.debugger.move_by(-page),
                KeyCode::PageDown => self.debugger.move_by(page),
                KeyCode::Enter | KeyCode::Char(' ') | KeyCode::Right | KeyCode::Left => {
                    match self.debugger.selected_row().cloned() {
                        Some(DebugRow::Target(_)) if key.code == KeyCode::Enter => {
                            self.debug_start_or_continue()
                        }
                        Some(DebugRow::Frame(i)) => self.debugger.select_frame(i),
                        Some(DebugRow::MoreFrames(_)) => {
                            self.debugger.all_frames = !self.debugger.all_frames;
                            self.debugger.rebuild();
                        }
                        Some(DebugRow::Var(i)) => self.debugger.toggle_var(i),
                        Some(DebugRow::Breakpoint(p, l)) if key.code == KeyCode::Enter => {
                            self.open_file_at(&p, l);
                            self.focus = Focus::Left;
                        }
                        _ => {}
                    }
                }
                KeyCode::Delete | KeyCode::Backspace | KeyCode::Char('d') => {
                    if let Some(DebugRow::Breakpoint(p, l)) = self.debugger.selected_row().cloned()
                    {
                        self.debugger.toggle_breakpoint(&p, l);
                    }
                }
                KeyCode::Char('c') => self.debugger.continue_(),
                KeyCode::Char('n') => self.debugger.step_over(),
                KeyCode::Char('s') => self.debugger.step_in(),
                KeyCode::Char('o') => self.debugger.step_out(),
                KeyCode::Char('p') => self.debugger.pause(),
                KeyCode::Char('x') => self.debugger.stop(),
                KeyCode::Char('r') => {
                    let root = self.root.clone();
                    self.debugger.discover(&root, None);
                }
                KeyCode::Esc if !self.tabs.is_empty() => {
                    self.focus = Focus::Right;
                }
                _ => {}
            },
            LeftMode::Tests => match key.code {
                KeyCode::Up | KeyCode::Char('k') => self.tests.move_by(-1),
                KeyCode::Down | KeyCode::Char('j') => self.tests.move_by(1),
                KeyCode::PageUp => self.tests.move_by(-page),
                KeyCode::PageDown => self.tests.move_by(page),
                KeyCode::Home => self.tests.selected = 0,
                KeyCode::End => self.tests.selected = self.tests.visible().len().saturating_sub(1),
                KeyCode::Left | KeyCode::Right | KeyCode::Char(' ') => self.tests.toggle(),
                KeyCode::Char('E') | KeyCode::Char('*') => self.tests.toggle_all(),
                KeyCode::Char('e') if alt && shift => self.tests.toggle_all(),
                KeyCode::Char('e') => self.tests.toggle(),
                KeyCode::Char('r') => {
                    self.tests.discover(&self.root);
                    self.set_message("tests rediscovered");
                }
                KeyCode::Enter => self.run_tests(false),
                KeyCode::Char('a') | KeyCode::Char('A') => self.run_tests(true),
                KeyCode::Char('o') => {
                    if let Some(n) = self.tests.selected_node().cloned() {
                        self.open_file_at(&n.file, n.line);
                    }
                }
                KeyCode::Esc if !self.tabs.is_empty() => {
                    self.focus = Focus::Right;
                }
                _ => {}
            },
        }
    }

    fn run_tests(&mut self, all: bool) {
        if self.tests.running {
            self.set_message("tests already running");
            return;
        }
        let root = self.root.clone();
        match self.tests.run(&root, all) {
            Some(label) => {
                let out = self.output_tab_mut("tests");
                out.follow = true;
                let idx = self
                    .tabs
                    .iter()
                    .position(|t| matches!(&t.kind, TabKind::Output(o) if o.title == "tests"))
                    .unwrap();
                self.active = idx;
                self.set_message(label);
            }
            None => self.set_message("nothing to run"),
        }
    }

    fn handle_right_key(&mut self, key: KeyEvent) {
        let Some(tab) = self.tabs.get_mut(self.active) else {
            self.focus = Focus::Left;
            return;
        };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let rows = self.right_content.height as usize;
        match &mut tab.kind {
            TabKind::Terminal(t) => t.send_key(key),
            TabKind::Diff(d) => match key.code {
                KeyCode::Up | KeyCode::Char('k') => d.move_by(-1),
                KeyCode::Down | KeyCode::Char('j') => d.move_by(1),
                KeyCode::PageUp => d.move_by(-(rows as isize)),
                KeyCode::PageDown => d.move_by(rows as isize),
                KeyCode::Home => d.selected = 0,
                KeyCode::End => d.selected = d.rows.len().saturating_sub(1),
                KeyCode::Left => d.hscroll = d.hscroll.saturating_sub(8),
                KeyCode::Right => d.hscroll += 8,
                KeyCode::Char('n') | KeyCode::Char(']') => d.next_change(true),
                KeyCode::Char('p') | KeyCode::Char('[') => d.next_change(false),
                KeyCode::Enter | KeyCode::Char('o') => {
                    let path = d.path.clone();
                    let line = d.rows.get(d.selected).and_then(|r| r.right_no).unwrap_or(1);
                    if path.exists() {
                        self.open_file_at(&path, line);
                    }
                }
                KeyCode::Esc => self.focus = Focus::Left,
                _ => {}
            },
            TabKind::Output(o) => match key.code {
                KeyCode::Up | KeyCode::Char('k') => {
                    o.follow = false;
                    o.scroll = o.scroll.saturating_sub(1);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    o.scroll += 1;
                    if o.scroll + rows >= o.lines.len() {
                        o.follow = true;
                    }
                }
                KeyCode::PageUp => {
                    o.follow = false;
                    o.scroll = o.scroll.saturating_sub(rows);
                }
                KeyCode::PageDown => {
                    o.scroll += rows;
                    if o.scroll + rows >= o.lines.len() {
                        o.follow = true;
                    }
                }
                KeyCode::Home => {
                    o.follow = false;
                    o.scroll = 0;
                }
                KeyCode::End => o.follow = true,
                KeyCode::Char('c') => {
                    o.lines.clear();
                    o.scroll = 0;
                }
                KeyCode::Esc => self.focus = Focus::Left,
                _ => {}
            },
            TabKind::Editor(ed) => {
                let page = rows.saturating_sub(2).max(1);
                if tab.preview {
                    let total = tab.preview_cache.as_ref().map(|c| c.2.len()).unwrap_or(0);
                    let max = total.saturating_sub(rows);
                    match key.code {
                        KeyCode::Up | KeyCode::Char('k') => {
                            tab.preview_scroll = tab.preview_scroll.saturating_sub(1)
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            tab.preview_scroll = (tab.preview_scroll + 1).min(max)
                        }
                        KeyCode::PageUp => {
                            tab.preview_scroll = tab.preview_scroll.saturating_sub(page)
                        }
                        KeyCode::PageDown | KeyCode::Char(' ') => {
                            tab.preview_scroll = (tab.preview_scroll + page).min(max)
                        }
                        KeyCode::Home => tab.preview_scroll = 0,
                        KeyCode::End => tab.preview_scroll = max,
                        KeyCode::Esc => self.focus = Focus::Left,
                        _ => {}
                    }
                    return;
                }
                match key.code {
                    // --- movement (fixed keys; shortcuts come from the keymap) ---
                    KeyCode::Left => ed.move_left(shift),
                    KeyCode::Right => ed.move_right(shift),
                    KeyCode::Up => ed.move_up(shift),
                    KeyCode::Down => ed.move_down(shift),
                    KeyCode::Home => ed.home(shift),
                    KeyCode::End => ed.end(shift),
                    KeyCode::PageUp => ed.page(page, false, shift),
                    KeyCode::PageDown => ed.page(page, true, shift),
                    KeyCode::Esc => {
                        if ed.cursors.len() > 1 || ed.primary().anchor.is_some() {
                            ed.collapse_cursors();
                        } else {
                            self.focus = Focus::Left;
                        }
                    }
                    // --- editing ---
                    KeyCode::Enter => ed.insert_newline(),
                    KeyCode::Backspace => ed.backspace(),
                    KeyCode::Delete => ed.delete_forward(),
                    KeyCode::Tab => ed.insert_tab(),
                    KeyCode::BackTab => ed.indent_selection(false),
                    KeyCode::Char(c) if !ctrl && !alt => ed.insert_char(c),
                    _ => {}
                }
            }
        }
    }

    fn handle_palette_key(&mut self, key: KeyEvent) {
        let Some(p) = self.palette.as_mut() else {
            return;
        };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => self.palette = None,
            KeyCode::Enter => {
                let chosen = p.items.get(p.selected).map(|i| i.0);
                self.palette = None;
                if let Some(a) = chosen {
                    self.perform(a, false);
                }
            }
            KeyCode::Up => p.selected = p.selected.saturating_sub(1),
            KeyCode::Down => p.selected = (p.selected + 1).min(p.items.len().saturating_sub(1)),
            KeyCode::PageUp => p.selected = p.selected.saturating_sub(10),
            KeyCode::PageDown => {
                p.selected = (p.selected + 10).min(p.items.len().saturating_sub(1))
            }
            KeyCode::Home => p.selected = 0,
            KeyCode::End => p.selected = p.items.len().saturating_sub(1),
            KeyCode::Backspace => {
                p.query.pop();
                p.refilter();
            }
            KeyCode::Char('u') if ctrl => {
                p.query.clear();
                p.refilter();
            }
            KeyCode::Char('n') if ctrl => {
                p.selected = (p.selected + 1).min(p.items.len().saturating_sub(1))
            }
            KeyCode::Char('p') if ctrl => p.selected = p.selected.saturating_sub(1),
            KeyCode::Char(c) if !ctrl => {
                p.query.push(c);
                p.selected = 0;
                p.refilter();
            }
            _ => {}
        }
    }

    fn handle_prompt_key(&mut self, key: KeyEvent) {
        let Some(prompt) = self.prompt.as_mut() else {
            return;
        };
        match &prompt.kind {
            PromptKind::ConfirmClose(i) => {
                let i = *i;
                match key.code {
                    KeyCode::Char('y') | KeyCode::Char('Y') => {
                        self.prompt = None;
                        self.force_close(i);
                    }
                    _ => self.prompt = None,
                }
                return;
            }
            PromptKind::ConfirmQuit => {
                match key.code {
                    KeyCode::Char('y') | KeyCode::Char('Y') => self.should_quit = true,
                    _ => {}
                }
                self.prompt = None;
                return;
            }
            PromptKind::ConfirmOverwrite => {
                self.prompt = None;
                if matches!(key.code, KeyCode::Char('y') | KeyCode::Char('Y')) {
                    self.save_force();
                }
                return;
            }
            _ => {}
        }
        match key.code {
            KeyCode::Esc => self.prompt = None,
            KeyCode::Backspace => {
                prompt.input.pop();
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                prompt.input.clear()
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                prompt.input.push(c)
            }
            KeyCode::Enter => {
                let p = self.prompt.take().unwrap();
                self.submit_prompt(p);
            }
            _ => {}
        }
    }

    fn submit_prompt(&mut self, p: Prompt) {
        let input = p.input.trim().to_string();
        match p.kind {
            PromptKind::Open => {
                if input.is_empty() {
                    return;
                }
                let path = PathBuf::from(&input);
                let path = if path.is_absolute() {
                    path
                } else {
                    self.root.join(path)
                };
                if path.is_dir() {
                    self.set_message("that is a directory");
                } else if path.exists() {
                    self.open_file(&path);
                } else {
                    let mut ed = Editor::new();
                    ed.set_lang(crate::editor::highlight::Lang::from_path(&path));
                    ed.path = Some(path);
                    ed.modified = true;
                    self.tabs.push(Tab::new(TabKind::Editor(ed), false));
                    self.active = self.tabs.len() - 1;
                    self.focus = Focus::Right;
                    self.set_message("new file (not yet saved)");
                }
            }
            PromptKind::SaveAs => {
                if input.is_empty() {
                    return;
                }
                let path = PathBuf::from(&input);
                let path = if path.is_absolute() {
                    path
                } else {
                    self.root.join(path)
                };
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let result = self.active_editor_mut().map(|ed| ed.save_as(path));
                match result {
                    Some(Ok(())) => {
                        self.set_message("saved");
                        self.refresh_git();
                        self.tests.discover(&self.root);
                    }
                    Some(Err(e)) => self.set_message(format!("save failed: {}", e)),
                    None => {}
                }
            }
            PromptKind::Find => {
                if let Some(ed) = self.active_editor_mut() {
                    ed.search = Some(input.clone());
                    if !ed.find_next(&input) {
                        self.set_message("not found");
                    }
                }
            }
            PromptKind::Goto => {
                if let Ok(n) = input.parse::<usize>()
                    && let Some(ed) = self.active_editor_mut()
                {
                    ed.goto_line(n);
                }
            }
            PromptKind::ConfirmClose(_)
            | PromptKind::ConfirmQuit
            | PromptKind::ConfirmOverwrite => {}
        }
    }
}
