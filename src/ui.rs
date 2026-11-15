//! Rendering of panes, bars, editor, diff, terminal and overlays.

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use unicode_width::UnicodeWidthChar;

use crate::app::{App, Focus, LeftMode, OutputTab, ProblemRow, TabKind};
use crate::debug::{Debugger, Row as DebugRow, State as DebugState};
use crate::diffview::{DiffTab, RowKind};
use crate::editor::{Editor, TAB_WIDTH};
use crate::tests::{TestStatus, TestsPane};
use crate::theme;

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    f.render_widget(Block::default().style(theme::base()), area);

    let vert = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(1)])
        .split(area);
    let body = vert[0];
    let status = vert[1];

    let (left, right) = if app.left_visible {
        let h = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(app.left_width.min(body.width / 2)),
                Constraint::Min(20),
            ])
            .split(body);
        (Some(h[0]), h[1])
    } else {
        (None, body)
    };

    if let Some(left) = left {
        draw_left(f, app, left);
    }
    draw_right(f, app, right);

    app.cursor_diag = {
        let cur = match app.active_tab().map(|t| &t.kind) {
            Some(TabKind::Editor(e)) if app.focus == Focus::Right => Some(e.primary().pos.line),
            _ => None,
        };
        cur.and_then(|line| {
            app.active_diagnostics()
                .into_iter()
                .filter(|d| d.line == line)
                .max_by_key(|d| d.severity)
        })
    };
    app.cursor_annotation = {
        let cur = match app.active_tab().map(|t| &t.kind) {
            Some(TabKind::Editor(e)) if app.focus == Focus::Right => Some(e.primary().pos.line),
            _ => None,
        };
        cur.and_then(|line| {
            app.active_annotations()
                .into_iter()
                .find(|a| a.0 == line)
                .map(|a| (a.1, a.2))
        })
    };
    if app.prompt.is_some() {
        draw_prompt(f, app, status);
    } else {
        draw_status(f, app, status);
    }
    if app.show_help {
        draw_help(f, app, area);
    }
    if app.palette.is_some() {
        draw_palette(f, app, area);
    }
}

// ---------- left pane ----------

fn draw_left(f: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Left;
    let modes = [
        LeftMode::Explorer,
        LeftMode::Scm,
        LeftMode::Tests,
        LeftMode::Debug,
        LeftMode::Problems,
    ];
    let mut title: Vec<Span> = vec![Span::raw(" ")];
    for (i, m) in modes.iter().enumerate() {
        let label = format!("{} {}", i + 1, m.title());
        if *m == app.left_mode {
            title.push(Span::styled(label, theme::accent()));
        } else {
            title.push(Span::styled(format!("{}", i + 1), theme::dim()));
        }
        title.push(Span::raw(" "));
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::border(focused))
        .title(Line::from(title))
        .style(theme::panel());
    let inner = block.inner(area);
    f.render_widget(block, area);
    app.left_rows = inner.height as usize;
    match app.left_mode {
        LeftMode::Explorer => draw_explorer(f, app, inner, focused),
        LeftMode::Scm => draw_scm(f, app, inner, focused),
        LeftMode::Tests => draw_tests(f, &mut app.tests, inner, focused),
        LeftMode::Debug => draw_debug(f, &mut app.debugger, inner, focused),
        LeftMode::Problems => draw_problems(f, app, inner, focused),
    }
}

fn clamp_scroll(scroll: &mut usize, selected: usize, rows: usize) {
    if rows == 0 {
        return;
    }
    if selected < *scroll {
        *scroll = selected;
    } else if selected >= *scroll + rows {
        *scroll = selected + 1 - rows;
    }
}

fn draw_explorer(f: &mut Frame, app: &mut App, area: Rect, focused: bool) {
    let rows = area.height as usize;
    clamp_scroll(&mut app.explorer.scroll, app.explorer.selected, rows);
    let scroll = app.explorer.scroll;
    let selected = app.explorer.selected;
    let mut lines: Vec<Line> = Vec::new();
    for (i, row) in app
        .explorer
        .rows()
        .iter()
        .enumerate()
        .skip(scroll)
        .take(rows)
    {
        let mut spans: Vec<Span> = Vec::new();
        let color = row.status.color();
        spans.push(Span::styled(
            format!("{} ", row.status.letter()),
            Style::default().fg(color),
        ));
        for g in &row.guides {
            spans.push(Span::styled(
                if *g { "│ " } else { "  " },
                Style::default().fg(theme::fg_border()),
            ));
        }
        if row.depth > 0 {
            spans.push(Span::styled(
                if row.last { "└ " } else { "├ " },
                Style::default().fg(theme::fg_border()),
            ));
        }
        let icon = if row.is_dir {
            if row.expanded { "▾ " } else { "▸ " }
        } else {
            "  "
        };
        spans.push(Span::styled(icon, Style::default().fg(theme::fg_dim())));
        let mut name_style = Style::default().fg(color);
        if row.is_dir {
            name_style = name_style.add_modifier(Modifier::BOLD);
        }
        if row.phantom {
            name_style = name_style.add_modifier(Modifier::CROSSED_OUT);
        }
        if row.ignored {
            name_style = Style::default().fg(theme::fg_dim());
        }
        let name = if row.is_dir {
            format!("{}/", row.name)
        } else {
            row.name.clone()
        };
        spans.push(Span::styled(name, name_style));
        let mut line = Line::from(spans);
        if i == selected {
            line = line.style(Style::default().bg(if focused {
                theme::bg_selection()
            } else {
                theme::bg_line()
            }));
        }
        lines.push(line);
    }
    f.render_widget(Paragraph::new(lines), area);
}

