//! The colour palette: dark and pastel by default, overridable from
//! `~/.redact.toml` and quantised for terminals without 24-bit colour.
//!
//! Colours live in atomics so a settings reload can change them while the
//! editor is running, and so reading one during a redraw costs nothing.

use std::sync::atomic::{AtomicU8, AtomicU32, Ordering};

use ratatui::style::{Color, Modifier, Style};

/// How much colour the terminal can show.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Depth {
    /// 24-bit `ESC[38;2;r;g;b m`, the default.
    True,
    /// The xterm 256-colour cube.
    Indexed256,
    /// The 16 ANSI colours.
    Ansi16,
}

impl Depth {
    pub fn parse(s: &str) -> Option<Depth> {
        match s.trim().to_ascii_lowercase().as_str() {
            "truecolor" | "true" | "24" | "24bit" | "rgb" => Some(Depth::True),
            "256" | "indexed" | "8bit" => Some(Depth::Indexed256),
            "16" | "ansi" | "4bit" => Some(Depth::Ansi16),
            _ => None,
        }
    }
}

const fn pack(r: u8, g: u8, b: u8) -> u32 {
    ((r as u32) << 16) | ((g as u32) << 8) | b as u32
}

static DEPTH: AtomicU8 = AtomicU8::new(0);

fn depth() -> Depth {
    match DEPTH.load(Ordering::Relaxed) {
        1 => Depth::Indexed256,
        2 => Depth::Ansi16,
        _ => Depth::True,
    }
}

pub fn set_depth(d: Depth) {
    DEPTH.store(
        match d {
            Depth::True => 0,
            Depth::Indexed256 => 1,
            Depth::Ansi16 => 2,
        },
        Ordering::Relaxed,
    );
}

/// Declare the palette: static cell, accessor, settings key and description.
macro_rules! palette {
    ($( $cell:ident, $name:ident, $r:literal, $g:literal, $b:literal, $doc:literal; )*) => {
        $(
            static $cell: AtomicU32 = AtomicU32::new(pack($r, $g, $b));
            #[doc = $doc]
            pub fn $name() -> Color {
                adjust_rgb($cell.load(Ordering::Relaxed))
            }
        )*

        /// Every colour: settings key, storage cell, default value, description.
        pub fn entries() -> Vec<(&'static str, &'static AtomicU32, u32, &'static str)> {
            vec![ $( (stringify!($name), &$cell, pack($r, $g, $b), $doc), )* ]
        }
    };
}

palette! {
    // cell, settings key, r, g, b, description
    C_BG,                  bg,                   0x1b, 0x1b, 0x22, "editor background";
    C_BG_PANEL,            bg_panel,             0x1f, 0x1f, 0x28, "left pane and overlay background";
    C_BG_BAR,              bg_bar,               0x2a, 0x2a, 0x36, "status and tab bar background";
    C_BG_LINE,             bg_line,              0x26, 0x26, 0x32, "current line background";
    C_BG_SELECTION,        bg_selection,         0x3b, 0x40, 0x5a, "selection and focused row background";
    C_BG_CURSOR_SECONDARY, bg_cursor_secondary,  0x6a, 0x6a, 0x8a, "extra cursors in multi-cursor edits";
    C_BG_DIFF_ADD,         bg_diff_add,          0x25, 0x35, 0x2b, "added line background in diffs";
    C_BG_DIFF_DEL,         bg_diff_del,          0x3a, 0x27, 0x2b, "deleted line background in diffs";
    C_BG_DIFF_CHANGE,      bg_diff_change,       0x3a, 0x35, 0x25, "changed line background, debug stop line";
    C_FG,                  fg,                   0xe6, 0xe6, 0xea, "default text";
    C_FG_DIM,              fg_dim,               0x8a, 0x8f, 0x98, "secondary text";
    C_FG_GUTTER,           fg_gutter,            0x5c, 0x60, 0x6c, "line numbers";
    C_FG_WHITESPACE,       fg_whitespace,        0x33, 0x33, 0x3e, "rendered space and tab marks";
    C_FG_ACCENT,           fg_accent,            0xc9, 0xa0, 0xdc, "headings, prompts, focused borders";
    C_FG_BORDER,           fg_border,            0x3c, 0x3c, 0x4c, "pane borders and tree guides";
    C_GIT_ADDED,           git_added,            0xb5, 0xe8, 0xb0, "git added, test passed";
    C_GIT_DELETED,         git_deleted,          0xff, 0xb3, 0xb3, "git deleted, errors, test failed";
    C_GIT_MODIFIED,        git_modified,         0xf9, 0xd9, 0x9a, "git modified, warnings, debug marks";
    C_GIT_UNTRACKED,       git_untracked,        0xa8, 0xd8, 0xff, "git untracked";
    C_GIT_NORMAL,          git_normal,           0xf0, 0xf0, 0xf0, "git clean";
    C_SYN_KEYWORD,         syn_keyword,          0xc9, 0xa0, 0xdc, "keywords, markdown headings";
    C_SYN_FUNCTION,        syn_function,         0x8e, 0xca, 0xe6, "functions and methods";
    C_SYN_TYPE,            syn_type,             0xf9, 0xd9, 0x9a, "types, modules, group headers";
    C_SYN_STRING,          syn_string,           0xb5, 0xe8, 0xb0, "strings and code spans";
    C_SYN_NUMBER,          syn_number,           0xff, 0xb7, 0xb2, "numbers and booleans";
    C_SYN_COMMENT,         syn_comment,          0x8a, 0x8f, 0x98, "comments";
    C_SYN_OPERATOR,        syn_operator,         0xd4, 0xd4, 0xdc, "operators";
    C_SYN_PUNCT,           syn_punct,            0xb0, 0xb0, 0xbc, "punctuation";
    C_SYN_VARIABLE,        syn_variable,         0xe6, 0xe6, 0xea, "variables and plain text";
    C_SYN_ATTRIBUTE,       syn_attribute,        0xf6, 0xc1, 0x77, "attributes and XML attribute names";
    C_SYN_CONSTANT,        syn_constant,         0xff, 0xc8, 0xa2, "constants and builtins";
    C_SYN_PROPERTY,        syn_property,         0xc6, 0xe2, 0xff, "properties, keys, links";
    C_SYN_MACRO,           syn_macro,            0xf6, 0xc1, 0x77, "macros";
    C_SYN_LABEL,           syn_label,            0xe0, 0xb0, 0xff, "labels";
}

