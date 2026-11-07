//! Named actions, key parsing and the binding table loaded from `~/.redact.toml`.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// A normalised key press: lowercase letters with `shift` as a modifier.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Key {
    pub code: KeyCode,
    pub mods: KeyModifiers,
}

const MODS: KeyModifiers = KeyModifiers::CONTROL
    .union(KeyModifiers::ALT)
    .union(KeyModifiers::SHIFT);

impl Key {
    pub fn from_event(ev: KeyEvent) -> Key {
        let mut mods = ev.modifiers & MODS;
        let code = match ev.code {
            KeyCode::Char(c) if c.is_uppercase() => {
                mods |= KeyModifiers::SHIFT;
                KeyCode::Char(c.to_lowercase().next().unwrap_or(c))
            }
            KeyCode::BackTab => {
                mods |= KeyModifiers::SHIFT;
                KeyCode::Tab
            }
            other => other,
        };
        Key { code, mods }
    }

    pub fn shift(&self) -> bool {
        self.mods.contains(KeyModifiers::SHIFT)
    }

    pub fn without_shift(&self) -> Key {
        Key {
            code: self.code,
            mods: self.mods - KeyModifiers::SHIFT,
        }
    }

    /// Parse `ctrl+shift+up`, `f5`, `alt+/`, `space`, `Z` (= shift+z).
    pub fn parse(s: &str) -> Result<Key, String> {
        let s = s.trim();
        if s.is_empty() {
            return Err("empty key".into());
        }
        let mut mods = KeyModifiers::NONE;
        let mut parts: Vec<&str> = s.split('+').collect();
        // a trailing "+" means the key itself is '+'
        if s.ends_with('+') {
            parts.pop();
            parts.push("+");
        }
        let last = parts.pop().unwrap();
        for m in parts.into_iter().filter(|p| !p.is_empty()) {
            match m.trim().to_ascii_lowercase().as_str() {
                "ctrl" | "control" | "c" => mods |= KeyModifiers::CONTROL,
                "alt" | "opt" | "option" | "meta" | "m" => mods |= KeyModifiers::ALT,
                "shift" | "s" => mods |= KeyModifiers::SHIFT,
                other => return Err(format!("unknown modifier `{}` in `{}`", other, s)),
            }
        }
        let name = last.trim();
        let lower = name.to_ascii_lowercase();
        let code = match lower.as_str() {
            "up" => KeyCode::Up,
            "down" => KeyCode::Down,
            "left" => KeyCode::Left,
            "right" => KeyCode::Right,
            "home" => KeyCode::Home,
            "end" => KeyCode::End,
            "pgup" | "pageup" => KeyCode::PageUp,
            "pgdn" | "pagedown" => KeyCode::PageDown,
            "enter" | "return" | "cr" => KeyCode::Enter,
            "tab" => KeyCode::Tab,
            "backtab" => {
                mods |= KeyModifiers::SHIFT;
                KeyCode::Tab
            }
            "backspace" | "bs" => KeyCode::Backspace,
            "delete" | "del" => KeyCode::Delete,
            "insert" | "ins" => KeyCode::Insert,
            "esc" | "escape" => KeyCode::Esc,
            "space" => KeyCode::Char(' '),
            "plus" => KeyCode::Char('+'),
            "comma" => KeyCode::Char(','),
            _ => {
                if let Some(n) = lower.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()) {
                    if (1..=24).contains(&n) {
                        KeyCode::F(n)
                    } else {
                        return Err(format!("bad function key `{}`", name));
                    }
                } else {
                    let mut chars = name.chars();
                    match (chars.next(), chars.next()) {
                        (Some(c), None) => {
                            if c.is_uppercase() {
                                mods |= KeyModifiers::SHIFT;
                                KeyCode::Char(c.to_lowercase().next().unwrap_or(c))
                            } else {
                                KeyCode::Char(c)
                            }
                        }
                        _ => return Err(format!("unknown key `{}`", name)),
                    }
                }
            }
        };
        Ok(Key { code, mods })
    }

    pub fn label(&self) -> String {
        let mut s = String::new();
        if self.mods.contains(KeyModifiers::CONTROL) {
            s.push_str("ctrl+");
        }
        if self.mods.contains(KeyModifiers::ALT) {
            s.push_str("alt+");
        }
        if self.mods.contains(KeyModifiers::SHIFT) {
            s.push_str("shift+");
        }
        let k = match self.code {
            KeyCode::Up => "↑".to_string(),
            KeyCode::Down => "↓".to_string(),
            KeyCode::Left => "←".to_string(),
            KeyCode::Right => "→".to_string(),
            KeyCode::Home => "home".into(),
            KeyCode::End => "end".into(),
            KeyCode::PageUp => "pgup".into(),
            KeyCode::PageDown => "pgdn".into(),
            KeyCode::Enter => "enter".into(),
            KeyCode::Tab => "tab".into(),
            KeyCode::Backspace => "backspace".into(),
            KeyCode::Delete => "delete".into(),
            KeyCode::Insert => "insert".into(),
            KeyCode::Esc => "esc".into(),
            KeyCode::Char(' ') => "space".into(),
            KeyCode::Char(',') => "comma".into(),
            KeyCode::Char(c) => c.to_string(),
            KeyCode::F(n) => format!("F{}", n),
            other => format!("{:?}", other).to_lowercase(),
        };
        s.push_str(&k);
        s
    }
}

