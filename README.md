# redact

A minimal terminal IDE in Rust (ratatui). *Editor → redact*: the point is cutting
things out — instant feedback, small memory, no plugin layer between you and the
text.

```
cargo run --release -- [folder]
```

## Layout

```
┌ 1 explorer 2 3 ─┐┌ tab bar: open files / terminals / diffs ───────┐
│ file tree       ││                                                │
│ (or scm/tests)  ││ editor · terminal · diff · test output          │
└─────────────────┘└────────────────────────────────────────────────┘
 activity  folder  branch  message                 Ln/Col  HH:MM
```

Left pane modes: **explorer** (git-decorated tree), **source control** (changed
files → side-by-side diff against HEAD), **tests** (discovered `#[test]` fns
and pytest tests, run one, a file, or all), **debug** (breakpoints, call stack
and variables for Rust via `lldb-dap` and Python via `debugpy`).

Explorer prefixes: `U` untracked (light blue), `A` added (green), `M` modified
(yellow), `D` deleted (red, struck through), `-` clean (white). Ignored paths
are dimmed. Directories roll up the "worst" status of their contents.

## Samples

`sample/` has one folder per format (`ini json md sh toml xml`) with a valid
file and an `invalid-*` file each, for trying highlighting, checking, preview
and formatting. Its README lists what each format gets.

## Settings

On first run redact writes `~/.redact.toml` (or the path in `REDACT_CONFIG`)
with every key binding and its default, in `key = "binding"` form:

```toml
[ui]
left_width = 34

[keys]
save              = "ctrl+s"
pin_tab           = "ctrl+k p, alt+/"
add_cursor_down   = "ctrl+k down, ctrl+shift+down, alt+shift+down"
```

An `[editor]` section holds `trim_trailing_whitespace` and `final_newline`,
both on by default. On save, for recognised file types, trailing spaces and
tabs are stripped and a missing final newline is added. Rows inside
multi-line strings, raw strings, heredocs and CDATA are left untouched (the
syntax tree says where they are), Markdown is never trimmed because two
trailing spaces are a line break, and unknown file types are not changed. The
cleanup is one undo step.

A `[colors]` table overrides any palette colour by name, as `#rrggbb`, `#rgb`
or `"r, g, b"`; the file lists every name commented out at its default, with a
note on what it paints. redact always emits 24-bit colour, so a terminal that
supports it shows exactly what you set. `color_depth` in `[ui]` is
`"truecolor"` (default), `"256"` or `"16"` for terminals that don't, and
quantises both the palette and anything a terminal tab prints.

`show_whitespace` (off by default) draws every space as a centre dot and every
tab as a small arrow in a grey only just above the background, so the marks
read as texture rather than text. The palette's "show / hide whitespace marks"
flips it for the session without touching the file.

Alternatives are comma-separated; two keys separated by a space make a chord.
Modifiers are `ctrl+`, `alt+`, `shift+`; keys are letters, digits,
punctuation, `f1`–`f12`, `up down left right home end pgup pgdn enter tab
backspace delete esc space`. A capital letter means `shift+letter`, an empty
string unbinds the action, and anything the file gets wrong is reported in
the problems pane, pointing at the line, with the default kept. `ctrl+k comma` opens the file in
redact itself; saving it applies the new bindings immediately. F1 shows the
bindings in effect. Arrow keys, enter, esc and the single-letter pane commands are fixed.

## Keys

Chosen to work in any terminal, including macOS Terminal.app and iTerm2 with
default settings: function keys, plain `ctrl+letter`, `ctrl+arrows`,
`shift+arrows`, and a `ctrl+k` chord for the rarer editor commands. (Alt
bindings are kept as aliases for terminals that send them.)

Global

