use iced::widget::{
    button, column, container, pick_list, row, scrollable, text, text_editor, text_input, Space,
};
use iced::{Element, Length, Task};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::ai_diff::FileDiff;

/// Tool calls loop at most this many rounds before the conversation is forced to stop, so a
/// model that keeps requesting tools (or a server that keeps claiming `tool_calls`) can't hang
/// the chat forever.
const MAX_TOOL_ROUNDS: usize = 8;

/// `run_command` stops a command after this long unless the model asks for more (up to the max).
const COMMAND_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_COMMAND_TIMEOUT: Duration = Duration::from_secs(600);
/// Output kept from one command; the rest is dropped so a noisy command can't exhaust memory.
const MAX_COMMAND_OUTPUT: usize = 1024 * 1024;

#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
enum Role {
    User,
    Assistant,
    Tool,
}

impl Role {
    fn label(self) -> &'static str {
        match self {
            Role::User => "You",
            Role::Assistant => "AI",
            Role::Tool => "Tool",
        }
    }

    /// The role folded into the wire-format history rebuilt for each request. `Tool` notices
    /// are sent back as assistant text rather than real `tool` messages -- structured
    /// `tool_call_id` round-tripping only happens within a single `send()`'s background-thread
    /// loop (see `run_conversation`), not across turns.
    fn wire_role(self) -> &'static str {
        match self {
            Role::User => "user",
            Role::Assistant | Role::Tool => "assistant",
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct ChatMsg {
    wire_content: Option<serde_json::Value>,
    role: Role,
    content: String,
    /// The edit a `write_file` / `edit_file` call made, for "View diff".
    #[serde(default)]
    diff: Option<FileDiff>,
}

impl ChatMsg {
    fn new(role: Role, content: String) -> Self {
        Self { wire_content: None, role, content, diff: None }
    }
}

/// One saved conversation.
#[derive(Clone, Default, Serialize, Deserialize)]
struct ChatThread {
    title: String,
    messages: Vec<ChatMsg>,
    /// Unix seconds of the last message, for ordering the history menu.
    #[serde(default)]
    updated: u64,
}

#[derive(Default, Serialize, Deserialize)]
struct ThreadStore {
    threads: Vec<ChatThread>,
    active: usize,
}

enum Event {
    Delta(String),
    /// A tool call the model requested, formatted for display (e.g. `read_file(...)`\).
    /// Closes out the current assistant bubble and opens a fresh one for whatever text
    /// follows.
    ToolStart(String),
    /// What the last started tool did: its edit, if any, and a short result note.
    ToolResult(Option<FileDiff>, String),
    /// Sent when a `write_file` tool call succeeds, so the main app can reload any open tabs
    /// and refresh the file tree to pick up the change.
    FileChanged,
    /// The background thread wants to run a tool and is blocked on `respond` until the main
    /// thread relays the user's Allow/Deny choice back through it.
    PermissionRequest(PendingPermission),
    Done,
    Error(String),
}

/// One tool call awaiting the user's Allow/Deny -- `respond` unblocks the background thread's
/// blocking `recv()` in `request_permission`. Dropping it without sending (e.g. because the
/// user hit Stop) reads as a denial on the other end.
struct PendingPermission {
    remember: bool,
    label: String,
    /// The change a file-writing call would make, shown instead of its raw arguments.
    diff: Option<FileDiff>,
    respond: mpsc::Sender<bool>,
}

enum TestEvent {
    Success(Vec<String>),
    Error(String),
}

/// One base URL/API key/model combination the user has saved, so switching between e.g. a
/// local Ollama instance and a hosted API doesn't mean retyping everything each time.
#[derive(Clone, Serialize, Deserialize)]
struct SavedConnection {
    name: String,
    base_url: String,
    api_key: String,
    model: String,
}

fn connections_path() -> Option<PathBuf> {
    crate::config_path("ai_connections.json")
}

fn load_connections() -> Vec<SavedConnection> {
    connections_path()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn save_connections(connections: &[SavedConnection]) {
    let Some(path) = connections_path() else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(text) = serde_json::to_string(connections) {
        let _ = std::fs::write(path, text);
    }
}

#[derive(Default)]
enum ConnectionStatus {
    #[default]
    Idle,
    Testing,
    Connected,
    Failed(String),
}

pub struct ChatState {
    pub attachments: Vec<crate::ai_context::Attachment>,
    session_grants: std::collections::HashSet<String>,
    cwd: Option<PathBuf>,
    /// Where conversations are saved; `None` keeps them in memory only (tests).
    store_dir: Option<PathBuf>,
    threads: Vec<ChatThread>,
    active_thread: usize,
    history: crate::ai_history::Menu,
    connection_name: String,
    base_url: String,
    api_key: String,
    model: String,
    models: Vec<String>,
    connections: Vec<SavedConnection>,
    connection_status: ConnectionStatus,
    messages: Vec<ChatMsg>,
    input: text_editor::Content,
    rx: Option<Receiver<Event>>,
    cancel: Option<Arc<AtomicBool>>,
    test_rx: Option<Receiver<TestEvent>>,
    pending_permission: Option<PendingPermission>,
    streaming: bool,
    settings_open: bool,
    files_changed: bool,
    /// Selectable copies of `messages`' text, kept in step by `sync_selectable`.
    selectable: crate::ai_selectable::Cache,
    /// Replies rendered as Markdown, kept in step with `selectable`.
    markdown: crate::ai_markdown::Cache,
    /// Replies the user switched to plain selectable text.
    plain_text: std::collections::HashSet<usize>,
}

impl Default for ChatState {
    fn default() -> Self {
        Self {
            attachments: Vec::new(),
            session_grants: Default::default(),
            cwd: None,
            store_dir: None,
            threads: vec![ChatThread::default()],
            active_thread: 0,
            history: Default::default(),
            connection_name: String::new(),
            base_url: "http://localhost:11434/v1".to_string(),
            api_key: String::new(),
            model: String::new(),
            models: Vec::new(),
            connections: load_connections(),
            connection_status: ConnectionStatus::default(),
            messages: Vec::new(),
            input: text_editor::Content::new(),
            rx: None,
            cancel: None,
            test_rx: None,
            pending_permission: None,
            streaming: false,
            settings_open: false,
            files_changed: false,
            selectable: Default::default(),
            markdown: Default::default(),
            plain_text: Default::default(),
        }
    }
}

#[derive(Debug, Clone)]
pub enum Message {
    Composer(crate::ai_composer::Action),
    ConnectionNameChanged(String),
    BaseUrlChanged(String),
    ApiKeyChanged(String),
    ModelChanged(String),
    ModelSelected(String),
    TestConnection,
    SaveConnection,
    LoadConnection(usize),
    DeleteConnection(usize),
    InputChanged(text_editor::Action),
    ToggleSettings,
    NewConversation,
    History(crate::ai_history::Message),
    /// Show reply `usize` as plain selectable text instead of Markdown, or back.
    TogglePlainText(usize),
    Markdown(crate::ai_markdown::Action),
    /// Open the edit made by tool message `usize` in the preview pane (handled by the app).
    ViewDiff(usize),
    UsePrompt(String),
    Send,
    Stop,
    Copy(String),
    PermissionChosen(bool),
    RememberPermission(bool),
    ResetPermissions,
    OllamaPreset,
    /// Selection or caret movement inside message `usize`'s text.
    Selectable(usize, text_editor::Action),
}

impl ChatState {
    pub fn new() -> Self {
        Self { store_dir: crate::config_path("chat_threads"), ..Self::default() }
    }

    /// Keeps the selectable and Markdown text in step with `messages`; called after every change.
    fn sync_selectable(&mut self) {
        self.selectable.sync(self.messages.iter().map(|message| message.content.as_str()));
        self.markdown.sync(self.messages.iter().map(|message| match message.role {
            Role::Assistant => message.content.as_str(),
            _ => "",
        }));
    }

    pub fn poll(&mut self) {
        self.poll_events();
        self.sync_selectable();
    }

    pub fn input_text(&self) -> String { self.input.text() }

    /// Replaces the draft, leaving the caret at the end.
    pub fn set_input(&mut self, text: &str) {
        self.input = text_editor::Content::with_text(text);
        self.input.perform(text_editor::Action::Move(text_editor::Motion::DocumentEnd));
    }

    /// Base URL, API key and model, once a model is chosen.
    pub fn connection(&self) -> Option<(String, String, String)> {
        (!self.model.trim().is_empty() && !self.base_url.trim().is_empty())
            .then(|| (self.base_url.trim_end_matches('/').to_string(), self.api_key.clone(), self.model.clone()))
    }

    /// The edit made by tool message `index`.
    pub fn diff(&self, index: usize) -> Option<&FileDiff> {
        self.messages.get(index)?.diff.as_ref()
    }

    /// Follows the open project: saves the previous project's conversations and loads this one's.
    pub fn set_project(&mut self, cwd: &Path) {
        if self.cwd.as_deref() == Some(cwd) { return; }
        if let Some(previous) = self.cwd.take() {
            self.stop();
            self.save_current_thread();
            self.persist(&previous);
            self.session_grants.clear();
            self.attachments.clear();
            self.input = text_editor::Content::new();
        }
        self.cwd = Some(cwd.to_path_buf());
        let store = self.threads_path(cwd)
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|text| serde_json::from_str::<ThreadStore>(&text).ok())
            .filter(|store| !store.threads.is_empty());
        match store {
            Some(store) => {
                self.active_thread = store.active.min(store.threads.len() - 1);
                self.threads = store.threads;
            }
            None => {
                self.threads = vec![ChatThread::default()];
                self.active_thread = 0;
            }
        }
        self.messages = self.threads[self.active_thread].messages.clone();
        self.plain_text.clear();
    }

    fn threads_path(&self, cwd: &Path) -> Option<PathBuf> {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        cwd.hash(&mut hasher);
        Some(self.store_dir.as_ref()?.join(format!("{:x}.json", hasher.finish())))
    }

    /// Copies the live messages into the active conversation, naming it after the first question.
    fn save_current_thread(&mut self) {
        let Some(thread) = self.threads.get_mut(self.active_thread) else { return };
        thread.messages = self.messages.iter().cloned().map(|mut message| {
            // Images would bloat the file; the text of the conversation is what's worth keeping.
            if let Some(serde_json::Value::Array(parts)) = &mut message.wire_content {
                parts.retain(|part| part["type"] != "image_url");
            }
            message
        }).collect();
        if thread.title.is_empty() {
            if let Some(first) = self.messages.iter().find(|message| message.role == Role::User) {
                thread.title = first.content.lines().next().unwrap_or_default().chars().take(40).collect();
            }
        }
    }

    fn persist(&self, cwd: &Path) {
        let Some(path) = self.threads_path(cwd) else { return };
        if let Some(parent) = path.parent() { let _ = std::fs::create_dir_all(parent); }
        let store = ThreadStore { threads: self.threads.clone(), active: self.active_thread };
        if let Ok(text) = serde_json::to_string(&store) { let _ = std::fs::write(path, text); }
    }

    fn persist_current(&mut self) {
        self.save_current_thread();
        if let Some(cwd) = self.cwd.clone() { self.persist(&cwd); }
    }

    /// Opens `index` (or a new conversation when `None`), keeping the current one saved.
    fn open_thread(&mut self, index: Option<usize>) {
        if self.streaming || index.is_some_and(|index| index >= self.threads.len() || index == self.active_thread) { return; }
        self.save_current_thread();
        self.active_thread = match index {
            Some(index) => index,
            None => {
                self.threads.push(ChatThread { updated: crate::ai_history::now(), ..ChatThread::default() });
                self.threads.len() - 1
            }
        };
        self.messages = self.threads[self.active_thread].messages.clone();
        self.attachments.clear();
        self.session_grants.clear();
        self.plain_text.clear();
        if let Some(cwd) = self.cwd.clone() { self.persist(&cwd); }
    }

    /// Removes a conversation; deleting the open one opens the most recent remaining (or a new) one.
    fn delete_thread(&mut self, index: usize) {
        if self.streaming || index >= self.threads.len() { return; }
        self.save_current_thread();
        self.threads.remove(index);
        if index == self.active_thread {
            if self.threads.is_empty() { self.threads.push(ChatThread::default()); }
            self.active_thread = self.threads.len() - 1;
            self.messages = self.threads[self.active_thread].messages.clone();
            self.session_grants.clear();
            self.plain_text.clear();
        } else if index < self.active_thread {
            self.active_thread -= 1;
        }
        if let Some(cwd) = self.cwd.clone() { self.persist(&cwd); }
    }

    fn history_items(&self) -> Vec<crate::ai_history::Item> {
        let searching = self.history.open;
        self.threads.iter().enumerate().map(|(index, thread)| {
            let messages = if index == self.active_thread { &self.messages } else { &thread.messages };
            let text = if searching {
                messages.iter().filter(|message| message.role != Role::Tool).map(|message| message.content.as_str()).collect::<Vec<_>>().join("\n")
            } else { String::new() };
            crate::ai_history::Item { index, title: thread.title.clone(), updated: thread.updated, text }
        }).collect()
    }

    fn poll_events(&mut self) {
        if let Some(rx) = &self.test_rx {
            let mut finished = false;
            while let Ok(event) = rx.try_recv() {
                match event {
                    TestEvent::Success(models) => {
                        self.models = models;
                        self.connection_status = ConnectionStatus::Connected;
                        if self.models.iter().all(|m| m != &self.model) {
                            if let Some(first) = self.models.first() {
                                self.model = first.clone();
                            }
                        }
                    }
                    TestEvent::Error(err) => {
                        self.models.clear();
                        self.connection_status = ConnectionStatus::Failed(err);
                    }
                }
                finished = true;
            }
            if finished {
                self.test_rx = None;
            }
        }

        let Some(rx) = &self.rx else { return };
        let mut finished = false;
        while let Ok(event) = rx.try_recv() {
            match event {
                Event::Delta(text) => {
                    if let Some(last) = self.messages.last_mut() {
                        last.content.push_str(&text);
                    }
                }
                Event::ToolStart(label) => {
                    self.messages.push(ChatMsg::new(Role::Tool, label));
                    self.messages.push(ChatMsg::new(Role::Assistant, String::new()));
                }
                Event::ToolResult(diff, note) => {
                    if let Some(tool) = self.messages.iter_mut().rev().find(|message| message.role == Role::Tool) {
                        if !note.is_empty() {
                            tool.content.push('\n');
                            tool.content.push_str(&note);
                        }
                        tool.diff = diff;
                    }
                }
                Event::FileChanged => self.files_changed = true,
                Event::PermissionRequest(pending) => {
                    if self.session_grants.contains(&pending.label) { let _ = pending.respond.send(true); }
                    else { self.pending_permission = Some(pending); }
                },
                Event::Done => finished = true,
                Event::Error(err) => {
                    if let Some(last) = self.messages.last_mut() {
                        last.content.push_str(&format!("\n[error: {err}]"));
                    }
                    finished = true;
                }
            }
        }
        if finished {
            self.streaming = false;
            self.rx = None;
            self.cancel = None;
            self.persist_current();
        }
    }

    /// Returns true (once) after a `write_file` tool call succeeds, so the caller can reload
    /// open tabs and refresh the file tree.
    pub fn take_files_changed(&mut self) -> bool {
        std::mem::take(&mut self.files_changed)
    }

    fn send(&mut self, cwd: PathBuf) {
        let text = self.input.text();
        if (text.trim().is_empty() && self.attachments.is_empty()) || self.streaming || self.model.trim().is_empty() || self.base_url.trim().is_empty() {
            return;
        }
        let text = text.trim_end().to_string();
        self.input = text_editor::Content::new();
        let mut parts = vec![serde_json::json!({"type":"text", "text":text})];
        let mut display = text.clone();
        for attachment in self.attachments.drain(..) {
            display.push_str(&format!("\n[Attached: {}]", attachment.name));
            parts.push(attachment.http());
        }
        let wire_content = Some(if parts.len() == 1 { serde_json::Value::String(text) } else { serde_json::Value::Array(parts) });
        self.messages.push(ChatMsg { wire_content, ..ChatMsg::new(Role::User, display) });
        self.messages.push(ChatMsg::new(Role::Assistant, String::new()));
        if let Some(thread) = self.threads.get_mut(self.active_thread) { thread.updated = crate::ai_history::now(); }
        self.persist_current();

        let history: Vec<(&'static str, serde_json::Value)> = self.messages[..self.messages.len() - 1]
            .iter()
            .map(|m| (m.role.wire_role(), m.wire_content.clone().unwrap_or_else(|| serde_json::Value::String(m.content.clone()))))
            .collect();

        let base_url = self.base_url.trim_end_matches('/').to_string();
        let api_key = self.api_key.clone();
        let model = self.model.clone();

        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        self.rx = Some(rx);
        self.cancel = Some(cancel.clone());
        self.streaming = true;

        std::thread::spawn(move || {
            run_conversation(base_url, api_key, model, history, cwd, tx, cancel);
        });
    }

    fn stop(&mut self) {
        if let Some(cancel) = &self.cancel {
            cancel.store(true, Ordering::Relaxed);
        }
        self.streaming = false;
        self.rx = None;
        self.cancel = None;
        // Dropped without a reply, this reads as a denial to whatever `request_permission`
        // call the background thread is blocked on, letting it unwind instead of hanging.
        self.pending_permission = None;
    }

    /// Hits the `/models` endpoint to confirm the base URL/API key work before letting the
    /// user chat, and populates the model picker from whatever it reports.
    fn test_connection(&mut self) {
        let base_url = self.base_url.trim_end_matches('/').to_string();
        let api_key = self.api_key.clone();

        let (tx, rx) = mpsc::channel();
        self.test_rx = Some(rx);
        self.connection_status = ConnectionStatus::Testing;
        self.models.clear();

        std::thread::spawn(move || fetch_models(base_url, api_key, tx));
    }

    /// Saves the current Base URL/API key/model as a named card, updating one of the same
    /// name in place if it already exists.
    fn save_connection(&mut self) {
        let name = if self.connection_name.trim().is_empty() {
            if self.model.is_empty() { self.base_url.clone() } else { self.model.clone() }
        } else {
            self.connection_name.clone()
        };
        let entry = SavedConnection {
            name: name.clone(),
            base_url: self.base_url.clone(),
            api_key: self.api_key.clone(),
            model: self.model.clone(),
        };
        match self.connections.iter_mut().find(|c| c.name == name) {
            Some(existing) => *existing = entry,
            None => self.connections.push(entry),
        }
        self.connection_name = name;
        save_connections(&self.connections);
    }

    /// Loads a saved card into the current form and immediately re-tests it, so picking a
    /// card is enough to get back to a working, connected state.
    fn load_connection(&mut self, index: usize) {
        self.session_grants.clear();
        let Some(conn) = self.connections.get(index) else { return };
        self.connection_name = conn.name.clone();
        self.base_url = conn.base_url.clone();
        self.api_key = conn.api_key.clone();
        self.model = conn.model.clone();
        self.test_connection();
        self.settings_open = false;
    }

    fn delete_connection(&mut self, index: usize) {
        if index >= self.connections.len() {
            return;
        }
        self.connections.remove(index);
        save_connections(&self.connections);
    }
}

pub fn update(state: &mut ChatState, message: Message, cwd: PathBuf) -> Task<Message> {
    state.set_project(&cwd);
    match message {
        Message::Composer(_) => {},
        Message::ConnectionNameChanged(text) => state.connection_name = text,
        Message::BaseUrlChanged(text) => {
            state.session_grants.clear();
            state.base_url = text;
            state.test_rx = None;
            state.connection_status = ConnectionStatus::Idle;
            state.models.clear();
        }
        Message::ApiKeyChanged(text) => {
            state.session_grants.clear();
            state.api_key = text;
            state.test_rx = None;
            state.connection_status = ConnectionStatus::Idle;
            state.models.clear();
        }
        Message::ModelChanged(text) => state.model = text,
        Message::ModelSelected(model) => state.model = model,
        Message::TestConnection => state.test_connection(),
        Message::SaveConnection => state.save_connection(),
        Message::LoadConnection(index) => state.load_connection(index),
        Message::DeleteConnection(index) => state.delete_connection(index),
        Message::InputChanged(action) => state.input.perform(action),
        Message::NewConversation => {
            if !state.messages.is_empty() { state.open_thread(None); }
        }
        Message::History(message) => match state.history.update(message) {
            Some(crate::ai_history::Action::Switch(index)) => state.open_thread(Some(index)),
            Some(crate::ai_history::Action::Delete(index)) => state.delete_thread(index),
            Some(crate::ai_history::Action::Rename(index, title)) => {
                if let Some(thread) = state.threads.get_mut(index) { thread.title = title; }
                state.persist_current();
            }
            None => {}
        },
        Message::TogglePlainText(index) => {
            if !state.plain_text.remove(&index) { state.plain_text.insert(index); }
        }
        Message::Markdown(crate::ai_markdown::Action::Copy(text)) => return iced::clipboard::write(text),
        Message::Markdown(crate::ai_markdown::Action::Link(url)) => crate::ai_markdown::open_link(&url),
        // Inserting into the editor and opening diffs are the app's to handle.
        Message::Markdown(crate::ai_markdown::Action::Insert(_)) | Message::ViewDiff(_) => {}
        Message::UsePrompt(prompt) => state.set_input(&prompt),
        Message::ToggleSettings => state.settings_open = !state.settings_open,
        Message::Send => state.send(cwd),
        Message::Stop => state.stop(),
        Message::Copy(text) => return iced::clipboard::write(text),
        Message::OllamaPreset => {
            state.session_grants.clear();
            state.base_url = "http://localhost:11434/v1".into();
            state.api_key.clear();
            state.connection_name = "Ollama".into();
            state.test_connection();
        }
        Message::ResetPermissions => state.session_grants.clear(),
        Message::RememberPermission(remember) => {
            if let Some(pending) = &mut state.pending_permission { pending.remember = remember; }
        }
        Message::PermissionChosen(allowed) => {
            if let Some(pending) = state.pending_permission.take() {
                if allowed && pending.remember { state.session_grants.insert(pending.label); }
                let _ = pending.respond.send(allowed);
            }
        }
        Message::Selectable(index, action) => state.selectable.perform(index, action),
    }
    state.sync_selectable();
    Task::none()
}

pub fn conversation_controls(state: &ChatState) -> Element<'_, Message> {
    row![
        crate::icon_control(lucide_icons::Icon::Plus, "New conversation", (!state.streaming && !state.messages.is_empty()).then_some(Message::NewConversation), false),
        crate::icon_control(lucide_icons::Icon::History, "Conversation history", Some(Message::History(crate::ai_history::Message::Toggle)), state.history.open),
    ].spacing(4).into()
}

pub fn view<'a>(state: &'a ChatState, composer: crate::ai_composer::Context<'a>) -> Element<'a, Message> {
    let mut header = column![row![
        text("Conversation").size(12),
        Space::new().width(Length::Fill),
        crate::icon_control(lucide_icons::Icon::Settings, "Model settings", Some(Message::ToggleSettings), state.settings_open),
    ]].spacing(4);
    if state.history.open {
        header = header.push(crate::ai_history::view(&state.history, state.history_items(), state.active_thread, !state.streaming).map(Message::History));
    }

    if state.settings_open {
        let field = |label: &'static str| text(label).width(Length::Fixed(70.0));

        let mut cards = column![].spacing(2);
        for (i, conn) in state.connections.iter().enumerate() {
            cards = cards.push(
                row![
                    button(
                        column![
                            text(conn.name.clone()),
                            text(format!("{} -- {}", conn.base_url, conn.model)).size(11),
                        ]
                        .spacing(1)
                    )
                    .width(Length::Fill)
                    .padding([4, 8])
                    .style(crate::flat_button_style)
                    .on_press(Message::LoadConnection(i)),
                    button(text("x"))
                        .padding([4, 8])
                        .style(crate::flat_button_style)
                        .on_press(Message::DeleteConnection(i)),
                ]
                .spacing(4)
                .align_y(iced::Alignment::Center),
            );
        }

        let status_text = match &state.connection_status {
            ConnectionStatus::Idle => String::new(),
            ConnectionStatus::Testing => "Testing...".to_string(),
            ConnectionStatus::Connected => format!("Connected -- {} model(s) found", state.models.len()),
            ConnectionStatus::Failed(err) => format!("Failed: {err}"),
        };

        let model_row = if state.models.is_empty() {
            row![
                field("Model"),
                text_input("", &state.model).on_input(Message::ModelChanged),
            ]
        } else {
            row![
                field("Model"),
                pick_list(state.models.as_slice(), Some(state.model.clone()), Message::ModelSelected)
                    .width(Length::Fill),
            ]
        }
        .spacing(6)
        .align_y(iced::Alignment::Center);

        let form = column![
            button("Use local Ollama").style(crate::flat_button_style).on_press(Message::OllamaPreset),
            cards,
            row![field("Name"), text_input("", &state.connection_name).on_input(Message::ConnectionNameChanged)]
                .spacing(6)
                .align_y(iced::Alignment::Center),
            row![field("Base URL"), text_input("", &state.base_url).on_input(Message::BaseUrlChanged)]
                .spacing(6)
                .align_y(iced::Alignment::Center),
            row![
                field("API Key"),
                text_input("optional", &state.api_key)
                    .on_input(Message::ApiKeyChanged)
                    .secure(true)
            ]
            .spacing(6)
            .align_y(iced::Alignment::Center),
            row![
                Space::new().width(Length::Fixed(70.0)),
                button(text("Test Connection"))
                    .style(crate::flat_button_style)
                    .on_press(Message::TestConnection),
                text(status_text).size(12),
            ]
            .spacing(6)
            .align_y(iced::Alignment::Center),
            model_row,
            row![
                Space::new().width(Length::Fixed(70.0)),
                button(text("Save"))
                    .style(crate::flat_button_style)
                    .on_press(Message::SaveConnection),
            ]
            .spacing(6)
            .align_y(iced::Alignment::Center),
        ]
        .spacing(6);

        header = header.push(container(form).padding(8).style(|theme: &iced::Theme| {
            let palette = theme.extended_palette();
            iced::widget::container::Style {
                background: Some(palette.background.weak.color.into()),
                border: iced::Border::default().rounded(6.0),
                ..iced::widget::container::Style::default()
            }
        }));
    }

    let mut messages_col = column![].spacing(6);
    if state.messages.is_empty() && !state.settings_open {
        messages_col = messages_col.push(container(column![
            text(if state.model.is_empty() { "Set up your assistant" } else { "Start a conversation" }).size(18),
            text(if state.model.is_empty() {
                "Connect a provider and choose a model to chat about your project."
            } else { "Ask about your code, plan a change, or describe a problem." })
                .size(13).style(iced::widget::text::secondary),
            button("Explain this project")
                .style(crate::flat_button_style)
                .on_press(Message::UsePrompt("Explore this project and explain its structure and main entry points.".into())),
            button("Review for bugs")
                .style(crate::flat_button_style)
                .on_press(Message::UsePrompt("Review this project for likely bugs. Explain your findings before making changes.".into())),
            button(if state.model.is_empty() { "Choose a model" } else { "Model settings" })
                .on_press(Message::ToggleSettings),
        ].spacing(12)).padding([24, 12]));
    }
    let last = state.messages.len().saturating_sub(1);
    for (i, message) in state.messages.iter().enumerate() {
        if message.content.is_empty() {
            if i == last && state.streaming {
                messages_col = messages_col.push(column![
                    text(message.role.label()).size(12),
                    text("Waiting for response...").style(|theme: &iced::Theme| {
                        let palette = theme.extended_palette();
                        iced::widget::text::Style { color: Some(palette.background.strong.color) }
                    }),
                ]);
            }
            continue;
        }
        let formatted = message.role == Role::Assistant;
        let plain = state.plain_text.contains(&i);
        // Replies render as Markdown unless switched to selectable text. Selectable text needs
        // the cache to have caught up with this message; plain text is the fallback.
        let body: Element<'a, Message> = match (state.markdown.get(i), state.selectable.get(i)) {
            (Some(parsed), _) if formatted && !plain => crate::ai_markdown::view(parsed, composer.theme, Message::Markdown),
            (_, Some(selectable)) => crate::ai_selectable::view(selectable, 13.0, move |action| Message::Selectable(i, action)),
            _ => text(message.content.clone()).size(13).into(),
        };
        let mut heading = row![text(message.role.label()).size(12), Space::new().width(Length::Fill)]
            .spacing(6).align_y(iced::Alignment::Center);
        if formatted {
            heading = heading.push(crate::icon_control(lucide_icons::Icon::TextCursor, if plain { "Show formatted" } else { "Select text" }, Some(Message::TogglePlainText(i)), plain));
        }
        let mut entry = column![
            heading.push(crate::icon_control(lucide_icons::Icon::Copy, "Copy message", Some(Message::Copy(message.content.clone())), false)),
            body,
        ].spacing(4);
        if let Some(diff) = &message.diff {
            let (added, removed) = diff.stats();
            entry = entry.push(button(row![
                text(char::from(lucide_icons::Icon::FileDiff)).font(iced::Font::with_name("lucide")).size(13),
                text(format!("View diff · {}", diff.file_name())).size(12),
                text(format!("+{added} −{removed}")).size(11).style(iced::widget::text::secondary),
            ].spacing(6).align_y(iced::Alignment::Center)).padding([3, 6]).style(crate::flat_button_style)
                .on_press(Message::ViewDiff(i)));
        }
        messages_col = messages_col.push(entry);
    }
    let messages = scrollable(messages_col.spacing(16)).anchor_bottom().height(Length::Fill);

    let mut bottom = column![].spacing(4);
    if let Some(pending) = &state.pending_permission {
        use crate::ai_approval::{Card, Choice, ChoiceKind};
        let (title, mut details) = pending.label.split_once('(').map(|(name, args)| {
            let details = serde_json::from_str::<serde_json::Value>(args.strip_suffix(')').unwrap_or(args))
                .map(|value| crate::ai_approval::format_input(&value)).unwrap_or_else(|_| args.to_string());
            (name.to_string(), details)
        }).unwrap_or_else(|| ("Review tool request".into(), pending.label.clone()));
        // A file write reads better as the change it makes than as the file's full new text.
        if let Some(diff) = &pending.diff { details = diff.review_text(); }
        bottom = bottom.push(crate::ai_approval::view(Card {
            title, details: details.clone(),
            choices: vec![
                Choice { label: "Reject".into(), message: Message::PermissionChosen(false), kind: ChoiceKind::Reject },
                Choice { label: if pending.remember { "Allow for session" } else { "Allow once" }.into(), message: Message::PermissionChosen(true), kind: ChoiceKind::Allow },
            ],
            remember: Some((pending.remember, Message::RememberPermission)),
            copy: Message::Copy(details),
        }));
    }
    if !state.session_grants.is_empty() { bottom = bottom.push(button("Reset session approvals").style(crate::flat_button_style).on_press(Message::ResetPermissions)); }
    bottom = bottom.push(crate::ai_composer::view(
        column![
            text_editor(&state.input)
                .size(13)
                .placeholder("Ask about your project… (Cmd/Ctrl+Enter to send)")
                .on_action(Message::InputChanged)
                .height(Length::Fixed(60.0))
                .key_binding(|key_press| crate::ai_composer::key_binding(key_press, Message::Send, Message::Composer)),
            row![
                text(if state.model.is_empty() {
                    "No model selected".to_string()
                } else {
                    state.model.clone()
                })
                .size(12),
                Space::new().width(Length::Fill),
                if state.streaming {
                    crate::icon_control(lucide_icons::Icon::CircleStop, "Stop response", Some(Message::Stop), false)
                } else {
                    crate::icon_control(lucide_icons::Icon::SendHorizonal, "Send message (Cmd/Ctrl+Enter)",
                        (!state.model.trim().is_empty() && !state.base_url.trim().is_empty() && (!state.input.text().trim().is_empty() || !state.attachments.is_empty())).then_some(Message::Send), false)
                },
            ]
            .align_y(iced::Alignment::Center),
        ]
        .spacing(4).into(),
        &state.attachments, composer, Message::Composer,
    ));

    container(column![header, messages, bottom].spacing(8))
        .padding(8)
        .height(Length::Fill)
        .into()
}

