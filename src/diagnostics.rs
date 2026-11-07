//! Compiler diagnostics from `cargo check` and `ruff`/`python`, keyed by file.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{Receiver, channel};

use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Warning,
    Error,
}

impl Severity {
    pub fn color(self) -> ratatui::style::Color {
        match self {
            Severity::Error => crate::theme::git_deleted(),
            Severity::Warning => crate::theme::git_modified(),
        }
    }
}

/// One diagnostic; positions are 0-based line and char column, end exclusive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub line: usize,
    pub col: usize,
    pub end_line: usize,
    pub end_col: usize,
    pub severity: Severity,
    pub message: String,
}

pub enum CheckEvent {
    /// Replace every cargo diagnostic.
    Cargo(HashMap<PathBuf, Vec<Diagnostic>>, Vec<String>),
    /// Replace the per-file tool diagnostics (python, shell) for one file.
    File(PathBuf, Vec<Diagnostic>),
}

pub struct Diagnostics {
    cargo: HashMap<PathBuf, Vec<Diagnostic>>,
    python: HashMap<PathBuf, Vec<Diagnostic>>,
    /// JSON / TOML / XML well-formedness, checked in-process.
    data: HashMap<PathBuf, Vec<Diagnostic>>,
    /// Problems in the settings file, replaced on every reload.
    settings: HashMap<PathBuf, Vec<Diagnostic>>,
    rx: Option<Receiver<CheckEvent>>,
    tx_cache: Option<std::sync::mpsc::Sender<CheckEvent>>,
    pub cargo_running: bool,
    cargo_rerun: bool,
    /// Rendered compiler output of the last cargo check, for the problems tab.
    pub rendered: Vec<String>,
    pub changed: bool,
}

impl Diagnostics {
    pub fn new() -> Diagnostics {
        Diagnostics {
            cargo: HashMap::new(),
            python: HashMap::new(),
            data: HashMap::new(),
            settings: HashMap::new(),
            rx: None,
            tx_cache: None,
            cargo_running: false,
            cargo_rerun: false,
            rendered: Vec::new(),
            changed: false,
        }
    }

    pub fn for_file(&self, path: &Path) -> Vec<Diagnostic> {
        let mut v: Vec<Diagnostic> = Vec::new();
        if let Some(d) = self.cargo.get(path) {
            v.extend(d.iter().cloned());
        }
        if let Some(d) = self.python.get(path) {
            v.extend(d.iter().cloned());
        }
        if let Some(d) = self.data.get(path) {
            v.extend(d.iter().cloned());
        }
        if let Some(d) = self.settings.get(path) {
            v.extend(d.iter().cloned());
        }
        v.sort_by_key(|d| (d.line, d.col));
        v
    }

    /// Parse a JSON, TOML or XML file with a real parser and record the first error.
    /// Replace the settings file's problems (an empty list clears them).
    pub fn set_settings(&mut self, path: &Path, diags: Vec<Diagnostic>) {
        self.settings.clear();
        if !diags.is_empty() {
            self.settings.insert(path.to_path_buf(), diags);
        }
        self.changed = true;
    }

    pub fn check_data(&mut self, path: &Path) {
        let diags = check_data_file(path);
        if diags.is_empty() {
            self.data.remove(path);
        } else {
            self.data.insert(path.to_path_buf(), diags);
        }
        self.changed = true;
    }

