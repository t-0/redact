//! Read-only rendering of document formats: Markdown as formatted text, and
//! JSON, TOML and XML as a structured tree.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::editor::highlight::Lang;
use crate::theme;

pub fn supported(lang: Lang) -> bool {
    matches!(lang, Lang::Markdown | Lang::Json | Lang::Xml | Lang::Toml)
}

pub fn render(text: &str, lang: Lang, width: usize) -> Vec<Line<'static>> {
    let width = width.max(20);
    match lang {
        Lang::Markdown => markdown(text, width),
        Lang::Json => tree_from_json(text, "JSON"),
        Lang::Toml => tree_from_json(text, "TOML"),
        Lang::Xml => xml(text),
        _ => Vec::new(),
    }
}

fn error_lines(msg: &str) -> Vec<Line<'static>> {
    vec![
        Line::from(Span::styled(
            " cannot preview: this file does not parse".to_string(),
            Style::default()
                .fg(theme::git_deleted())
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            format!(" {}", msg),
            Style::default().fg(theme::fg()),
        )),
    ]
}

// ---------- markdown ----------

/// A piece of inline text with its style, before line wrapping.
type Piece = (String, Style);

fn inline(src: &str) -> Vec<Piece> {
    let plain = Style::default().fg(theme::fg());
    let mut out: Vec<Piece> = Vec::new();
    let chars: Vec<char> = src.chars().collect();
    let mut buf = String::new();
    let mut i = 0;
    let push = |buf: &mut String, out: &mut Vec<Piece>| {
        if !buf.is_empty() {
            out.push((std::mem::take(buf), plain));
        }
    };
    while i < chars.len() {
        let c = chars[i];
        // `code`
        if c == '`'
            && let Some(end) = (i + 1..chars.len()).find(|&j| chars[j] == '`')
        {
            push(&mut buf, &mut out);
            let text: String = chars[i + 1..end].iter().collect();
            out.push((text, Style::default().fg(theme::syn_string())));
            i = end + 1;
            continue;
        }
        // **strong** / __strong__
        if (c == '*' || c == '_')
            && i + 1 < chars.len()
            && chars[i + 1] == c
            && let Some(end) =
                (i + 2..chars.len().saturating_sub(1)).find(|&j| chars[j] == c && chars[j + 1] == c)
        {
            push(&mut buf, &mut out);
            let text: String = chars[i + 2..end].iter().collect();
            out.push((
                text,
                Style::default()
                    .fg(theme::fg())
                    .add_modifier(Modifier::BOLD),
            ));
            i = end + 2;
            continue;
        }
        // ~~strike~~
        if c == '~'
            && i + 1 < chars.len()
            && chars[i + 1] == '~'
            && let Some(end) = (i + 2..chars.len().saturating_sub(1))
                .find(|&j| chars[j] == '~' && chars[j + 1] == '~')
        {
            push(&mut buf, &mut out);
            let text: String = chars[i + 2..end].iter().collect();
            out.push((
                text,
                Style::default()
                    .fg(theme::fg_dim())
                    .add_modifier(Modifier::CROSSED_OUT),
            ));
            i = end + 2;
            continue;
        }
        // *emphasis* / _emphasis_
        if (c == '*' || c == '_')
            && let Some(end) = (i + 1..chars.len()).find(|&j| chars[j] == c)
            && end > i + 1
        {
            push(&mut buf, &mut out);
            let text: String = chars[i + 1..end].iter().collect();
            out.push((
                text,
                Style::default()
                    .fg(theme::fg())
                    .add_modifier(Modifier::ITALIC),
            ));
            i = end + 1;
            continue;
        }
        // [text](url) and ![alt](url)
        if c == '[' || (c == '!' && i + 1 < chars.len() && chars[i + 1] == '[') {
            let open = if c == '!' { i + 1 } else { i };
            if let Some(close) = (open + 1..chars.len()).find(|&j| chars[j] == ']')
                && close + 1 < chars.len()
                && chars[close + 1] == '('
                && let Some(rparen) = (close + 2..chars.len()).find(|&j| chars[j] == ')')
            {
                push(&mut buf, &mut out);
                let label: String = chars[open + 1..close].iter().collect();
                let url: String = chars[close + 2..rparen].iter().collect();
                let label = if label.is_empty() { url.clone() } else { label };
                out.push((
                    label,
                    Style::default()
                        .fg(theme::syn_property())
                        .add_modifier(Modifier::UNDERLINED),
                ));
                out.push((format!(" ({})", url), Style::default().fg(theme::fg_dim())));
                i = rparen + 1;
                continue;
            }
        }
        buf.push(c);
        i += 1;
    }
    push(&mut buf, &mut out);
    out
}