fn draw_scm(f: &mut Frame, app: &mut App, area: Rect, focused: bool) {
    let rows = area.height as usize;
    clamp_scroll(&mut app.scm.scroll, app.scm.selected, rows);
    let mut lines: Vec<Line> = Vec::new();
    if app.scm.items.is_empty() {
        lines.push(Line::from(Span::styled(
            " working tree clean",
            theme::dim(),
        )));
    }
    let wd = app.git.workdir.clone().unwrap_or_else(|| app.root.clone());
    for (i, (path, st)) in app
        .scm
        .items
        .iter()
        .enumerate()
        .skip(app.scm.scroll)
        .take(rows)
    {
        let rel = path
            .strip_prefix(&wd)
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned();
        let mut line = Line::from(vec![
            Span::styled(
                format!(" {} ", st.letter()),
                Style::default().fg(st.color()).add_modifier(Modifier::BOLD),
            ),
            Span::styled(rel, Style::default().fg(st.color())),
        ]);
        if i == app.scm.selected {
            line = line.style(Style::default().bg(if focused {
                theme::bg_selection()
            } else {
                theme::bg_line()
            }));
        }
        lines.push(line);
    }
    f.render_widget(Paragraph::new(lines), area);
}

fn draw_tests(f: &mut Frame, tests: &mut TestsPane, area: Rect, focused: bool) {
    let rows = area.height as usize;
    clamp_scroll(&mut tests.scroll, tests.selected, rows);
    let mut lines: Vec<Line> = Vec::new();
    if tests.nodes.is_empty() {
        let msg = if tests.discovering() {
            " discovering tests…"
        } else {
            " no tests found (r to rescan)"
        };
        lines.push(Line::from(Span::styled(msg, theme::dim())));
    }
    let visible: Vec<usize> = tests.visible().to_vec();
    for (i, &ni) in visible.iter().enumerate().skip(tests.scroll).take(rows) {
        let n = &tests.nodes[ni];
        let (mark, color) = match n.status {
            TestStatus::Unknown => ("·", theme::fg_dim()),
            TestStatus::Running => ("…", theme::git_modified()),
            TestStatus::Passed => ("✓", theme::git_added()),
            TestStatus::Failed => ("✗", theme::git_deleted()),
        };
        let mut spans = vec![Span::styled(
            format!(" {} ", mark),
            Style::default().fg(color),
        )];
        spans.push(Span::raw("  ".repeat(n.depth)));
        if n.is_group {
            spans.push(Span::styled(
                if n.expanded { "▾ " } else { "▸ " },
                theme::dim(),
            ));
            spans.push(Span::styled(
                n.name.clone(),
                Style::default()
                    .fg(theme::syn_type())
                    .add_modifier(Modifier::BOLD),
            ));
        } else {
            spans.push(Span::styled(
                n.name.clone(),
                Style::default().fg(theme::fg()),
            ));
        }
        let mut line = Line::from(spans);
        if i == tests.selected {
            line = line.style(Style::default().bg(if focused {
                theme::bg_selection()
            } else {
                theme::bg_line()
            }));
        }
        lines.push(line);
    }
    f.render_widget(Paragraph::new(lines), area);
}

