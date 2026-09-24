//! JSON-RPC over stdio to one language server process.
//!
//! Three helper threads (stdout reader, stdin writer, stderr tail) keep every blocking pipe
//! operation off the UI thread. The UI drains [`Client::try_recv`] on its tick. Anything the
//! server sends that doesn't parse is reported as an [`Incoming::Error`] and dropped; a
//! misbehaving server can never panic the editor.
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Largest message accepted from a server. rust-analyzer's biggest payloads are a few MiB.
const MAX_MESSAGE: usize = 64 * 1024 * 1024;
const STDERR_LINES: usize = 30;

#[allow(dead_code)] // `result` and the error text are read once hover/definition arrive.
pub enum Incoming {
    Notification { method: String, params: Value },
    Response { id: i64, result: Option<Value>, error: Option<Value> },
    /// A request from the server; answer it with [`Client::respond`].
    Request { id: Value, method: String, params: Value },
    Error(String),
    /// The process ended or its stdout closed. No more messages will arrive.
    Exited(String),
}

pub struct Client {
    child: Child,
    writer: Sender<String>,
    incoming: Receiver<Incoming>,
    next_id: i64,
    stderr: Arc<Mutex<VecDeque<String>>>,
    exited: bool,
}

impl Client {
    pub fn spawn(binary: &Path, args: &[&str], cwd: &Path) -> Result<Self, String> {
        let mut command = Command::new(binary);
        command.args(args).current_dir(cwd).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        crate::updater::hide_console(&mut command);
        let mut child = command.spawn().map_err(|e| format!("could not start {}: {e}", binary.display()))?;
        let stdin = child.stdin.take().ok_or("no stdin")?;
        let stdout = child.stdout.take().ok_or("no stdout")?;
        let stderr_pipe = child.stderr.take().ok_or("no stderr")?;

        let (incoming_tx, incoming) = mpsc::channel();
        let reader_tx = incoming_tx.clone();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                match read_message(&mut reader) {
                    Ok(Some(message)) => {
                        if reader_tx.send(classify(message)).is_err() { break; }
                    }
                    Ok(None) => {
                        let _ = reader_tx.send(Incoming::Exited("stdout closed".into()));
                        break;
                    }
                    Err(err) => {
                        if reader_tx.send(Incoming::Error(err)).is_err() { break; }
                    }
                }
            }
        });

        let (writer, outgoing) = mpsc::channel::<String>();
        std::thread::spawn(move || {
            let mut stdin = stdin;
            for body in outgoing {
                if write!(stdin, "Content-Length: {}\r\n\r\n{body}", body.len()).and_then(|_| stdin.flush()).is_err() {
                    let _ = incoming_tx.send(Incoming::Exited("stdin closed".into()));
                    break;
                }
            }
        });

        // Servers block once their stderr pipe fills, so it must always be drained.
        let stderr = Arc::new(Mutex::new(VecDeque::new()));
        let tail = stderr.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr_pipe).lines().map_while(Result::ok) {
                let mut tail = tail.lock().unwrap_or_else(|e| e.into_inner());
                if tail.len() == STDERR_LINES { tail.pop_front(); }
                tail.push_back(line);
            }
        });

        Ok(Self { child, writer, incoming, next_id: 0, stderr, exited: false })
    }

    pub fn request(&mut self, method: &str, params: Value) -> i64 {
        self.next_id += 1;
        let id = self.next_id;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        id
    }

    pub fn notify(&self, method: &str, params: Value) {
        self.send(json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }

    pub fn respond(&self, id: Value, result: Value) {
        self.send(json!({"jsonrpc": "2.0", "id": id, "result": result}));
    }

    pub fn respond_error(&self, id: Value, code: i64, message: &str) {
        self.send(json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}));
    }

    fn send(&self, message: Value) {
        let _ = self.writer.send(message.to_string());
    }

    pub fn try_recv(&mut self) -> Option<Incoming> {
        if self.exited { return None; }
        match self.incoming.try_recv() {
            Ok(Incoming::Exited(reason)) => {
                self.exited = true;
                Some(Incoming::Exited(reason))
            }
            Ok(message) => Some(message),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                self.exited = true;
                Some(Incoming::Exited("reader stopped".into()))
            }
        }
    }

    /// The last lines the server wrote to stderr, for crash notices.
    pub fn stderr_tail(&self) -> String {
        let tail = self.stderr.lock().unwrap_or_else(|e| e.into_inner());
        tail.iter().cloned().collect::<Vec<_>>().join("\n")
    }

    /// Polite `shutdown`/`exit`, then a kill if the process lingers.
    pub fn shutdown(mut self) {
        if !self.exited {
            self.request("shutdown", Value::Null);
            self.notify("exit", Value::Null);
            let deadline = Instant::now() + Duration::from_millis(500);
            while Instant::now() < deadline {
                if matches!(self.child.try_wait(), Ok(Some(_))) { return; }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// One framed message, or `None` at a clean EOF.
fn read_message(reader: &mut impl BufRead) -> Result<Option<Value>, String> {
    let mut length: Option<usize> = None;
    loop {
        let mut line = String::new();
        let read = reader.read_line(&mut line).map_err(|e| e.to_string())?;
        if read == 0 { return Ok(None); }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            if length.is_some() { break; }
            continue;
        }
        if let Some(value) = line.strip_prefix("Content-Length:") {
            length = Some(value.trim().parse().map_err(|_| format!("bad Content-Length: {value}"))?);
        }
    }
    let length = length.ok_or("message without Content-Length")?;
    if length > MAX_MESSAGE { return Err(format!("message of {length} bytes exceeds the limit")); }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).map_err(|e| e.to_string())?;
    serde_json::from_slice(&body).map(Some).map_err(|e| format!("invalid JSON from server: {e}"))
}

fn classify(message: Value) -> Incoming {
    let method = message.get("method").and_then(Value::as_str).map(str::to_string);
    let id = message.get("id").cloned().filter(|id| !id.is_null());
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    match (method, id) {
        (Some(method), Some(id)) => Incoming::Request { id, method, params },
        (Some(method), None) => Incoming::Notification { method, params },
        (None, Some(id)) => match id.as_i64() {
            Some(id) => Incoming::Response {
                id,
                result: message.get("result").cloned(),
                error: message.get("error").cloned(),
            },
            None => Incoming::Error(format!("response with non-numeric id {id}")),
        },
        (None, None) => Incoming::Error("message with neither method nor id".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_are_parsed_and_classified() {
        let body = r#"{"jsonrpc":"2.0","id":3,"result":{"ok":true}}"#;
        let input = format!("Content-Length: {}\r\nContent-Type: application/vscode-jsonrpc\r\n\r\n{body}", body.len());
        let mut reader = std::io::Cursor::new(input.into_bytes());
        let message = read_message(&mut reader).unwrap().unwrap();
        assert!(matches!(classify(message), Incoming::Response { id: 3, .. }));
        assert!(read_message(&mut reader).unwrap().is_none(), "clean EOF");

        let notification = json!({"jsonrpc": "2.0", "method": "textDocument/publishDiagnostics", "params": {"uri": "x"}});
        assert!(matches!(classify(notification), Incoming::Notification { method, .. } if method == "textDocument/publishDiagnostics"));
        let request = json!({"jsonrpc": "2.0", "id": "r1", "method": "workspace/configuration"});
        assert!(matches!(classify(request), Incoming::Request { .. }));

        let huge = format!("Content-Length: {}\r\n\r\n", MAX_MESSAGE + 1);
        assert!(read_message(&mut std::io::Cursor::new(huge.into_bytes())).is_err());
        let garbage = "Content-Length: 3\r\n\r\n{{{";
        assert!(read_message(&mut std::io::Cursor::new(garbage.as_bytes().to_vec())).is_err());
    }
}