/// Wrap styled pieces to `width`, indenting continuation lines by `hang`.
fn wrap(pieces: &[Piece], width: usize, indent: usize, hang: usize) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut spans: Vec<Span<'static>> = vec![Span::raw(" ".repeat(indent))];
    let mut col = indent;
    for (text, style) in pieces {
        // split into words, keeping single spaces between them
        for word in text.split_inclusive(' ') {
            let w = word.trim_end().chars().count();
            if col + w > width.saturating_sub(1) && col > indent {
                lines.push(Line::from(std::mem::take(&mut spans)));
                let ind = indent + hang;
                spans.push(Span::raw(" ".repeat(ind)));
                col = ind;
                let trimmed = word.trim_start();
                if trimmed.is_empty() {
                    continue;
                }
                col += trimmed.chars().count();
                spans.push(Span::styled(trimmed.to_string(), *style));
            } else {
                col += word.chars().count();
                spans.push(Span::styled(word.to_string(), *style));
            }
        }
    }
    if spans.iter().any(|s| !s.content.trim().is_empty()) {
        lines.push(Line::from(spans));
    }
    lines
}

fn markdown(text: &str, width: usize) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut para: Vec<Piece> = Vec::new();
    let mut in_code = false;

    let flush = |para: &mut Vec<Piece>, out: &mut Vec<Line<'static>>| {
        if !para.is_empty() {
            out.extend(wrap(para, width, 1, 0));
            para.clear();
        }
    };

    for raw in text.lines() {
        let line = raw.trim_end();
        let trimmed = line.trim_start();
        let indent_cols = line.chars().count() - trimmed.chars().count();

        // fenced code
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            flush(&mut para, &mut out);
            if in_code {
                in_code = false;
                out.push(Line::from(""));
            } else {
                in_code = true;
                let lang = trimmed.trim_start_matches(['`', '~']).trim();
                if !lang.is_empty() {
                    out.push(Line::from(Span::styled(
                        format!(" {} ", lang),
                        Style::default().fg(theme::fg_dim()).bg(theme::bg_line()),
                    )));
                }
            }
            continue;
        }
        if in_code {
            out.push(Line::from(vec![
                Span::styled("   ".to_string(), Style::default().bg(theme::bg_line())),
                Span::styled(
                    format!("{} ", line),
                    Style::default()
                        .fg(theme::syn_string())
                        .bg(theme::bg_line()),
                ),
            ]));
            continue;
        }

        // blank line ends a paragraph
        if trimmed.is_empty() {
            flush(&mut para, &mut out);
            if !out.last().map(|l| l.spans.is_empty()).unwrap_or(true) {
                out.push(Line::from(""));
            }
            continue;
        }

        // setext heading underline
        if !para.is_empty()
            && (trimmed.chars().all(|c| c == '=') || trimmed.chars().all(|c| c == '-'))
            && trimmed.len() >= 2
        {
            let level = if trimmed.starts_with('=') { 1 } else { 2 };
            let pieces: Vec<Piece> = para
                .drain(..)
                .map(|(t, _)| (t, heading_style(level)))
                .collect();
            out.extend(wrap(&pieces, width, 1, 0));
            if level == 1 {
                out.push(Line::from(Span::styled(
                    "─".repeat(width.saturating_sub(2)),
                    Style::default().fg(theme::fg_border()),
                )));
            }
            continue;
        }

        // ATX heading
        if let Some(rest) = trimmed.strip_prefix('#') {
            let hashes = 1 + rest.chars().take_while(|c| *c == '#').count();
            let body = trimmed[hashes..].trim();
            if hashes <= 6 && (body.is_empty() || trimmed.chars().nth(hashes) == Some(' ')) {
                flush(&mut para, &mut out);
                if !out.is_empty() && !out.last().map(|l| l.spans.is_empty()).unwrap_or(false) {
                    out.push(Line::from(""));
                }
                let pieces: Vec<Piece> = inline(body)
                    .into_iter()
                    .map(|(t, _)| (t, heading_style(hashes)))
                    .collect();
                out.extend(wrap(&pieces, width, 1, 0));
                if hashes == 1 {
                    out.push(Line::from(Span::styled(
                        "─".repeat(width.saturating_sub(2)),
                        Style::default().fg(theme::fg_border()),
                    )));
                }
                continue;
            }
        }

        // horizontal rule
        if trimmed.len() >= 3
            && (trimmed.chars().all(|c| c == '-')
                || trimmed.chars().all(|c| c == '*')
                || trimmed.chars().all(|c| c == '_'))
        {
            flush(&mut para, &mut out);
            out.push(Line::from(Span::styled(
                "┈".repeat(width.saturating_sub(2)),
                Style::default().fg(theme::fg_border()),
            )));
            continue;
        }

        // block quote
        if let Some(rest) = trimmed.strip_prefix('>') {
            flush(&mut para, &mut out);
            let mut pieces = vec![("│ ".to_string(), Style::default().fg(theme::fg_accent()))];
            pieces.extend(
                inline(rest.trim())
                    .into_iter()
                    .map(|(t, s)| (t, s.fg(theme::fg_dim()))),
            );
            out.extend(wrap(&pieces, width, 1, 2));
            continue;
        }

        // bullet / numbered list item
        let bullet =
            trimmed.starts_with("- ") || trimmed.starts_with("* ") || trimmed.starts_with("+ ");
        let numbered = trimmed
            .find(". ")
            .filter(|i| *i > 0 && *i <= 3 && trimmed[..*i].chars().all(|c| c.is_ascii_digit()))
            .map(|i| i + 2);
        if bullet || numbered.is_some() {
            flush(&mut para, &mut out);
            let indent = 1 + (indent_cols / 2) * 2;
            let (marker, body) = match numbered {
                Some(skip) => (
                    format!("{} ", &trimmed[..skip - 1]),
                    trimmed[skip..].trim_start(),
                ),
                None => ("•  ".to_string(), trimmed[2..].trim_start()),
            };
            let mut body = body.to_string();
            let mut marker = marker;
            let mut marker_style = Style::default().fg(theme::fg_accent());
            // task list
            if let Some(rest) = body.strip_prefix("[ ] ") {
                body = rest.to_string();
                marker = "☐  ".to_string();
                marker_style = Style::default().fg(theme::fg_dim());
            } else if let Some(rest) = body
                .strip_prefix("[x] ")
                .or_else(|| body.strip_prefix("[X] "))
            {
                body = rest.to_string();
                marker = "☑  ".to_string();
                marker_style = Style::default().fg(theme::git_added());
            }
            let marker_len = marker.chars().count();
            let mut pieces = vec![(marker, marker_style)];
            pieces.extend(inline(&body));
            out.extend(wrap(&pieces, width, indent, marker_len));
            continue;
        }

        // table row: keep the columns, drop the pipes
        if trimmed.starts_with('|') && trimmed.ends_with('|') && trimmed.len() > 2 {
            flush(&mut para, &mut out);
            let cells: Vec<&str> = trimmed
                .trim_matches('|')
                .split('|')
                .map(|c| c.trim())
                .collect();
            let separator = cells
                .iter()
                .all(|c| !c.is_empty() && c.chars().all(|ch| ch == '-' || ch == ':'));
            if separator {
                out.push(Line::from(Span::styled(
                    "─".repeat(width.saturating_sub(2)),
                    Style::default().fg(theme::fg_border()),
                )));
            } else {
                let w = (width.saturating_sub(2) / cells.len().max(1)).max(8);
                let mut spans = vec![Span::raw(" ".to_string())];
                for c in cells {
                    let text: String = c.chars().take(w - 1).collect();
                    spans.push(Span::styled(
                        format!("{:<w$}", text, w = w),
                        Style::default().fg(theme::fg()),
                    ));
                }
                out.push(Line::from(spans));
            }
            continue;
        }

        // ordinary paragraph text
        if !para.is_empty() {
            para.push((" ".to_string(), Style::default().fg(theme::fg())));
        }
        para.extend(inline(trimmed));
    }
    flush(&mut para, &mut out);
    out
}