    /// Every diagnostic in the project, sorted by file then position.
    pub fn all(&self) -> Vec<(PathBuf, Diagnostic)> {
        let mut v: Vec<(PathBuf, Diagnostic)> = Vec::new();
        for (p, ds) in self
            .cargo
            .iter()
            .chain(self.python.iter())
            .chain(self.data.iter())
            .chain(self.settings.iter())
        {
            for d in ds {
                v.push((p.clone(), d.clone()));
            }
        }
        v.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then(a.1.line.cmp(&b.1.line))
                .then(a.1.col.cmp(&b.1.col))
        });
        v
    }

    pub fn counts(&self) -> (usize, usize) {
        let mut e = 0;
        let mut w = 0;
        for ds in self
            .cargo
            .values()
            .chain(self.python.values())
            .chain(self.settings.values())
            .chain(self.data.values())
        {
            for d in ds {
                match d.severity {
                    Severity::Error => e += 1,
                    Severity::Warning => w += 1,
                }
            }
        }
        (e, w)
    }

    fn sender(&mut self) -> std::sync::mpsc::Sender<CheckEvent> {
        // one channel shared by every checker thread
        if self.rx.is_none() {
            let (tx, rx) = channel();
            self.rx = Some(rx);
            self.tx_cache = Some(tx);
        }
        self.tx_cache.clone().unwrap()
    }

    /// Run `cargo check` in the background (queued if one is already running).
    pub fn check_cargo(&mut self, root: &Path) {
        if !root.join("Cargo.toml").exists() {
            return;
        }
        if self.cargo_running {
            self.cargo_rerun = true;
            return;
        }
        self.cargo_running = true;
        let tx = self.sender();
        let root = root.to_path_buf();
        std::thread::spawn(move || {
            let (map, rendered) = run_cargo_check(&root);
            let _ = tx.send(CheckEvent::Cargo(map, rendered));
        });
    }

    /// Check one python file in the background.
    pub fn check_python(&mut self, root: &Path, file: &Path) {
        let tx = self.sender();
        let root = root.to_path_buf();
        let file = file.to_path_buf();
        std::thread::spawn(move || {
            let diags = run_python_check(&root, &file);
            let _ = tx.send(CheckEvent::File(file, diags));
        });
    }

    /// Check a shell script in the background with shellcheck, or the shell's own `-n`.
    pub fn check_shell(&mut self, root: &Path, file: &Path) {
        let tx = self.sender();
        let root = root.to_path_buf();
        let file = file.to_path_buf();
        std::thread::spawn(move || {
            let diags = run_shell_check(&root, &file);
            let _ = tx.send(CheckEvent::File(file, diags));
        });
    }

    /// Drain results; returns true if anything changed.
    pub fn pump(&mut self, root: &Path) -> bool {
        let mut events = Vec::new();
        if let Some(rx) = &self.rx {
            while let Ok(ev) = rx.try_recv() {
                events.push(ev);
            }
        }
        let mut changed = false;
        for ev in events {
            changed = true;
            match ev {
                CheckEvent::Cargo(map, rendered) => {
                    self.cargo = map;
                    self.rendered = rendered;
                    self.cargo_running = false;
                    if self.cargo_rerun {
                        self.cargo_rerun = false;
                        self.check_cargo(root);
                    }
                }
                CheckEvent::File(file, diags) => {
                    if diags.is_empty() {
                        self.python.remove(&file);
                    } else {
                        self.python.insert(file, diags);
                    }
                }
            }
        }
        if changed {
            self.changed = true;
        }
        changed
    }
}