fn draw_problems(f: &mut Frame, app: &mut App, area: Rect, focused: bool) {
    let rows = area.height as usize;
    clamp_scroll(&mut app.problems.scroll, app.problems.selected, rows);
    let root = app.root.clone();
    let mut lines: Vec<Line> = Vec::new();
    if app.problems.rows.is_empty() {
        let msg = if app.diags.cargo_running {
            " checking…"
        } else {
            " no problems  (r: cargo check)"
        };
        lines.push(Line::from(Span::styled(msg, theme::dim())));
    }
    for (i, row) in app
        .problems
        .rows
        .iter()
        .enumerate()
        .skip(app.problems.scroll)
        .take(rows)
    {
        let line = match row {
            ProblemRow::File(p, n) => {
                let rel = p.strip_prefix(&root).unwrap_or(p).display().to_string();
                Line::from(vec![
                    Span::styled(
                        format!(" {}", rel),
                        Style::default()
                            .fg(theme::syn_type())
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(format!("  {}", n), theme::dim()),
                ])
            }
            ProblemRow::Item(_, d) => {
                let mark = match d.severity {
                    crate::diagnostics::Severity::Error => "✗",
                    crate::diagnostics::Severity::Warning => "▲",
                };
                Line::from(vec![
                    Span::styled(
                        format!("  {} ", mark),
                        Style::default().fg(d.severity.color()),
                    ),
                    Span::styled(format!("{}:{} ", d.line + 1, d.col + 1), theme::dim()),
                    Span::styled(
                        d.message.replace('\n', " "),
                        Style::default().fg(theme::fg()),
                    ),
                ])
            }
        };
        let line = if i == app.problems.selected {
            line.style(Style::default().bg(if focused {
                theme::bg_selection()
            } else {
                theme::bg_line()
            }))
        } else {
            line
        };
        lines.push(line);
    }
    f.render_widget(Paragraph::new(lines), area);
}

fn draw_debug(f: &mut Frame, dbg: &mut Debugger, area: Rect, focused: bool) {
    let rows = area.height as usize;
    clamp_scroll(&mut dbg.scroll, dbg.selected, rows);
    let mut lines: Vec<Line> = Vec::new();
    let sess = dbg.session.as_ref();
    for (i, row) in dbg.rows().iter().enumerate().skip(dbg.scroll).take(rows) {
        let line = match row {
            DebugRow::Header(h) => Line::from(Span::styled(format!(" {}", h), theme::accent())),
            DebugRow::Text(t) => Line::from(Span::styled(format!("   {}", t), theme::dim())),
            DebugRow::Target(ti) => {
                let t = &dbg.targets[*ti];
                let mark = if dbg.last_target.as_ref() == Some(&t.kind) {
                    "▸"
                } else {
                    " "
                };
                Line::from(vec![
                    Span::styled(format!(" {} ", mark), theme::dim()),
                    Span::styled(t.label.clone(), Style::default().fg(theme::fg())),
                ])
            }
            DebugRow::Frame(fi) => {
                let Some(s) = sess else { continue };
                let fr = &s.frames[*fi];
                let cur = if *fi == s.frame_idx { "▶" } else { " " };
                let loc = fr
                    .file
                    .as_ref()
                    .map(|p| {
                        format!(
                            "{}:{}",
                            p.file_name()
                                .map(|n| n.to_string_lossy().into_owned())
                                .unwrap_or_default(),
                            fr.line
                        )
                    })
                    .unwrap_or_default();
                let has_src = fr.file.as_ref().map(|p| p.exists()).unwrap_or(false);
                Line::from(vec![
                    Span::styled(
                        format!(" {} ", cur),
                        Style::default().fg(theme::git_modified()),
                    ),
                    Span::styled(
                        fr.name.clone(),
                        Style::default().fg(if has_src {
                            theme::syn_function()
                        } else {
                            theme::fg_dim()
                        }),
                    ),
                    Span::styled(format!("  {}", loc), theme::dim()),
                ])
            }
            DebugRow::Var(vi) => {
                let Some(s) = sess else { continue };
                let v = &s.vars[*vi];
                let arrow = if v.vref > 0 {
                    if v.expanded { "▾ " } else { "▸ " }
                } else {
                    "  "
                };
                let mut spans = vec![
                    Span::raw(format!("  {}", "  ".repeat(v.depth))),
                    Span::styled(arrow, theme::dim()),
                ];
                if v.depth == 0 {
                    spans.push(Span::styled(
                        v.name.clone(),
                        Style::default()
                            .fg(theme::syn_type())
                            .add_modifier(Modifier::BOLD),
                    ));
                } else {
                    spans.push(Span::styled(
                        v.name.clone(),
                        Style::default().fg(theme::syn_property()),
                    ));
                    if !v.value.is_empty() {
                        spans.push(Span::styled(" = ", theme::dim()));
                        spans.push(Span::styled(
                            v.value.chars().take(80).collect::<String>(),
                            Style::default().fg(theme::fg()),
                        ));
                    }
                }
                Line::from(spans)
            }
            DebugRow::MoreFrames(n) => Line::from(Span::styled(
                if dbg.all_frames {
                    "   ▴ fewer frames".to_string()
                } else {
                    format!("   … {} more frames", n)
                },
                theme::dim(),
            )),
            DebugRow::Breakpoint(p, l) => Line::from(vec![
                Span::styled(" ● ", Style::default().fg(theme::git_deleted())),
                Span::styled(
                    format!(
                        "{}:{}",
                        p.file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default(),
                        l
                    ),
                    Style::default().fg(theme::fg()),
                ),
            ]),
        };
        let line = if i == dbg.selected {
            line.style(Style::default().bg(if focused {
                theme::bg_selection()
            } else {
                theme::bg_line()
            }))
        } else {
            line
        };
        lines.push(line);
    }
    if let Some(s) = sess
        && s.state == DebugState::Running
        && lines.len() < rows
    {
        lines.push(Line::from(Span::styled(
            "   running…  p pauses, x stops",
            theme::dim(),
        )));
    }
    f.render_widget(Paragraph::new(lines), area);
}

// ---------- right pane ----------

fn draw_right(f: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Right;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::border(focused))
        .style(theme::base());
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height < 2 {
        return;
    }
    let bar = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: 1,
    };
    let content = Rect {
        x: inner.x,
        y: inner.y + 1,
        width: inner.width,
        height: inner.height - 1,
    };
    app.right_content = content;

    draw_tab_bar(f, app, bar);

    let diags = app.active_diagnostics();
    let annotations = app.active_annotations();
    let active = app.active;
    let Some(tab) = app.tabs.get_mut(active) else {
        let msg = vec![
            Line::from(""),
            Line::from(Span::styled("  redact", theme::accent())),
            Line::from(Span::styled("  cut what you don't need", theme::dim())),
            Line::from(""),
            Line::from(Span::styled("  F1 for keys", theme::dim())),
        ];
        f.render_widget(Paragraph::new(msg), content);
        return;
    };
    if tab.preview && matches!(tab.kind, TabKind::Editor(_)) {
        draw_preview(f, tab, content);
        return;
    }
    match &mut tab.kind {
        TabKind::Editor(ed) => {
            let decor = EditorDecor {
                diags,
                annotations,
                show_whitespace: app.show_whitespace,
                breakpoints: ed
                    .path
                    .as_ref()
                    .and_then(|p| app.debugger.breakpoints.get(p))
                    .cloned()
                    .unwrap_or_default(),
                current: match (&app.debugger.current, &ed.path) {
                    (Some((cp, l)), Some(p)) if cp == p => Some(*l),
                    _ => None,
                },
            };
            draw_editor(f, ed, content, focused, &decor)
        }
        TabKind::Terminal(t) => {
            t.resize(content.height, content.width);
            let cursor = t.render(content, f.buffer_mut());
            if let (Some((x, y)), true) = (cursor, focused) {
                f.set_cursor_position((x, y));
            }
        }
        TabKind::Diff(d) => draw_diff(f, d, content),
        TabKind::Output(o) => draw_output(f, o, content),
    }
}

