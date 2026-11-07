//! Incremental tree-sitter syntax highlighting.
//!
//! The syntax tree is kept per editor and edited in place on every change, so a
//! keystroke costs a small incremental reparse instead of a full one. Highlight
//! queries only run over the lines currently on screen.

use ratatui::style::Style;
use streaming_iterator::StreamingIterator;
use tree_sitter::{InputEdit, Node, Parser, Point, Query, QueryCursor, TextProvider, Tree};

use crate::theme;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    Rust,
    Python,
    Json,
    Toml,
    Xml,
    Ini,
    Markdown,
    Shell,
    Plain,
}

impl Lang {
    /// Language from the path, refined by a shebang when the file has one.
    pub fn detect(path: &std::path::Path, text: &str) -> Lang {
        let by_path = Lang::from_path(path);
        if by_path != Lang::Plain {
            return by_path;
        }
        let first = text.lines().next().unwrap_or("");
        let Some(rest) = first.strip_prefix("#!") else {
            return Lang::Plain;
        };
        // "#!/usr/bin/env bash" -> bash, "#!/bin/sh -e" -> sh
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
        let interp = interp.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.');
        match interp {
            "sh" | "bash" | "zsh" | "ksh" | "mksh" | "dash" | "ash" | "pdksh" => Lang::Shell,
            "python" => Lang::Python,
            _ => Lang::Plain,
        }
    }

    pub fn from_path(path: &std::path::Path) -> Lang {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if matches!(
            name,
            ".gitconfig" | ".gitmodules" | ".editorconfig" | ".npmrc"
        ) {
            return Lang::Ini;
        }
        if matches!(
            name,
            ".bashrc"
                | ".bash_profile"
                | ".bash_aliases"
                | ".zshrc"
                | ".zshenv"
                | ".zprofile"
                | ".profile"
                | ".kshrc"
                | "PKGBUILD"
        ) {
            return Lang::Shell;
        }
        match path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref()
        {
            Some("rs") => Lang::Rust,
            Some("py") | Some("pyi") => Lang::Python,
            Some("json") | Some("jsonc") | Some("geojson") => Lang::Json,
            Some("toml") => Lang::Toml,
            Some("xml") | Some("svg") | Some("xsl") | Some("xslt") | Some("xsd")
            | Some("plist") | Some("xaml") | Some("csproj") | Some("fsproj") => Lang::Xml,
            Some("ini") | Some("cfg") | Some("conf") | Some("properties") | Some("desktop")
            | Some("service") => Lang::Ini,
            Some("md") | Some("markdown") | Some("mdx") => Lang::Markdown,
            Some("sh") | Some("bash") | Some("zsh") | Some("ksh") | Some("mksh") | Some("dash")
            | Some("ash") => Lang::Shell,
            _ => Lang::Plain,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Lang::Rust => "rust",
            Lang::Python => "python",
            Lang::Json => "json",
            Lang::Toml => "toml",
            Lang::Xml => "xml",
            Lang::Ini => "ini",
            Lang::Markdown => "markdown",
            Lang::Shell => "shell",
            Lang::Plain => "text",
        }
    }
}

/// A styled run on one line, in char columns `[start, end)`.
#[derive(Clone, Debug)]
pub struct Run {
    pub start: usize,
    pub end: usize,
    pub style: Style,
}

/// A parse error reported by tree-sitter; rows and columns are 0-based, columns in bytes.
#[derive(Clone, Debug)]
pub struct SyntaxError {
    pub row: usize,
    pub col: usize,
    pub end_row: usize,
    pub end_col: usize,
    pub message: String,
}

/// Lets tree-sitter read predicate text straight out of the line vector.
struct LinesText<'a>(&'a [String]);

impl<'a> TextProvider<&'a [u8]> for LinesText<'a> {
    type I = std::vec::IntoIter<&'a [u8]>;

    fn text(&mut self, node: Node) -> Self::I {
        let s = node.start_position();
        let e = node.end_position();
        let mut chunks: Vec<&'a [u8]> = Vec::new();
        for row in s.row..=e.row {
            let Some(line) = self.0.get(row) else { break };
            let b = line.as_bytes();
            let from = if row == s.row {
                s.column.min(b.len())
            } else {
                0
            };
            let to = if row == e.row {
                e.column.min(b.len())
            } else {
                b.len()
            };
            chunks.push(&b[from..to]);
            if row != e.row {
                chunks.push(b"\n");
            }
        }
        chunks.into_iter()
    }
}

/// A second grammar parsed over sub-ranges of the main tree (markdown inline content).
struct Injected {
    parser: Parser,
    query: Query,
    tree: Option<Tree>,
    styles: Vec<Style>,
    /// Node kind in the outer tree whose ranges are handed to this grammar.
    outer_kind: &'static str,
}

pub struct Syntax {
    parser: Parser,
    query: Query,
    tree: Option<Tree>,
    styles: Vec<Style>,
    dirty: bool,
    errors: Vec<SyntaxError>,
    injected: Option<Injected>,
    /// Whether parse errors are worth reporting (markdown has none; the XML grammar
    /// rejects valid prologs, so XML is left to the real parser).
    report_errors: bool,
}

