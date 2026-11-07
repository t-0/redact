//! A shell running in a pseudo-terminal, rendered as a tab in the right pane.

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use portable_pty::{CommandBuilder, MasterPty, PtySize};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

pub struct TerminalTab {
    pub title: String,
    parser: Arc<Mutex<vt100::Parser>>,
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    rows: u16,
    cols: u16,
    exited: Arc<Mutex<bool>>,
}

impl TerminalTab {
    pub fn spawn(
        cwd: &std::path::Path,
        title: String,
        rows: u16,
        cols: u16,
    ) -> anyhow::Result<TerminalTab> {
        let rows = rows.max(2);
        let cols = cols.max(10);
        let pty = portable_pty::native_pty_system();
        let pair = pty.openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
        let mut cmd = CommandBuilder::new(shell);
        cmd.arg("-l");
        cmd.cwd(cwd);
        cmd.env("TERM", "xterm-256color");
        let mut child = pair.slave.spawn_command(cmd)?;
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;
        let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, 2000)));
        let exited = Arc::new(Mutex::new(false));
        {
            let parser = parser.clone();
            let exited = exited.clone();
            std::thread::spawn(move || {
                let mut buf = [0u8; 8192];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => parser.lock().unwrap().process(&buf[..n]),
                    }
                }
                let _ = child.wait();
                *exited.lock().unwrap() = true;
            });
        }
        Ok(TerminalTab {
            title,
            parser,
            master: pair.master,
            writer,
            rows,
            cols,
            exited,
        })
    }

    pub fn exited(&self) -> bool {
        *self.exited.lock().unwrap()
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        let rows = rows.max(2);
        let cols = cols.max(10);
        if rows == self.rows && cols == self.cols {
            return;
        }
        self.rows = rows;
        self.cols = cols;
        let _ = self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });
        self.parser.lock().unwrap().set_size(rows, cols);
    }

    pub fn write(&mut self, bytes: &[u8]) {
        let _ = self.writer.write_all(bytes);
        let _ = self.writer.flush();
    }

    /// Translate a key press into the bytes a terminal would send.
    pub fn send_key(&mut self, key: KeyEvent) {
        let app_cursor = self.parser.lock().unwrap().screen().application_cursor();
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let mut out: Vec<u8> = Vec::new();
        let arrow = |c: char, out: &mut Vec<u8>| {
            if shift || ctrl || alt {
                let m = 1 + (shift as u8) + (alt as u8) * 2 + (ctrl as u8) * 4;
                out.extend_from_slice(format!("\x1b[1;{}{}", m, c).as_bytes());
            } else if app_cursor {
                out.extend_from_slice(format!("\x1bO{}", c).as_bytes());
            } else {
                out.extend_from_slice(format!("\x1b[{}", c).as_bytes());
            }
        };
        match key.code {
            KeyCode::Char(c) => {
                if ctrl {
                    let b = c.to_ascii_lowercase() as u8;
                    if b.is_ascii_lowercase() {
                        out.push(b - b'a' + 1);
                    } else {
                        match c {
                            '[' | '3' => out.push(0x1b),
                            '\\' | '4' => out.push(0x1c),
                            ']' | '5' => out.push(0x1d),
                            '^' | '6' => out.push(0x1e),
                            '_' | '7' | '/' => out.push(0x1f),
                            ' ' | '2' | '@' => out.push(0),
                            _ => {}
                        }
                    }
                } else {
                    if alt {
                        out.push(0x1b);
                    }
                    let mut b = [0u8; 4];
                    out.extend_from_slice(c.encode_utf8(&mut b).as_bytes());
                }
            }
            KeyCode::Enter => out.push(b'\r'),
            KeyCode::Backspace => {
                if alt {
                    out.push(0x1b);
                }
                out.push(0x7f)
            }
            KeyCode::Tab => out.push(b'\t'),
            KeyCode::BackTab => out.extend_from_slice(b"\x1b[Z"),
            KeyCode::Esc => out.push(0x1b),
            KeyCode::Up => arrow('A', &mut out),
            KeyCode::Down => arrow('B', &mut out),
            KeyCode::Right => arrow('C', &mut out),
            KeyCode::Left => arrow('D', &mut out),
            KeyCode::Home => arrow('H', &mut out),
            KeyCode::End => arrow('F', &mut out),
            KeyCode::PageUp => out.extend_from_slice(b"\x1b[5~"),
            KeyCode::PageDown => out.extend_from_slice(b"\x1b[6~"),
            KeyCode::Insert => out.extend_from_slice(b"\x1b[2~"),
            KeyCode::Delete => out.extend_from_slice(b"\x1b[3~"),
            KeyCode::F(n) => {
                let seq: &[u8] = match n {
                    1 => b"\x1bOP",
                    2 => b"\x1bOQ",
                    3 => b"\x1bOR",
                    4 => b"\x1bOS",
                    5 => b"\x1b[15~",
                    6 => b"\x1b[17~",
                    7 => b"\x1b[18~",
                    8 => b"\x1b[19~",
                    9 => b"\x1b[20~",
                    10 => b"\x1b[21~",
                    11 => b"\x1b[23~",
                    12 => b"\x1b[24~",
                    _ => b"",
                };
                out.extend_from_slice(seq);
            }
            _ => {}
        }
        if !out.is_empty() {
            self.write(&out);
        }
    }

    /// Draw the screen contents into `area` and return the cursor position, if visible.
    pub fn render(&self, area: Rect, buf: &mut Buffer) -> Option<(u16, u16)> {
        let parser = self.parser.lock().unwrap();
        let screen = parser.screen();
        for row in 0..area.height.min(self.rows) {
            for col in 0..area.width.min(self.cols) {
                let Some(cell) = screen.cell(row, col) else {
                    continue;
                };
                let x = area.x + col;
                let y = area.y + row;
                let mut style = Style::default()
                    .fg(conv(cell.fgcolor(), true))
                    .bg(conv(cell.bgcolor(), false));
                if cell.bold() {
                    style = style.add_modifier(Modifier::BOLD);
                }
                if cell.italic() {
                    style = style.add_modifier(Modifier::ITALIC);
                }
                if cell.underline() {
                    style = style.add_modifier(Modifier::UNDERLINED);
                }
                if cell.inverse() {
                    style = style.add_modifier(Modifier::REVERSED);
                }
                let contents = cell.contents();
                let c = &mut buf[(x, y)];
                if contents.is_empty() {
                    c.set_symbol(" ");
                } else if cell.is_wide_continuation() {
                    continue;
                } else {
                    c.set_symbol(&contents);
                }
                c.set_style(style);
            }
        }
        if screen.hide_cursor() {
            return None;
        }
        let (r, c) = screen.cursor_position();
        if r < area.height && c < area.width {
            Some((area.x + c, area.y + r))
        } else {
            None
        }
    }
}

fn conv(c: vt100::Color, fg: bool) -> Color {
    match c {
        vt100::Color::Default => {
            if fg {
                crate::theme::fg()
            } else {
                crate::theme::bg()
            }
        }
        vt100::Color::Idx(i) => crate::theme::adjust(Color::Indexed(i)),
        vt100::Color::Rgb(r, g, b) => crate::theme::adjust(Color::Rgb(r, g, b)),
    }
}