fn draw_tab_bar(f: &mut Frame, app: &App, area: Rect) {
    let mut spans: Vec<Span> = Vec::new();
    for (i, t) in app.tabs.iter().enumerate() {
        let mut label = String::new();
        if t.pinned {
            label.push_str("⚲ ");
        }
        label.push_str(&t.title());
        if t.modified() {
            label.push_str(" ●");
        }
        if matches!(&t.kind, TabKind::Editor(e) if e.conflict) {
            label.push_str(" ⚠");
        }
        if t.preview {
            label.push_str(" ◫");
        }
        let style = if i == app.active {
            Style::default()
                .fg(theme::fg())
                .bg(theme::bg_selection())
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme::fg_dim()).bg(theme::bg_bar())
        };
        spans.push(Span::styled(format!(" {} ", label), style));
        spans.push(Span::styled(" ", theme::bar()));
    }
    // right-aligned detail: full path of the active tab
    let detail = match app.active_tab().map(|t| &t.kind) {
        Some(TabKind::Editor(e)) => e
            .path
            .as_ref()
            .map(|p| {
                p.strip_prefix(&app.root)
                    .unwrap_or(p)
                    .to_string_lossy()
                    .into_owned()
            })
            .unwrap_or_default(),
        Some(TabKind::Diff(d)) => format!("+{} -{}  HEAD ⇔ working tree", d.stats.0, d.stats.1),
        Some(TabKind::Terminal(_)) => "F9 pin  ctrl+w close".into(),
        _ => String::new(),
    };
    f.render_widget(Paragraph::new(Line::from(spans)).style(theme::bar()), area);
    if !detail.is_empty() {
        let w = detail.chars().count() as u16 + 1;
        if w < area.width / 2 {
            let r = Rect {
                x: area.x + area.width - w,
                y: area.y,
                width: w,
                height: 1,
            };
            f.render_widget(
                Paragraph::new(Span::styled(detail, theme::dim())).style(theme::bar()),
                r,
            );
        }
    }
}

// ---------- editor ----------

/// Debugger decorations for one buffer: breakpoint lines and the stopped line (1-based).
pub struct EditorDecor {
    pub breakpoints: std::collections::BTreeSet<usize>,
    pub current: Option<usize>,
    pub diags: Vec<crate::diagnostics::Diagnostic>,
    /// End-of-line notes (0-based line, text, colour), e.g. dependency health.
    pub annotations: Vec<(usize, String, ratatui::style::Color)>,
    pub show_whitespace: bool,
}

