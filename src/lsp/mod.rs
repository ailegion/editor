//! Language Server Protocol support: one server process per language per workspace,
//! document sync from the open tabs, and diagnostics back into them.
//!
//! Everything here runs on the UI thread and never blocks: process IO lives on the threads
//! inside [`client::Client`], installs run on their own thread, and [`Manager::poll`] drains
//! what arrived since the last tick. A server that crashes is restarted with backoff and
//! given up on after three crashes, so a broken server costs a notice, never the editor.
pub mod client;
pub mod install;
pub mod registry;

use client::{Client, Incoming};
use registry::Server;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

/// Pause after the last keystroke before the server sees the new text.
pub const CHANGE_DEBOUNCE: Duration = Duration::from_millis(150);
const MAX_CRASHES: u32 = 3;
const MAX_DIAGNOSTICS: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity { Hint, Information, Warning, Error }

/// One diagnostic. Columns are UTF-16 code units as LSP sends them; see [`utf16_to_byte`].
#[derive(Debug, Clone, PartialEq)]
pub struct Diagnostic {
    pub line: usize,
    pub start: usize,
    pub end_line: usize,
    pub end: usize,
    pub severity: Severity,
    pub message: String,
}

pub enum Event {
    Diagnostics(PathBuf, Vec<Diagnostic>),
    Notice(String),
    /// A file needs `server`, which isn't installed. Shown once per server per session.
    Prompt(&'static Server),
    /// Hover text for a position (UTF-16 `column`); `lines` is empty when there was none.
    Hover { path: PathBuf, line: usize, column: usize, lines: Vec<String> },
    /// Where a definition request landed (UTF-16 `column`).
    Definition { path: PathBuf, line: usize, column: usize },
}

struct Doc { server: &'static str, language: &'static str, version: i64, text: String }

/// What an outstanding request will produce when its response arrives.
enum Pending {
    Hover { path: PathBuf, line: usize, column: usize },
    Definition,
}
struct InFlight { kind: Pending, method: &'static str, params: Value, sent: Instant, attempts: u8 }

/// Responses older than this are dropped. Generous because rust-analyzer answers nothing
/// until its initial indexing is done; the UI decides whether a late answer is still wanted.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
/// `ContentModified`: the server discarded the request because the document changed under
/// it, and expects the client to ask again (VS Code's client does the same).
const CONTENT_MODIFIED: i64 = -32801;
const RETRY_DELAY: Duration = Duration::from_millis(250);
const MAX_ATTEMPTS: u8 = 8;
const HOVER_COLUMNS: usize = 96;
const HOVER_ROWS: usize = 16;

struct Slot {
    client: Option<Client>,
    init_id: Option<i64>,
    initialized: bool,
    crashes: u32,
    retry_at: Option<Instant>,
    disabled: bool,
}

#[derive(Default)]
pub struct Manager {
    root: Option<PathBuf>,
    slots: HashMap<&'static str, Slot>,
    docs: HashMap<PathBuf, Doc>,
    /// Documents edited since their last `didChange`, with the moment to send it.
    due: HashMap<PathBuf, Instant>,
    prompted: HashSet<&'static str>,
    installing: HashMap<&'static str, Receiver<Result<PathBuf, String>>>,
    /// Explicit binaries that beat [`Server::locate`], e.g. a freshly installed one.
    binaries: HashMap<&'static str, PathBuf>,
    /// Requests awaiting a response, keyed by server and request id.
    pending: HashMap<(&'static str, i64), InFlight>,
    /// Requests the server asked us to repeat, and when to do so.
    retries: Vec<(&'static str, InFlight, Instant)>,
    events: Vec<Event>,
}

impl Manager {
    /// Servers are per workspace: a new root shuts down the old ones.
    pub fn set_root(&mut self, root: &Path) {
        if self.root.as_deref() == Some(root) { return; }
        self.root = Some(root.to_path_buf());
        for (_, slot) in self.slots.drain() {
            if let Some(client) = slot.client { client.shutdown(); }
        }
        self.docs.clear();
        self.due.clear();
    }

    /// A tab opened `path`. Starts the language's server if needed, or asks to install it.
    pub fn open(&mut self, path: &Path, text: String) {
        let Some(language) = registry::language_for(path) else { return };
        let Some(server) = registry::server_for(language) else { return };
        self.docs.insert(path.to_path_buf(), Doc { server: server.id, language, version: 1, text });
        let started = self.slots.contains_key(server.id) || self.start(server);
        if !started {
            if self.installing.contains_key(server.id) || !self.prompted.insert(server.id) { return; }
            self.events.push(Event::Prompt(server));
            return;
        }
        let Some(slot) = self.slots.get(server.id) else { return };
        if let (Some(client), true) = (&slot.client, slot.initialized) {
            let doc = &self.docs[path];
            client.notify("textDocument/didOpen", json!({"textDocument": {
                "uri": path_to_uri(path), "languageId": doc.language, "version": doc.version, "text": doc.text,
            }}));
        }
    }

    /// The buffer changed; the text is fetched when the debounce elapses (see [`Self::take_due`]).
    pub fn changed(&mut self, path: &Path) {
        if self.docs.contains_key(path) {
            self.due.insert(path.to_path_buf(), Instant::now() + CHANGE_DEBOUNCE);
        }
    }

    /// Documents whose debounce elapsed; feed each one's current text to [`Self::change`].
    pub fn take_due(&mut self) -> Vec<PathBuf> {
        let now = Instant::now();
        let ready: Vec<PathBuf> = self.due.iter().filter(|(_, at)| **at <= now).map(|(path, _)| path.clone()).collect();
        for path in &ready { self.due.remove(path); }
        ready
    }

    pub fn change(&mut self, path: &Path, text: String) {
        let Some(doc) = self.docs.get_mut(path) else { return };
        if doc.text == text { return; }
        doc.text = text;
        doc.version += 1;
        let (server, version) = (doc.server, doc.version);
        let doc = &self.docs[path];
        if let Some(client) = self.ready_client(server) {
            // A change without a range replaces the whole document, which every server accepts.
            client.notify("textDocument/didChange", json!({
                "textDocument": {"uri": path_to_uri(path), "version": version},
                "contentChanges": [{"text": doc.text}],
            }));
        }
    }

    pub fn saved(&mut self, path: &Path, text: String) {
        self.due.remove(path);
        self.change(path, text);
        let Some(doc) = self.docs.get(path) else { return };
        if let Some(client) = self.ready_client(doc.server) {
            client.notify("textDocument/didSave", json!({"textDocument": {"uri": path_to_uri(path)}, "text": doc.text}));
        }
    }

    pub fn closed(&mut self, path: &Path) {
        self.due.remove(path);
        let Some(doc) = self.docs.remove(path) else { return };
        if let Some(client) = self.ready_client(doc.server) {
            client.notify("textDocument/didClose", json!({"textDocument": {"uri": path_to_uri(path)}}));
        }
    }

    /// "Not now" on the install prompt: don't ask again this session.
    pub fn dismiss(&mut self, server: &'static Server) {
        self.prompted.insert(server.id);
    }

    /// Use `binary` for `server` instead of searching for it.
    #[cfg_attr(not(test), allow(dead_code))] // Reserved for a per-server path setting.
    pub fn set_binary(&mut self, server: &'static Server, binary: PathBuf) {
        self.binaries.insert(server.id, binary);
    }

    /// Asks what's at (`line`, UTF-16 `column`); answered by [`Event::Hover`].
    pub fn hover(&mut self, path: &Path, line: usize, column: usize) {
        self.send_request(path, "textDocument/hover", line, column, Pending::Hover { path: path.to_path_buf(), line, column });
    }

    /// Asks where the symbol at (`line`, UTF-16 `column`) is defined; answered by
    /// [`Event::Definition`], or a notice when there is none.
    pub fn definition(&mut self, path: &Path, line: usize, column: usize) {
        self.send_request(path, "textDocument/definition", line, column, Pending::Definition);
    }

    fn send_request(&mut self, path: &Path, method: &'static str, line: usize, column: usize, kind: Pending) {
        let Some(server) = self.docs.get(path).map(|doc| doc.server) else { return };
        // One outstanding request of each kind per server: the previous one is obsolete.
        let stale: Vec<i64> = self.pending.iter()
            .filter(|((s, _), inflight)| *s == server && std::mem::discriminant(&inflight.kind) == std::mem::discriminant(&kind))
            .map(|((_, id), _)| *id).collect();
        for id in stale {
            self.pending.remove(&(server, id));
            if let Some(client) = self.ready_client(server) { client.notify("$/cancelRequest", json!({"id": id})); }
        }
        self.retries.retain(|(s, inflight, _)| *s != server || std::mem::discriminant(&inflight.kind) != std::mem::discriminant(&kind));
        let params = json!({
            "textDocument": {"uri": path_to_uri(path)},
            "position": {"line": line, "character": column},
        });
        self.dispatch(server, InFlight { kind, method, params, sent: Instant::now(), attempts: 0 });
    }

    /// Sends `inflight` to `server` (initial attempt or a retry) and tracks its id.
    fn dispatch(&mut self, server: &'static str, mut inflight: InFlight) {
        let Some(slot) = self.slots.get_mut(server) else { return };
        if !slot.initialized { return }
        let Some(client) = slot.client.as_mut() else { return };
        inflight.attempts += 1;
        let id = client.request(inflight.method, inflight.params.clone());
        self.pending.insert((server, id), inflight);
    }

    pub fn install(&mut self, server: &'static Server) {
        if self.installing.contains_key(server.id) { return; }
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || { let _ = tx.send(install::install(server)); });
        self.installing.insert(server.id, rx);
        self.events.push(Event::Notice(format!("Installing {}…", server.name)));
    }

    pub fn shutdown_all(&mut self) {
        for (_, slot) in self.slots.drain() {
            if let Some(client) = slot.client { client.shutdown(); }
        }
    }

    /// Drains server output, restarts crashed servers, and finishes installs.
    pub fn poll(&mut self) -> Vec<Event> {
        let ids: Vec<&'static str> = self.slots.keys().copied().collect();
        for id in ids {
            self.poll_server(id);
        }
        let now = Instant::now();
        self.pending.retain(|_, inflight| now.duration_since(inflight.sent) < REQUEST_TIMEOUT);
        let due: Vec<(&'static str, InFlight)> = {
            let (ready, waiting): (Vec<_>, Vec<_>) = self.retries.drain(..).partition(|(_, _, at)| *at <= now);
            self.retries = waiting;
            ready.into_iter().map(|(server, inflight, _)| (server, inflight)).collect()
        };
        for (server, inflight) in due {
            self.dispatch(server, inflight);
        }
        let finished: Vec<(&'static str, Result<PathBuf, String>)> = self.installing.iter()
            .filter_map(|(id, rx)| match rx.try_recv() {
                Ok(result) => Some((*id, result)),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => Some((*id, Err("install thread stopped".into()))),
            }).collect();
        for (id, result) in finished {
            self.installing.remove(id);
            let Some(server) = registry::by_id(id) else { continue };
            match result {
                Ok(path) => {
                    self.events.push(Event::Notice(format!("Installed {} to {}", server.name, path.display())));
                    self.binaries.insert(id, path);
                    self.slots.remove(id);
                    self.start(server);
                }
                Err(err) => self.events.push(Event::Notice(format!("Could not install {}: {err}", server.name))),
            }
        }
        std::mem::take(&mut self.events)
    }

    fn ready_client(&self, server: &str) -> Option<&Client> {
        let slot = self.slots.get(server)?;
        if slot.initialized { slot.client.as_ref() } else { None }
    }

    /// Spawns `server` and sends `initialize`. `false` when its binary can't be found.
    fn start(&mut self, server: &'static Server) -> bool {
        let Some(root) = self.root.clone() else { return false };
        let Some(binary) = self.binaries.get(server.id).cloned().or_else(|| server.locate()) else { return false };
        let crashes = self.slots.get(server.id).map_or(0, |slot| slot.crashes);
        let mut client = match Client::spawn(&binary, server.args, &root) {
            Ok(client) => client,
            Err(err) => {
                self.events.push(Event::Notice(format!("{}: {err}", server.name)));
                return false;
            }
        };
        let init_id = client.request("initialize", json!({
            "processId": std::process::id(),
            "clientInfo": {"name": "editor", "version": env!("CARGO_PKG_VERSION")},
            "rootUri": path_to_uri(&root),
            "workspaceFolders": [{"uri": path_to_uri(&root), "name": root.file_name().map(|n| n.to_string_lossy()).unwrap_or_default()}],
            "capabilities": {
                "textDocument": {
                    "synchronization": {"didSave": true},
                    "publishDiagnostics": {"relatedInformation": false},
                },
                "workspace": {"configuration": false, "workspaceFolders": true},
                "window": {"workDoneProgress": true},
            },
        }));
        self.slots.insert(server.id, Slot { client: Some(client), init_id: Some(init_id), initialized: false, crashes, retry_at: None, disabled: false });
        true
    }

    fn poll_server(&mut self, id: &'static str) {
        let Some(server) = registry::by_id(id) else { return };
        let slot = self.slots.get_mut(id).expect("slot exists");
        if slot.client.is_none() {
            if slot.retry_at.is_some_and(|at| at <= Instant::now()) {
                slot.retry_at = None;
                self.start(server);
            }
            return;
        }
        let mut exited = None;
        let mut initialized_now = false;
        {
            let client = slot.client.as_mut().expect("checked above");
            // Bounded per tick so a chatty server can't hold the frame.
            for _ in 0..256 {
                let Some(message) = client.try_recv() else { break };
                match message {
                    Incoming::Response { id, error, .. } if Some(id) == slot.init_id => {
                        slot.init_id = None;
                        if let Some(error) = error {
                            exited = Some(format!("initialize failed: {error}"));
                            break;
                        }
                        client.notify("initialized", json!({}));
                        slot.initialized = true;
                        initialized_now = true;
                    }
                    Incoming::Response { id: request_id, result, error } => {
                        let Some(inflight) = self.pending.remove(&(id, request_id)) else { continue };
                        if let Some(error) = error {
                            let content_modified = error.get("code").and_then(Value::as_i64) == Some(CONTENT_MODIFIED);
                            if content_modified && inflight.attempts < MAX_ATTEMPTS {
                                self.retries.push((id, inflight, Instant::now() + RETRY_DELAY));
                                continue;
                            }
                            // Only a failed jump is worth telling the user about; a hover that
                            // never came just doesn't appear, and ContentModified is routine.
                            if matches!(inflight.kind, Pending::Definition) && !content_modified {
                                let message = error.get("message").and_then(Value::as_str).unwrap_or("request failed");
                                self.events.push(Event::Notice(format!("{}: {message}", server.name)));
                            }
                            continue;
                        }
                        match inflight.kind {
                            Pending::Hover { path, line, column } => {
                                let lines = result.as_ref().and_then(|r| r.get("contents")).map(hover_lines).unwrap_or_default();
                                self.events.push(Event::Hover { path, line, column, lines });
                            }
                            Pending::Definition => match result.as_ref().and_then(first_location) {
                                Some((path, line, column)) => self.events.push(Event::Definition { path, line, column }),
                                None => self.events.push(Event::Notice("No definition found".into())),
                            },
                        }
                    }
                    Incoming::Notification { method, params } => match method.as_str() {
                        "textDocument/publishDiagnostics" => {
                            if let Some((path, diagnostics)) = parse_diagnostics(&params) {
                                self.events.push(Event::Diagnostics(path, diagnostics));
                            }
                        }
                        "window/showMessage" if params.get("type").and_then(Value::as_i64) == Some(1) => {
                            let text = params.get("message").and_then(Value::as_str).unwrap_or_default();
                            self.events.push(Event::Notice(format!("{}: {text}", server.name)));
                        }
                        _ => {}
                    },
                    Incoming::Request { id, method, params } => match method.as_str() {
                        // Empty settings for each requested item; servers then use their defaults.
                        "workspace/configuration" => {
                            let count = params.get("items").and_then(Value::as_array).map_or(0, Vec::len);
                            client.respond(id, Value::Array(vec![Value::Null; count]));
                        }
                        "window/workDoneProgress/create" | "client/registerCapability" | "client/unregisterCapability"
                        | "window/showMessageRequest" | "workspace/semanticTokens/refresh" | "workspace/inlayHint/refresh"
                        | "workspace/codeLens/refresh" | "workspace/diagnostic/refresh" => client.respond(id, Value::Null),
                        "workspace/workspaceFolders" => {
                            let root = self.root.clone().unwrap_or_default();
                            client.respond(id, json!([{"uri": path_to_uri(&root), "name": root.file_name().map(|n| n.to_string_lossy()).unwrap_or_default()}]));
                        }
                        _ => client.respond_error(id, -32601, "method not supported"),
                    },
                    Incoming::Error(_) => {}
                    Incoming::Exited(reason) => {
                        exited = Some(reason);
                        break;
                    }
                }
            }
        }
        if initialized_now {
            let client = slot.client.as_ref().expect("still running");
            for (path, doc) in self.docs.iter().filter(|(_, doc)| doc.server == id) {
                client.notify("textDocument/didOpen", json!({"textDocument": {
                    "uri": path_to_uri(path), "languageId": doc.language, "version": doc.version, "text": doc.text,
                }}));
            }
        }
        if let Some(reason) = exited {
            let client = slot.client.take().expect("was running");
            let tail = client.stderr_tail();
            drop(client);
            let never_initialized = !slot.initialized && slot.crashes == 0;
            slot.initialized = false;
            slot.crashes += 1;
            self.pending.retain(|(server, _), _| *server != id);
            self.retries.retain(|(server, _, _)| *server != id);
            if never_initialized {
                // Found but unusable, e.g. rustup's proxy without the component installed.
                // Offer a managed install instead of retrying something that can't start.
                slot.disabled = true;
                self.events.push(Event::Notice(format!("{} exited before it initialized ({reason}).\n{tail}", server.name)));
                if self.prompted.insert(server.id) { self.events.push(Event::Prompt(server)); }
            } else if slot.crashes >= MAX_CRASHES {
                slot.disabled = true;
                self.events.push(Event::Notice(format!("{} stopped {} times ({reason}); disabled until restart.\n{tail}", server.name, slot.crashes)));
            } else {
                let delay = Duration::from_secs(1 << slot.crashes);
                slot.retry_at = Some(Instant::now() + delay);
                self.events.push(Event::Notice(format!("{} stopped ({reason}); restarting in {}s", server.name, delay.as_secs())));
            }
            // Its diagnostics are stale now.
            for (path, doc) in &self.docs {
                if doc.server == id { self.events.push(Event::Diagnostics(path.clone(), Vec::new())); }
            }
        }
    }
}

fn parse_diagnostics(params: &Value) -> Option<(PathBuf, Vec<Diagnostic>)> {
    let path = uri_to_path(params.get("uri")?.as_str()?)?;
    let position = |value: &Value| -> Option<(usize, usize)> {
        Some((value.get("line")?.as_u64()? as usize, value.get("character")?.as_u64()? as usize))
    };
    let mut diagnostics: Vec<Diagnostic> = params.get("diagnostics")?.as_array()?.iter().take(MAX_DIAGNOSTICS).filter_map(|item| {
        let range = item.get("range")?;
        let (line, start) = position(range.get("start")?)?;
        let (end_line, end) = position(range.get("end")?)?;
        let severity = match item.get("severity").and_then(Value::as_i64) {
            Some(2) => Severity::Warning,
            Some(3) => Severity::Information,
            Some(4) => Severity::Hint,
            _ => Severity::Error,
        };
        Some(Diagnostic { line, start, end_line, end, severity, message: item.get("message")?.as_str()?.to_string() })
    }).collect();
    // Most severe first, so the first diagnostic on a line is the one worth showing.
    diagnostics.sort_by(|a, b| a.line.cmp(&b.line).then(b.severity.cmp(&a.severity)));
    Some((path, diagnostics))
}

/// First target of a `textDocument/definition` result: a `Location`, an array of them, or
/// an array of `LocationLink`s.
fn first_location(result: &Value) -> Option<(PathBuf, usize, usize)> {
    let item = match result { Value::Array(items) => items.first()?, other => other };
    let (uri, range) = match item.get("targetUri") {
        Some(uri) => (uri, item.get("targetSelectionRange").or_else(|| item.get("targetRange"))?),
        None => (item.get("uri")?, item.get("range")?),
    };
    let start = range.get("start")?;
    Some((uri_to_path(uri.as_str()?)?, start.get("line")?.as_u64()? as usize, start.get("character")?.as_u64()? as usize))
}

/// Plain-text lines from a hover's `contents` (`MarkupContent`, `MarkedString`, or arrays
/// of either): code fences dropped, blank runs collapsed, long lines word-wrapped to
/// [`HOVER_COLUMNS`], and the whole thing cut at [`HOVER_ROWS`].
fn hover_lines(contents: &Value) -> Vec<String> {
    fn collect(value: &Value, out: &mut String) {
        match value {
            Value::String(text) => { out.push_str(text); out.push('\n'); }
            Value::Array(items) => items.iter().for_each(|item| collect(item, out)),
            Value::Object(map) => if let Some(Value::String(text)) = map.get("value") { out.push_str(text); out.push('\n'); },
            _ => {}
        }
    }
    let mut raw = String::new();
    collect(contents, &mut raw);
    let mut lines: Vec<String> = Vec::new();
    for line in raw.lines() {
        let line = line.trim_end();
        if line.trim_start().starts_with("```") { continue; }
        if line.is_empty() {
            if lines.last().is_some_and(|last| !last.is_empty()) { lines.push(String::new()); }
            continue;
        }
        let indent = &line[..line.len() - line.trim_start().len()];
        let mut current = indent.to_string();
        for word in line.trim_start().split(' ') {
            if current.len() > indent.len() && current.chars().count() + 1 + word.chars().count() > HOVER_COLUMNS {
                lines.push(std::mem::replace(&mut current, indent.to_string()));
            }
            if current.len() > indent.len() { current.push(' '); }
            current.push_str(word);
        }
        while current.chars().count() > HOVER_COLUMNS {
            let rest: String = current.chars().skip(HOVER_COLUMNS).collect();
            current.truncate(current.char_indices().nth(HOVER_COLUMNS).map_or(current.len(), |(i, _)| i));
            lines.push(std::mem::replace(&mut current, rest));
        }
        lines.push(current);
    }
    while lines.last().is_some_and(String::is_empty) { lines.pop(); }
    if lines.len() > HOVER_ROWS {
        lines.truncate(HOVER_ROWS);
        lines.push("…".into());
    }
    lines
}

/// UTF-16 column of byte offset `byte` in `text`.
pub fn byte_to_utf16(text: &str, byte: usize) -> usize {
    text.char_indices().take_while(|(offset, _)| *offset < byte).map(|(_, ch)| ch.len_utf16()).sum()
}

/// Byte offset in `text` of the UTF-16 column `utf16`, clamped to the text length.
pub fn utf16_to_byte(text: &str, utf16: usize) -> usize {
    let mut units = 0;
    for (offset, ch) in text.char_indices() {
        if units >= utf16 { return offset; }
        units += ch.len_utf16();
    }
    text.len()
}

pub fn path_to_uri(path: &Path) -> String {
    use percent_encoding::{utf8_percent_encode, AsciiSet, CONTROLS};
    const SEGMENT: &AsciiSet = &CONTROLS.add(b' ').add(b'"').add(b'#').add(b'%').add(b'<').add(b'>').add(b'?').add(b'[').add(b']').add(b'`').add(b'{').add(b'}');
    let text = path.to_string_lossy().replace('\\', "/");
    let encoded: Vec<String> = text.split('/').map(|segment| utf8_percent_encode(segment, SEGMENT).to_string()).collect();
    let joined = encoded.join("/");
    if joined.starts_with('/') { format!("file://{joined}") } else { format!("file:///{joined}") }
}

pub fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let decoded = percent_encoding::percent_decode_str(rest).decode_utf8().ok()?.into_owned();
    // Windows drive paths arrive as `/C:/...`; strip the leading slash.
    let bytes = decoded.as_bytes();
    let text = if bytes.len() > 2 && bytes[0] == b'/' && bytes[1].is_ascii_alphabetic() && bytes[2] == b':' { &decoded[1..] } else { &decoded };
    Some(PathBuf::from(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uris_round_trip_and_columns_convert() {
        let path = if cfg!(windows) { PathBuf::from(r"C:\Projects\my app\src\ma#in.rs") } else { PathBuf::from("/home/me/my app/src/ma#in.rs") };
        let uri = path_to_uri(&path);
        assert!(uri.starts_with("file:///"), "{uri}");
        assert!(uri.contains("my%20app") && uri.contains("ma%23in.rs"), "{uri}");
        assert_eq!(uri_to_path(&uri).unwrap(), path);
        assert_eq!(utf16_to_byte("héllo", 2), 3);
        assert_eq!(utf16_to_byte("a😀b", 3), 5);
        assert_eq!(utf16_to_byte("abc", 10), 3);
        assert_eq!(byte_to_utf16("héllo", 3), 2);
        assert_eq!(byte_to_utf16("a😀b", 5), 3);
        assert_eq!(byte_to_utf16("abc", 99), 3);
    }

    #[test]
    fn hover_contents_become_short_plain_lines_and_definitions_resolve() {
        let markdown = json!({"kind": "markdown", "value": "```rust\nfn main()\n```\n\n\nDoes things.\n\n    indented code\n"});
        assert_eq!(hover_lines(&markdown), ["fn main()", "", "Does things.", "", "    indented code"]);
        let mixed = json!(["plain", {"language": "rust", "value": "let x = 1;"}]);
        assert_eq!(hover_lines(&mixed), ["plain", "let x = 1;"]);
        let long = json!({"kind": "plaintext", "value": format!("{} {}", "word ".repeat(30).trim(), "x".repeat(200))});
        let wrapped = hover_lines(&long);
        assert!(wrapped.iter().all(|line| line.chars().count() <= HOVER_COLUMNS), "{wrapped:?}");
        let tall = json!((0..40).map(|i| i.to_string()).collect::<Vec<_>>().join("\n"));
        let cut = hover_lines(&tall);
        assert_eq!(cut.len(), HOVER_ROWS + 1);
        assert_eq!(cut.last().map(String::as_str), Some("…"));

        let uri = path_to_uri(Path::new(if cfg!(windows) { "C:/x/lib.rs" } else { "/x/lib.rs" }));
        let location = json!({"uri": uri, "range": {"start": {"line": 3, "character": 4}, "end": {"line": 3, "character": 9}}});
        assert_eq!(first_location(&location).unwrap().1, 3);
        let links = json!([{"targetUri": uri, "targetRange": {"start": {"line": 1, "character": 0}, "end": {"line": 9, "character": 0}},
            "targetSelectionRange": {"start": {"line": 2, "character": 7}, "end": {"line": 2, "character": 12}}}]);
        assert_eq!(first_location(&links).map(|(_, l, c)| (l, c)), Some((2, 7)));
        assert!(first_location(&Value::Null).is_none());
        assert!(first_location(&json!([])).is_none());
    }

    #[test]
    fn diagnostics_are_parsed_sorted_and_capped() {
        let params = json!({"uri": path_to_uri(Path::new(if cfg!(windows) { "C:/x/a.rs" } else { "/x/a.rs" })), "diagnostics": [
            {"range": {"start": {"line": 4, "character": 0}, "end": {"line": 4, "character": 3}}, "severity": 2, "message": "warn"},
            {"range": {"start": {"line": 4, "character": 5}, "end": {"line": 4, "character": 8}}, "severity": 1, "message": "err"},
            {"range": {"start": {"line": 1, "character": 0}, "end": {"line": 2, "character": 0}}, "message": "no severity means error"},
        ]});
        let (_, diagnostics) = parse_diagnostics(&params).unwrap();
        assert_eq!(diagnostics.iter().map(|d| (d.line, d.severity)).collect::<Vec<_>>(),
            [(1, Severity::Error), (4, Severity::Error), (4, Severity::Warning)]);
        let many: Vec<Value> = (0..MAX_DIAGNOSTICS + 10).map(|i| json!({"range": {"start": {"line": i, "character": 0}, "end": {"line": i, "character": 1}}, "message": "m"})).collect();
        let (_, capped) = parse_diagnostics(&json!({"uri": "file:///x/a.rs", "diagnostics": many})).unwrap();
        assert_eq!(capped.len(), MAX_DIAGNOSTICS);
    }

    /// End to end against rust-analyzer when it's on PATH (rustup ships it): handshake,
    /// didOpen, diagnostics back, then a fix via didChange clears them.
    #[test]
    fn rust_analyzer_reports_and_clears_an_error() {
        let server = registry::by_id("rust-analyzer").unwrap();
        // rustup leaves a proxy on PATH even when the component is absent; only a binary
        // that answers `--version` counts.
        let usable = server.locate().filter(|binary| {
            std::process::Command::new(binary).arg("--version").output().is_ok_and(|out| out.status.success())
        });
        match usable {
            Some(binary) => live_rust_analyzer(binary),
            None => eprintln!("rust-analyzer not installed; skipping"),
        }
    }

    /// A binary that exits before initializing is reported once and prompts an install,
    /// rather than being restarted.
    #[test]
    fn unusable_binary_prompts_instead_of_restarting() {
        let mut manager = Manager::default();
        let server = registry::by_id("just-lsp").unwrap();
        // Any program that exits immediately without speaking LSP.
        let stub = if cfg!(windows) { PathBuf::from("C:/Windows/System32/cmd.exe") } else { PathBuf::from("/bin/true") };
        manager.set_binary(server, stub);
        manager.set_root(Path::new("."));
        manager.open(Path::new("justfile"), "".into());
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut prompted = false;
        let mut notices = 0;
        while Instant::now() < deadline && !prompted {
            for event in manager.poll() {
                match event {
                    Event::Prompt(s) if s.id == "just-lsp" => prompted = true,
                    Event::Notice(text) => { assert!(text.contains("before it initialized"), "{text}"); notices += 1; }
                    _ => {}
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(prompted && notices == 1);
        std::thread::sleep(Duration::from_millis(200));
        assert!(manager.poll().is_empty(), "no restart attempts");
        assert!(manager.slots["just-lsp"].disabled);
    }

    /// Same, but first downloads rust-analyzer with the installer. Network: opt-in via
    /// `cargo test -- --ignored downloads`.
    #[test]
    #[ignore]
    fn downloads_rust_analyzer_then_reports_and_clears_an_error() {
        let root = std::env::temp_dir().join(format!("editor-lsp-ra-download-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let binary = install::install_to(registry::by_id("rust-analyzer").unwrap(), &root).unwrap();
        live_rust_analyzer(binary);
        let _ = std::fs::remove_dir_all(&root);
    }

    fn live_rust_analyzer(binary: PathBuf) {
        let root = std::env::temp_dir().join(format!("editor-lsp-ra-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("Cargo.toml"), "[package]\nname = \"probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n").unwrap();
        let main = root.join("src").join("main.rs");
        std::fs::write(&main, "fn main() { let x: i32 = \"no\"; }\n").unwrap();

        let mut manager = Manager::default();
        manager.set_binary(registry::by_id("rust-analyzer").unwrap(), binary);
        manager.set_root(&root);
        manager.open(&main, std::fs::read_to_string(&main).unwrap());
        let wait_for = |manager: &mut Manager, want_error: bool, secs: u64| -> bool {
            let deadline = Instant::now() + Duration::from_secs(secs);
            while Instant::now() < deadline {
                for event in manager.poll() {
                    match event {
                        Event::Diagnostics(path, diagnostics) => {
                            eprintln!("diagnostics for {}: {:?}", path.display(), diagnostics.iter().map(|d| (d.line, d.start, d.severity, d.message.as_str())).collect::<Vec<_>>());
                            let has_error = diagnostics.iter().any(|d| d.severity == Severity::Error);
                            if path == main && has_error == want_error { return true; }
                        }
                        Event::Notice(text) => eprintln!("notice: {text}"),
                        Event::Prompt(_) | Event::Hover { .. } | Event::Definition { .. } => {}
                    }
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            false
        };
        assert!(wait_for(&mut manager, true, 120), "expected a type error diagnostic");
        // Like the editor: the buffer changes first, then the user saves and the file hits disk.
        let fixed = "fn main() { let x: i32 = 1; let _ = x; }\n";
        manager.change(&main, fixed.into());
        let cleared_on_change = wait_for(&mut manager, false, 15);
        eprintln!("cleared after didChange alone: {cleared_on_change}");
        std::fs::write(&main, fixed).unwrap();
        manager.saved(&main, fixed.into());
        assert!(cleared_on_change || wait_for(&mut manager, false, 90), "expected the error to clear after the fix was saved");

        // `fn main() { let x: i32 = 1; let _ = x; }`: `x` is declared at column 16 and used at 36.
        let wait_event = |manager: &mut Manager, secs: u64, accept: &dyn Fn(&Event) -> bool| -> bool {
            let deadline = Instant::now() + Duration::from_secs(secs);
            while Instant::now() < deadline {
                for event in manager.poll() {
                    if let Event::Notice(text) = &event { eprintln!("notice: {text}"); }
                    if accept(&event) { return true; }
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            false
        };
        manager.hover(&main, 0, 16);
        assert!(wait_event(&mut manager, 30, &|event| match event {
            Event::Hover { line: 0, column: 16, lines, .. } => { eprintln!("hover: {lines:?}"); lines.iter().any(|l| l.contains("i32")) }
            _ => false,
        }), "expected hover text mentioning i32");
        manager.definition(&main, 0, 36);
        assert!(wait_event(&mut manager, 30, &|event| match event {
            Event::Definition { path, line, column } => { eprintln!("definition: {} {line}:{column}", path.display()); *path == main && *line == 0 && *column == 16 }
            _ => false,
        }), "expected the definition of x at 0:16");
        manager.shutdown_all();
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Reuses an already-downloaded rust-analyzer: `EDITOR_TEST_RA=<path> cargo test -- --ignored local_rust`.
    #[test]
    #[ignore]
    fn local_rust_analyzer_reports_and_clears_an_error() {
        let Some(binary) = std::env::var_os("EDITOR_TEST_RA") else { return };
        live_rust_analyzer(PathBuf::from(binary));
    }

    #[test]
    fn manager_without_servers_prompts_once_and_tracks_debounce() {
        let mut manager = Manager::default();
        manager.set_root(Path::new("."));
        // A language nobody here serves: silently ignored.
        manager.open(Path::new("notes.txt"), "x".into());
        assert!(manager.poll().is_empty());
        // Only prompt when the server is genuinely absent; a machine with it on PATH starts it.
        let server = registry::by_id("just-lsp").unwrap();
        if server.locate().is_none() {
            manager.open(Path::new("justfile"), "build:\n  cargo build\n".into());
            assert!(matches!(manager.poll().as_slice(), [Event::Prompt(s)] if s.id == "just-lsp"));
            manager.open(Path::new("other.just"), "".into());
            assert!(manager.poll().is_empty(), "prompted once per session");
        }
        manager.changed(Path::new("justfile"));
        assert!(manager.take_due().is_empty(), "not before the debounce");
        std::thread::sleep(CHANGE_DEBOUNCE + Duration::from_millis(20));
        assert_eq!(manager.take_due(), vec![PathBuf::from("justfile")]);
        manager.closed(Path::new("justfile"));
        manager.changed(Path::new("justfile"));
        std::thread::sleep(CHANGE_DEBOUNCE + Duration::from_millis(20));
        assert!(manager.take_due().is_empty(), "closed documents are forgotten");
    }
}
