//! Left pane mode 3: discovered Rust and Python tests, runnable individually or all at once.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TestStatus {
    Unknown,
    Running,
    Passed,
    Failed,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Runner {
    Cargo,
    Pytest,
}

#[derive(Clone, Debug)]
pub struct TestNode {
    pub name: String,
    pub file: PathBuf,
    pub line: usize,
    /// Filter passed to the runner (`module::name` for cargo, `file::Class::name` for pytest).
    pub filter: String,
    pub runner: Runner,
    pub depth: usize,
    pub is_group: bool,
    pub expanded: bool,
    pub parent: Option<usize>,
    pub status: TestStatus,
}

pub enum TestEvent {
    Line(String),
    Result { filter: String, passed: bool },
    Done(i32),
}

pub struct TestsPane {
    pub nodes: Vec<TestNode>,
    pub selected: usize,
    pub scroll: usize,
    visible: Vec<usize>,
    pub rx: Option<Receiver<TestEvent>>,
    pub running: bool,
    /// In-flight discovery, if any; its result replaces `nodes` when it lands.
    discover_rx: Option<Receiver<Vec<TestNode>>>,
    /// A rescan was requested while one was already running.
    discover_again: bool,
}

/// Upper bound on directory entries visited during one discovery pass, so a
/// pathological root (a home directory, say) stays cheap and bounded.
const WALK_BUDGET: usize = 100_000;

fn walk_files(dir: &Path, out: &mut Vec<PathBuf>, depth: usize, budget: &mut usize) {
    if depth > 12 || *budget == 0 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = rd.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        if *budget == 0 {
            return;
        }
        *budget -= 1;
        let p = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if is_dir {
            // Hidden directories hold toolchains, caches and registries
            // (~/.cargo, ~/.rustup, ~/.cache, .venv) full of sources that are
            // never this project's tests.
            if name.starts_with('.')
                || matches!(
                    name.as_str(),
                    "target"
                        | "node_modules"
                        | "venv"
                        | "__pycache__"
                        | "site-packages"
                        | "build"
                        | "dist"
                )
            {
                continue;
            }
            walk_files(&p, out, depth + 1, budget);
        } else if name.ends_with(".rs") || name.ends_with(".py") {
            out.push(p);
        }
    }
}

/// Walk `root` and parse every Rust/Python source for tests. Runs on a
/// background thread; see `TestsPane::discover`.
fn discover_nodes(root: &Path) -> Vec<TestNode> {
    let mut files = Vec::new();
    let mut budget = WALK_BUDGET;
    walk_files(root, &mut files, 0, &mut budget);
    let mut nodes = Vec::new();
    for f in files {
        if f.extension().map(|e| e == "rs").unwrap_or(false) {
            discover_rust(root, &f, &mut nodes);
        } else {
            discover_python(root, &f, &mut nodes);
        }
    }
    nodes
}

/// Module path prefix for a Rust source file relative to its crate root.
fn rust_module_prefix(root: &Path, file: &Path) -> String {
    let rel = file.strip_prefix(root).unwrap_or(file);
    let mut comps: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    // drop a leading crate dir / src / tests segment
    while let Some(first) = comps.first() {
        if first == "src" || first == "tests" {
            comps.remove(0);
            break;
        }
        if comps.len() > 1 && (comps[1] == "src" || comps[1] == "tests") {
            comps.remove(0);
            continue;
        }
        break;
    }
    if let Some(last) = comps.last_mut() {
        if last == "main.rs" || last == "lib.rs" || last == "mod.rs" {
            comps.pop();
        } else if let Some(stem) = last.strip_suffix(".rs") {
            *last = stem.to_string();
        }
    }
    comps.join("::")
}