| key | action |
|---|---|
| `ctrl+p` | command palette: type to filter every command, ↑↓ to choose, enter to run |
| `F1` | key reference |
| `F2` `F3` `F4` | explorer / source control / tests (and focus left) |
| `F5` | debug: launch the selected target, or continue when stopped |
| `F9` | toggle a breakpoint on the cursor line |
| `F10` / `F11` / `F12` | step over / into / out |
| `ctrl+k d` / `ctrl+k x` | show the debug pane / stop the session |
| `ctrl+k t` | run all tests |
| `F6` | hide / show left pane |
| `F7` / `F8` | previous / next tab (also `ctrl+pgup` / `ctrl+pgdn`) |
| `ctrl+k p` | pin / unpin the active tab |
| `ctrl+b` | switch focus between left and right pane |
| `ctrl+t` | new terminal tab (opens pinned) |
| `ctrl+w` | close tab — on a pinned tab just moves to the next one |
| `ctrl+o` / `ctrl+n` | open a path / new empty buffer |
| `ctrl+q` | quit (asks if something is unsaved) |

Inside a terminal tab only the function keys, `ctrl+b`, `ctrl+t`, `ctrl+w`,
`ctrl+q` and `ctrl+pgup/pgdn` are intercepted; everything else reaches the shell.

Left pane

| key | action |
|---|---|
| `↑↓` `pgup/pgdn` `home/end` | move |
| `enter` / `space` | open file · toggle folder · open diff · run test |
| `←` `→` | collapse (or go to parent) / expand |
| `e` / `E` | expand-collapse selected / all (also `ctrl+k a`) |
| `d` | diff the selected file (explorer) |
| `o` | open the file (source control, tests) |
| `a` | run all tests |
| `r` | refresh |
| `esc` | focus the right pane |

Editor

| key | action |
|---|---|
| `ctrl+←/→` | word skip (`shift+arrows` selects; `esc-b`/`esc-f` also work) |
| `ctrl+↑/↓` | move line or selection up / down |
| `ctrl+k ↑/↓` | add a cursor above / below |
| `ctrl+e` | a cursor at the end of every selected line (or every line) |
| `ctrl+d` | select word, then add cursor at next match |
| `esc` | collapse to one cursor, then focus left |
| `ctrl+s` / `ctrl+k s` | save / save as |
| `ctrl+z` / `ctrl+y` | undo / redo |
| `ctrl+c` `ctrl+x` `ctrl+v` | copy / cut / paste (per cursor; whole line when nothing is selected) |
| `ctrl+f` / `ctrl+g` / `ctrl+l` | find / find next / go to line |
| `ctrl+k c` | toggle line comment |
| `ctrl+k k` | delete line |
| `tab` / `shift+tab` | indent / outdent |

Debug pane: `enter` launches a target, selects a frame, expands a variable or
jumps to a breakpoint; `c` `n` `s` `o` continue / step over / into / out;
`p` pause, `x` stop, `d` or `delete` removes a breakpoint.

Format: `ctrl+k f` runs the formatter for the file's language over the buffer
and applies the result as one undo step, keeping cursors where they were:
`rustfmt` (edition from Cargo.toml), `ruff format` (or `uvx ruff`, or `black`),
`shfmt`, `taplo fmt`, `xmllint --format`, built-in JSON pretty-printing, and a
whitespace tidy for Markdown and INI. A missing tool is reported by name.

Problems: `ctrl+k e` shows the problems pane (`enter` jumps to a diagnostic,
`r` re-runs `cargo check`, `o` opens the compiler's full rendered output in a
tab). `ctrl+k ]` / `ctrl+k [` jump to the next / previous diagnostic in the
current file.

Preview: `ctrl+k v` switches a Markdown, JSON, TOML or XML tab between the
editor and a rendered view (formatted text for Markdown, an indented tree for
the data formats; ◫ marks a tab in preview). Arrows, pgup/pgdn, home/end
scroll; `ctrl+k v` again returns to editing with the cursor where it was.

Diff tab: `n`/`p` next / previous hunk, `enter` opens the file at that line.

