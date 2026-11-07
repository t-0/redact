//! Minimal Debug Adapter Protocol client: JSON messages over the adapter's stdio.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, Sender, channel};

use serde_json::{Value, json};

pub enum DapMessage {
    Response {
        request_seq: i64,
        command: String,
        success: bool,
        message: Option<String>,
        body: Value,
    },
    Event {
        event: String,
        body: Value,
    },
    /// A line the adapter wrote to its stderr.
    Stderr(String),
    /// The adapter process went away.
    Closed,
}

pub struct DapClient {
    child: Child,
    stdin: ChildStdin,
    seq: i64,
    pub rx: Receiver<DapMessage>,
}

fn read_loop(mut reader: BufReader<impl Read>, tx: Sender<DapMessage>) {
    loop {
        // headers
        let mut len: Option<usize> = None;
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => {
                    let _ = tx.send(DapMessage::Closed);
                    return;
                }
                Ok(_) => {}
            }
            let t = line.trim_end();
            if t.is_empty() {
                break;
            }
            if let Some(v) = t.strip_prefix("Content-Length:") {
                len = v.trim().parse().ok();
            }
        }
        let Some(len) = len else { continue };
        let mut body = vec![0u8; len];
        if reader.read_exact(&mut body).is_err() {
            let _ = tx.send(DapMessage::Closed);
            return;
        }
        let Ok(v) = serde_json::from_slice::<Value>(&body) else {
            continue;
        };
        let msg = match v.get("type").and_then(|t| t.as_str()) {
            Some("response") => DapMessage::Response {
                request_seq: v.get("request_seq").and_then(|s| s.as_i64()).unwrap_or(0),
                command: v
                    .get("command")
                    .and_then(|c| c.as_str())
                    .unwrap_or("")
                    .to_string(),
                success: v.get("success").and_then(|s| s.as_bool()).unwrap_or(false),
                message: v
                    .get("message")
                    .and_then(|m| m.as_str())
                    .map(|s| s.to_string()),
                body: v.get("body").cloned().unwrap_or(Value::Null),
            },
            Some("event") => DapMessage::Event {
                event: v
                    .get("event")
                    .and_then(|e| e.as_str())
                    .unwrap_or("")
                    .to_string(),
                body: v.get("body").cloned().unwrap_or(Value::Null),
            },
            _ => continue,
        };
        if tx.send(msg).is_err() {
            return;
        }
    }
}

impl DapClient {
    pub fn spawn(program: &str, args: &[String], cwd: &Path) -> anyhow::Result<DapClient> {
        let mut cmd = Command::new(program);
        cmd.args(args)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // Detach the adapter (and so the debuggee) from our terminal: debugpy's
        // launcher otherwise makes the debuggee the terminal's foreground process
        // group and steals every keystroke from the editor.
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            unsafe {
                cmd.pre_exec(|| {
                    libc::setsid();
                    Ok(())
                });
            }
        }
        let mut child = cmd.spawn()?;
        let stdin = child.stdin.take().expect("stdin");
        let stdout = child.stdout.take().expect("stdout");
        let stderr = child.stderr.take().expect("stderr");
        let (tx, rx) = channel();
        {
            let tx = tx.clone();
            std::thread::spawn(move || read_loop(BufReader::new(stdout), tx));
        }
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if tx.send(DapMessage::Stderr(line)).is_err() {
                    break;
                }
            }
        });
        Ok(DapClient {
            child,
            stdin,
            seq: 0,
            rx,
        })
    }

    /// Send a request and return its sequence number for matching the response.
    pub fn request(&mut self, command: &str, arguments: Value) -> i64 {
        self.seq += 1;
        let msg =
            json!({"seq": self.seq, "type": "request", "command": command, "arguments": arguments});
        let body = msg.to_string();
        let _ = write!(self.stdin, "Content-Length: {}\r\n\r\n{}", body.len(), body);
        let _ = self.stdin.flush();
        self.seq
    }

    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for DapClient {
    fn drop(&mut self) {
        self.kill();
    }
}