fn discover_rust(root: &Path, file: &Path, nodes: &mut Vec<TestNode>) {
    let Ok(text) = std::fs::read_to_string(file) else {
        return;
    };
    let prefix = rust_module_prefix(root, file);
    let mut found: Vec<(String, usize, String)> = Vec::new();
    let mut mods: Vec<(String, i32)> = Vec::new(); // (name, depth at which it was opened)
    let mut depth: i32 = 0;
    let mut pending_test = false;
    for (i, line) in text.lines().enumerate() {
        let t = line.trim();
        if t.starts_with("#[test")
            || t.starts_with("#[tokio::test")
            || t.starts_with("#[async_std::test")
            || t.starts_with("#[rstest")
        {
            pending_test = true;
        }
        if let Some(rest) = t.strip_prefix("mod ")
            && let Some(name) = rest
                .split(|c: char| !(c.is_alphanumeric() || c == '_'))
                .next()
            && t.contains('{')
        {
            mods.push((name.to_string(), depth));
        }
        if pending_test {
            if let Some(idx) = t.find("fn ") {
                let after = &t[idx + 3..];
                let name: String = after
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                if !name.is_empty() {
                    let mut path: Vec<String> = Vec::new();
                    if !prefix.is_empty() {
                        path.push(prefix.clone());
                    }
                    path.extend(mods.iter().map(|m| m.0.clone()));
                    path.push(name.clone());
                    found.push((name, i + 1, path.join("::")));
                    pending_test = false;
                }
            } else if t.starts_with("fn ") || t.contains(" fn ") {
                pending_test = false;
            }
        }
        for c in line.chars() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    while mods.last().map(|m| m.1 >= depth).unwrap_or(false) {
                        mods.pop();
                    }
                }
                _ => {}
            }
        }
    }
    if found.is_empty() {
        return;
    }
    let rel = file
        .strip_prefix(root)
        .unwrap_or(file)
        .to_string_lossy()
        .into_owned();
    let group = nodes.len();
    nodes.push(TestNode {
        name: rel,
        file: file.to_path_buf(),
        line: 1,
        filter: if prefix.is_empty() {
            String::new()
        } else {
            prefix.clone()
        },
        runner: Runner::Cargo,
        depth: 0,
        is_group: true,
        expanded: true,
        parent: None,
        status: TestStatus::Unknown,
    });
    for (name, line, filter) in found {
        nodes.push(TestNode {
            name,
            file: file.to_path_buf(),
            line,
            filter,
            runner: Runner::Cargo,
            depth: 1,
            is_group: false,
            expanded: false,
            parent: Some(group),
            status: TestStatus::Unknown,
        });
    }
}

fn discover_python(root: &Path, file: &Path, nodes: &mut Vec<TestNode>) {
    let fname = file
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let looks_like_test =
        fname.starts_with("test_") || fname.ends_with("_test.py") || fname == "tests.py";
    let Ok(text) = std::fs::read_to_string(file) else {
        return;
    };
    if !looks_like_test && !text.contains("def test_") {
        return;
    }
    let rel = file
        .strip_prefix(root)
        .unwrap_or(file)
        .to_string_lossy()
        .into_owned();
    let mut items: Vec<(Option<String>, String, usize)> = Vec::new();
    let mut current_class: Option<(String, usize)> = None;
    for (i, line) in text.lines().enumerate() {
        let indent = line.len() - line.trim_start().len();
        let t = line.trim_start();
        if let Some(rest) = t.strip_prefix("class ") {
            let name: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if name.starts_with("Test") {
                current_class = Some((name, indent));
            } else {
                current_class = None;
            }
            continue;
        }
        if let Some(rest) = t
            .strip_prefix("def ")
            .or_else(|| t.strip_prefix("async def "))
        {
            let name: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if name.starts_with("test") {
                let cls = match &current_class {
                    Some((c, ci)) if indent > *ci => Some(c.clone()),
                    _ => None,
                };
                items.push((cls, name, i + 1));
            }
        }
    }
    if items.is_empty() {
        return;
    }
    let group = nodes.len();
    nodes.push(TestNode {
        name: rel.clone(),
        file: file.to_path_buf(),
        line: 1,
        filter: rel.clone(),
        runner: Runner::Pytest,
        depth: 0,
        is_group: true,
        expanded: true,
        parent: None,
        status: TestStatus::Unknown,
    });
    let mut class_idx: Option<(String, usize)> = None;
    for (cls, name, line) in items {
        let (depth, parent) = match cls {
            Some(c) => {
                if class_idx.as_ref().map(|x| x.0 != c).unwrap_or(true) {
                    nodes.push(TestNode {
                        name: c.clone(),
                        file: file.to_path_buf(),
                        line,
                        filter: format!("{}::{}", rel, c),
                        runner: Runner::Pytest,
                        depth: 1,
                        is_group: true,
                        expanded: true,
                        parent: Some(group),
                        status: TestStatus::Unknown,
                    });
                    class_idx = Some((c.clone(), nodes.len() - 1));
                }
                (2, class_idx.as_ref().map(|x| x.1))
            }
            None => (1, Some(group)),
        };
        let filter = match parent.and_then(|p| nodes.get(p)) {
            Some(p) if p.depth == 1 => format!("{}::{}", p.filter, name),
            _ => format!("{}::{}", rel, name),
        };
        nodes.push(TestNode {
            name,
            file: file.to_path_buf(),
            line,
            filter,
            runner: Runner::Pytest,
            depth,
            is_group: false,
            expanded: false,
            parent,
            status: TestStatus::Unknown,
        });
    }
}