If your terminal sends them, `ctrl+shift+↑/↓` or `alt+shift+↑/↓` also add
cursors, `alt+↑/↓` moves lines, `ctrl+shift+enter` places line-end cursors and
`alt+1/2/3` switch left-pane modes. On macOS these need "Option as Esc+/Meta"
in the terminal profile.

## Design notes

- **Highlighting** is tree-sitter (Rust, Python, shell, JSON, TOML, XML, INI
  and Markdown, the last with its inline grammar injected for emphasis and
  links). Shell scripts are recognised by extension (`.sh .bash .zsh .ksh`,
  rc files) or by a `#!` line naming any `*sh`; the shebang also catches
  Python scripts without an extension.
  with the syntax tree kept per buffer and edited incrementally; the highlight query only runs over the lines
  on screen, so cost per keystroke does not grow with file size.
- **Undo** is an operation log, not buffer snapshots.
- **Terminal** tabs are real PTYs (`portable-pty`) with a `vt100` screen model.
- **Watching**: a native filesystem watcher (`notify`) on the root, debounced
  150 ms, keeps the explorer, git status, source-control list, open diffs and
  test discovery in step with whatever happens outside: `git checkout`, a
  build, another editor. Open buffers with no unsaved edits reload in place
  (cursor and scroll kept). A buffer with unsaved edits is marked ⚠ instead;
  `ctrl+k r` reloads it from disk, `ctrl+s` asks before overwriting. Saving
  always checks the file's modification time first, so a newer file is never
  clobbered silently. A 3-second poll remains as a fallback.
- **In-source diagnostics** come from two layers. Tree-sitter parse errors
  (ERROR and MISSING nodes, found by walking only error-bearing subtrees) show
  as soon as typing pauses for 600 ms. `cargo check --all-targets` runs at
  startup, on every `.rs` save, and whenever a `.rs` file changes on disk;
  `ruff` (or `uvx ruff`, falling back to Python's own parser) runs on each
  `.py` save. Shell scripts go through `shellcheck` when it is installed
  (errors, warnings and info; style notes are dropped), otherwise the script's
  own shell runs `-n` and its syntax errors are mapped back. JSON, TOML and
  XML files are parsed with `serde_json`, `toml`
  and `roxmltree` on open and on save, so the first well-formedness error is
  marked in place. Both render as a coloured underline, a coloured line number, and
  the message after the line; the status bar shows ✗/▲ counts and the message
  for the cursor line.
- **Dependency health** in `Cargo.toml`: each dependency line shows, inline,
  the version resolved in `Cargo.lock` with ✓ when it is the newest
  non-prerelease on crates.io, ⬆ with the newer version when it is behind, ⚠
  when the resolved version has been yanked, and a dim note for path, git and
  workspace dependencies. Versions come from the crates.io sparse index via
  `curl`, fetched in the background when the file is opened and cached for an
  hour; the status bar summarises once every crate has answered.
- **Debugging** speaks the Debug Adapter Protocol to `lldb-dap` (found via
  `xcrun`, `PATH`, or `REDACT_LLDB_DAP`) for Rust and to `debugpy` (through
  `uv run --with debugpy`, or `python3 -m debugpy.adapter`) for Python. Rust
  targets come from `cargo metadata`; launching runs `cargo build` (or
  `cargo test --no-run`) with JSON messages to find the executable, loads the
  Rust pretty-printers from the toolchain's `lldb_commands`, and then launches.
  Breakpoints live in the editor gutter (●), the stopped line is marked ▶, and
  program and adapter output go to a `debug` output tab.
- **Tests** are discovered by scanning source (no compile), run with
  `cargo test <module::path> -- --exact` or `uv run pytest file::Class::name -v`,
  and results stream into a `tests` output tab while ✓/✗ marks update the tree.

## Roadmap

- find in files with multi-select results you can edit in place across files
- understanding `cargo` / `uv` as a build system (problems pane, jump to error)
- debugging view for Rust (lldb/codelldb) and Python (debugpy) that just works
- rope-backed buffer for very large files; mouse support
