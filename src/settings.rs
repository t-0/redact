//! `~/.redact.toml`: created with the defaults on first run, then read back.

use std::path::PathBuf;

use crate::keymap::{Action, Keymap};

/// Something wrong in the settings file, with the line it is on when known.
pub struct Problem {
    pub message: String,
    pub line: Option<usize>,
}

pub struct Settings {
    pub path: PathBuf,
    pub keymap: Keymap,
    pub left_width: u16,
    /// On save: strip trailing spaces and tabs (outside multi-line strings).
    pub trim_trailing_whitespace: bool,
    /// On save: make sure known file types end with a newline.
    pub final_newline: bool,
    /// Draw spaces as · and tabs as → in a near-background grey.
    pub show_whitespace: bool,
    /// Problems found while reading the file, reported in the status bar.
    pub errors: Vec<Problem>,
    pub created: bool,
}

/// Line (0-based) of the first `key = ` assignment, for pointing at a problem.
fn line_of_key(text: &str, key: &str) -> Option<usize> {
    text.lines().position(|l| {
        let t = l.trim_start();
        t.strip_prefix(key)
            .map(|rest| rest.trim_start().starts_with('='))
            .unwrap_or(false)
    })
}

fn line_of_span(text: &str, span: Option<std::ops::Range<usize>>) -> Option<usize> {
    let span = span?;
    Some(text[..span.start.min(text.len())].matches('\n').count())
}

fn push(errors: &mut Vec<Problem>, text: &str, key: &str, message: String) {
    errors.push(Problem {
        message: format!("{}: {}", key, message),
        line: line_of_key(text, key),
    });
}

pub fn settings_path() -> PathBuf {
    if let Ok(p) = std::env::var("REDACT_CONFIG") {
        return PathBuf::from(p);
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".redact.toml")
}

/// The file written on first run: every action with its default binding.
pub fn default_text() -> String {
    let mut s = String::new();
    s.push_str("# redact settings\n");
    s.push_str("#\n");
    s.push_str("# Bindings are key = \"binding\". Separate alternatives with commas; two keys\n");
    s.push_str("# separated by a space make a chord (press the first, then the second).\n");
    s.push_str("# Modifiers: ctrl+ alt+ shift+   Keys: a-z 0-9 punctuation f1-f12 up down\n");
    s.push_str("# left right home end pgup pgdn enter tab backspace delete esc space.\n");
    s.push_str("# A capital letter means shift+letter. Write `comma` and `plus` for those keys.\n");
    s.push_str("# An empty string unbinds the action.\n");
    s.push_str("# In the editor, adding shift to a movement binding extends the selection.\n");
    s.push_str("#\n");
    s.push_str("# Pane keys that are not listed here are fixed: arrows, enter, esc, and the\n");
    s.push_str("# single-letter commands shown in the F1 overlay.\n");
    s.push_str("\n[ui]\n");
    s.push_str("# width of the left pane in columns\n");
    s.push_str("left_width = 34\n");
    s.push_str("# colour the terminal can show: truecolor (24-bit), 256, or 16\n");
    s.push_str("color_depth = \"truecolor\"\n");
    s.push_str("\n[editor]\n");
    s.push_str("# on save, for recognised file types: strip trailing spaces and tabs,\n");
    s.push_str("# leaving lines inside multi-line strings, heredocs and markdown alone\n");
    s.push_str("trim_trailing_whitespace = true\n");
    s.push_str("# on save, for recognised file types: end the file with a newline\n");
    s.push_str("final_newline = true\n");
    s.push_str("# draw spaces as a faint centre dot and tabs as a faint arrow\n");
    s.push_str("show_whitespace = false\n");
    s.push_str("\n[colors]\n");
    s.push_str("# Any of these can be set to #rrggbb, #rgb or \"r, g, b\".\n");
    s.push_str("# They are shown commented out at their defaults; uncomment to change one.\n");
    for (name, _, default, doc) in crate::theme::entries() {
        let hex = format!("#{:06x}", default);
        s.push_str(&format!("# {:<20} = \"{}\"   # {}\n", name, hex, doc));
    }
    s.push_str("\n[keys]\n");
    let mut section = "";
    for a in Action::ALL {
        if a.section() != section {
            section = a.section();
            s.push_str(&format!("\n# {}\n", section));
        }
        s.push_str(&format!(
            "{:<17} = \"{}\"   # {}\n",
            a.name(),
            a.default_binding(),
            a.describe()
        ));
    }
    s
}