macro_rules! actions {
    ($( $variant:ident, $name:literal, $section:literal, $default:literal, $term:literal, $desc:literal; )*) => {
        #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
        pub enum Action { $( $variant, )* }

        impl Action {
            pub const ALL: &'static [Action] = &[ $( Action::$variant, )* ];

            pub fn name(self) -> &'static str {
                match self { $( Action::$variant => $name, )* }
            }
            pub fn section(self) -> &'static str {
                match self { $( Action::$variant => $section, )* }
            }
            pub fn default_binding(self) -> &'static str {
                match self { $( Action::$variant => $default, )* }
            }
            /// Intercepted even while a terminal tab has focus.
            pub fn terminal_safe(self) -> bool {
                match self { $( Action::$variant => $term, )* }
            }
            pub fn describe(self) -> &'static str {
                match self { $( Action::$variant => $desc, )* }
            }
            pub fn from_name(s: &str) -> Option<Action> {
                match s { $( $name => Some(Action::$variant), )* _ => None }
            }
        }
    };
}

actions! {
    // variant, config name, section, default binding(s), terminal-safe, description
    Palette,       "command_palette", "global", "ctrl+p, ctrl+shift+p, ctrl+k space", true, "command palette";
    Help,          "help",            "global", "f1",                   true,  "key reference";
    Explorer,      "explorer",        "global", "f2, alt+1",            true,  "explorer pane";
    Scm,           "source_control",  "global", "f3, alt+2",            true,  "source control pane";
    Tests,         "tests",           "global", "f4, alt+3",            true,  "tests pane";
    Debug,         "debug",           "global", "ctrl+k d, alt+4",      true,  "debug pane";
    Problems,      "problems",        "global", "ctrl+k e, alt+5",      true,  "problems pane";
    FocusToggle,   "focus_toggle",    "global", "ctrl+b",               true,  "switch focus left ⇄ right";
    LeftToggle,    "left_toggle",     "global", "f6, ctrl+k h, alt+h",  true,  "hide / show left pane";
    Terminal,      "terminal",        "global", "ctrl+t",               true,  "new terminal tab (pinned)";
    CloseTab,      "close_tab",       "global", "ctrl+w, ctrl+k w",     true,  "close tab (pinned: skip to next)";
    PinTab,        "pin_tab",         "global", "ctrl+k p, alt+/",      true,  "pin / unpin tab";
    PrevTab,       "prev_tab",        "global", "f7, ctrl+pgup, alt+[", true,  "previous tab";
    NextTab,       "next_tab",        "global", "f8, ctrl+pgdn, alt+]", true,  "next tab";
    Open,          "open",            "global", "ctrl+o",               false, "open a path";
    NewFile,       "new_file",        "global", "ctrl+n",               false, "new empty buffer";
    Save,          "save",            "global", "ctrl+s",               false, "save";
    SaveAs,        "save_as",         "global", "ctrl+k s, ctrl+shift+s", false, "save as";
    Reload,        "reload",          "global", "ctrl+k r",             false, "reload file from disk";
    Format,        "format",          "editor", "ctrl+k f, alt+shift+f", false, "format document";
    ToggleWhitespace, "toggle_whitespace", "editor", "",                 false, "show / hide whitespace marks";
    RunTests,      "run_tests",       "global", "ctrl+k t",             false, "run all tests";
    ExpandAll,     "expand_all",      "global", "ctrl+k a",             false, "expand / collapse all (explorer, tests)";
    Preview,       "preview",         "global", "ctrl+k v",             false, "toggle edit / preview (markdown, json, toml, xml)";
    Settings,      "settings",        "global", "ctrl+k comma",             false, "edit settings (saving applies them)";
    Quit,          "quit",            "global", "ctrl+q",               true,  "quit";
    DebugContinue, "debug_continue",  "debug",  "f5",                   true,  "launch target / continue";
    StepOver,      "step_over",       "debug",  "f10",                  true,  "step over";
    StepIn,        "step_in",         "debug",  "f11",                  true,  "step into";
    StepOut,       "step_out",        "debug",  "f12, shift+f11",       true,  "step out";
    DebugStop,     "debug_stop",      "debug",  "ctrl+k x",             false, "stop session";
    Breakpoint,    "breakpoint",      "debug",  "f9, ctrl+k b",         false, "toggle breakpoint on cursor line";
    NextProblem,   "next_problem",    "problems", "ctrl+k ]",           false, "next problem in file";
    PrevProblem,   "prev_problem",    "problems", "ctrl+k [",           false, "previous problem in file";
    ProblemsOutput,"problems_output", "problems", "ctrl+k E",           false, "compiler output tab";
    CargoCheck,    "cargo_check",     "problems", "ctrl+k !",           false, "re-run cargo check";
    WordLeft,      "word_left",       "editor", "ctrl+left, alt+b",     false, "word left (shift: select)";
    WordRight,     "word_right",      "editor", "ctrl+right, alt+f",    false, "word right (shift: select)";
    DocStart,      "doc_start",       "editor", "ctrl+home",            false, "start of file (shift: select)";
    DocEnd,        "doc_end",         "editor", "ctrl+end",             false, "end of file (shift: select)";
    MoveLineUp,    "move_line_up",    "editor", "ctrl+up, alt+up",      false, "move line / selection up";
    MoveLineDown,  "move_line_down",  "editor", "ctrl+down, alt+down",  false, "move line / selection down";
    AddCursorUp,   "add_cursor_up",   "editor", "ctrl+k up, ctrl+shift+up, alt+shift+up",       false, "add cursor above";
    AddCursorDown, "add_cursor_down", "editor", "ctrl+k down, ctrl+shift+down, alt+shift+down", false, "add cursor below";
    LineEnds,      "line_ends",       "editor", "ctrl+e, ctrl+k enter, ctrl+shift+enter", false, "cursor at end of every selected line";
    SelectNext,    "select_next",     "editor", "ctrl+d",               false, "select word, then next match";
    SelectAll,     "select_all",      "editor", "ctrl+a",               false, "select all";
    Undo,          "undo",            "editor", "ctrl+z",               false, "undo";
    Redo,          "redo",            "editor", "ctrl+y, ctrl+shift+z", false, "redo";
    Copy,          "copy",            "editor", "ctrl+c",               false, "copy (per cursor)";
    Cut,           "cut",             "editor", "ctrl+x",               false, "cut";
    Paste,         "paste",           "editor", "ctrl+v",               false, "paste";
    Find,          "find",            "editor", "ctrl+f",               false, "find";
    FindNext,      "find_next",       "editor", "ctrl+g",               false, "find next";
    GotoLine,      "goto_line",       "editor", "ctrl+l",               false, "go to line";
    ToggleComment, "toggle_comment",  "editor", "ctrl+k c, ctrl+/",     false, "toggle line comment";
    DeleteLine,    "delete_line",     "editor", "ctrl+k k, ctrl+shift+k", false, "delete line";
    DeleteWordBack,"delete_word_back","editor", "ctrl+backspace, alt+backspace", false, "delete word backwards";
}