/// tree-sitter's `parse_with` callback over a line vector.
fn chunk_at(lines: &[String], pt: Point) -> &[u8] {
    let last = lines.len().saturating_sub(1);
    match lines.get(pt.row) {
        Some(l) => {
            let b = l.as_bytes();
            if pt.column < b.len() {
                &b[pt.column..]
            } else if pt.row < last {
                b"\n"
            } else {
                b""
            }
        }
        None => b"",
    }
}

impl Syntax {
    pub fn new(lang: Lang) -> Option<Syntax> {
        let (language, query_src): (tree_sitter::Language, &str) = match lang {
            Lang::Rust => (
                tree_sitter_rust::LANGUAGE.into(),
                tree_sitter_rust::HIGHLIGHTS_QUERY,
            ),
            Lang::Python => (
                tree_sitter_python::LANGUAGE.into(),
                tree_sitter_python::HIGHLIGHTS_QUERY,
            ),
            Lang::Json => (
                tree_sitter_json::LANGUAGE.into(),
                tree_sitter_json::HIGHLIGHTS_QUERY,
            ),
            Lang::Toml => (
                tree_sitter_toml_ng::LANGUAGE.into(),
                tree_sitter_toml_ng::HIGHLIGHTS_QUERY,
            ),
            Lang::Xml => (
                tree_sitter_xml::LANGUAGE_XML.into(),
                tree_sitter_xml::XML_HIGHLIGHT_QUERY,
            ),
            Lang::Ini => (
                tree_sitter_ini::LANGUAGE.into(),
                tree_sitter_ini::HIGHLIGHTS_QUERY,
            ),
            Lang::Markdown => (
                tree_sitter_md::LANGUAGE.into(),
                tree_sitter_md::HIGHLIGHT_QUERY_BLOCK,
            ),
            Lang::Shell => (
                tree_sitter_bash::LANGUAGE.into(),
                tree_sitter_bash::HIGHLIGHT_QUERY,
            ),
            Lang::Plain => return None,
        };
        let mut parser = Parser::new();
        parser.set_language(&language).ok()?;
        let query = Query::new(&language, query_src).ok()?;
        let styles = query
            .capture_names()
            .iter()
            .map(|n| theme::syntax_style(n))
            .collect();
        let injected = if lang == Lang::Markdown {
            let inline: tree_sitter::Language = tree_sitter_md::INLINE_LANGUAGE.into();
            let mut p = Parser::new();
            p.set_language(&inline).ok()?;
            let q = Query::new(&inline, tree_sitter_md::HIGHLIGHT_QUERY_INLINE).ok()?;
            let st = q
                .capture_names()
                .iter()
                .map(|n| theme::syntax_style(n))
                .collect();
            Some(Injected {
                parser: p,
                query: q,
                tree: None,
                styles: st,
                outer_kind: "inline",
            })
        } else {
            None
        };
        Some(Syntax {
            parser,
            query,
            tree: None,
            styles,
            dirty: true,
            errors: Vec::new(),
            injected,
            report_errors: !matches!(lang, Lang::Markdown | Lang::Xml),
        })
    }

    /// Record an edit so the next parse can be incremental.
    pub fn edit(&mut self, edit: &InputEdit) {
        if let Some(t) = self.tree.as_mut() {
            t.edit(edit);
        }
        self.dirty = true;
    }

    pub fn invalidate(&mut self) {
        self.tree = None;
        self.dirty = true;
    }