impl TestsPane {
    pub fn new() -> TestsPane {
        TestsPane {
            nodes: Vec::new(),
            selected: 0,
            scroll: 0,
            visible: Vec::new(),
            rx: None,
            running: false,
            discover_rx: None,
            discover_again: false,
        }
    }

    /// Start (or queue) a background scan of `root`; results arrive through
    /// `pump_discovery`. Never blocks, so a huge root cannot stall startup.
    pub fn discover(&mut self, root: &Path) {
        if self.discover_rx.is_some() {
            self.discover_again = true;
            return;
        }
        let (tx, rx) = channel();
        let root = root.to_path_buf();
        std::thread::spawn(move || {
            let _ = tx.send(discover_nodes(&root));
        });
        self.discover_rx = Some(rx);
    }

    pub fn discovering(&self) -> bool {
        self.discover_rx.is_some()
    }

    /// Pick up a finished scan. Returns true when the node list was replaced.
    pub fn pump_discovery(&mut self, root: &Path) -> bool {
        let Some(rx) = &self.discover_rx else {
            return false;
        };
        let nodes = match rx.try_recv() {
            Ok(n) => n,
            Err(TryRecvError::Empty) => return false,
            Err(TryRecvError::Disconnected) => {
                self.discover_rx = None;
                return false;
            }
        };
        self.discover_rx = None;
        self.install(nodes);
        if self.discover_again {
            self.discover_again = false;
            self.discover(root);
        }
        true
    }

    fn install(&mut self, mut nodes: Vec<TestNode>) {
        // keep previous statuses where filters match
        for n in nodes.iter_mut() {
            if let Some(old) = self
                .nodes
                .iter()
                .find(|o| o.filter == n.filter && o.runner == n.runner)
            {
                n.status = old.status;
                n.expanded = old.expanded;
            }
        }
        self.nodes = nodes;
        self.rebuild();
        self.selected = self.selected.min(self.visible.len().saturating_sub(1));
    }

    pub fn rebuild(&mut self) {
        let mut vis = Vec::new();
        for (i, n) in self.nodes.iter().enumerate() {
            let mut shown = true;
            let mut p = n.parent;
            while let Some(pi) = p {
                if !self.nodes[pi].expanded {
                    shown = false;
                    break;
                }
                p = self.nodes[pi].parent;
            }
            if shown {
                vis.push(i);
            }
        }
        self.visible = vis;
    }

    pub fn visible(&self) -> &[usize] {
        &self.visible
    }

    pub fn selected_node(&self) -> Option<&TestNode> {
        self.visible.get(self.selected).map(|&i| &self.nodes[i])
    }

    pub fn move_by(&mut self, delta: isize) {
        if self.visible.is_empty() {
            return;
        }
        self.selected =
            (self.selected as isize + delta).clamp(0, self.visible.len() as isize - 1) as usize;
    }