/// One tool call the model has requested, accumulated across streamed SSE deltas: `id` and
/// `name` normally arrive whole in the first delta for a given `index`, while `arguments`
/// arrives as a JSON string built up fragment by fragment.
#[derive(Default)]
struct ToolCallAccum {
    id: String,
    name: String,
    arguments: String,
}

/// Drives the whole exchange for one `Send`: sends the request, streams the reply, and if the
/// model asks for tool calls, executes them locally and loops back with the results appended --
/// up to `MAX_TOOL_ROUNDS` -- until it gets a plain text reply.
fn run_conversation(
    base_url: String,
    api_key: String,
    model: String,
    history: Vec<(&'static str, serde_json::Value)>,
    cwd: PathBuf,
    tx: mpsc::Sender<Event>,
    cancel: Arc<AtomicBool>,
) {
    let mut messages: Vec<serde_json::Value> = history
        .iter()
        .map(|(role, content)| serde_json::json!({ "role": role, "content": content }))
        .collect();

    for _ in 0..MAX_TOOL_ROUNDS {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let Some((content, tool_calls)) = run_request(&base_url, &api_key, &model, &messages, &tx, &cancel)
        else {
            return;
        };
        if tool_calls.is_empty() {
            break;
        }

        messages.push(serde_json::json!({
            "role": "assistant",
            "content": if content.is_empty() { serde_json::Value::Null } else { serde_json::Value::String(content) },
            "tool_calls": tool_calls.iter().map(|call| serde_json::json!({
                "id": call.id,
                "type": "function",
                "function": { "name": call.name, "arguments": call.arguments },
            })).collect::<Vec<_>>(),
        }));

        for call in &tool_calls {
            if cancel.load(Ordering::Relaxed) {
                return;
            }
            let preview = call.arguments.clone();
            let label = format!("{}({})", call.name, preview);
            let diff = proposed_diff(&cwd, &call.name, &call.arguments);

            if !request_permission(&tx, &label, diff.clone()) {
                let _ = tx.send(Event::ToolStart(format!("{label}  [denied]")));
                messages.push(serde_json::json!({
                    "role": "tool",
                    "tool_call_id": call.id,
                    "content": "error: user denied permission for this tool call",
                }));
                continue;
            }

            let _ = tx.send(Event::ToolStart(label));
            let result = execute_tool(&cwd, &call.name, &call.arguments, &cancel);
            let failed = result.starts_with("error");
            let changes_files = matches!(call.name.as_str(), "write_file" | "edit_file" | "run_command");
            if changes_files && !failed {
                let _ = tx.send(Event::FileChanged);
            }
            // Commands and failures get a short note under the call; file edits get their diff.
            let note = if call.name == "run_command" || failed {
                let lines: Vec<&str> = result.lines().collect();
                let shown = lines.iter().take(12).copied().collect::<Vec<_>>().join("\n");
                if lines.len() > 12 { format!("{shown}\n… {} more lines", lines.len() - 12) } else { shown }
            } else { String::new() };
            let _ = tx.send(Event::ToolResult(diff.filter(|_| !failed), note));
            messages.push(serde_json::json!({
                "role": "tool",
                "tool_call_id": call.id,
                "content": result,
            }));
        }
    }
    let _ = tx.send(Event::Done);
}

/// Blocks the background thread until the main thread relays the user's Allow/Deny choice for
/// `label` back through the one-shot channel bundled into the `PermissionRequest` event. If
/// the channel disconnects without a reply (e.g. the user hit Stop), that reads as a denial.
fn request_permission(tx: &mpsc::Sender<Event>, label: &str, diff: Option<FileDiff>) -> bool {
    let (respond, response) = mpsc::channel();
    let request = PendingPermission { label: label.to_string(), remember: false, diff, respond };
    if tx.send(Event::PermissionRequest(request)).is_err() {
        return false;
    }
    response.recv().unwrap_or(false)
}

/// Calls the OpenAI-compatible `GET /models` endpoint to both confirm the base URL/API key
/// work and list what's available to pick from -- local runtimes like Ollama serve this from
/// their OpenAI-compatible port without needing an API key.
fn fetch_models(base_url: String, api_key: String, tx: mpsc::Sender<TestEvent>) {
    let url = format!("{base_url}/models");
    let mut request = ureq::get(&url).header("Content-Type", "application/json");
    if !api_key.trim().is_empty() {
        request = request.header("Authorization", &format!("Bearer {api_key}"));
    }

    let response = match request.call() {
        Ok(response) => response,
        Err(err) => {
            let _ = tx.send(TestEvent::Error(err.to_string()));
            return;
        }
    };

    let mut reader = response.into_body().into_reader();
    let mut text = String::new();
    if let Err(err) = std::io::Read::read_to_string(&mut reader, &mut text) {
        let _ = tx.send(TestEvent::Error(err.to_string()));
        return;
    }

    let value: serde_json::Value = match serde_json::from_str(&text) {
        Ok(value) => value,
        Err(err) => {
            let _ = tx.send(TestEvent::Error(format!("invalid response: {err}")));
            return;
        }
    };

    let models: Vec<String> = value["data"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry["id"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    if models.is_empty() {
        let _ = tx.send(TestEvent::Error("connected, but no models were returned".to_string()));
    } else {
        let _ = tx.send(TestEvent::Success(models));
    }
}

/// Sends one chat-completions request and streams the SSE response, forwarding text deltas as
/// `Event::Delta` as they arrive. Returns the full accumulated assistant text plus any tool
/// calls the model requested, or `None` if the request failed or was cancelled (in which case
/// an `Event::Error` has already been sent, or nothing further should happen for a cancel).
fn run_request(
    base_url: &str,
    api_key: &str,
    model: &str,
    messages: &[serde_json::Value],
    tx: &mpsc::Sender<Event>,
    cancel: &Arc<AtomicBool>,
) -> Option<(String, Vec<ToolCallAccum>)> {
    let body = serde_json::json!({
        "model": model,
        "stream": true,
        "messages": messages,
        "tools": tool_defs(),
    })
    .to_string();

    let url = format!("{base_url}/chat/completions");
    let mut request = ureq::post(&url).header("Content-Type", "application/json");
    if !api_key.trim().is_empty() {
        request = request.header("Authorization", &format!("Bearer {api_key}"));
    }
    let result = request.send(body);

    let response = match result {
        Ok(response) => response,
        Err(err) => {
            let _ = tx.send(Event::Error(err.to_string()));
            return None;
        }
    };

    let mut content = String::new();
    let mut tool_calls: Vec<ToolCallAccum> = Vec::new();
    let mut saw_done = false;
    let mut read_error: Option<String> = None;

    let reader = BufReader::new(response.into_body().into_reader());
    for line in reader.lines() {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        let line = match line {
            Ok(line) => line,
            Err(err) => {
                read_error = Some(err.to_string());
                break;
            }
        };
        let Some(data) = line.strip_prefix("data: ") else {
            continue;
        };
        if data == "[DONE]" {
            saw_done = true;
            break;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(data) else {
            continue;
        };
        let delta = &value["choices"][0]["delta"];
        if let Some(text) = delta["content"].as_str() {
            content.push_str(text);
            let _ = tx.send(Event::Delta(text.to_string()));
        }
        if let Some(calls) = delta["tool_calls"].as_array() {
            for call in calls {
                let index = call["index"].as_u64().unwrap_or(0) as usize;
                while tool_calls.len() <= index {
                    tool_calls.push(ToolCallAccum::default());
                }
                let entry = &mut tool_calls[index];
                if let Some(id) = call["id"].as_str() {
                    entry.id.push_str(id);
                }
                if let Some(name) = call["function"]["name"].as_str() {
                    entry.name.push_str(name);
                }
                if let Some(args) = call["function"]["arguments"].as_str() {
                    entry.arguments.push_str(args);
                }
            }
        }
    }

    // A well-behaved OpenAI-compatible stream always ends with a `[DONE]` marker; if the
    // connection was cut short before that (dropped, reset, server crash mid-generation),
    // silently returning whatever was gathered so far would look to the user like the AI just
    // stopped replying with no explanation. Surface it instead.
    if !saw_done && !cancel.load(Ordering::Relaxed) {
        let reason = read_error.unwrap_or_else(|| "connection closed unexpectedly".to_string());
        if content.is_empty() && tool_calls.is_empty() {
            let _ = tx.send(Event::Error(reason));
            return None;
        }
        let _ = tx.send(Event::Delta(format!("\n\n[response interrupted: {reason}]")));
    }

    Some((content, tool_calls))
}

/// The shell `run_command` uses, named in its description so the model writes matching syntax.
const SHELL: &str = if cfg!(windows) { "PowerShell" } else { "sh" };

/// OpenAI-format `tools` array describing the functions the model may call: enough for the
/// model to read/search its way around the open project, `edit_file` / `write_file` to change
/// files in it, and `run_command` to build or test it. Every call needs the user's approval.
fn tool_defs() -> serde_json::Value {
    serde_json::json!([
        {
            "type": "function",
            "function": {
                "name": "read_file",
                "description": "Read the contents of a text file in the open project, given a path relative to the project root.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Path relative to the project root" },
                    },
                    "required": ["path"],
                },
            },
        },
        {
            "type": "function",
            "function": {
                "name": "list_dir",
                "description": "List files and folders inside a directory in the open project, given a path relative to the project root ('.' for the root).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Path relative to the project root" },
                    },
                    "required": ["path"],
                },
            },
        },
        {
            "type": "function",
            "function": {
                "name": "search_project",
                "description": "Case-insensitive substring search across every text file in the open project. Returns matching file:line pairs with a preview.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": { "type": "string" },
                    },
                    "required": ["query"],
                },
            },
        },
        {
            "type": "function",
            "function": {
                "name": "write_file",
                "description": "Create a new file or overwrite an existing one in the open project, given a path relative to the project root and the file's full text content.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Path relative to the project root" },
                        "content": { "type": "string", "description": "The full text content to write" },
                    },
                    "required": ["path", "content"],
                },
            },
        },
        {
            "type": "function",
            "function": {
                "name": "edit_file",
                "description": "Change part of an existing file by replacing exact text. Prefer this over write_file for edits. old_string must match the file exactly (including indentation) and, unless replace_all is true, exactly once; include surrounding lines to make it unique.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Path relative to the project root" },
                        "old_string": { "type": "string", "description": "The exact text to replace" },
                        "new_string": { "type": "string", "description": "The replacement text" },
                        "replace_all": { "type": "boolean", "description": "Replace every occurrence instead of exactly one" },
                    },
                    "required": ["path", "old_string", "new_string"],
                },
            },
        },
        {
            "type": "function",
            "function": {
                "name": "run_command",
                "description": format!("Run a {SHELL} command in the project root, e.g. to build or run tests, and return its exit code and combined output. Commands are non-interactive (no input), stopped after timeout_seconds (default {}, max {}).", COMMAND_TIMEOUT.as_secs(), MAX_COMMAND_TIMEOUT.as_secs()),
                "parameters": {
                    "type": "object",
                    "properties": {
                        "command": { "type": "string", "description": format!("The {SHELL} command line to run") },
                        "timeout_seconds": { "type": "integer", "description": "How long to let it run" },
                    },
                    "required": ["command"],
                },
            },
        },
    ])
}

