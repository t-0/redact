//! "Format document": the right formatter per language, run over the buffer text.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::editor::highlight::Lang;

pub enum Outcome {
    Formatted(String),
    Unchanged,
    /// The formatter this language needs is not installed.
    Unavailable(String),
    /// The formatter ran and rejected the input (first line of its complaint).
    Failed(String),
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

/// Run `prog args` with `input` on stdin; Ok((stdout, stderr, success)).
fn run_stdin(
    prog: &str,
    args: &[&str],
    cwd: &Path,
    input: &str,
) -> std::io::Result<(String, String, bool)> {
    let mut child = Command::new(prog)
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    {
        let mut stdin = child.stdin.take().expect("stdin");
        let _ = stdin.write_all(input.as_bytes());
    }
    let out = child.wait_with_output()?;
    Ok((
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.success(),
    ))
}

fn first_line(s: &str) -> String {
    s.lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("formatter failed")
        .trim()
        .to_string()
}

fn tool(prog: &str, args: &[&str], cwd: &Path, input: &str) -> Outcome {
    match run_stdin(prog, args, cwd, input) {
        Ok((out, _, true)) => done(input, out),
        Ok((_, err, false)) => Outcome::Failed(format!("{}: {}", prog, first_line(&err))),
        Err(e) => Outcome::Failed(format!("{}: {}", prog, e)),
    }
}

fn done(input: &str, out: String) -> Outcome {
    if out == input {
        Outcome::Unchanged
    } else {
        Outcome::Formatted(out)
    }
}

/// Edition from the nearest Cargo.toml, for rustfmt.
fn rust_edition(root: &Path) -> String {
    std::fs::read_to_string(root.join("Cargo.toml"))
        .ok()
        .and_then(|t| {
            t.lines().find_map(|l| {
                l.trim().strip_prefix("edition").map(|r| {
                    r.trim_start_matches([' ', '='])
                        .trim()
                        .trim_matches('"')
                        .to_string()
                })
            })
        })
        .filter(|e| !e.is_empty())
        .unwrap_or_else(|| "2021".to_string())
}

pub fn format(lang: Lang, text: &str, root: &Path, path: Option<&Path>) -> Outcome {
    let name = path
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "buffer".into());
    match lang {
        Lang::Rust => {
            if !which("rustfmt") {
                return Outcome::Unavailable("rustfmt (rustup component add rustfmt)".into());
            }
            let edition = rust_edition(root);
            tool(
                "rustfmt",
                &["--edition", &edition, "--emit", "stdout", "--quiet"],
                root,
                text,
            )
        }
        Lang::Python => {
            if which("ruff") {
                tool(
                    "ruff",
                    &["format", "--stdin-filename", &name, "-"],
                    root,
                    text,
                )
            } else if which("uvx") {
                tool(
                    "uvx",
                    &["ruff", "format", "--stdin-filename", &name, "-"],
                    root,
                    text,
                )
            } else if which("black") {
                tool("black", &["-q", "-"], root, text)
            } else {
                Outcome::Unavailable("ruff (or uv, or black)".into())
            }
        }
        Lang::Shell => {
            if which("shfmt") {
                tool("shfmt", &["-filename", &name, "-"], root, text)
            } else {
                Outcome::Unavailable("shfmt".into())
            }
        }
        Lang::Toml => {
            if which("taplo") {
                tool("taplo", &["fmt", "-"], root, text)
            } else {
                Outcome::Unavailable("taplo".into())
            }
        }
        Lang::Xml => {
            if which("xmllint") {
                tool("xmllint", &["--format", "-"], root, text)
            } else {
                Outcome::Unavailable("xmllint".into())
            }
        }
        Lang::Json => match serde_json::from_str::<serde_json::Value>(text) {
            Ok(v) => {
                let mut out = serde_json::to_string_pretty(&v).unwrap_or_default();
                out.push('\n');
                done(text, out)
            }
            Err(e) => Outcome::Failed(format!("json: {}", e)),
        },
        Lang::Markdown => done(text, tidy(text, false)),
        Lang::Ini | Lang::Plain => done(text, tidy(text, true)),
    }
}

/// Whitespace-only cleanup: collapse runs of blank lines, single trailing newline,
/// and (unless `keep_trailing`, for Markdown's two-space line breaks) strip
/// trailing spaces.
fn tidy(text: &str, strip_trailing: bool) -> String {
    let mut out = String::with_capacity(text.len());
    let mut blank_run = 0;
    for line in text.lines() {
        let l = if strip_trailing {
            line.trim_end()
        } else {
            line.trim_end_matches(['\t', '\r'])
        };
        if l.trim().is_empty() {
            blank_run += 1;
            if blank_run > 2 {
                continue;
            }
            out.push('\n');
        } else {
            blank_run = 0;
            out.push_str(l);
            out.push('\n');
        }
    }
    // exactly one trailing newline
    while out.ends_with("\n\n") {
        out.pop();
    }
    if out.is_empty() {
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tidy_collapses_blank_lines_and_trailing_space() {
        let t = "a  \n\n\n\n b\t\n\n\n";
        assert_eq!(tidy(t, true), "a\n\n\n b\n");
        // markdown keeps two-space breaks
        assert_eq!(tidy("x  \ny\n", false), "x  \ny\n");
    }

    #[test]
    fn json_pretty_prints_and_keeps_order() {
        let r = format(Lang::Json, "{\"b\":1,\"a\":[1,2]}", Path::new("."), None);
        match r {
            Outcome::Formatted(s) => {
                assert_eq!(s, "{\n  \"b\": 1,\n  \"a\": [\n    1,\n    2\n  ]\n}\n")
            }
            _ => panic!("expected formatted output"),
        }
        assert!(matches!(
            format(Lang::Json, "{\n  \"a\": 1\n}\n", Path::new("."), None),
            Outcome::Unchanged
        ));
        assert!(matches!(
            format(Lang::Json, "{", Path::new("."), None),
            Outcome::Failed(_)
        ));
    }
}