impl Action {
    /// Movement actions take `shift` as "extend the selection".
    pub fn is_movement(self) -> bool {
        matches!(
            self,
            Action::WordLeft | Action::WordRight | Action::DocStart | Action::DocEnd
        )
    }
}

pub enum Lookup {
    Action(Action),
    /// The keys so far are the start of a longer binding.
    Prefix,
    None,
}

pub struct Keymap {
    bindings: Vec<(Vec<Key>, Action)>,
}

impl Keymap {
    pub fn defaults() -> Keymap {
        let mut km = Keymap {
            bindings: Vec::new(),
        };
        for a in Action::ALL {
            let _ = km.bind(*a, a.default_binding());
        }
        km
    }

    /// Parse a spec like `"f5, ctrl+k p"` and make it the only binding for `action`.
    pub fn bind(&mut self, action: Action, spec: &str) -> Result<(), String> {
        let mut parsed: Vec<Vec<Key>> = Vec::new();
        for alt in spec.split(',') {
            let alt = alt.trim();
            if alt.is_empty() {
                continue;
            }
            let keys: Result<Vec<Key>, String> = alt.split_whitespace().map(Key::parse).collect();
            let keys = keys.map_err(|e| format!("{} = \"{}\": {}", action.name(), spec, e))?;
            if keys.len() > 2 {
                return Err(format!(
                    "{} = \"{}\": at most two keys per binding",
                    action.name(),
                    spec
                ));
            }
            parsed.push(keys);
        }
        self.bindings.retain(|(_, a)| *a != action);
        for keys in parsed {
            // a key sequence can only mean one thing
            self.bindings.retain(|(k, _)| *k != keys);
            self.bindings.push((keys, action));
        }
        Ok(())
    }