fn draw_editor(f: &mut Frame, ed: &mut Editor, area: Rect, focused: bool, decor: &EditorDecor) {
    let gutter_w = format!("{}", ed.lines.len()).len().max(3) as u16 + 2;
    let text_w = area.width.saturating_sub(gutter_w) as usize;
    let rows = area.height as usize;
    ed.scroll_to_cursor(rows, text_w.max(1));
    let first = ed.scroll_line;
    let count = rows.min(ed.lines.len().saturating_sub(first));
    let runs = ed.runs(first, count);
    let primary = *ed.primary();
    let buf = f.buffer_mut();

    // selection ranges and cursor positions
    let selections: Vec<(crate::editor::Pos, crate::editor::Pos)> =
        ed.cursors.iter().filter_map(|c| c.selection()).collect();
    let n_cursors = ed.cursors.len();

    for (k, line_idx) in (first..first + count).enumerate() {
        let y = area.y + k as u16;
        let is_cur_line = line_idx == primary.pos.line;
        let is_debug_line = decor.current == Some(line_idx + 1);
        let line_bg = if is_debug_line {
            theme::bg_diff_change()
        } else if is_cur_line && n_cursors == 1 {
            theme::bg_line()
        } else {
            theme::bg()
        };
        // gutter
        let has_bp = decor.breakpoints.contains(&(line_idx + 1));
        let num = format!(
            "{:>w$}{}",
            line_idx + 1,
            if is_debug_line {
                "▶"
            } else if has_bp {
                "●"
            } else {
                " "
            },
            w = (gutter_w - 2) as usize
        );
        let line_diags: Vec<&crate::diagnostics::Diagnostic> =
            decor.diags.iter().filter(|d| d.line == line_idx).collect();
        let worst = line_diags.iter().map(|d| d.severity).max();
        let gstyle = Style::default()
            .fg(match worst {
                Some(sev) => sev.color(),
                None if is_cur_line => theme::fg_dim(),
                None => theme::fg_gutter(),
            })
            .bg(line_bg);
        buf.set_string(area.x, y, format!(" {}", num), gstyle);
        if is_debug_line || has_bp {
            let mx = area.x + gutter_w - 1;
            buf[(mx, y)].set_style(
                Style::default()
                    .fg(if is_debug_line {
                        theme::git_modified()
                    } else {
                        theme::git_deleted()
                    })
                    .bg(line_bg),
            );
        }
        for x in area.x + gutter_w..area.x + area.width {
            buf[(x, y)]
                .set_symbol(" ")
                .set_style(Style::default().bg(line_bg));
        }

        // expand the line into display cells
        let text = &ed.lines[line_idx];
        let line_runs = &runs[k];
        let mut x_disp = 0usize;
        for (col, ch) in text.chars().enumerate() {
            let width = if ch == '\t' {
                TAB_WIDTH - (x_disp % TAB_WIDTH)
            } else {
                ch.width().unwrap_or(1).max(1)
            };
            let mut style = Style::default().fg(theme::fg()).bg(line_bg);
            if let Some(r) = line_runs.iter().find(|r| col >= r.start && col < r.end) {
                style = style.patch(r.style);
            }
            let p = crate::editor::Pos::new(line_idx, col);
            if let Some(d) = line_diags
                .iter()
                .filter(|d| col >= d.col && col < d.end_col)
                .max_by_key(|d| d.severity)
            {
                style = style
                    .fg(d.severity.color())
                    .add_modifier(Modifier::UNDERLINED);
            }
            if selections.iter().any(|(s, e)| p >= *s && p < *e) {
                style = style.bg(theme::bg_selection());
            }
            let cursor_here = ed.cursors.iter().position(|c| c.pos == p);
            if let Some(ci) = cursor_here {
                if focused {
                    style = if ci == n_cursors - 1 {
                        style.add_modifier(Modifier::REVERSED)
                    } else {
                        style.bg(theme::bg_cursor_secondary())
                    };
                } else {
                    style = style.add_modifier(Modifier::UNDERLINED);
                }
            }
            for w in 0..width {
                let xd = x_disp + w;
                if xd < ed.scroll_col {
                    continue;
                }
                let xx = xd - ed.scroll_col;
                if xx >= text_w {
                    break;
                }
                let cell = &mut buf[(area.x + gutter_w + xx as u16, y)];
                let mut cell_style = style;
                if ch == '\t' {
                    if decor.show_whitespace && w == 0 {
                        cell.set_symbol("→");
                        cell_style = whitespace_mark(style);
                    } else {
                        cell.set_symbol(" ");
                    }
                } else if ch == ' ' && decor.show_whitespace {
                    cell.set_symbol("·");
                    cell_style = whitespace_mark(style);
                } else if w == 0 {
                    let mut s = String::new();
                    s.push(ch);
                    cell.set_symbol(&s);
                } else {
                    cell.set_symbol("");
                }
                cell.set_style(cell_style);
            }
            x_disp += width;
        }
        // dependency health and other end-of-line notes, when no diagnostic claims the line
        if line_diags.is_empty()
            && let Some((_, note, color)) = decor.annotations.iter().find(|a| a.0 == line_idx)
        {
            let line_w = ed.display_col(line_idx, text.chars().count());
            let start = line_w.saturating_sub(ed.scroll_col) + 3;
            if start + 4 < text_w {
                let room = text_w - start;
                let msg: String = note.chars().take(room).collect();
                buf.set_string(
                    area.x + gutter_w + start as u16,
                    y,
                    msg,
                    Style::default()
                        .fg(*color)
                        .bg(line_bg)
                        .add_modifier(Modifier::ITALIC),
                );
            }
        }
        // inline message after the line (error-lens style)
        if let Some(d) = line_diags
            .iter()
            .max_by_key(|d| (d.severity, std::cmp::Reverse(d.col)))
        {
            let line_w = ed.display_col(line_idx, text.chars().count());
            let start = line_w.saturating_sub(ed.scroll_col) + 3;
            if start + 8 < text_w {
                let room = text_w - start;
                let msg: String = format!("■ {}", d.message.replace('\n', " "))
                    .chars()
                    .take(room)
                    .collect();
                buf.set_string(
                    area.x + gutter_w + start as u16,
                    y,
                    msg,
                    Style::default()
                        .fg(d.severity.color())
                        .bg(line_bg)
                        .add_modifier(Modifier::ITALIC),
                );
            }
        }
        // cursors sitting at end of line
        for (ci, c) in ed.cursors.iter().enumerate() {
            if c.pos.line == line_idx && c.pos.col >= text.chars().count() {
                let xd = ed.display_col(line_idx, c.pos.col);
                if xd >= ed.scroll_col && xd - ed.scroll_col < text_w {
                    let cell = &mut buf[(area.x + gutter_w + (xd - ed.scroll_col) as u16, y)];
                    let base = Style::default().fg(theme::fg()).bg(line_bg);
                    let style = if !focused {
                        base.add_modifier(Modifier::UNDERLINED)
                    } else if ci == n_cursors - 1 {
                        base.add_modifier(Modifier::REVERSED)
                    } else {
                        base.bg(theme::bg_cursor_secondary())
                    };
                    cell.set_symbol(" ").set_style(style);
                }
            }
        }
        // selection extending past end of line
        if selections
            .iter()
            .any(|(s, e)| line_idx >= s.line && line_idx < e.line)
        {
            let xd = ed.display_col(line_idx, text.chars().count());
            if xd >= ed.scroll_col && xd - ed.scroll_col < text_w {
                buf[(area.x + gutter_w + (xd - ed.scroll_col) as u16, y)]
                    .set_style(Style::default().bg(theme::bg_selection()));
            }
        }
    }
    // blank rows below the file end
    for k in count..rows {
        let y = area.y + k as u16;
        buf.set_string(
            area.x,
            y,
            format!("{:>w$}", "~", w = gutter_w as usize - 1),
            Style::default().fg(theme::fg_gutter()),
        );
    }
}