    pub fn toggle(&mut self) {
        if let Some(&i) = self.visible.get(self.selected)
            && self.nodes[i].is_group
        {
            self.nodes[i].expanded = !self.nodes[i].expanded;
            self.rebuild();
        }
    }

    pub fn toggle_all(&mut self) {
        let any = self.nodes.iter().any(|n| n.is_group && n.expanded);
        for n in self.nodes.iter_mut() {
            if n.is_group {
                n.expanded = !any;
            }
        }
        self.rebuild();
        self.selected = self.selected.min(self.visible.len().saturating_sub(1));
    }

    fn python_cmd(root: &Path) -> (String, Vec<String>) {
        let has_uv = root.join("uv.lock").exists() || root.join("pyproject.toml").exists();
        if has_uv && which("uv") {
            ("uv".into(), vec!["run".into(), "pytest".into()])
        } else if which("pytest") {
            ("pytest".into(), vec![])
        } else {
            ("python3".into(), vec!["-m".into(), "pytest".into()])
        }
    }

    /// Build the command for running `node` (or everything when None).
    fn commands(&self, root: &Path, node: Option<&TestNode>) -> Vec<(String, Vec<String>, String)> {
        let mut cmds = Vec::new();
        let has_rust = self.nodes.iter().any(|n| n.runner == Runner::Cargo);
        let has_py = self.nodes.iter().any(|n| n.runner == Runner::Pytest);
        match node {
            None => {
                if has_rust {
                    cmds.push((
                        "cargo".to_string(),
                        vec![
                            "test".to_string(),
                            "--".to_string(),
                            "--nocapture".to_string(),
                        ],
                        "cargo test".to_string(),
                    ));
                }
                if has_py {
                    let (c, mut a) = Self::python_cmd(root);
                    a.push("-v".into());
                    let label = format!("{} {}", c, a.join(" "));
                    cmds.push((c, a, label));
                }
            }
            Some(n) => match n.runner {
                Runner::Cargo => {
                    let mut a = vec!["test".to_string()];
                    if !n.filter.is_empty() {
                        a.push(n.filter.clone());
                    }
                    a.push("--".into());
                    a.push("--nocapture".into());
                    if !n.is_group {
                        a.push("--exact".into());
                    }
                    let label = format!("cargo {}", a.join(" "));
                    cmds.push(("cargo".to_string(), a, label));
                }
                Runner::Pytest => {
                    let (c, mut a) = Self::python_cmd(root);
                    a.push("-v".into());
                    a.push(n.filter.clone());
                    let label = format!("{} {}", c, a.join(" "));
                    cmds.push((c, a, label));
                }
            },
        }
        cmds
    }

    /// Run the selected node (or all tests). Output arrives through `self.rx`.
    pub fn run(&mut self, root: &Path, all: bool) -> Option<String> {
        if self.running {
            return None;
        }
        let node = if all {
            None
        } else {
            self.selected_node().cloned()
        };
        let cmds = self.commands(root, node.as_ref());
        if cmds.is_empty() {
            return None;
        }
        // mark affected nodes as running
        for n in self.nodes.iter_mut() {
            let affected = match &node {
                None => true,
                Some(sel) => {
                    n.runner == sel.runner
                        && (n.filter == sel.filter
                            || (sel.is_group && n.filter.starts_with(&sel.filter)))
                }
            };
            if affected && !n.is_group {
                n.status = TestStatus::Running;
            }
        }
        let (tx, rx) = channel();
        self.rx = Some(rx);
        self.running = true;
        let root = root.to_path_buf();
        let label = cmds
            .iter()
            .map(|c| c.2.clone())
            .collect::<Vec<_>>()
            .join(" && ");
        std::thread::spawn(move || {
            let mut code = 0;
            for (prog, args, label) in cmds {
                let _ = tx.send(TestEvent::Line(format!("$ {}", label)));
                code = run_one(&root, &prog, &args, &tx);
                if code != 0 {
                    break;
                }
            }
            let _ = tx.send(TestEvent::Done(code));
        });
        Some(label)
    }

