//! Left pane mode 4: a debugger driven over DAP (lldb-dap for Rust, debugpy for Python).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{Receiver, channel};

use serde_json::{Value, json};

use crate::dap::{DapClient, DapMessage};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TargetKind {
    /// A cargo binary target.
    CargoBin(String),
    /// The crate's unit-test binary.
    CargoTests,
    /// A python script.
    Python(PathBuf),
}

#[derive(Clone, Debug)]
pub struct Target {
    pub label: String,
    pub kind: TargetKind,
}

#[derive(Clone, Debug)]
pub struct Frame {
    pub id: i64,
    pub name: String,
    pub file: Option<PathBuf>,
    pub line: usize,
}

#[derive(Clone, Debug)]
pub struct Var {
    pub name: String,
    pub value: String,
    pub vref: i64,
    pub depth: usize,
    pub expanded: bool,
    pub loaded: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    Launching,
    Running,
    Stopped,
    Exited,
}

enum Pending {
    Initialize,
    Threads,
    StackTrace,
    Scopes,
    /// Children of the variable with this variablesReference.
    Variables(i64),
    Other,
}

pub enum BuildEvent {
    Line(String),
    Done(Result<PathBuf, String>),
}

pub struct Session {
    client: DapClient,
    pub state: State,
    pub thread_id: Option<i64>,
    pub frames: Vec<Frame>,
    pub frame_idx: usize,
    pub vars: Vec<Var>,
    pending: HashMap<i64, Pending>,
    pub stop_reason: String,
    launch_args: Value,
    pub label: String,
}

/// Rows shown in the pane; the debugger rebuilds them after every change.
#[derive(Clone, Debug)]
pub enum Row {
    Header(String),
    Text(String),
    Target(usize),
    Frame(usize),
    Var(usize),
    Breakpoint(PathBuf, usize),
    /// "… N more frames" toggle row.
    MoreFrames(usize),
}

pub struct Debugger {
    pub targets: Vec<Target>,
    pub selected: usize,
    pub scroll: usize,
    pub session: Option<Session>,
    pub breakpoints: BTreeMap<PathBuf, BTreeSet<usize>>,
    rows: Vec<Row>,
    /// File and line the debugger is currently stopped at.
    pub current: Option<(PathBuf, usize)>,
    pub last_target: Option<TargetKind>,
    build_rx: Option<Receiver<BuildEvent>>,
    build_target: Option<Target>,
    /// Messages for the output tab and status bar.
    pub output: Vec<String>,
    pub status: Option<String>,
    /// Set when a stop should move the editor to the frame.
    pub jump: Option<(PathBuf, usize)>,
    pub all_frames: bool,
    /// Output text received without a trailing newline yet.
    partial: String,
}

fn cargo_targets(root: &Path) -> Vec<Target> {
    let out = Command::new("cargo")
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .current_dir(root)
        .stderr(Stdio::null())
        .output();
    let Ok(out) = out else { return Vec::new() };
    let Ok(v) = serde_json::from_slice::<Value>(&out.stdout) else {
        return Vec::new();
    };
    let mut targets = Vec::new();
    let mut has_rust = false;
    for pkg in v
        .get("packages")
        .and_then(|p| p.as_array())
        .into_iter()
        .flatten()
    {
        for t in pkg
            .get("targets")
            .and_then(|t| t.as_array())
            .into_iter()
            .flatten()
        {
            let kinds: Vec<&str> = t
                .get("kind")
                .and_then(|k| k.as_array())
                .map(|a| a.iter().filter_map(|k| k.as_str()).collect())
                .unwrap_or_default();
            let name = t
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("")
                .to_string();
            has_rust = true;
            if kinds.contains(&"bin") {
                targets.push(Target {
                    label: format!("cargo run --bin {}", name),
                    kind: TargetKind::CargoBin(name),
                });
            }
        }
    }
    if has_rust {
        targets.push(Target {
            label: "cargo test (unit tests)".into(),
            kind: TargetKind::CargoTests,
        });
    }
    targets
}

