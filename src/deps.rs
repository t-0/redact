//! Dependency health for Cargo.toml: resolved version from Cargo.lock, latest
//! from the crates.io sparse index, shown inline next to each dependency.

use std::collections::HashMap;
use std::process::{Command, Stdio};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use semver::{Version, VersionReq};

/// One dependency line found in Cargo.toml.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DepLine {
    /// Crate name on crates.io (after `package = "..."` renames).
    pub name: String,
    /// Version requirement text, or None for path/git/workspace dependencies.
    pub req: Option<String>,
    pub line: usize,
    pub source: Source,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Registry,
    Path,
    Git,
    Workspace,
}

/// Versions published for a crate, newest first, with yanked flags.
#[derive(Clone, Debug)]
pub struct CrateVersions {
    pub versions: Vec<(Version, bool)>,
    pub found: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Health {
    Fetching,
    UpToDate(String),
    /// (latest, resolved-or-requirement)
    Outdated(String, String),
    Yanked(String),
    NotFound,
    Local(&'static str),
    NoCurl,
}

impl Health {
    pub fn color(&self) -> ratatui::style::Color {
        use crate::theme;
        match self {
            Health::UpToDate(_) => theme::git_added(),
            Health::Outdated(..) => theme::git_modified(),
            Health::Yanked(_) | Health::NotFound => theme::git_deleted(),
            Health::Fetching | Health::Local(_) | Health::NoCurl => theme::fg_dim(),
        }
    }

    pub fn text(&self) -> String {
        match self {
            Health::Fetching => "…".into(),
            Health::UpToDate(v) => format!("✓ {}", v),
            Health::Outdated(latest, have) => format!("⬆ {} available  (have {})", latest, have),
            Health::Yanked(v) => format!("⚠ {} is yanked", v),
            Health::NotFound => "not on crates.io".into(),
            Health::Local(kind) => kind.to_string(),
            Health::NoCurl => "curl not found".into(),
        }
    }
}

/// Find dependency lines by scanning the TOML text section by section.
pub fn parse_cargo_toml(text: &str) -> Vec<DepLine> {
    let mut out = Vec::new();
    let mut in_deps = false;
    // `[dependencies.foo]` style: the dep is named by the header
    let mut header_dep: Option<(String, usize)> = None;
    for (i, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.starts_with('[') {
            let name = line.trim_matches(|c| c == '[' || c == ']').trim();
            header_dep = None;
            in_deps = false;
            let parts: Vec<&str> = name.split('.').collect();
            // dependencies / dev-dependencies / build-dependencies, optionally under
            // target."cfg".  or workspace.
            let idx = parts.iter().position(|p| p.ends_with("dependencies"));
            if let Some(idx) = idx {
                if idx + 1 == parts.len() {
                    in_deps = true;
                } else if idx + 2 == parts.len() {
                    header_dep = Some((parts[idx + 1].trim_matches('"').to_string(), i));
                }
            }
            continue;
        }
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((name, _)) = &header_dep {
            // inside [dependencies.name]: look for version / path / git / package
            if let Some((k, v)) = line.split_once('=') {
                let k = k.trim();
                let v = v.trim().trim_matches('"');
                match k {
                    "version" => out.push(DepLine {
                        name: name.clone(),
                        req: Some(v.to_string()),
                        line: i,
                        source: Source::Registry,
                    }),
                    "path" => out.push(DepLine {
                        name: name.clone(),
                        req: None,
                        line: i,
                        source: Source::Path,
                    }),
                    "git" => out.push(DepLine {
                        name: name.clone(),
                        req: None,
                        line: i,
                        source: Source::Git,
                    }),
                    "package" => {
                        if let Some(last) = out.last_mut()
                            && last.line >= i.saturating_sub(5)
                        {
                            last.name = v.to_string();
                        }
                    }
                    _ => {}
                }
            }
            continue;
        }
        if !in_deps {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        // `foo.workspace = true` / `foo.version = "1"` dotted keys
        let (name, dotted) = match key.split_once('.') {
            Some((n, rest)) => (n.trim().trim_matches('"'), Some(rest.trim())),
            None => (key.trim_matches('"'), None),
        };
        if name.is_empty() {
            continue;
        }
        let mut dep = DepLine {
            name: name.to_string(),
            req: None,
            line: i,
            source: Source::Registry,
        };
        if let Some(d) = dotted {
            match d {
                "workspace" => dep.source = Source::Workspace,
                "version" => dep.req = Some(value.trim_matches('"').to_string()),
                "path" => dep.source = Source::Path,
                "git" => dep.source = Source::Git,
                _ => continue,
            }
            out.push(dep);
            continue;
        }
        if value.starts_with('"') || value.starts_with('\'') {
            dep.req = Some(value.trim_matches(|c| c == '"' || c == '\'').to_string());
        } else if value.starts_with('{') {
            let inner = value.trim_start_matches('{').trim_end_matches('}');
            for field in split_inline_table(inner) {
                let Some((k, v)) = field.split_once('=') else {
                    continue;
                };
                let k = k.trim();
                let v = v.trim().trim_matches('"');
                match k {
                    "version" => dep.req = Some(v.to_string()),
                    "path" => dep.source = Source::Path,
                    "git" => dep.source = Source::Git,
                    "workspace" => dep.source = Source::Workspace,
                    "package" => dep.name = v.to_string(),
                    _ => {}
                }
            }
        } else {
            continue;
        }
        if dep.source == Source::Registry && dep.req.is_none() {
            // `foo = { features = [...] }` without a version: nothing to compare
            continue;
        }
        out.push(dep);
    }
    out
}

/// Split `a = "1", features = ["x", "y"]` at top-level commas.
fn split_inline_table(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0;
    let mut in_str = false;
    let mut cur = String::new();
    for c in s.chars() {
        match c {
            '"' => in_str = !in_str,
            '[' | '{' if !in_str => depth += 1,
            ']' | '}' if !in_str => depth -= 1,
            ',' if !in_str && depth == 0 => {
                out.push(std::mem::take(&mut cur));
                continue;
            }
            _ => {}
        }
        cur.push(c);
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out
}

/// Resolved versions from Cargo.lock: name -> every locked version.
pub fn parse_lock(text: &str) -> HashMap<String, Vec<Version>> {
    let mut out: HashMap<String, Vec<Version>> = HashMap::new();
    let Ok(table) = toml::from_str::<toml::Table>(text) else {
        return out;
    };
    for pkg in table
        .get("package")
        .and_then(|p| p.as_array())
        .into_iter()
        .flatten()
    {
        let (Some(name), Some(ver)) = (
            pkg.get("name").and_then(|n| n.as_str()),
            pkg.get("version").and_then(|v| v.as_str()),
        ) else {
            continue;
        };
        if let Ok(v) = Version::parse(ver) {
            out.entry(name.to_string()).or_default().push(v);
        }
    }
    out
}

fn index_url(name: &str) -> String {
    let n = name.to_ascii_lowercase();
    let path = match n.len() {
        1 => format!("1/{}", n),
        2 => format!("2/{}", n),
        3 => format!("3/{}/{}", &n[..1], n),
        _ => format!("{}/{}/{}", &n[..2], &n[2..4], n),
    };
    format!("https://index.crates.io/{}", path)
}

fn fetch_versions(name: &str) -> CrateVersions {
    let out = Command::new("curl")
        .args(["-sL", "--max-time", "15", "-w", "\n%{http_code}"])
        .arg(index_url(name))
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();
    let Ok(out) = out else {
        return CrateVersions {
            versions: Vec::new(),
            found: false,
        };
    };
    let body = String::from_utf8_lossy(&out.stdout);
    let mut lines: Vec<&str> = body.lines().collect();
    let code = lines.pop().unwrap_or("").trim().to_string();
    if code != "200" {
        return CrateVersions {
            versions: Vec::new(),
            found: false,
        };
    }
    let mut versions: Vec<(Version, bool)> = Vec::new();
    for l in lines {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(l) else {
            continue;
        };
        let Some(vers) = v.get("vers").and_then(|x| x.as_str()) else {
            continue;
        };
        let yanked = v.get("yanked").and_then(|y| y.as_bool()).unwrap_or(false);
        if let Ok(ver) = Version::parse(vers) {
            versions.push((ver, yanked));
        }
    }
    versions.sort_by(|a, b| b.0.cmp(&a.0));
    CrateVersions {
        versions,
        found: true,
    }
}

/// Background fetcher with an in-memory cache.
pub struct DepsService {
    cache: Arc<Mutex<HashMap<String, (Instant, CrateVersions)>>>,
    in_flight: Arc<Mutex<std::collections::HashSet<String>>>,
    tx: Sender<String>,
    pub rx: Receiver<String>,
    has_curl: bool,
    /// Bumped whenever a fetch completes, so views know to rebuild.
    pub generation: u64,
}

const TTL: Duration = Duration::from_secs(60 * 60);

impl DepsService {
    pub fn new() -> DepsService {
        let (tx, rx) = channel();
        let has_curl = Command::new("curl")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        DepsService {
            cache: Arc::new(Mutex::new(HashMap::new())),
            in_flight: Arc::new(Mutex::new(Default::default())),
            tx,
            rx,
            has_curl,
            generation: 0,
        }
    }

    /// Kick off fetches for any crate not cached (or stale); returns how many started.
    pub fn request(&mut self, names: &[String]) -> usize {
        if !self.has_curl {
            return 0;
        }
        let mut started = 0;
        for name in names {
            {
                let cache = self.cache.lock().unwrap();
                if cache
                    .get(name)
                    .map(|(t, _)| t.elapsed() < TTL)
                    .unwrap_or(false)
                {
                    continue;
                }
            }
            if !self.in_flight.lock().unwrap().insert(name.clone()) {
                continue;
            }
            started += 1;
            let cache = self.cache.clone();
            let in_flight = self.in_flight.clone();
            let tx = self.tx.clone();
            let name = name.clone();
            std::thread::spawn(move || {
                let v = fetch_versions(&name);
                cache
                    .lock()
                    .unwrap()
                    .insert(name.clone(), (Instant::now(), v));
                in_flight.lock().unwrap().remove(&name);
                let _ = tx.send(name);
            });
        }
        started
    }

    /// Drain completion notices; true if anything arrived.
    pub fn pump(&mut self) -> bool {
        let mut any = false;
        while self.rx.try_recv().is_ok() {
            any = true;
        }
        if any {
            self.generation += 1;
        }
        any
    }

    pub fn versions(&self, name: &str) -> Option<CrateVersions> {
        self.cache.lock().unwrap().get(name).map(|(_, v)| v.clone())
    }

    /// Health of one dependency line given the lock file's resolved versions.
    pub fn health(&self, dep: &DepLine, locked: &HashMap<String, Vec<Version>>) -> Health {
        match dep.source {
            Source::Path => return Health::Local("path"),
            Source::Git => return Health::Local("git"),
            Source::Workspace => return Health::Local("workspace"),
            Source::Registry => {}
        }
        if !self.has_curl {
            return Health::NoCurl;
        }
        let Some(info) = self.versions(&dep.name) else {
            return Health::Fetching;
        };
        if !info.found {
            return Health::NotFound;
        }
        let req = dep.req.as_deref().and_then(|r| VersionReq::parse(r).ok());
        // resolved: the locked version that satisfies the requirement, else the highest locked
        let resolved: Option<Version> = locked.get(&dep.name).and_then(|vs| {
            let mut vs: Vec<&Version> = vs.iter().collect();
            vs.sort();
            vs.reverse();
            match &req {
                Some(r) => vs
                    .iter()
                    .find(|v| r.matches(v))
                    .or(vs.first())
                    .map(|v| (*v).clone()),
                None => vs.first().map(|v| (*v).clone()),
            }
        });
        let allow_pre = resolved
            .as_ref()
            .map(|v| !v.pre.is_empty())
            .unwrap_or(false)
            || dep.req.as_deref().map(|r| r.contains('-')).unwrap_or(false);
        let latest = info
            .versions
            .iter()
            .find(|(v, yanked)| !yanked && (allow_pre || v.pre.is_empty()))
            .map(|(v, _)| v.clone());
        let Some(latest) = latest else {
            return Health::NotFound;
        };
        if let Some(r) = &resolved
            && info.versions.iter().any(|(v, yanked)| *yanked && v == r)
        {
            return Health::Yanked(r.to_string());
        }
        let have = resolved
            .clone()
            .map(|v| v.to_string())
            .unwrap_or_else(|| dep.req.clone().unwrap_or_default());
        match &resolved {
            Some(r) if *r >= latest => Health::UpToDate(r.to_string()),
            Some(_) => Health::Outdated(latest.to_string(), have),
            None => {
                // no lock file: judge the requirement itself
                match &req {
                    Some(r) if r.matches(&latest) => Health::UpToDate(latest.to_string()),
                    _ => Health::Outdated(latest.to_string(), have),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_dependency_forms() {
        let text = r#"
[package]
name = "x"

[dependencies]
anyhow = "1"
serde_json = { version = "1", features = ["preserve_order"] }
local = { path = "../local" }
renamed = { package = "real-name", version = "0.3" }
ws.workspace = true

[dev-dependencies]
similar = '2'

[dependencies.tree-sitter]
version = "0.24"

[target.'cfg(unix)'.dependencies]
libc = "0.2"
"#;
        let deps = parse_cargo_toml(text);
        let names: Vec<(String, Option<String>, Source)> = deps
            .iter()
            .map(|d| (d.name.clone(), d.req.clone(), d.source))
            .collect();
        assert!(names.contains(&("anyhow".into(), Some("1".into()), Source::Registry)));
        assert!(names.contains(&("serde_json".into(), Some("1".into()), Source::Registry)));
        assert!(names.contains(&("local".into(), None, Source::Path)));
        assert!(names.contains(&("real-name".into(), Some("0.3".into()), Source::Registry)));
        assert!(names.contains(&("ws".into(), None, Source::Workspace)));
        assert!(names.contains(&("similar".into(), Some("2".into()), Source::Registry)));
        assert!(names.contains(&("tree-sitter".into(), Some("0.24".into()), Source::Registry)));
        assert!(names.contains(&("libc".into(), Some("0.2".into()), Source::Registry)));
        let anyhow = deps.iter().find(|d| d.name == "anyhow").unwrap();
        assert_eq!(anyhow.line, 5);
    }

    #[test]
    fn index_paths() {
        assert_eq!(index_url("a"), "https://index.crates.io/1/a");
        assert_eq!(index_url("ab"), "https://index.crates.io/2/ab");
        assert_eq!(index_url("abc"), "https://index.crates.io/3/a/abc");
        assert_eq!(
            index_url("ratatui"),
            "https://index.crates.io/ra/ta/ratatui"
        );
    }
}