    pub fn lookup(&self, pending: &[Key], key: Key) -> Lookup {
        let mut seq: Vec<Key> = pending.to_vec();
        seq.push(key);
        for (keys, action) in &self.bindings {
            if *keys == seq {
                return Lookup::Action(*action);
            }
        }
        if self
            .bindings
            .iter()
            .any(|(keys, _)| keys.len() > seq.len() && keys[..seq.len()] == seq[..])
        {
            return Lookup::Prefix;
        }
        Lookup::None
    }

    /// Human-readable bindings for an action, e.g. `F5`, `ctrl+k p`.
    pub fn labels(&self, action: Action) -> String {
        self.bindings
            .iter()
            .filter(|(_, a)| *a == action)
            .map(|(keys, _)| keys.iter().map(|k| k.label()).collect::<Vec<_>>().join(" "))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_keys() {
        assert_eq!(
            Key::parse("ctrl+shift+up").unwrap(),
            Key {
                code: KeyCode::Up,
                mods: KeyModifiers::CONTROL | KeyModifiers::SHIFT
            }
        );
        assert_eq!(
            Key::parse("F5").unwrap(),
            Key {
                code: KeyCode::F(5),
                mods: KeyModifiers::NONE
            }
        );
        assert_eq!(
            Key::parse("alt+/").unwrap(),
            Key {
                code: KeyCode::Char('/'),
                mods: KeyModifiers::ALT
            }
        );
        assert_eq!(
            Key::parse("E").unwrap(),
            Key {
                code: KeyCode::Char('e'),
                mods: KeyModifiers::SHIFT
            }
        );
        assert_eq!(
            Key::parse("ctrl++").unwrap(),
            Key {
                code: KeyCode::Char('+'),
                mods: KeyModifiers::CONTROL
            }
        );
        assert!(Key::parse("hyper+x").is_err());
    }

    #[test]
    fn chords_and_prefixes() {
        let km = Keymap::defaults();
        let k = |s: &str| Key::parse(s).unwrap();
        assert!(matches!(km.lookup(&[], k("ctrl+k")), Lookup::Prefix));
        assert!(matches!(
            km.lookup(&[k("ctrl+k")], k("p")),
            Lookup::Action(Action::PinTab)
        ));
        assert!(matches!(
            km.lookup(&[], k("f5")),
            Lookup::Action(Action::DebugContinue)
        ));
        assert!(matches!(km.lookup(&[], k("ctrl+shift+q")), Lookup::None));
    }

    #[test]
    fn rebinding_replaces_defaults() {
        let mut km = Keymap::defaults();
        km.bind(Action::Save, "f2").unwrap();
        assert!(matches!(
            km.lookup(&[], Key::parse("f2").unwrap()),
            Lookup::Action(Action::Save)
        ));
        assert!(matches!(
            km.lookup(&[], Key::parse("ctrl+s").unwrap()),
            Lookup::None
        ));
        assert!(matches!(
            km.lookup(&[], Key::parse("alt+1").unwrap()),
            Lookup::Action(Action::Explorer)
        ));
        assert_eq!(km.labels(Action::Save), "F2");
    }
}

/// Case-insensitive fuzzy match: every query char must appear in order.
/// Returns (score, matched char positions); lower score is better.
pub fn fuzzy_match(query: &str, text: &str) -> Option<(usize, Vec<usize>)> {
    let q: Vec<char> = query
        .to_lowercase()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    if q.is_empty() {
        return Some((0, Vec::new()));
    }
    let t: Vec<char> = text.to_lowercase().chars().collect();
    // exact substring: best
    let tl: String = t.iter().collect();
    let ql: String = q.iter().collect();
    if let Some(pos) = tl.find(&ql) {
        let start = tl[..pos].chars().count();
        let bonus = if start == 0 || !t[start - 1].is_alphanumeric() {
            0
        } else {
            5
        };
        return Some((bonus + start / 10, (start..start + q.len()).collect()));
    }
    // subsequence: penalise gaps
    let mut positions = Vec::with_capacity(q.len());
    let mut ti = 0;
    let mut score = 50;
    let mut last: Option<usize> = None;
    for &qc in &q {
        let mut found = None;
        while ti < t.len() {
            if t[ti] == qc {
                found = Some(ti);
                ti += 1;
                break;
            }
            ti += 1;
        }
        let pos = found?;
        if let Some(l) = last {
            score += (pos - l - 1).min(20);
        }
        if pos == 0 || !t[pos - 1].is_alphanumeric() {
            score = score.saturating_sub(2);
        }
        positions.push(pos);
        last = Some(pos);
    }
    Some((score, positions))
}