fn python_targets(root: &Path, active: Option<&Path>) -> Vec<Target> {
    let mut t = Vec::new();
    let push = |p: PathBuf, t: &mut Vec<Target>| {
        if t.iter().any(|x| x.kind == TargetKind::Python(p.clone())) {
            return;
        }
        let rel = p.strip_prefix(root).unwrap_or(&p).display().to_string();
        t.push(Target {
            label: format!("python {}", rel),
            kind: TargetKind::Python(p),
        });
    };
    if let Some(a) = active
        && a.extension().map(|e| e == "py").unwrap_or(false)
    {
        push(a.to_path_buf(), &mut t);
    }
    // every script at the top level of the project
    if let Ok(rd) = std::fs::read_dir(root) {
        let mut files: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().map(|e| e == "py").unwrap_or(false))
            .collect();
        files.sort();
        for p in files.into_iter().take(20) {
            push(p, &mut t);
        }
    }
    t
}

fn lldb_dap_path() -> Option<String> {
    if let Ok(p) = std::env::var("REDACT_LLDB_DAP") {
        return Some(p);
    }
    for cand in ["lldb-dap", "lldb-vscode"] {
        if Command::new("which")
            .arg(cand)
            .stdout(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
        {
            return Some(cand.to_string());
        }
    }
    if let Ok(out) = Command::new("xcrun")
        .args(["-f", "lldb-dap"])
        .stderr(Stdio::null())
        .output()
        && out.status.success()
    {
        let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !p.is_empty() {
            return Some(p);
        }
    }
    None
}

fn rust_init_commands() -> Vec<String> {
    let Ok(out) = Command::new("rustc").args(["--print", "sysroot"]).output() else {
        return Vec::new();
    };
    let sysroot = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let etc = PathBuf::from(&sysroot).join("lib/rustlib/etc");
    let mut cmds = Vec::new();
    if etc.join("lldb_lookup.py").exists() {
        cmds.push(format!(
            "command script import \"{}\"",
            etc.join("lldb_lookup.py").display()
        ));
    }
    if etc.join("lldb_commands").exists() {
        cmds.push(format!(
            "command source -s 0 \"{}\"",
            etc.join("lldb_commands").display()
        ));
    }
    cmds
}

fn python_adapter(root: &Path) -> (String, Vec<String>) {
    let uv_project = root.join("pyproject.toml").exists() || root.join("uv.lock").exists();
    let has_uv = Command::new("which")
        .arg("uv")
        .stdout(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if has_uv {
        let mut args = vec!["run".to_string(), "--with".into(), "debugpy".into()];
        if !uv_project {
            args.push("--no-project".into());
        }
        args.extend(["python".to_string(), "-m".into(), "debugpy.adapter".into()]);
        ("uv".into(), args)
    } else {
        (
            "python3".into(),
            vec!["-m".into(), "debugpy.adapter".into()],
        )
    }
}

impl Debugger {
    pub fn new() -> Debugger {
        let mut d = Debugger {
            targets: Vec::new(),
            selected: 0,
            scroll: 0,
            session: None,
            breakpoints: BTreeMap::new(),
            rows: Vec::new(),
            current: None,
            last_target: None,
            build_rx: None,
            build_target: None,
            output: Vec::new(),
            status: None,
            jump: None,
            all_frames: false,
            partial: String::new(),
        };
        d.rebuild();
        d
    }

    pub fn discover(&mut self, root: &Path, active: Option<&Path>) {
        let mut t = cargo_targets(root);
        t.extend(python_targets(root, active));
        self.targets = t;
        self.rebuild();
    }

    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    pub fn rebuild(&mut self) {
        let mut rows = Vec::new();
        match &self.session {
            None => {
                rows.push(Row::Header("targets  (enter: launch)".into()));
                if self.targets.is_empty() {
                    rows.push(Row::Text("no cargo or python targets found".into()));
                }
                for i in 0..self.targets.len() {
                    rows.push(Row::Target(i));
                }
                if self.build_rx.is_some() {
                    rows.push(Row::Text("building…".into()));
                }
            }
            Some(s) => {
                rows.push(Row::Header(format!(
                    "{}  [{}]",
                    s.label,
                    match s.state {
                        State::Launching => "launching",
                        State::Running => "running",
                        State::Stopped => "stopped",
                        State::Exited => "exited",
                    }
                )));
                if s.state == State::Stopped {
                    rows.push(Row::Header(format!("call stack  ({})", s.stop_reason)));
                    let cap = if self.all_frames {
                        s.frames.len()
                    } else {
                        8.min(s.frames.len())
                    };
                    for i in 0..cap {
                        rows.push(Row::Frame(i));
                    }
                    if s.frames.len() > 8 {
                        rows.push(Row::MoreFrames(s.frames.len() - 8));
                    }
                    rows.push(Row::Header("variables".into()));
                    let mut hide_below: Option<usize> = None;
                    for (i, v) in s.vars.iter().enumerate() {
                        if let Some(d) = hide_below {
                            if v.depth > d {
                                continue;
                            }
                            hide_below = None;
                        }
                        rows.push(Row::Var(i));
                        if v.vref > 0 && !v.expanded {
                            hide_below = Some(v.depth);
                        }
                    }
                }
            }
        }
        rows.push(Row::Header("breakpoints  (F9 toggles)".into()));
        for (p, lines) in &self.breakpoints {
            for l in lines {
                rows.push(Row::Breakpoint(p.clone(), *l));
            }
        }
        self.rows = rows;
        if self.selected >= self.rows.len() {
            self.selected = self.rows.len().saturating_sub(1);
        }
    }

    pub fn move_by(&mut self, delta: isize) {
        if self.rows.is_empty() {
            return;
        }
        let n = self.rows.len() as isize;
        let mut i = (self.selected as isize + delta).clamp(0, n - 1);
        // skip headers in the direction of travel
        while matches!(self.rows[i as usize], Row::Header(_) | Row::Text(_)) {
            let next = i + delta.signum();
            if next < 0 || next >= n {
                break;
            }
            i = next;
        }
        self.selected = i as usize;
    }

    pub fn selected_row(&self) -> Option<&Row> {
        self.rows.get(self.selected)
    }

    // ---------- breakpoints ----------

    pub fn toggle_breakpoint(&mut self, file: &Path, line: usize) {
        let set = self.breakpoints.entry(file.to_path_buf()).or_default();
        if !set.remove(&line) {
            set.insert(line);
        }
        if set.is_empty() {
            self.breakpoints.remove(file);
        }
        self.send_breakpoints_for(file);
        self.rebuild();
    }

    fn send_breakpoints_for(&mut self, file: &Path) {
        let lines: Vec<usize> = self
            .breakpoints
            .get(file)
            .map(|s| s.iter().copied().collect())
            .unwrap_or_default();
        if let Some(s) = self.session.as_mut() {
            let bps: Vec<Value> = lines.iter().map(|l| json!({"line": l})).collect();
            let seq = s.client.request(
                "setBreakpoints",
                json!({"source": {"path": file.to_string_lossy()}, "breakpoints": bps}),
            );
            s.pending.insert(seq, Pending::Other);
        }
    }

    fn send_all_breakpoints(&mut self) {
        let files: Vec<PathBuf> = self.breakpoints.keys().cloned().collect();
        for f in files {
            self.send_breakpoints_for(&f);
        }
    }

    // ---------- launching ----------

    /// Start the selected (or last used) target. Returns a status line.
    pub fn launch(&mut self, root: &Path, target: Option<Target>) -> String {
        if self.session.is_some() || self.build_rx.is_some() {
            return "debug session already active".into();
        }
        let target = match target
            .or_else(|| {
                let last = self.last_target.clone()?;
                self.targets.iter().find(|t| t.kind == last).cloned()
            })
            .or_else(|| self.targets.first().cloned())
        {
            Some(t) => t,
            None => return "no debug target".into(),
        };
        self.last_target = Some(target.kind.clone());
        self.output.clear();
        match &target.kind {
            TargetKind::CargoBin(_) | TargetKind::CargoTests => {
                let (tx, rx) = channel();
                let root = root.to_path_buf();
                let kind = target.kind.clone();
                std::thread::spawn(move || {
                    let mut args: Vec<String> = match &kind {
                        TargetKind::CargoBin(name) => {
                            vec!["build".into(), "--bin".into(), name.clone()]
                        }
                        _ => vec!["test".into(), "--no-run".into()],
                    };
                    args.push("--message-format=json-diagnostic-rendered-ansi".into());
                    let _ = tx.send(BuildEvent::Line(format!("$ cargo {}", args.join(" "))));
                    let child = Command::new("cargo")
                        .args(&args)
                        .current_dir(&root)
                        .env("CARGO_TERM_COLOR", "never")
                        .stdout(Stdio::piped())
                        .stderr(Stdio::piped())
                        .spawn();
                    let mut child = match child {
                        Ok(c) => c,
                        Err(e) => {
                            let _ = tx.send(BuildEvent::Done(Err(format!(
                                "cargo failed to start: {}",
                                e
                            ))));
                            return;
                        }
                    };
                    let stderr = child.stderr.take().unwrap();
                    let tx2 = tx.clone();
                    let h = std::thread::spawn(move || {
                        use std::io::BufRead;
                        for line in std::io::BufReader::new(stderr)
                            .lines()
                            .map_while(Result::ok)
                        {
                            let _ = tx2.send(BuildEvent::Line(line));
                        }
                    });
                    let mut exe: Option<PathBuf> = None;
                    {
                        use std::io::BufRead;
                        let stdout = child.stdout.take().unwrap();
                        for line in std::io::BufReader::new(stdout)
                            .lines()
                            .map_while(Result::ok)
                        {
                            let Ok(v) = serde_json::from_str::<Value>(&line) else {
                                continue;
                            };
                            match v.get("reason").and_then(|r| r.as_str()) {
                                Some("compiler-artifact") => {
                                    let is_test = v
                                        .pointer("/profile/test")
                                        .and_then(|t| t.as_bool())
                                        .unwrap_or(false);
                                    let kinds: Vec<&str> = v
                                        .pointer("/target/kind")
                                        .and_then(|k| k.as_array())
                                        .map(|a| a.iter().filter_map(|k| k.as_str()).collect())
                                        .unwrap_or_default();
                                    if let Some(e) = v.get("executable").and_then(|e| e.as_str()) {
                                        let want = match &kind {
                                            TargetKind::CargoBin(name) => {
                                                !is_test
                                                    && kinds.contains(&"bin")
                                                    && v.pointer("/target/name")
                                                        .and_then(|n| n.as_str())
                                                        == Some(name.as_str())
                                            }
                                            _ => {
                                                is_test
                                                    && (kinds.contains(&"bin")
                                                        || kinds.contains(&"lib"))
                                            }
                                        };
                                        if want {
                                            exe = Some(PathBuf::from(e));
                                        }
                                    }
                                }
                                Some("compiler-message") => {
                                    if let Some(r) =
                                        v.pointer("/message/rendered").and_then(|r| r.as_str())
                                    {
                                        for l in strip_ansi(r).lines() {
                                            let _ = tx.send(BuildEvent::Line(l.to_string()));
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    let _ = h.join();
                    let status = child.wait().map(|s| s.success()).unwrap_or(false);
                    let _ = tx.send(BuildEvent::Done(match (status, exe) {
                        (true, Some(e)) => Ok(e),
                        (true, None) => {
                            Err("build succeeded but no executable was produced".into())
                        }
                        (false, _) => Err("build failed".into()),
                    }));
                });
                self.build_rx = Some(rx);
                self.build_target = Some(target.clone());
                self.rebuild();
                format!("building {}", target.label)
            }
            TargetKind::Python(file) => {
                let (prog, args) = python_adapter(root);
                let launch = json!({
                    "request": "launch",
                    "type": "python",
                    "name": target.label,
                    "program": file.to_string_lossy(),
                    "cwd": root.to_string_lossy(),
                    "console": "internalConsole",
                    "justMyCode": true,
                    "stopOnEntry": false,
                });
                match self.start_session(&prog, &args, root, launch, target.label.clone()) {
                    Ok(()) => format!("debugging {}", target.label),
                    Err(e) => format!("debugger failed: {}", e),
                }
            }
        }
    }

    fn start_lldb(&mut self, root: &Path, exe: PathBuf, target: Target) -> String {
        let Some(adapter) = lldb_dap_path() else {
            return "lldb-dap not found (install Xcode command line tools or set REDACT_LLDB_DAP)"
                .into();
        };
        let args: Vec<String> = match &target.kind {
            TargetKind::CargoTests => vec!["--test-threads=1".into(), "--nocapture".into()],
            _ => Vec::new(),
        };
        let launch = json!({
            "request": "launch",
            "type": "lldb-dap",
            "name": target.label,
            "program": exe.to_string_lossy(),
            "args": args,
            "cwd": root.to_string_lossy(),
            "stopOnEntry": false,
            "initCommands": rust_init_commands(),
        });
        match self.start_session(&adapter, &[], root, launch, target.label.clone()) {
            Ok(()) => format!("debugging {}", target.label),
            Err(e) => format!("debugger failed: {}", e),
        }
    }

    fn start_session(
        &mut self,
        prog: &str,
        args: &[String],
        root: &Path,
        launch: Value,
        label: String,
    ) -> anyhow::Result<()> {
        let mut client = DapClient::spawn(prog, args, root)?;
        let seq = client.request(
            "initialize",
            json!({
                "clientID": "redact",
                "clientName": "redact",
                "adapterID": "redact",
                "linesStartAt1": true,
                "columnsStartAt1": true,
                "pathFormat": "path",
                "supportsVariableType": true,
                "supportsRunInTerminalRequest": false,
            }),
        );
        let mut pending = HashMap::new();
        pending.insert(seq, Pending::Initialize);
        self.session = Some(Session {
            client,
            state: State::Launching,
            thread_id: None,
            frames: Vec::new(),
            frame_idx: 0,
            vars: Vec::new(),
            pending,
            stop_reason: String::new(),
            launch_args: launch,
            label,
        });
        self.current = None;
        self.rebuild();
        Ok(())
    }

    pub fn stop(&mut self) {
        if let Some(s) = self.session.as_mut() {
            let seq = s
                .client
                .request("disconnect", json!({"terminateDebuggee": true}));
            s.pending.insert(seq, Pending::Other);
            s.state = State::Exited;
        }
        self.end_session();
        self.status = Some("debug session stopped".into());
    }

    fn end_session(&mut self) {
        self.session = None;
        self.current = None;
        self.rebuild();
    }

    // ---------- stepping ----------

    fn thread_request(&mut self, cmd: &str) {
        if let Some(s) = self.session.as_mut() {
            if s.state != State::Stopped {
                return;
            }
            let tid = s.thread_id.unwrap_or(1);
            let seq = s.client.request(cmd, json!({"threadId": tid}));
            s.pending.insert(seq, Pending::Other);
            s.state = State::Running;
            s.frames.clear();
            s.vars.clear();
        }
        self.current = None;
        self.rebuild();
    }

    pub fn continue_(&mut self) {
        self.thread_request("continue");
    }
    pub fn step_over(&mut self) {
        self.thread_request("next");
    }
    pub fn step_in(&mut self) {
        self.thread_request("stepIn");
    }
    pub fn step_out(&mut self) {
        self.thread_request("stepOut");
    }

    pub fn pause(&mut self) {
        if let Some(s) = self.session.as_mut()
            && s.state == State::Running
        {
            let tid = s.thread_id.unwrap_or(1);
            let seq = s.client.request("pause", json!({"threadId": tid}));
            s.pending.insert(seq, Pending::Other);
        }
    }

    pub fn select_frame(&mut self, idx: usize) {
        let Some(s) = self.session.as_mut() else {
            return;
        };
        if idx >= s.frames.len() {
            return;
        }
        s.frame_idx = idx;
        s.vars.clear();
        let fid = s.frames[idx].id;
        let seq = s.client.request("scopes", json!({"frameId": fid}));
        s.pending.insert(seq, Pending::Scopes);
        if let Some(f) = s.frames[idx].file.clone() {
            let line = s.frames[idx].line;
            self.current = Some((f.clone(), line));
            self.jump = Some((f, line));
        }
        self.rebuild();
    }

    pub fn toggle_var(&mut self, idx: usize) {
        let Some(s) = self.session.as_mut() else {
            return;
        };
        let Some(v) = s.vars.get_mut(idx) else { return };
        if v.vref == 0 {
            return;
        }
        v.expanded = !v.expanded;
        if v.expanded && !v.loaded {
            v.loaded = true;
            let vref = v.vref;
            let seq = s
                .client
                .request("variables", json!({"variablesReference": vref}));
            s.pending.insert(seq, Pending::Variables(vref));
        }
        self.rebuild();
    }

    // ---------- event pump ----------

    /// Drain build and adapter messages. Returns true when the pane changed.
    pub fn pump(&mut self, root: &Path) -> bool {
        let mut changed = false;
        // build
        if let Some(rx) = &self.build_rx {
            let mut done: Option<Result<PathBuf, String>> = None;
            while let Ok(ev) = rx.try_recv() {
                match ev {
                    BuildEvent::Line(l) => self.output.push(l),
                    BuildEvent::Done(r) => done = Some(r),
                }
            }
            if let Some(r) = done {
                self.build_rx = None;
                let target = self.build_target.take();
                changed = true;
                match (r, target) {
                    (Ok(exe), Some(t)) => {
                        let msg = self.start_lldb(root, exe, t);
                        self.status = Some(msg);
                    }
                    (Err(e), _) => {
                        self.status = Some(e);
                        self.rebuild();
                    }
                    _ => {}
                }
            }
        }
        // adapter
        let mut msgs = Vec::new();
        if let Some(s) = &self.session {
            while let Ok(m) = s.client.rx.try_recv() {
                msgs.push(m);
            }
        }
        for m in msgs {
            changed = true;
            self.handle(m);
        }
        changed
    }

    fn handle(&mut self, m: DapMessage) {
        match m {
            DapMessage::Stderr(l) => self.output.push(format!("[adapter] {}", l)),
            DapMessage::Closed => {
                if self.session.is_some() {
                    self.output.push("[debug session ended]".into());
                    self.status = Some("debug session ended".into());
                    self.end_session();
                }
            }
            DapMessage::Event { event, body } => self.handle_event(&event, body),
            DapMessage::Response {
                request_seq,
                command,
                success,
                message,
                body,
            } => {
                let Some(s) = self.session.as_mut() else {
                    return;
                };
                let pending = s.pending.remove(&request_seq).unwrap_or(Pending::Other);
                if !success {
                    let msg = message
                        .or_else(|| {
                            body.pointer("/error/format")
                                .and_then(|f| f.as_str())
                                .map(|s| s.to_string())
                        })
                        .unwrap_or_default();
                    self.output.push(format!("[{} failed] {}", command, msg));
                    if command == "launch" || command == "initialize" {
                        self.status = Some(format!("{} failed: {}", command, msg));
                        self.end_session();
                    }
                    return;
                }
                match pending {
                    Pending::Initialize => {
                        let launch = s.launch_args.clone();
                        let seq = s.client.request("launch", launch);
                        s.pending.insert(seq, Pending::Other);
                    }
                    Pending::Threads => {
                        if s.thread_id.is_none() {
                            s.thread_id = body.pointer("/threads/0/id").and_then(|i| i.as_i64());
                        }
                        let tid = s.thread_id.unwrap_or(1);
                        let seq = s.client.request(
                            "stackTrace",
                            json!({"threadId": tid, "startFrame": 0, "levels": 64}),
                        );
                        s.pending.insert(seq, Pending::StackTrace);
                    }
                    Pending::StackTrace => {
                        s.frames = body
                            .get("stackFrames")
                            .and_then(|f| f.as_array())
                            .map(|a| {
                                a.iter()
                                    .map(|f| Frame {
                                        id: f.get("id").and_then(|i| i.as_i64()).unwrap_or(0),
                                        name: f
                                            .get("name")
                                            .and_then(|n| n.as_str())
                                            .unwrap_or("?")
                                            .to_string(),
                                        file: f
                                            .pointer("/source/path")
                                            .and_then(|p| p.as_str())
                                            .map(PathBuf::from),
                                        line: f.get("line").and_then(|l| l.as_u64()).unwrap_or(0)
                                            as usize,
                                    })
                                    .collect()
                            })
                            .unwrap_or_default();
                        // pick the first frame that has source
                        let idx = s
                            .frames
                            .iter()
                            .position(|f| f.file.as_ref().map(|p| p.exists()).unwrap_or(false))
                            .unwrap_or(0);
                        self.select_frame(idx);
                        return;
                    }
                    Pending::Scopes => {
                        let scopes = body
                            .get("scopes")
                            .and_then(|a| a.as_array())
                            .cloned()
                            .unwrap_or_default();
                        s.vars.clear();
                        for sc in scopes {
                            let name = sc
                                .get("name")
                                .and_then(|n| n.as_str())
                                .unwrap_or("scope")
                                .to_string();
                            let vref = sc
                                .get("variablesReference")
                                .and_then(|v| v.as_i64())
                                .unwrap_or(0);
                            let expensive = sc
                                .get("expensive")
                                .and_then(|e| e.as_bool())
                                .unwrap_or(false);
                            s.vars.push(Var {
                                name,
                                value: String::new(),
                                vref,
                                depth: 0,
                                expanded: !expensive,
                                loaded: false,
                            });
                        }
                        // load the non-expensive scopes
                        for i in 0..s.vars.len() {
                            if s.vars[i].expanded && s.vars[i].vref > 0 {
                                s.vars[i].loaded = true;
                                let vref = s.vars[i].vref;
                                let seq = s
                                    .client
                                    .request("variables", json!({"variablesReference": vref}));
                                s.pending.insert(seq, Pending::Variables(vref));
                            }
                        }
                    }
                    Pending::Variables(vref) => {
                        let Some(parent) = s.vars.iter().position(|v| v.vref == vref) else {
                            return;
                        };
                        let depth = s.vars[parent].depth + 1;
                        let children: Vec<Var> = body
                            .get("variables")
                            .and_then(|a| a.as_array())
                            .map(|a| {
                                a.iter()
                                    .map(|v| Var {
                                        name: v
                                            .get("name")
                                            .and_then(|n| n.as_str())
                                            .unwrap_or("?")
                                            .to_string(),
                                        value: v
                                            .get("value")
                                            .and_then(|n| n.as_str())
                                            .unwrap_or("")
                                            .replace('\n', " "),
                                        vref: v
                                            .get("variablesReference")
                                            .and_then(|r| r.as_i64())
                                            .unwrap_or(0),
                                        depth,
                                        expanded: false,
                                        loaded: false,
                                    })
                                    .collect()
                            })
                            .unwrap_or_default();
                        // insert after the parent's existing subtree
                        let mut at = parent + 1;
                        while at < s.vars.len() && s.vars[at].depth > s.vars[parent].depth {
                            at += 1;
                        }
                        s.vars.splice(at..at, children);
                    }
                    Pending::Other => {}
                }
                self.rebuild();
            }
        }
    }

    fn handle_event(&mut self, event: &str, body: Value) {
        let Some(s) = self.session.as_mut() else {
            return;
        };
        match event {
            "initialized" => {
                self.send_all_breakpoints();
                let s = self.session.as_mut().unwrap();
                let seq = s
                    .client
                    .request("setExceptionBreakpoints", json!({"filters": []}));
                s.pending.insert(seq, Pending::Other);
                let seq = s.client.request("configurationDone", json!({}));
                s.pending.insert(seq, Pending::Other);
                s.state = State::Running;
            }
            "stopped" => {
                s.state = State::Stopped;
                s.stop_reason = body
                    .get("reason")
                    .and_then(|r| r.as_str())
                    .unwrap_or("stopped")
                    .to_string();
                if let Some(t) = body.get("description").and_then(|d| d.as_str())
                    && !t.is_empty()
                {
                    s.stop_reason = t.to_string();
                }
                if let Some(tid) = body.get("threadId").and_then(|t| t.as_i64()) {
                    s.thread_id = Some(tid);
                }
                let seq = s.client.request("threads", json!({}));
                s.pending.insert(seq, Pending::Threads);
                self.status = Some(format!(
                    "stopped: {}",
                    self.session.as_ref().unwrap().stop_reason
                ));
            }
            "continued" => {
                s.state = State::Running;
                s.frames.clear();
                s.vars.clear();
                self.current = None;
            }
            "output" => {
                let text = body.get("output").and_then(|o| o.as_str()).unwrap_or("");
                let cat = body
                    .get("category")
                    .and_then(|c| c.as_str())
                    .unwrap_or("console");
                if cat != "telemetry" {
                    self.partial.push_str(text);
                    while let Some(i) = self.partial.find('\n') {
                        let line = self.partial[..i].to_string();
                        self.output.push(line);
                        self.partial.drain(..=i);
                    }
                }
            }
            "exited" => {
                if !self.partial.is_empty() {
                    let rest = std::mem::take(&mut self.partial);
                    self.output.push(rest);
                }
                let code = body.get("exitCode").and_then(|c| c.as_i64()).unwrap_or(-1);
                self.output
                    .push(format!("[program exited with code {}]", code));
                s.state = State::Exited;
                self.status = Some(format!("program exited ({})", code));
            }
            "terminated" => {
                self.output.push("[debug session terminated]".into());
                self.status = Some("debug session terminated".into());
                self.end_session();
                return;
            }
            _ => {}
        }
        self.rebuild();
    }
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for c2 in chars.by_ref() {
                    if c2.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}