/// The focused-border colour follows the accent unless overridden.
pub fn fg_border_focus() -> Color {
    fg_accent()
}

/// Reset every colour to its built-in default.
pub fn reset() {
    for (_, cell, default, _) in entries() {
        cell.store(default, Ordering::Relaxed);
    }
}

/// Override one colour by settings key. False if the key is unknown.
pub fn set_named(key: &str, rgb: (u8, u8, u8)) -> bool {
    let key = key.trim().to_ascii_lowercase();
    for (name, cell, _, _) in entries() {
        if name == key {
            cell.store(pack(rgb.0, rgb.1, rgb.2), Ordering::Relaxed);
            return true;
        }
    }
    false
}

/// Parse `#rrggbb`, `rrggbb`, `#rgb` or `r,g,b`.
pub fn parse_color(s: &str) -> Option<(u8, u8, u8)> {
    let t = s.trim();
    if t.contains(',') {
        let parts: Vec<&str> = t.split(',').map(|p| p.trim()).collect();
        if parts.len() == 3 {
            let v: Option<Vec<u8>> = parts.iter().map(|p| p.parse::<u8>().ok()).collect();
            if let Some(v) = v {
                return Some((v[0], v[1], v[2]));
            }
        }
        return None;
    }
    let h = t.trim_start_matches('#');
    match h.len() {
        6 => {
            let n = u32::from_str_radix(h, 16).ok()?;
            Some((
                ((n >> 16) & 0xff) as u8,
                ((n >> 8) & 0xff) as u8,
                (n & 0xff) as u8,
            ))
        }
        3 => {
            let n = u32::from_str_radix(h, 16).ok()?;
            let x = |v: u32| ((v * 17) & 0xff) as u8;
            Some((x((n >> 8) & 0xf), x((n >> 4) & 0xf), x(n & 0xf)))
        }
        _ => None,
    }
}

/// Map a packed RGB value to what the terminal can show.
fn adjust_rgb(packed: u32) -> Color {
    let (r, g, b) = (
        ((packed >> 16) & 0xff) as u8,
        ((packed >> 8) & 0xff) as u8,
        (packed & 0xff) as u8,
    );
    match depth() {
        Depth::True => Color::Rgb(r, g, b),
        Depth::Indexed256 => Color::Indexed(to_256(r, g, b)),
        Depth::Ansi16 => Color::Indexed(to_16(r, g, b)),
    }
}