fn heading_style(level: usize) -> Style {
    let color = match level {
        1 => theme::syn_keyword(),
        2 => theme::syn_function(),
        3 => theme::syn_type(),
        _ => theme::fg(),
    };
    Style::default().fg(color).add_modifier(Modifier::BOLD)
}

// ---------- json / toml tree ----------

fn tree_from_json(text: &str, label: &str) -> Vec<Line<'static>> {
    let parsed: Result<serde_json::Value, String> = if label == "TOML" {
        toml::from_str(text).map_err(|e| e.message().to_string())
    } else {
        serde_json::from_str(text).map_err(|e| e.to_string())
    };
    match parsed {
        Ok(v) => {
            let mut out = Vec::new();
            json_value(&v, 0, &mut out);
            if out.is_empty() {
                out.push(Line::from(Span::styled(
                    " (empty)".to_string(),
                    theme::dim(),
                )));
            }
            out
        }
        Err(e) => error_lines(&format!("{}: {}", label, e)),
    }
}

fn scalar_span(v: &serde_json::Value) -> Span<'static> {
    use serde_json::Value;
    match v {
        Value::Null => Span::styled("null".to_string(), Style::default().fg(theme::fg_dim())),
        Value::Bool(b) => Span::styled(
            b.to_string(),
            Style::default()
                .fg(theme::syn_number())
                .add_modifier(Modifier::BOLD),
        ),
        Value::Number(n) => Span::styled(n.to_string(), Style::default().fg(theme::syn_number())),
        Value::String(s) => Span::styled(
            s.replace('\n', "⏎"),
            Style::default().fg(theme::syn_string()),
        ),
        _ => Span::raw(String::new()),
    }
}