fn run_cargo_check(root: &Path) -> (HashMap<PathBuf, Vec<Diagnostic>>, Vec<String>) {
    let mut map: HashMap<PathBuf, Vec<Diagnostic>> = HashMap::new();
    let mut rendered: Vec<String> = Vec::new();
    let out = Command::new("cargo")
        .args(["check", "--all-targets", "--message-format=json"])
        .current_dir(root)
        .env("CARGO_TERM_COLOR", "never")
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .stdout(Stdio::piped())
        .output();
    let Ok(out) = out else {
        rendered.push("cargo check failed to start".into());
        return (map, rendered);
    };
    // workspace root for resolving relative file names
    let ws_root = Command::new("cargo")
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .current_dir(root)
        .stderr(Stdio::null())
        .output()
        .ok()
        .and_then(|o| serde_json::from_slice::<Value>(&o.stdout).ok())
        .and_then(|v| {
            v.get("workspace_root")
                .and_then(|w| w.as_str())
                .map(PathBuf::from)
        })
        .unwrap_or_else(|| root.to_path_buf());
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if v.get("reason").and_then(|r| r.as_str()) != Some("compiler-message") {
            continue;
        }
        let Some(msg) = v.get("message") else {
            continue;
        };
        let level = msg.get("level").and_then(|l| l.as_str()).unwrap_or("");
        let severity = match level {
            "error" => Severity::Error,
            "warning" => Severity::Warning,
            _ => continue,
        };
        let text = msg
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("")
            .to_string();
        if text.starts_with("aborting due to")
            || text.contains("warning emitted")
            || text.contains("warnings emitted")
        {
            continue;
        }
        if let Some(r) = msg.get("rendered").and_then(|r| r.as_str()) {
            rendered.extend(r.lines().map(|l| l.to_string()));
        }
        let spans = msg
            .get("spans")
            .and_then(|s| s.as_array())
            .cloned()
            .unwrap_or_default();
        // primary span, or the first span that is in the project
        let span = spans
            .iter()
            .find(|s| {
                s.get("is_primary")
                    .and_then(|p| p.as_bool())
                    .unwrap_or(false)
            })
            .or_else(|| spans.first());
        let Some(span) = span else { continue };
        let file = span.get("file_name").and_then(|f| f.as_str()).unwrap_or("");
        let path = PathBuf::from(file);
        let path = if path.is_absolute() {
            path
        } else {
            ws_root.join(path)
        };
        if !path.starts_with(root) {
            continue;
        }
        let label = span.get("label").and_then(|l| l.as_str()).unwrap_or("");
        let message = if label.is_empty() || label == text {
            text.clone()
        } else {
            format!("{}: {}", text, label)
        };
        let d = Diagnostic {
            line: span
                .get("line_start")
                .and_then(|l| l.as_u64())
                .unwrap_or(1)
                .saturating_sub(1) as usize,
            col: span
                .get("column_start")
                .and_then(|c| c.as_u64())
                .unwrap_or(1)
                .saturating_sub(1) as usize,
            end_line: span
                .get("line_end")
                .and_then(|l| l.as_u64())
                .unwrap_or(1)
                .saturating_sub(1) as usize,
            end_col: span
                .get("column_end")
                .and_then(|c| c.as_u64())
                .unwrap_or(1)
                .saturating_sub(1) as usize,
            severity,
            message,
        };
        let entry = map.entry(path).or_default();
        if !entry.contains(&d) {
            entry.push(d);
        }
    }
    if !out.status.success() && map.is_empty() {
        let err = String::from_utf8_lossy(&out.stderr);
        rendered.extend(err.lines().map(|l| l.to_string()));
    }
    (map, rendered)
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