/// Quantise any colour the same way the palette is quantised (terminal output).
pub fn adjust(c: Color) -> Color {
    match (c, depth()) {
        (Color::Rgb(r, g, b), Depth::Indexed256) => Color::Indexed(to_256(r, g, b)),
        (Color::Rgb(r, g, b), Depth::Ansi16) => Color::Indexed(to_16(r, g, b)),
        (Color::Indexed(i), Depth::Ansi16) if i >= 16 => {
            let (r, g, b) = from_256(i);
            Color::Indexed(to_16(r, g, b))
        }
        _ => c,
    }
}

/// Nearest xterm-256 index: the 6x6x6 cube or the 24-step grey ramp.
fn to_256(r: u8, g: u8, b: u8) -> u8 {
    let cube_at = |v: u8| -> u8 {
        const STEPS: [u8; 6] = [0, 95, 135, 175, 215, 255];
        let mut best = 0;
        let mut dist = i32::MAX;
        for (i, s) in STEPS.iter().enumerate() {
            let d = (v as i32 - *s as i32).abs();
            if d < dist {
                dist = d;
                best = i as u8;
            }
        }
        best
    };
    let (ri, gi, bi) = (cube_at(r), cube_at(g), cube_at(b));
    const STEPS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let cube = (STEPS[ri as usize], STEPS[gi as usize], STEPS[bi as usize]);
    let cube_err = dist2((r, g, b), cube);
    // grey ramp 232..=255 is 8,18,...,238
    let grey_level = (((r as u32 + g as u32 + b as u32) / 3).saturating_sub(8) / 10).min(23) as u8;
    let grey_v = 8 + grey_level * 10;
    let grey_err = dist2((r, g, b), (grey_v, grey_v, grey_v));
    if grey_err < cube_err {
        232 + grey_level
    } else {
        16 + 36 * ri + 6 * gi + bi
    }
}

fn from_256(i: u8) -> (u8, u8, u8) {
    const STEPS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    if i >= 232 {
        let v = 8 + (i - 232) * 10;
        (v, v, v)
    } else {
        let n = i - 16;
        (
            STEPS[(n / 36) as usize],
            STEPS[((n / 6) % 6) as usize],
            STEPS[(n % 6) as usize],
        )
    }
}

/// Nearest of the 16 ANSI colours, using the common xterm values.
fn to_16(r: u8, g: u8, b: u8) -> u8 {
    const BASE: [(u8, u8, u8); 16] = [
        (0, 0, 0),
        (205, 0, 0),
        (0, 205, 0),
        (205, 205, 0),
        (0, 0, 238),
        (205, 0, 205),
        (0, 205, 205),
        (229, 229, 229),
        (127, 127, 127),
        (255, 0, 0),
        (0, 255, 0),
        (255, 255, 0),
        (92, 92, 255),
        (255, 0, 255),
        (0, 255, 255),
        (255, 255, 255),
    ];
    let mut best = 0u8;
    let mut dist = u32::MAX;
    for (i, c) in BASE.iter().enumerate() {
        let d = dist2((r, g, b), *c);
        if d < dist {
            dist = d;
            best = i as u8;
        }
    }
    best
}

fn dist2(a: (u8, u8, u8), b: (u8, u8, u8)) -> u32 {
    let d = |x: u8, y: u8| {
        let v = x as i32 - y as i32;
        (v * v) as u32
    };
    // weighted towards perceived brightness
    2 * d(a.0, b.0) + 4 * d(a.1, b.1) + d(a.2, b.2)
}

pub fn base() -> Style {
    Style::default().fg(fg()).bg(bg())
}

pub fn panel() -> Style {
    Style::default().fg(fg()).bg(bg_panel())
}

pub fn bar() -> Style {
    Style::default().fg(fg()).bg(bg_bar())
}

pub fn dim() -> Style {
    Style::default().fg(fg_dim())
}

pub fn accent() -> Style {
    Style::default()
        .fg(fg_accent())
        .add_modifier(Modifier::BOLD)
}

pub fn border(focused: bool) -> Style {
    if focused {
        Style::default().fg(fg_border_focus())
    } else {
        Style::default().fg(fg_border())
    }
}