/// Style for a rendered space or tab: the faint mark colour, keeping any
/// background or cursor emphasis the cell already carries.
fn whitespace_mark(style: Style) -> Style {
    style.fg(theme::fg_whitespace())
}

// ---------- preview ----------

fn draw_preview(f: &mut Frame, tab: &mut crate::app::Tab, area: Rect) {
    let width = area.width.saturating_sub(1) as usize;
    let (revision, text, lang) = match &tab.kind {
        TabKind::Editor(ed) => (ed.revision, ed.text(), ed.lang),
        _ => return,
    };
    let stale = tab
        .preview_cache
        .as_ref()
        .map(|c| c.0 != revision || c.1 != width)
        .unwrap_or(true);
    if stale {
        let lines = crate::preview::render(&text, lang, width);
        tab.preview_cache = Some((revision, width, lines));
    }
    let lines = &tab.preview_cache.as_ref().unwrap().2;
    let rows = area.height as usize;
    tab.preview_scroll = tab.preview_scroll.min(lines.len().saturating_sub(rows));
    let visible: Vec<Line> = lines
        .iter()
        .skip(tab.preview_scroll)
        .take(rows)
        .cloned()
        .collect();
    f.render_widget(Paragraph::new(visible).style(theme::base()), area);
    // scroll position hint
    if lines.len() > rows {
        let pct = (tab.preview_scroll * 100) / lines.len().saturating_sub(rows).max(1);
        let hint = format!(" {}% ", pct);
        let w = hint.len() as u16;
        if w < area.width {
            f.render_widget(
                Paragraph::new(Span::styled(hint, theme::dim())),
                Rect {
                    x: area.x + area.width - w,
                    y: area.y + area.height - 1,
                    width: w,
                    height: 1,
                },
            );
        }
    }
}

// ---------- diff ----------

fn draw_diff(f: &mut Frame, d: &mut DiffTab, area: Rect) {
    let rows = area.height as usize;
    d.scroll_to_selected(rows);
    let half = area.width / 2;
    let buf = f.buffer_mut();
    let num_w = 5usize;
    for k in 0..rows {
        let y = area.y + k as u16;
        let Some(row) = d.rows.get(d.scroll + k) else {
            break;
        };
        let (lbg, rbg) = match row.kind {
            RowKind::Equal => (theme::bg(), theme::bg()),
            RowKind::Insert => (theme::bg(), theme::bg_diff_add()),
            RowKind::Delete => (theme::bg_diff_del(), theme::bg()),
            RowKind::Replace => (theme::bg_diff_change(), theme::bg_diff_change()),
        };
        let selected = d.scroll + k == d.selected;
        let draw_side = |buf: &mut Buffer,
                         x0: u16,
                         w: u16,
                         no: Option<usize>,
                         text: &str,
                         bg: ratatui::style::Color| {
            let mut style = Style::default().fg(theme::fg()).bg(bg);
            if selected {
                style = style.add_modifier(Modifier::BOLD);
            }
            for x in x0..x0 + w {
                buf[(x, y)].set_symbol(" ").set_style(style);
            }
            let num = no
                .map(|n| format!("{:>w$} ", n, w = num_w - 1))
                .unwrap_or_else(|| " ".repeat(num_w));
            buf.set_string(
                x0,
                y,
                num,
                Style::default()
                    .fg(if selected {
                        theme::fg()
                    } else {
                        theme::fg_gutter()
                    })
                    .bg(bg),
            );
            let visible: String = text
                .chars()
                .skip(d.hscroll)
                .take(w.saturating_sub(num_w as u16) as usize)
                .collect();
            let visible = visible.replace('\t', "    ");
            buf.set_string(x0 + num_w as u16, y, visible, style);
        };
        draw_side(buf, area.x, half, row.left_no, &row.left, lbg);
        buf[(area.x + half, y)]
            .set_symbol("│")
            .set_style(Style::default().fg(if selected {
                theme::fg_accent()
            } else {
                theme::fg_border()
            }));
        draw_side(
            buf,
            area.x + half + 1,
            area.width - half - 1,
            row.right_no,
            &row.right,
            rbg,
        );
    }
    if d.rows.is_empty() {
        buf.set_string(area.x + 1, area.y, "no differences", theme::dim());
    }
}

// ---------- output ----------

fn draw_output(f: &mut Frame, o: &mut OutputTab, area: Rect) {
    let rows = area.height as usize;
    if o.follow {
        o.scroll = o.lines.len().saturating_sub(rows);
    }
    o.scroll = o.scroll.min(o.lines.len().saturating_sub(1));
    let lines: Vec<Line> = o
        .lines
        .iter()
        .skip(o.scroll)
        .take(rows)
        .map(|l| {
            let style = if l.contains("FAILED") || l.contains("panicked") || l.starts_with("error")
            {
                Style::default().fg(theme::git_deleted())
            } else if l.ends_with(" ok") || l.contains("PASSED") || l.contains("test result: ok") {
                Style::default().fg(theme::git_added())
            } else if l.starts_with('$') {
                theme::accent()
            } else {
                Style::default().fg(theme::fg())
            };
            Line::from(Span::styled(l.replace('\t', "    "), style))
        })
        .collect();
    f.render_widget(Paragraph::new(lines), area);
}