fn run_python_check(root: &Path, file: &Path) -> Vec<Diagnostic> {
    // Prefer ruff (syntax + lint); fall back to the parser for syntax errors only.
    let ruff: Option<(String, Vec<String>)> = if which("ruff") {
        Some(("ruff".into(), vec![]))
    } else if which("uvx") {
        Some(("uvx".into(), vec!["ruff".into()]))
    } else {
        None
    };
    if let Some((prog, pre)) = ruff {
        let mut args = pre;
        args.extend([
            "check".to_string(),
            "--output-format=json".into(),
            "--quiet".into(),
            "--no-cache".into(),
            file.to_string_lossy().into_owned(),
        ]);
        if let Ok(out) = Command::new(&prog)
            .args(&args)
            .current_dir(root)
            .stdin(Stdio::null())
            .output()
            && let Ok(v) = serde_json::from_slice::<Value>(&out.stdout)
        {
            let mut diags = Vec::new();
            for item in v.as_array().into_iter().flatten() {
                let code = item.get("code").and_then(|c| c.as_str()).unwrap_or("");
                let msg = item
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("")
                    .to_string();
                let loc = item.get("location").cloned().unwrap_or(Value::Null);
                let end = item.get("end_location").cloned().unwrap_or(Value::Null);
                let severity = if code.is_empty()
                    || code.starts_with("E9")
                    || code == "F821"
                    || code == "F822"
                    || code == "F823"
                {
                    Severity::Error
                } else {
                    Severity::Warning
                };
                diags.push(Diagnostic {
                    line: loc
                        .get("row")
                        .and_then(|r| r.as_u64())
                        .unwrap_or(1)
                        .saturating_sub(1) as usize,
                    col: loc
                        .get("column")
                        .and_then(|c| c.as_u64())
                        .unwrap_or(1)
                        .saturating_sub(1) as usize,
                    end_line: end
                        .get("row")
                        .and_then(|r| r.as_u64())
                        .unwrap_or(1)
                        .saturating_sub(1) as usize,
                    end_col: end
                        .get("column")
                        .and_then(|c| c.as_u64())
                        .unwrap_or(1)
                        .saturating_sub(1) as usize,
                    severity,
                    message: if code.is_empty() {
                        msg
                    } else {
                        format!("{} [{}]", msg, code)
                    },
                });
            }
            return diags;
        }
    }
    // fallback: python's own parser
    let script = "import ast,sys,json\np=sys.argv[1]\ntry:\n    ast.parse(open(p,encoding='utf-8').read(), p)\nexcept SyntaxError as e:\n    print(json.dumps({'line':e.lineno,'col':e.offset,'end_line':e.end_lineno,'end_col':e.end_offset,'msg':e.msg}))\n";
    let out = Command::new("python3")
        .args(["-I", "-c", script])
        .arg(file)
        .current_dir(root)
        .stdin(Stdio::null())
        .output();
    let Ok(out) = out else { return Vec::new() };
    let Ok(v) = serde_json::from_slice::<Value>(&out.stdout) else {
        return Vec::new();
    };
    let line = v
        .get("line")
        .and_then(|l| l.as_u64())
        .unwrap_or(1)
        .saturating_sub(1) as usize;
    let col = v
        .get("col")
        .and_then(|c| c.as_u64())
        .unwrap_or(1)
        .saturating_sub(1) as usize;
    vec![Diagnostic {
        line,
        col,
        end_line: v
            .get("end_line")
            .and_then(|l| l.as_u64())
            .map(|l| l.saturating_sub(1) as usize)
            .unwrap_or(line),
        end_col: v
            .get("end_col")
            .and_then(|c| c.as_u64())
            .map(|c| c.saturating_sub(1) as usize)
            .unwrap_or(col + 1),
        severity: Severity::Error,
        message: v
            .get("msg")
            .and_then(|m| m.as_str())
            .unwrap_or("syntax error")
            .to_string(),
    }]
}

fn line_col_of_byte(text: &str, byte: usize) -> (usize, usize) {
    let byte = byte.min(text.len());
    let before = &text[..byte];
    let line = before.matches('\n').count();
    let col = before.rsplit('\n').next().unwrap_or("").chars().count();
    (line, col)
}

/// Strip a trailing " at line N column M" / " at N:M" that parsers append.
fn strip_position(msg: &str) -> String {
    let mut m = msg.to_string();
    if let Some(i) = m.rfind(" at line ") {
        m.truncate(i);
    } else if let Some(i) = m.rfind(" at ")
        && m[i + 4..].chars().all(|c| c.is_ascii_digit() || c == ':')
    {
        m.truncate(i);
    }
    m
}

/// File extensions that get an in-process well-formedness check.
pub fn is_data_file(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref(),
        Some(
            "json" | "geojson" | "toml" | "xml" | "svg" | "xsl" | "xslt" | "xsd" | "plist" | "xaml"
        )
    )
}