    /// Apply a result event to the matching node.
    pub fn apply_result(&mut self, filter: &str, passed: bool) {
        let st = if passed {
            TestStatus::Passed
        } else {
            TestStatus::Failed
        };
        for n in self.nodes.iter_mut() {
            if n.is_group {
                continue;
            }
            let hit = match n.runner {
                Runner::Cargo => {
                    n.filter == filter
                        || filter.ends_with(&format!("::{}", n.filter))
                        || n.filter.ends_with(&format!("::{}", filter))
                }
                Runner::Pytest => n.filter == filter || filter.ends_with(&n.filter),
            };
            if hit {
                n.status = st;
            }
        }
    }

    pub fn finish(&mut self) {
        self.running = false;
        for n in self.nodes.iter_mut() {
            if n.status == TestStatus::Running {
                n.status = TestStatus::Unknown;
            }
        }
        // roll up group status
        for i in 0..self.nodes.len() {
            if !self.nodes[i].is_group {
                continue;
            }
            let children: Vec<TestStatus> = self
                .nodes
                .iter()
                .filter(|n| {
                    n.parent == Some(i) || n.parent.and_then(|p| self.nodes[p].parent) == Some(i)
                })
                .filter(|n| !n.is_group)
                .map(|n| n.status)
                .collect();
            self.nodes[i].status = if children.contains(&TestStatus::Failed) {
                TestStatus::Failed
            } else if !children.is_empty() && children.iter().all(|s| *s == TestStatus::Passed) {
                TestStatus::Passed
            } else {
                TestStatus::Unknown
            };
        }
    }
}

fn which(prog: &str) -> bool {
    Command::new("which")
        .arg(prog)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn parse_result(line: &str) -> Option<(String, bool)> {
    let t = line.trim();
    // cargo: "test editor::tests::undo_redo ... ok"
    if let Some(rest) = t.strip_prefix("test ")
        && let Some((name, outcome)) = rest.rsplit_once(" ... ")
    {
        let o = outcome.trim();
        if o.starts_with("ok") {
            return Some((name.trim().to_string(), true));
        }
        if o.starts_with("FAILED") {
            return Some((name.trim().to_string(), false));
        }
    }
    // pytest -v: "tests/test_x.py::TestA::test_b PASSED [ 50%]"
    if t.contains("::") {
        let mut parts = t.split_whitespace();
        if let (Some(name), Some(outcome)) = (parts.next(), parts.next()) {
            match outcome {
                "PASSED" | "XPASS" => return Some((name.to_string(), true)),
                "FAILED" | "ERROR" | "XFAIL" => return Some((name.to_string(), false)),
                _ => {}
            }
        }
    }
    None
}

fn run_one(root: &Path, prog: &str, args: &[String], tx: &Sender<TestEvent>) -> i32 {
    let mut child = match Command::new(prog)
        .args(args)
        .current_dir(root)
        .env("CARGO_TERM_COLOR", "never")
        .env("PY_COLORS", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            let _ = tx.send(TestEvent::Line(format!("failed to start {}: {}", prog, e)));
            return 127;
        }
    };
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let tx2 = tx.clone();
    let h = std::thread::spawn(move || {
        use std::io::BufRead;
        for line in std::io::BufReader::new(stderr)
            .lines()
            .map_while(Result::ok)
        {
            let _ = tx2.send(TestEvent::Line(line));
        }
    });
    {
        use std::io::BufRead;
        for line in std::io::BufReader::new(stdout)
            .lines()
            .map_while(Result::ok)
        {
            if let Some((name, passed)) = parse_result(&line) {
                let _ = tx.send(TestEvent::Result {
                    filter: name,
                    passed,
                });
            }
            let _ = tx.send(TestEvent::Line(line));
        }
    }
    let _ = h.join();
    child.wait().map(|s| s.code().unwrap_or(-1)).unwrap_or(-1)
}