/// Map a tree-sitter highlight capture name to a style.
pub fn syntax_style(capture: &str) -> Style {
    let bold = |c: Color| Style::default().fg(c).add_modifier(Modifier::BOLD);
    let plain = |c: Color| Style::default().fg(c);
    let head = capture.split('.').next().unwrap_or(capture);
    match head {
        "keyword" => bold(syn_keyword()),
        "function" => match capture {
            "function.macro" => bold(syn_macro()),
            _ => bold(syn_function()),
        },
        "method" | "constructor" => bold(syn_function()),
        "type" => bold(syn_type()),
        "string" if capture == "string.special.key" => plain(syn_property()),
        "string" | "character" | "escape" => plain(syn_string()),
        "text" => match capture {
            "text.title" => bold(syn_keyword()),
            "text.emphasis" => Style::default()
                .fg(syn_variable())
                .add_modifier(Modifier::ITALIC),
            "text.strong" => bold(syn_variable()),
            "text.literal" => plain(syn_string()),
            "text.reference" => plain(syn_function()),
            "text.uri" => Style::default()
                .fg(syn_property())
                .add_modifier(Modifier::UNDERLINED),
            "text.strike" => Style::default()
                .fg(fg_dim())
                .add_modifier(Modifier::CROSSED_OUT),
            _ => plain(syn_variable()),
        },
        "markup" => match capture {
            "markup.heading" => bold(syn_keyword()),
            "markup.link" => Style::default()
                .fg(syn_property())
                .add_modifier(Modifier::UNDERLINED),
            "markup.raw" => plain(syn_string()),
            _ => plain(syn_variable()),
        },
        "error" => Style::default()
            .fg(git_deleted())
            .add_modifier(Modifier::UNDERLINED),
        "none" | "spell" => plain(syn_variable()),
        "number" | "float" | "boolean" => plain(syn_number()),
        "comment" => Style::default()
            .fg(syn_comment())
            .add_modifier(Modifier::ITALIC),
        "operator" => plain(syn_operator()),
        "punctuation" => plain(syn_punct()),
        "variable" => match capture {
            "variable.builtin" => bold(syn_constant()),
            "variable.parameter" => plain(syn_property()),
            _ => plain(syn_variable()),
        },
        "attribute" => plain(syn_attribute()),
        "constant" => bold(syn_constant()),
        "property" => plain(syn_property()),
        "label" => plain(syn_label()),
        "module" | "namespace" => plain(syn_type()),
        "tag" => bold(syn_keyword()),
        "embedded" => plain(syn_variable()),
        _ => plain(syn_variable()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_colour_forms() {
        assert_eq!(parse_color("#c9a0dc"), Some((0xc9, 0xa0, 0xdc)));
        assert_eq!(parse_color("c9a0dc"), Some((0xc9, 0xa0, 0xdc)));
        assert_eq!(parse_color("#abc"), Some((0xaa, 0xbb, 0xcc)));
        assert_eq!(parse_color(" 18, 52, 86 "), Some((18, 52, 86)));
        assert_eq!(parse_color("#12345"), None);
        assert_eq!(parse_color("puce"), None);
    }

    // One test for both: the palette and depth are process-global, so splitting
    // these across tests lets a parallel run see the other's state.
    #[test]
    fn overrides_reset_and_quantisation() {
        set_depth(Depth::True);
        assert!(set_named("syn_keyword", (1, 2, 3)));
        assert_eq!(syn_keyword(), Color::Rgb(1, 2, 3));
        assert!(!set_named("not_a_colour", (1, 2, 3)));
        reset();
        assert_eq!(syn_keyword(), Color::Rgb(0xc9, 0xa0, 0xdc));
        assert_eq!(adjust(Color::Rgb(1, 2, 3)), Color::Rgb(1, 2, 3));

        set_depth(Depth::Indexed256);
        // the ends of the cube, and a grey that should pick the grey ramp
        assert_eq!(adjust(Color::Rgb(0, 0, 0)), Color::Indexed(16));
        assert_eq!(adjust(Color::Rgb(255, 255, 255)), Color::Indexed(231));
        match adjust(Color::Rgb(0x33, 0x33, 0x3e)) {
            Color::Indexed(i) => assert!(i >= 232 || i == 16, "got {}", i),
            c => panic!("expected indexed, got {:?}", c),
        }
        // the palette itself quantises, not just adjust()
        assert!(matches!(fg(), Color::Indexed(_)));

        set_depth(Depth::Ansi16);
        assert_eq!(adjust(Color::Rgb(255, 0, 0)), Color::Indexed(9));
        assert_eq!(adjust(Color::Rgb(0, 0, 0)), Color::Indexed(0));
        // a 256-index from a terminal tab folds down too
        assert!(matches!(adjust(Color::Indexed(200)), Color::Indexed(i) if i < 16));

        set_depth(Depth::True);
    }
}