pub fn check_data_file(path: &Path) -> Vec<Diagnostic> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    let one = |line: usize, col: usize, message: String| {
        let end_col = text
            .lines()
            .nth(line)
            .map(|l| l.chars().count())
            .unwrap_or(col + 1)
            .max(col + 1);
        vec![Diagnostic {
            line,
            col,
            end_line: line,
            end_col,
            severity: Severity::Error,
            message,
        }]
    };
    match ext.as_str() {
        "json" | "geojson" => match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(_) => Vec::new(),
            Err(e) => one(
                e.line().saturating_sub(1),
                e.column().saturating_sub(1),
                strip_position(&e.to_string()),
            ),
        },
        "toml" => match toml::from_str::<toml::Table>(&text) {
            Ok(_) => Vec::new(),
            Err(e) => {
                let (line, col) = e
                    .span()
                    .map(|s| line_col_of_byte(&text, s.start))
                    .unwrap_or((0, 0));
                one(line, col, e.message().to_string())
            }
        },
        "xml" | "svg" | "xsl" | "xslt" | "xsd" | "plist" | "xaml" => {
            match roxmltree::Document::parse(&text) {
                Ok(_) => Vec::new(),
                Err(e) => {
                    let pos = e.pos();
                    one(
                        (pos.row as usize).saturating_sub(1),
                        (pos.col as usize).saturating_sub(1),
                        strip_position(&e.to_string()),
                    )
                }
            }
        }
        _ => Vec::new(),
    }
}

/// Which shell a script is written for, from its shebang or extension.
fn shell_for(file: &Path, text: &str) -> String {
    if let Some(rest) = text.lines().next().and_then(|l| l.strip_prefix("#!")) {
        let mut words = rest.split_whitespace();
        let mut interp = words
            .next()
            .unwrap_or("")
            .rsplit('/')
            .next()
            .unwrap_or("")
            .to_string();
        if interp == "env" {
            interp = words
                .find(|w| !w.starts_with('-'))
                .unwrap_or("")
                .to_string();
        }
        if !interp.is_empty() {
            return interp;
        }
    }
    match file.extension().and_then(|e| e.to_str()) {
        Some("bash") => "bash".into(),
        Some("zsh") => "zsh".into(),
        Some("ksh") | Some("mksh") => "ksh".into(),
        Some("dash") => "dash".into(),
        _ => "bash".into(),
    }
}

/// Pull "line N" / ":N:" out of a shell's `-n` error message.
fn parse_shell_error(line: &str, file: &Path) -> Option<(usize, String)> {
    let name = file.to_string_lossy();
    let rest = line.strip_prefix(name.as_ref()).unwrap_or(line);
    let rest = rest.trim_start_matches(':').trim();
    // bash / ksh: "line 5: msg" or "syntax error at line 5: msg"
    if let Some(i) = rest.find("line ") {
        let after = &rest[i + 5..];
        let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(n) = digits.parse::<usize>() {
            let msg = after[digits.len()..].trim_start_matches(':').trim();
            let prefix = rest[..i].trim().trim_end_matches("at").trim();
            let msg = if prefix.is_empty() {
                msg.to_string()
            } else {
                format!("{}: {}", prefix.trim_end_matches(':'), msg)
            };
            return Some((n, msg));
        }
    }
    // zsh: "5: msg"
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if let Ok(n) = digits.parse::<usize>() {
        let msg = rest[digits.len()..]
            .trim_start_matches(':')
            .trim()
            .to_string();
        return Some((n, msg));
    }
    None
}