// ---------- bars ----------

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    let branch = app
        .git
        .branch
        .clone()
        .unwrap_or_else(|| "<no branch>".to_string());
    let root = app
        .root
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| app.root.to_string_lossy().into_owned());
    let time = chrono::Local::now().format("%H:%M").to_string();
    let mut left: Vec<Span> = vec![
        Span::styled(
            format!(" {} ", app.activity()),
            theme::accent().bg(theme::bg_bar()),
        ),
        Span::styled(
            format!(" {} ", root),
            Style::default()
                .fg(theme::fg())
                .bg(theme::bg_bar())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" {} ", branch),
            Style::default().fg(theme::syn_string()).bg(theme::bg_bar()),
        ),
    ];
    if !app.pending.is_empty() {
        let seq = app
            .pending
            .iter()
            .map(|k| k.label())
            .collect::<Vec<_>>()
            .join(" ");
        left.push(Span::styled(
            format!("  {} … ", seq),
            Style::default()
                .fg(theme::syn_function())
                .bg(theme::bg_bar()),
        ));
    }
    if let Some((msg, _)) = &app.message {
        left.push(Span::styled(
            format!("  {} ", msg),
            Style::default()
                .fg(theme::git_modified())
                .bg(theme::bg_bar()),
        ));
    }
    let (ec, wc) = app.diags.counts();
    if ec + wc > 0 || app.diags.cargo_running {
        let mut s = String::new();
        if app.diags.cargo_running {
            s.push_str(" checking…");
        }
        if ec > 0 {
            s.push_str(&format!(" ✗{}", ec));
        }
        if wc > 0 {
            s.push_str(&format!(" ▲{}", wc));
        }
        s.push(' ');
        left.push(Span::styled(
            s,
            Style::default()
                .fg(if ec > 0 {
                    theme::git_deleted()
                } else {
                    theme::git_modified()
                })
                .bg(theme::bg_bar()),
        ));
    }
    if app.cursor_diag.is_none()
        && let Some((note, color)) = &app.cursor_annotation
    {
        left.push(Span::styled(
            format!("  {} ", note),
            Style::default().fg(*color).bg(theme::bg_bar()),
        ));
    }
    if let Some(d) = &app.cursor_diag {
        left.push(Span::styled(
            format!("  {} ", d.message.replace('\n', " ")),
            Style::default().fg(d.severity.color()).bg(theme::bg_bar()),
        ));
    }
    let pos = match app.active_tab().map(|t| &t.kind) {
        Some(TabKind::Editor(e)) => {
            let p = e.primary().pos;
            let col = e.display_col(p.line, p.col) + 1;
            let multi = if e.cursors.len() > 1 {
                format!(" {} cursors ", e.cursors.len())
            } else {
                String::new()
            };
            format!("{}Ln {}/{}  Col {} ", multi, p.line + 1, e.lines.len(), col)
        }
        Some(TabKind::Diff(d)) => format!("row {}/{} ", d.selected + 1, d.rows.len()),
        _ => String::new(),
    };
    let right = format!("{} {} ", pos, time);
    f.render_widget(Paragraph::new(Line::from(left)).style(theme::bar()), area);
    let w = right.chars().count() as u16;
    if w < area.width {
        let r = Rect {
            x: area.x + area.width - w,
            y: area.y,
            width: w,
            height: 1,
        };
        f.render_widget(
            Paragraph::new(Span::styled(
                right,
                Style::default().fg(theme::fg()).bg(theme::bg_bar()),
            )),
            r,
        );
    }
}

fn draw_prompt(f: &mut Frame, app: &App, area: Rect) {
    let Some(p) = &app.prompt else { return };
    let line = Line::from(vec![
        Span::styled(
            format!(" {}: ", p.label),
            theme::accent().bg(theme::bg_bar()),
        ),
        Span::styled(
            p.input.clone(),
            Style::default().fg(theme::fg()).bg(theme::bg_bar()),
        ),
    ]);
    f.render_widget(Paragraph::new(line).style(theme::bar()), area);
    let x = area.x + (p.label.chars().count() + 3 + p.input.chars().count()) as u16;
    f.set_cursor_position((x.min(area.x + area.width - 1), area.y));
}