    fn ensure_parsed(&mut self, lines: &[String]) {
        if !self.dirty {
            return;
        }
        let mut cb = |_byte: usize, pt: Point| chunk_at(lines, pt);
        self.tree = self.parser.parse_with(&mut cb, self.tree.as_ref());
        self.dirty = false;
        self.errors = if self.report_errors {
            self.tree
                .as_ref()
                .map(|t| collect_errors(t.root_node(), lines))
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        // inner grammar over the outer tree's designated ranges
        if let (Some(inj), Some(tree)) = (self.injected.as_mut(), self.tree.as_ref()) {
            let mut ranges: Vec<tree_sitter::Range> = Vec::new();
            collect_ranges(tree.root_node(), inj.outer_kind, &mut ranges);
            if ranges.is_empty() {
                inj.tree = None;
            } else if inj.parser.set_included_ranges(&ranges).is_ok() {
                let mut cb2 = |_byte: usize, pt: Point| chunk_at(lines, pt);
                inj.tree = inj.parser.parse_with(&mut cb2, None);
            }
        }
    }

    /// Parse errors in the whole buffer (cheap: only error-bearing subtrees are walked).
    /// Rows (0-based) that lie inside a string, raw string, heredoc, CDATA or
    /// similar node spanning more than one line, where trailing whitespace is content.
    pub fn protected_rows(&mut self, lines: &[String]) -> std::collections::BTreeSet<usize> {
        self.ensure_parsed(lines);
        let mut rows = std::collections::BTreeSet::new();
        fn walk(node: Node, rows: &mut std::collections::BTreeSet<usize>) {
            let kind = node.kind();
            let stringy = kind.contains("string")
                || kind.contains("heredoc")
                || kind.contains("cdata")
                || kind.contains("template")
                || kind == "char_literal";
            let s = node.start_position().row;
            let e = node.end_position().row;
            if stringy && e > s {
                rows.extend(s..=e);
                return;
            }
            // only descend where a multi-line node could hide a string
            if e > s {
                let mut c = node.walk();
                for child in node.children(&mut c) {
                    walk(child, rows);
                }
            }
        }
        if let Some(t) = self.tree.as_ref() {
            walk(t.root_node(), &mut rows);
        }
        rows
    }

    pub fn errors(&mut self, lines: &[String]) -> &[SyntaxError] {
        self.ensure_parsed(lines);
        &self.errors
    }

    /// Style runs for lines `first..first+count`, index 0 corresponding to `first`.
    pub fn runs(&mut self, lines: &[String], first: usize, count: usize) -> Vec<Vec<Run>> {
        let mut out: Vec<Vec<Run>> = vec![Vec::new(); count];
        if count == 0 || first >= lines.len() {
            return out;
        }
        self.ensure_parsed(lines);
        let Some(tree) = self.tree.as_ref() else {
            return out;
        };
        let last = (first + count).min(lines.len());

        // Byte range of the viewport.
        let mut start_byte = 0usize;
        for l in &lines[..first] {
            start_byte += l.len() + 1;
        }
        let mut end_byte = start_byte;
        for l in &lines[first..last] {
            end_byte += l.len() + 1;
        }

        // inner-grammar runs first so they take precedence where they overlap
        if let Some(inj) = self.injected.as_ref()
            && let Some(t) = inj.tree.as_ref()
        {
            collect_runs(
                &inj.query,
                &inj.styles,
                t.root_node(),
                lines,
                first,
                last,
                start_byte..end_byte,
                &mut out,
            );
        }
        collect_runs(
            &self.query,
            &self.styles,
            tree.root_node(),
            lines,
            first,
            last,
            start_byte..end_byte,
            &mut out,
        );
        out
    }
}

#[allow(clippy::too_many_arguments)]
fn collect_runs(
    query: &Query,
    styles: &[Style],
    root: Node,
    lines: &[String],
    first: usize,
    last: usize,
    bytes: std::ops::Range<usize>,
    out: &mut [Vec<Run>],
) {
    let mut cursor = QueryCursor::new();
    cursor.set_byte_range(bytes);
    let mut caps = cursor.captures(query, root, LinesText(lines));
    let mut seen: Option<(usize, usize)> = None;
    while let Some((m, idx)) = caps.next() {
        let cap = m.captures[*idx];
        let node = cap.node;
        let range = (node.start_byte(), node.end_byte());
        // tree-sitter-highlight semantics: the first capture for a node wins.
        if seen == Some(range) {
            continue;
        }
        seen = Some(range);
        let style = styles[cap.index as usize];
        let s = node.start_position();
        let e = node.end_position();
        let row_from = s.row.max(first);
        let row_to = e.row.min(last - 1);
        if row_from > row_to {
            continue;
        }
        for row in row_from..=row_to {
            let text = &lines[row];
            let bfrom = if row == s.row {
                s.column.min(text.len())
            } else {
                0
            };
            let bto = if row == e.row {
                e.column.min(text.len())
            } else {
                text.len()
            };
            if bfrom >= bto {
                continue;
            }
            let sc = text[..bfrom].chars().count();
            let ec = sc + text[bfrom..bto].chars().count();
            out[row - first].push(Run {
                start: sc,
                end: ec,
                style,
            });
        }
    }
}

fn collect_ranges(node: Node, kind: &str, out: &mut Vec<tree_sitter::Range>) {
    if node.kind() == kind {
        out.push(node.range());
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_ranges(child, kind, out);
    }
}

fn collect_errors(root: Node, lines: &[String]) -> Vec<SyntaxError> {
    fn walk(node: Node, lines: &[String], out: &mut Vec<SyntaxError>) {
        if !node.has_error() || out.len() >= 200 {
            return;
        }
        if node.is_error() || node.is_missing() {
            let s = node.start_position();
            let e = node.end_position();
            let message = if node.is_missing() {
                format!("missing `{}`", node.kind())
            } else {
                let text: String = lines
                    .get(s.row)
                    .map(|l| {
                        l.get(s.column.min(l.len())..)
                            .unwrap_or("")
                            .chars()
                            .take(16)
                            .collect()
                    })
                    .unwrap_or_default();
                let text = text.trim_end();
                if text.is_empty() {
                    "syntax error".to_string()
                } else {
                    format!("unexpected `{}`", text)
                }
            };
            out.push(SyntaxError {
                row: s.row,
                col: s.column,
                end_row: e.row,
                end_col: e.column,
                message,
            });
            if node.is_missing() {
                return;
            }
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            walk(child, lines, out);
        }
    }
    let mut out = Vec::new();
    walk(root, lines, &mut out);
    out
}