fn execute_tool(cwd: &Path, name: &str, arguments: &str, cancel: &AtomicBool) -> String {
    let args: serde_json::Value = serde_json::from_str(arguments).unwrap_or(serde_json::Value::Null);
    match name {
        "edit_file" => {
            let Some(rel) = args["path"].as_str() else {
                return "error: missing 'path'".to_string();
            };
            let Some(path) = resolve_path(cwd, rel) else {
                return "error: file not found in the project (use write_file to create files)".to_string();
            };
            let content = match std::fs::read_to_string(&path) {
                Ok(content) => content,
                Err(err) => return format!("error reading file: {err}"),
            };
            let (old, new) = (args["old_string"].as_str().unwrap_or(""), args["new_string"].as_str().unwrap_or(""));
            match apply_edit(&content, old, new, args["replace_all"].as_bool().unwrap_or(false)) {
                Ok((edited, count)) => match std::fs::write(&path, edited) {
                    Ok(()) => format!("replaced {count} occurrence(s) in {rel}"),
                    Err(err) => format!("error writing file: {err}"),
                },
                Err(err) => format!("error: {err}"),
            }
        }
        "run_command" => {
            let Some(command) = args["command"].as_str().filter(|command| !command.trim().is_empty()) else {
                return "error: missing 'command'".to_string();
            };
            let timeout = args["timeout_seconds"].as_u64().map_or(COMMAND_TIMEOUT, Duration::from_secs).min(MAX_COMMAND_TIMEOUT);
            let mut process = shell(command);
            process.current_dir(cwd);
            match run_process(process, timeout, cancel) {
                Ok(output) => truncate(&output.report(timeout), 20_000),
                Err(err) => format!("error: could not start {SHELL}: {err}"),
            }
        }
        "read_file" => {
            let Some(rel) = args["path"].as_str() else {
                return "error: missing 'path'".to_string();
            };
            match resolve_path(cwd, rel) {
                Some(path) => std::fs::read_to_string(&path)
                    .map(|s| truncate(&s, 20_000))
                    .unwrap_or_else(|err| format!("error reading file: {err}")),
                None => "error: path escapes the project root".to_string(),
            }
        }
        "list_dir" => {
            let rel = args["path"].as_str().unwrap_or(".");
            match resolve_path(cwd, rel) {
                Some(path) => match std::fs::read_dir(&path) {
                    Ok(entries) => {
                        let mut names: Vec<String> = entries
                            .flatten()
                            .map(|entry| {
                                let name = entry.file_name().to_string_lossy().to_string();
                                if entry.path().is_dir() { format!("{name}/") } else { name }
                            })
                            .collect();
                        names.sort();
                        names.join("\n")
                    }
                    Err(err) => format!("error listing directory: {err}"),
                },
                None => "error: path escapes the project root".to_string(),
            }
        }
        "search_project" => {
            let query = args["query"].as_str().unwrap_or("");
            let results = crate::project_search::search(cwd, query);
            if results.is_empty() {
                "no matches".to_string()
            } else {
                results
                    .iter()
                    .take(50)
                    .map(|r| {
                        let rel = r.path.strip_prefix(cwd).unwrap_or(&r.path);
                        format!("{}:{}: {}", rel.display(), r.line, r.preview)
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        }
        "write_file" => {
            let Some(rel) = args["path"].as_str() else {
                return "error: missing 'path'".to_string();
            };
            let Some(content) = args["content"].as_str() else {
                return "error: missing 'content'".to_string();
            };
            match resolve_write_path(cwd, rel) {
                Some(path) => match std::fs::write(&path, content) {
                    Ok(()) => format!("wrote {} bytes to {rel}", content.len()),
                    Err(err) => format!("error writing file: {err}"),
                },
                None => "error: path escapes the project root".to_string(),
            }
        }
        other => format!("error: unknown tool '{other}'"),
    }
}

/// Replaces `old` in `content` (exactly once unless `replace_all`), returning the new text and
/// how many places changed. Files with CRLF line endings accept `old`/`new` written with LF.
fn apply_edit(content: &str, old: &str, new: &str, replace_all: bool) -> Result<(String, usize), String> {
    if old.is_empty() { return Err("old_string must not be empty".into()); }
    let crlf = content.contains("\r\n") && !old.contains("\r\n") && old.contains('\n');
    let (old, new) = if crlf { (old.replace('\n', "\r\n"), new.replace('\n', "\r\n")) } else { (old.to_string(), new.to_string()) };
    match content.matches(old.as_str()).count() {
        0 => Err("old_string was not found in the file; read the file and copy the text exactly".into()),
        count if count > 1 && !replace_all => Err(format!("old_string matches {count} places; include more surrounding lines or set replace_all")),
        count => Ok((if replace_all { content.replace(&old, &new) } else { content.replacen(&old, &new, 1) }, count)),
    }
}

/// The change a `write_file` / `edit_file` call would make, for review before it runs.
fn proposed_diff(cwd: &Path, name: &str, arguments: &str) -> Option<FileDiff> {
    let args: serde_json::Value = serde_json::from_str(arguments).ok()?;
    let rel = args["path"].as_str()?;
    let current = resolve_path(cwd, rel).and_then(|path| std::fs::read_to_string(path).ok());
    let new = match name {
        "write_file" => args["content"].as_str()?.to_string(),
        "edit_file" => apply_edit(current.as_deref()?, args["old_string"].as_str()?, args["new_string"].as_str()?, args["replace_all"].as_bool().unwrap_or(false)).ok()?.0,
        _ => return None,
    };
    Some(FileDiff { path: rel.to_string(), old: current, new })
}

fn shell(command: &str) -> std::process::Command {
    if cfg!(windows) {
        let mut process = std::process::Command::new("powershell.exe");
        process.args(["-NoProfile", "-NonInteractive", "-Command", command]);
        process
    } else {
        let mut process = std::process::Command::new("sh");
        process.args(["-c", command]);
        process
    }
}

struct ProcessOutput {
    /// Exit code; `None` when it was stopped or killed by a signal.
    code: Option<i32>,
    /// stdout and stderr, interleaved as they arrived.
    text: String,
    timed_out: bool,
    cancelled: bool,
}

impl ProcessOutput {
    fn report(&self, timeout: Duration) -> String {
        let status = if self.cancelled {
            "error: stopped by the user".to_string()
        } else if self.timed_out {
            format!("error: timed out after {}s and was stopped", timeout.as_secs())
        } else {
            match self.code {
                Some(0) => "exit code 0".to_string(),
                Some(code) => format!("error: exit code {code}"),
                None => "error: terminated by a signal".to_string(),
            }
        };
        if self.text.trim().is_empty() { format!("{status}\n(no output)") } else { format!("{status}\n{}", self.text.trim_end()) }
    }
}

/// Runs `command` with no input, collecting its output until it exits, `timeout` passes or
/// `cancel` is set (the last two kill it).
fn run_process(mut command: std::process::Command, timeout: Duration, cancel: &AtomicBool) -> std::io::Result<ProcessOutput> {
    use std::process::Stdio;
    command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = command.spawn()?;
    let output = Arc::new(Mutex::new(Vec::new()));
    let pipes: [Option<Box<dyn Read + Send>>; 2] = [
        child.stdout.take().map(|pipe| Box::new(pipe) as Box<dyn Read + Send>),
        child.stderr.take().map(|pipe| Box::new(pipe) as Box<dyn Read + Send>),
    ];
    let readers: Vec<_> = pipes.into_iter().flatten().map(|mut pipe| {
        let output = output.clone();
        std::thread::spawn(move || {
            let mut buffer = [0u8; 8192];
            while let Ok(read @ 1..) = pipe.read(&mut buffer) {
                let mut output = output.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                let room = MAX_COMMAND_OUTPUT.saturating_sub(output.len());
                output.extend_from_slice(&buffer[..read.min(room)]);
            }
        })
    }).collect();
    let started = Instant::now();
    let (mut timed_out, mut cancelled) = (false, false);
    let code = loop {
        if let Some(status) = child.try_wait()? { break status.code(); }
        cancelled = cancel.load(Ordering::Relaxed);
        timed_out = started.elapsed() >= timeout;
        if cancelled || timed_out {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    // Background processes the command started can hold the pipes open; don't wait on them.
    let grace = Instant::now();
    while readers.iter().any(|reader| !reader.is_finished()) && grace.elapsed() < Duration::from_secs(2) {
        std::thread::sleep(Duration::from_millis(10));
    }
    let text = String::from_utf8_lossy(&output.lock().unwrap_or_else(|poisoned| poisoned.into_inner())).into_owned();
    Ok(ProcessOutput { code, text, timed_out, cancelled })
}

/// Resolves `rel` against `cwd`, rejecting anything that canonicalizes outside `cwd` -- the
/// model only gets to read within the open project, not the rest of the filesystem.
fn resolve_path(cwd: &Path, rel: &str) -> Option<PathBuf> {
    let root = cwd.canonicalize().ok()?;
    let candidate = root.join(rel).canonicalize().ok()?;
    candidate.starts_with(&root).then_some(candidate)
}

/// Like `resolve_path`, but for a file that may not exist yet: canonicalizes the parent
/// directory (creating it if needed) instead of the file itself, and rejects anything whose
/// parent falls outside `cwd`.
fn resolve_write_path(cwd: &Path, rel: &str) -> Option<PathBuf> {
    let root = cwd.canonicalize().ok()?;
    let candidate = root.join(rel);
    let parent = candidate.parent()?;
    std::fs::create_dir_all(parent).ok()?;
    let canon_parent = parent.canonicalize().ok()?;
    if !canon_parent.starts_with(&root) {
        return None;
    }
    Some(canon_parent.join(candidate.file_name()?))
}

fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let truncated: String = s.chars().take(max_chars).collect();
    format!("{truncated}\n... [truncated]")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remembering_a_rejected_request_does_not_grant_permission() {
        let mut state = ChatState::default();
        let (respond, reply) = mpsc::channel();
        state.pending_permission = Some(PendingPermission { remember: false, label: "write_file({})".into(), diff: None, respond });
        let _ = update(&mut state, Message::RememberPermission(true), PathBuf::from("."));
        let _ = update(&mut state, Message::PermissionChosen(false), PathBuf::from("."));
        assert!(!reply.try_recv().unwrap());
        assert!(state.session_grants.is_empty());
    }

    #[test]
    fn project_changes_clear_grants_and_context() {
        let mut state = ChatState::default();
        state.set_project(Path::new("first"));
        state.session_grants.insert("write_file(...)".into());
        state.input = text_editor::Content::with_text("private project context");
        state.set_project(Path::new("second"));
        assert!(state.session_grants.is_empty());
        assert!(state.input.text().is_empty());
    }

    #[test]
    fn send_without_model_preserves_draft() {
        let mut state = ChatState::default();
        state.input = text_editor::Content::with_text("Explain this project");
        state.send(PathBuf::from("."));
        assert_eq!(state.input.text(), "Explain this project");
        assert!(state.messages.is_empty());
        assert!(!state.streaming);
    }

    #[test]
    fn edits_replace_exactly_one_match_unless_asked() {
        assert_eq!(apply_edit("a b a", "b", "c", false), Ok(("a c a".into(), 1)));
        assert!(apply_edit("a b a", "a", "c", false).unwrap_err().contains("2 places"));
        assert_eq!(apply_edit("a b a", "a", "c", true), Ok(("c b c".into(), 2)));
        assert!(apply_edit("a", "zzz", "c", false).unwrap_err().contains("not found"));
        assert!(apply_edit("a", "", "c", false).is_err());
        // CRLF files accept LF-written edits and keep their line endings.
        assert_eq!(apply_edit("one\r\ntwo\r\n", "one\ntwo", "1\n2", false), Ok(("1\r\n2\r\n".into(), 1)));
    }

    #[test]
    fn file_tools_preview_and_apply_edits_inside_the_project_only() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("lib.rs"), "fn a() {}\nfn b() {}\n").unwrap();
        let cancel = AtomicBool::new(false);
        let edit = r#"{"path":"lib.rs","old_string":"fn b() {}","new_string":"fn b() { a() }"}"#;
        let diff = proposed_diff(root, "edit_file", edit).unwrap();
        assert_eq!(diff.old.as_deref(), Some("fn a() {}\nfn b() {}\n"));
        assert_eq!(diff.new, "fn a() {}\nfn b() { a() }\n");
        assert_eq!(std::fs::read_to_string(root.join("lib.rs")).unwrap(), "fn a() {}\nfn b() {}\n", "previewing changes nothing");
        assert_eq!(execute_tool(root, "edit_file", edit, &cancel), "replaced 1 occurrence(s) in lib.rs");
        assert_eq!(std::fs::read_to_string(root.join("lib.rs")).unwrap(), diff.new);
        assert!(execute_tool(root, "edit_file", edit, &cancel).starts_with("error"), "the old text is gone now");
        assert!(proposed_diff(root, "edit_file", edit).is_none());

        let create = r#"{"path":"new/file.txt","content":"hi\n"}"#;
        let diff = proposed_diff(root, "write_file", create).unwrap();
        assert_eq!((diff.old, diff.new.as_str()), (None, "hi\n"));
        assert!(!root.join("new").exists(), "previewing a new file creates nothing");
        assert!(execute_tool(root, "edit_file", r#"{"path":"../outside.txt","old_string":"a","new_string":"b"}"#, &cancel).starts_with("error"));
        assert!(proposed_diff(root, "read_file", r#"{"path":"lib.rs"}"#).is_none());
    }

    /// Stands in for a long-running command when this test binary runs itself; see below.
    #[test]
    #[ignore]
    fn sleeping_child_process() {
        if std::env::var_os("EDITOR_TEST_SLEEP").is_some() {
            std::thread::sleep(Duration::from_secs(300));
        }
    }

    /// Commands run this test binary itself, so no shell or system program is needed.
    fn this_binary(args: &[&str]) -> std::process::Command {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command.args(args);
        command
    }

    #[test]
    fn processes_report_output_exit_codes_timeouts_and_cancellation() {
        let idle = AtomicBool::new(false);
        let listed = run_process(this_binary(&["--list"]), MAX_COMMAND_TIMEOUT, &idle).unwrap();
        assert_eq!(listed.code, Some(0));
        assert!(listed.text.contains("processes_report_output_exit_codes_timeouts_and_cancellation"), "{}", listed.text);
        assert!(listed.report(MAX_COMMAND_TIMEOUT).starts_with("exit code 0\n"));

        let failed = run_process(this_binary(&["--no-such-flag"]), MAX_COMMAND_TIMEOUT, &idle).unwrap();
        assert!(failed.code.is_some_and(|code| code != 0));
        assert!(failed.report(MAX_COMMAND_TIMEOUT).starts_with("error: exit code"));

        let sleeper = || {
            let mut command = this_binary(&["--ignored", "--exact", "chat::tests::sleeping_child_process"]);
            command.env("EDITOR_TEST_SLEEP", "1");
            command
        };
        let timed_out = run_process(sleeper(), Duration::from_millis(300), &idle).unwrap();
        assert!(timed_out.timed_out && !timed_out.cancelled && timed_out.code.is_none());
        assert!(timed_out.report(Duration::from_secs(1)).starts_with("error: timed out after 1s"));

        let stop = AtomicBool::new(true);
        let cancelled = run_process(sleeper(), MAX_COMMAND_TIMEOUT, &stop).unwrap();
        assert!(cancelled.cancelled && cancelled.code.is_none());
        assert!(cancelled.report(MAX_COMMAND_TIMEOUT).starts_with("error: stopped by the user"));

        let missing = std::process::Command::new(std::env::temp_dir().join("editor-no-such-program"));
        assert!(run_process(missing, MAX_COMMAND_TIMEOUT, &idle).is_err());
    }

    #[test]
    fn conversations_are_saved_per_project_without_images() {
        let store = tempfile::tempdir().unwrap();
        let new_state = || ChatState { store_dir: Some(store.path().to_path_buf()), ..ChatState::default() };
        let (first, second) = (store.path().join("first"), store.path().join("second"));
        let mut state = new_state();
        state.set_project(&first);
        state.messages.push(ChatMsg {
            wire_content: Some(serde_json::json!([{"type":"text","text":"What is this?"}, {"type":"image_url","image_url":{"url":"data:image/png;base64,AAAA"}}])),
            ..ChatMsg::new(Role::User, "What is this?\n[Attached: shot.png]".into())
        });
        state.messages.push(ChatMsg::new(Role::Assistant, "A **screenshot**.".into()));
        let _ = update(&mut state, Message::NewConversation, first.clone());
        assert_eq!(state.threads.len(), 2);
        assert!(state.messages.is_empty());
        state.set_project(&second);
        assert_eq!(state.threads.len(), 1, "another project starts empty");
        assert!(state.messages.is_empty());

        let mut reopened = new_state();
        reopened.set_project(&first);
        assert_eq!(reopened.threads.len(), 2);
        assert_eq!(reopened.threads[0].title, "What is this?");
        let saved = serde_json::to_string(&reopened.threads[0].messages[0].wire_content).unwrap();
        assert!(saved.contains("What is this?") && !saved.contains("base64"), "{saved}");
        let _ = update(&mut reopened, Message::History(crate::ai_history::Message::Switch(0)), first.clone());
        assert_eq!(reopened.messages.len(), 2);
        assert_eq!(reopened.messages[1].content, "A **screenshot**.");
        let _ = update(&mut reopened, Message::History(crate::ai_history::Message::Delete(0)), first.clone());
        assert_eq!(reopened.threads.len(), 1);
        assert!(reopened.messages.is_empty(), "deleting the open conversation opens the remaining one");
        assert!(ChatState::default().threads_path(&first).is_none(), "tests never write to the user's config");
    }

    #[test]
    fn tool_results_attach_diffs_and_notes_to_the_call() {
        let mut state = ChatState::default();
        let (tx, rx) = mpsc::channel();
        state.rx = Some(rx);
        state.streaming = true;
        let diff = FileDiff { path: "a.txt".into(), old: Some("a\n".into()), new: "b\n".into() };
        tx.send(Event::ToolStart("edit_file({})".into())).unwrap();
        tx.send(Event::ToolResult(Some(diff.clone()), String::new())).unwrap();
        tx.send(Event::ToolStart("run_command({})".into())).unwrap();
        tx.send(Event::ToolResult(None, "exit code 0\nok".into())).unwrap();
        state.poll();
        assert_eq!(state.diff(0), Some(&diff));
        assert_eq!(state.messages[2].content, "run_command({})\nexit code 0\nok");
        assert_eq!(state.diff(2), None);
    }

    #[test]
    fn changing_provider_discards_in_flight_model_discovery() {
        let mut state = ChatState::default();
        let (tx, rx) = mpsc::channel();
        state.test_rx = Some(rx);
        state.connection_status = ConnectionStatus::Testing;
        let _ = update(&mut state, Message::BaseUrlChanged("http://localhost:1234/v1".into()), PathBuf::from("."));
        assert!(tx.send(TestEvent::Success(vec!["stale-model".into()])).is_err());
        state.poll();
        assert!(state.models.is_empty());
        assert!(matches!(state.connection_status, ConnectionStatus::Idle));
    }
}