fn draw_palette(f: &mut Frame, app: &mut App, area: Rect) {
    let keymap_labels: Vec<(crate::keymap::Action, String)> = crate::keymap::Action::ALL
        .iter()
        .map(|a| (*a, app.keymap.labels(*a)))
        .collect();
    let Some(p) = app.palette.as_mut() else {
        return;
    };
    let w = (area.width * 3 / 5).clamp(40, 90).min(area.width);
    let max_rows = (area.height * 2 / 5).clamp(6, 20) as usize;
    let rows = p.items.len().min(max_rows).max(1);
    let h = rows as u16 + 4; // border + input + separator + rows + border
    let y = area.y + area.height / 8;
    let h = h.min(area.height.saturating_sub(y - area.y)).max(1);
    let r = Rect {
        x: area.x + (area.width - w) / 2,
        y,
        width: w,
        height: h,
    };
    f.render_widget(Clear, r);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::border(true))
        .title(" command palette ")
        .style(theme::panel());
    let inner = block.inner(r);
    f.render_widget(block, r);
    if inner.height < 3 {
        return;
    }
    // input line
    let input = Line::from(vec![
        Span::styled("> ", theme::accent()),
        Span::styled(p.query.clone(), Style::default().fg(theme::fg())),
    ]);
    f.render_widget(
        Paragraph::new(input),
        Rect {
            x: inner.x,
            y: inner.y,
            width: inner.width,
            height: 1,
        },
    );
    f.set_cursor_position((inner.x + 2 + p.query.chars().count() as u16, inner.y));
    f.render_widget(
        Paragraph::new(Span::styled(
            "─".repeat(inner.width as usize),
            Style::default().fg(theme::fg_border()),
        )),
        Rect {
            x: inner.x,
            y: inner.y + 1,
            width: inner.width,
            height: 1,
        },
    );
    // list
    let list_rows = (inner.height - 2) as usize;
    clamp_scroll(&mut p.scroll, p.selected, list_rows);
    let mut lines: Vec<Line> = Vec::new();
    if p.items.is_empty() {
        lines.push(Line::from(Span::styled(
            "  no matching command",
            theme::dim(),
        )));
    }
    for (i, (action, positions)) in p.items.iter().enumerate().skip(p.scroll).take(list_rows) {
        let label = crate::app::Palette::label(*action);
        // primary binding only; the full list lives in F1
        let keys = keymap_labels
            .iter()
            .find(|(a, _)| a == action)
            .map(|(_, k)| k.split(',').next().unwrap_or("").trim().to_string())
            .unwrap_or_default();
        let name = if inner.width >= 72 { action.name() } else { "" };
        let selected = i == p.selected;
        let base = Style::default().fg(theme::fg());
        let hit = Style::default()
            .fg(theme::syn_function())
            .add_modifier(Modifier::BOLD);
        let mut spans: Vec<Span> = vec![Span::styled(
            if selected { " ▸ " } else { "   " },
            theme::accent(),
        )];
        // right side: binding and config name; the label gets whatever is left
        let right = if name.is_empty() {
            keys.clone()
        } else {
            format!("{}  {}", keys, name)
        };
        let right_w = right.chars().count();
        let label_room = (inner.width as usize)
            .saturating_sub(3 + right_w + 2)
            .max(8);
        let label_len = label.chars().count();
        let shown = if label_len > label_room {
            label_room.saturating_sub(1)
        } else {
            label_len
        };
        for (ci, ch) in label.chars().take(shown).enumerate() {
            let style = if positions.contains(&ci) { hit } else { base };
            spans.push(Span::styled(ch.to_string(), style));
        }
        if shown < label_len {
            spans.push(Span::styled("…", theme::dim()));
        }
        let used = 3 + shown + usize::from(shown < label_len);
        let pad = (inner.width as usize).saturating_sub(used + right_w + 1);
        spans.push(Span::raw(" ".repeat(pad.max(1))));
        spans.push(Span::styled(
            keys.clone(),
            Style::default().fg(theme::syn_type()),
        ));
        if !name.is_empty() {
            spans.push(Span::styled(format!("  {}", name), theme::dim()));
        }
        let mut line = Line::from(spans);
        if selected {
            line = line.style(Style::default().bg(theme::bg_selection()));
        }
        lines.push(line);
    }
    f.render_widget(
        Paragraph::new(lines),
        Rect {
            x: inner.x,
            y: inner.y + 2,
            width: inner.width,
            height: inner.height - 2,
        },
    );
}

fn draw_help(f: &mut Frame, app: &App, area: Rect) {
    use crate::keymap::Action;
    let mut text: Vec<(String, String)> = Vec::new();
    let mut section = "";
    for a in Action::ALL {
        if a.section() != section {
            section = a.section();
            if !text.is_empty() {
                text.push((String::new(), String::new()));
            }
            text.push((section.to_string(), String::new()));
        }
        let keys = app.keymap.labels(*a);
        text.push((
            if keys.is_empty() {
                "(unbound)".into()
            } else {
                keys
            },
            a.describe().to_string(),
        ));
    }
    text.push((String::new(), String::new()));
    text.push(("panes".into(), String::new()));
    text.push((
        "enter / space".into(),
        "open · toggle · run · launch · jump".into(),
    ));
    text.push(("e / E".into(), "expand-collapse selected / all".into()));
    text.push((
        "d · o · a · r".into(),
        "diff · open · run all · refresh".into(),
    ));
    text.push((
        "c n s o p x".into(),
        "debug: continue, over, into, out, pause, stop".into(),
    ));
    text.push((String::new(), String::new()));
    text.push((
        format!("settings: {}", crate::settings::settings_path().display()),
        String::new(),
    ));

    let h = (text.len() + 2) as u16;
    let w = 70u16.min(area.width);
    let height = h.min(area.height);
    let r = Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width: w,
        height,
    };
    f.render_widget(Clear, r);
    let lines: Vec<Line> = text
        .iter()
        .map(|(k, v)| {
            if v.is_empty() {
                Line::from(Span::styled(k.clone(), theme::accent()))
            } else {
                Line::from(vec![
                    Span::styled(
                        format!("{:<30}", k),
                        Style::default().fg(theme::syn_function()),
                    ),
                    Span::styled(v.clone(), Style::default().fg(theme::fg())),
                ])
            }
        })
        .collect();
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::border(true))
        .title(" keys (any key to close) ")
        .style(theme::panel());
    f.render_widget(Paragraph::new(lines).block(block), r);
}