fn run_shell_check(root: &Path, file: &Path) -> Vec<Diagnostic> {
    let text = std::fs::read_to_string(file).unwrap_or_default();
    let shell = shell_for(file, &text);
    if which("shellcheck") {
        // shellcheck only knows sh/bash/dash/ksh dialects; map the rest
        let dialect = match shell.as_str() {
            "bash" | "sh" | "dash" | "ksh" => shell.clone(),
            "zsh" => "bash".into(),
            _ => "sh".into(),
        };
        let out = Command::new("shellcheck")
            .args(["--format=json1", "--shell", &dialect])
            .arg(file)
            .current_dir(root)
            .stdin(Stdio::null())
            .output();
        if let Ok(out) = out
            && let Ok(v) = serde_json::from_slice::<Value>(&out.stdout)
        {
            let mut diags = Vec::new();
            for c in v
                .get("comments")
                .and_then(|c| c.as_array())
                .into_iter()
                .flatten()
            {
                let level = c.get("level").and_then(|l| l.as_str()).unwrap_or("");
                let severity = match level {
                    "error" => Severity::Error,
                    "warning" | "info" => Severity::Warning,
                    _ => continue, // "style"
                };
                let line = c
                    .get("line")
                    .and_then(|l| l.as_u64())
                    .unwrap_or(1)
                    .saturating_sub(1) as usize;
                let col = c
                    .get("column")
                    .and_then(|l| l.as_u64())
                    .unwrap_or(1)
                    .saturating_sub(1) as usize;
                let end_line = c
                    .get("endLine")
                    .and_then(|l| l.as_u64())
                    .unwrap_or(1)
                    .saturating_sub(1) as usize;
                let mut end_col = c
                    .get("endColumn")
                    .and_then(|l| l.as_u64())
                    .unwrap_or(1)
                    .saturating_sub(1) as usize;
                if end_line == line && end_col <= col {
                    end_col = col + 1;
                }
                let code = c.get("code").and_then(|c| c.as_u64()).unwrap_or(0);
                let msg = c
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("")
                    .to_string();
                diags.push(Diagnostic {
                    line,
                    col,
                    end_line,
                    end_col,
                    severity,
                    message: format!("{} [SC{}]", msg, code),
                });
            }
            return diags;
        }
    }
    // fallback: the shell's own parser
    let prog = if which(&shell) {
        shell
    } else {
        "sh".to_string()
    };
    let out = Command::new(&prog)
        .arg("-n")
        .arg(file)
        .current_dir(root)
        .stdin(Stdio::null())
        .output();
    let Ok(out) = out else { return Vec::new() };
    let mut diags = Vec::new();
    for l in String::from_utf8_lossy(&out.stderr).lines() {
        if let Some((n, msg)) = parse_shell_error(l, file) {
            let line = n.saturating_sub(1);
            let end_col = text
                .lines()
                .nth(line)
                .map(|l| l.chars().count())
                .unwrap_or(1)
                .max(1);
            diags.push(Diagnostic {
                line,
                col: 0,
                end_line: line,
                end_col,
                severity: Severity::Error,
                message: msg,
            });
        }
    }
    diags
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_error_formats() {
        let f = Path::new("/tmp/x.sh");
        // bash
        let (n, m) =
            parse_shell_error("/tmp/x.sh: line 5: syntax error: unexpected end of file", f)
                .unwrap();
        assert_eq!(n, 5);
        assert_eq!(m, "syntax error: unexpected end of file");
        // bash, unmatched quote
        let (n, m) = parse_shell_error(
            "/tmp/x.sh: line 5: unexpected EOF while looking for matching `\"'",
            f,
        )
        .unwrap();
        assert_eq!(n, 5);
        assert!(m.starts_with("unexpected EOF"));
        // zsh
        let (n, m) = parse_shell_error("/tmp/x.sh:6: unmatched \"", f).unwrap();
        assert_eq!((n, m.as_str()), (6, "unmatched \""));
        // ksh
        let (n, m) =
            parse_shell_error("/tmp/x.sh: syntax error at line 3: `fi' unexpected", f).unwrap();
        assert_eq!(n, 3);
        assert_eq!(m, "syntax error: `fi' unexpected");
        // noise
        assert!(parse_shell_error("something unrelated", f).is_none());
    }

    #[test]
    fn shell_from_shebang_or_extension() {
        assert_eq!(shell_for(Path::new("a"), "#!/usr/bin/env bash\n"), "bash");
        assert_eq!(shell_for(Path::new("a"), "#!/bin/sh -e\n"), "sh");
        assert_eq!(shell_for(Path::new("a.zsh"), ""), "zsh");
        assert_eq!(shell_for(Path::new("a.sh"), ""), "bash");
    }
}