fn json_value(v: &serde_json::Value, depth: usize, out: &mut Vec<Line<'static>>) {
    use serde_json::Value;
    let pad = |d: usize| " ".repeat(1 + d * 2);
    match v {
        Value::Object(map) => {
            for (k, val) in map {
                match val {
                    Value::Object(_) | Value::Array(_) => {
                        out.push(Line::from(vec![
                            Span::raw(pad(depth)),
                            Span::styled(
                                k.clone(),
                                Style::default()
                                    .fg(theme::syn_type())
                                    .add_modifier(Modifier::BOLD),
                            ),
                            Span::styled(format!("  {}", shape(val)), theme::dim()),
                        ]));
                        json_value(val, depth + 1, out);
                    }
                    _ => out.push(Line::from(vec![
                        Span::raw(pad(depth)),
                        Span::styled(k.clone(), Style::default().fg(theme::syn_property())),
                        Span::styled(": ".to_string(), theme::dim()),
                        scalar_span(val),
                    ])),
                }
            }
        }
        Value::Array(items) => {
            for (i, val) in items.iter().enumerate() {
                match val {
                    Value::Object(_) | Value::Array(_) => {
                        out.push(Line::from(vec![
                            Span::raw(pad(depth)),
                            Span::styled(
                                format!("[{}]", i),
                                Style::default().fg(theme::fg_accent()),
                            ),
                            Span::styled(format!("  {}", shape(val)), theme::dim()),
                        ]));
                        json_value(val, depth + 1, out);
                    }
                    _ => out.push(Line::from(vec![
                        Span::raw(pad(depth)),
                        Span::styled("- ".to_string(), Style::default().fg(theme::fg_accent())),
                        scalar_span(val),
                    ])),
                }
            }
        }
        other => out.push(Line::from(vec![Span::raw(pad(depth)), scalar_span(other)])),
    }
}

fn shape(v: &serde_json::Value) -> String {
    use serde_json::Value;
    match v {
        Value::Object(m) => format!(
            "{{{} field{}}}",
            m.len(),
            if m.len() == 1 { "" } else { "s" }
        ),
        Value::Array(a) => format!("[{} item{}]", a.len(), if a.len() == 1 { "" } else { "s" }),
        _ => String::new(),
    }
}

// ---------- xml ----------

fn xml(text: &str) -> Vec<Line<'static>> {
    let doc = match roxmltree::Document::parse(text) {
        Ok(d) => d,
        Err(e) => return error_lines(&format!("XML: {}", e)),
    };
    let mut out = Vec::new();
    xml_node(doc.root_element(), 0, &mut out);
    out
}

fn xml_node(node: roxmltree::Node, depth: usize, out: &mut Vec<Line<'static>>) {
    let pad = " ".repeat(1 + depth * 2);
    let mut spans = vec![
        Span::raw(pad.clone()),
        Span::styled(
            node.tag_name().name().to_string(),
            Style::default()
                .fg(theme::syn_keyword())
                .add_modifier(Modifier::BOLD),
        ),
    ];
    for attr in node.attributes() {
        spans.push(Span::styled(
            format!(" {}", attr.name()),
            Style::default().fg(theme::syn_attribute()),
        ));
        spans.push(Span::styled("=".to_string(), theme::dim()));
        spans.push(Span::styled(
            attr.value().to_string(),
            Style::default().fg(theme::syn_string()),
        ));
    }
    // a single text child goes on the same line
    let children: Vec<roxmltree::Node> = node
        .children()
        .filter(|c| c.is_element() || (c.is_text() && !c.text().unwrap_or("").trim().is_empty()))
        .collect();
    if children.len() == 1 && children[0].is_text() {
        let t = children[0].text().unwrap_or("").trim().replace('\n', " ");
        spans.push(Span::styled(
            format!("  {}", t),
            Style::default().fg(theme::fg()),
        ));
        out.push(Line::from(spans));
        return;
    }
    out.push(Line::from(spans));
    for child in children {
        if child.is_element() {
            xml_node(child, depth + 1, out);
        } else if let Some(t) = child.text() {
            let t = t.trim();
            if !t.is_empty() {
                out.push(Line::from(vec![
                    Span::raw(" ".repeat(1 + (depth + 1) * 2)),
                    Span::styled(t.replace('\n', " "), Style::default().fg(theme::fg())),
                ]));
            }
        }
    }
}