pub fn load() -> Settings {
    let path = settings_path();
    let mut settings = Settings {
        trim_trailing_whitespace: true,
        final_newline: true,
        show_whitespace: false,
        path: path.clone(),
        keymap: Keymap::defaults(),
        left_width: 34,
        errors: Vec::new(),
        created: false,
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => {
            let text = default_text();
            match std::fs::write(&path, &text) {
                Ok(()) => settings.created = true,
                Err(e) => settings.errors.push(Problem {
                    message: format!("could not write {}: {}", path.display(), e),
                    line: None,
                }),
            }
            text
        }
    };
    let table: toml::Table = match toml::from_str(&text) {
        Ok(t) => t,
        Err(e) => {
            settings.errors.push(Problem {
                message: format!("{}: {}", path.display(), e.message()),
                line: line_of_span(&text, e.span()),
            });
            return settings;
        }
    };
    crate::theme::reset();
    crate::theme::set_depth(crate::theme::Depth::True);
    if let Some(ui) = table.get("ui").and_then(|v| v.as_table())
        && let Some(d) = ui.get("color_depth").and_then(|v| v.as_str())
    {
        match crate::theme::Depth::parse(d) {
            Some(depth) => crate::theme::set_depth(depth),
            None => push(
                &mut settings.errors,
                &text,
                "color_depth",
                format!("`{}` is not truecolor, 256 or 16", d),
            ),
        }
    }
    if let Some(colors) = table.get("colors").and_then(|v| v.as_table()) {
        for (name, value) in colors {
            let Some(spec) = value.as_str() else {
                push(&mut settings.errors, &text, name, "must be a string".into());
                continue;
            };
            match crate::theme::parse_color(spec) {
                Some(rgb) => {
                    if !crate::theme::set_named(name, rgb) {
                        push(
                            &mut settings.errors,
                            &text,
                            name,
                            format!("unknown colour `{}`", name),
                        );
                    }
                }
                None => push(
                    &mut settings.errors,
                    &text,
                    name,
                    format!("`{}` is not a colour", spec),
                ),
            }
        }
    }
    if let Some(ui) = table.get("ui").and_then(|v| v.as_table())
        && let Some(w) = ui.get("left_width").and_then(|v| v.as_integer())
    {
        settings.left_width = w.clamp(16, 120) as u16;
    }
    if let Some(ed) = table.get("editor").and_then(|v| v.as_table()) {
        if let Some(b) = ed.get("trim_trailing_whitespace").and_then(|v| v.as_bool()) {
            settings.trim_trailing_whitespace = b;
        }
        if let Some(b) = ed.get("final_newline").and_then(|v| v.as_bool()) {
            settings.final_newline = b;
        }
        if let Some(b) = ed.get("show_whitespace").and_then(|v| v.as_bool()) {
            settings.show_whitespace = b;
        }
    }
    if let Some(keys) = table.get("keys").and_then(|v| v.as_table()) {
        for (name, value) in keys {
            let Some(action) = Action::from_name(name) else {
                push(
                    &mut settings.errors,
                    &text,
                    name,
                    format!("unknown action `{}`", name),
                );
                continue;
            };
            let Some(spec) = value.as_str() else {
                push(&mut settings.errors, &text, name, "must be a string".into());
                continue;
            };
            if let Err(e) = settings.keymap.bind(action, spec) {
                push(&mut settings.errors, &text, name, e);
            }
        }
    }
    settings
}
