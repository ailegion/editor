use iced::widget::{
    button, column, container, pick_list, row, scrollable, text, text_editor, text_input, Space,
};
use iced::{Element, Length, Task};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;

/// Tool calls loop at most this many rounds before the conversation is forced to stop, so a
/// model that keeps requesting tools (or a server that keeps claiming `tool_calls`) can't hang
/// the chat forever.
const MAX_TOOL_ROUNDS: usize = 8;

#[derive(Clone, Copy, PartialEq)]
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

struct ChatMsg {
    wire_content: Option<serde_json::Value>,
    role: Role,
    content: String,
}

enum Event {
    Delta(String),
    /// A tool call the model requested, formatted for display (e.g. `read_file(...)`\).
    /// Closes out the current assistant bubble and opens a fresh one for whatever text
    /// follows.
    ToolStart(String),
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
}

impl Default for ChatState {
    fn default() -> Self {
        Self {
            attachments: Vec::new(),
            session_grants: Default::default(),
            cwd: None,
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
    UsePrompt(String),
    Send,
    Stop,
    Copy(String),
    PermissionChosen(bool),
    RememberPermission(bool),
    ResetPermissions,
    OllamaPreset,
}

impl ChatState {
    pub fn set_project(&mut self, cwd: &Path) {
        if self.cwd.as_deref().is_some_and(|previous| previous != cwd) {
            self.stop();
            self.session_grants.clear();
            self.messages.clear();
            self.attachments.clear();
            self.input = text_editor::Content::new();
        }
        self.cwd = Some(cwd.to_path_buf());
    }

    pub fn poll(&mut self) {
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
                    self.messages.push(ChatMsg { wire_content: None, role: Role::Tool, content: label });
                    self.messages.push(ChatMsg { wire_content: None, role: Role::Assistant, content: String::new() });
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
        self.messages.push(ChatMsg { wire_content, role: Role::User, content: display });
        self.messages.push(ChatMsg {
            wire_content: None,
            role: Role::Assistant,
            content: String::new(),
        });

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
            if !state.streaming {
                state.messages.clear();
                state.attachments.clear();
                state.session_grants.clear();
            }
        }
        Message::UsePrompt(prompt) => state.input = text_editor::Content::with_text(&prompt),
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
    }
    Task::none()
}

pub fn conversation_controls(state: &ChatState) -> Element<'_, Message> {
    row![
        crate::icon_control(lucide_icons::Icon::Plus, "New conversation", (!state.streaming && !state.messages.is_empty()).then_some(Message::NewConversation), false),
    ].spacing(4).into()
}

pub fn view<'a>(state: &'a ChatState, composer: crate::ai_composer::Context<'a>) -> Element<'a, Message> {
    let mut header = column![row![
        text("Conversation").size(12),
        Space::new().width(Length::Fill),
        crate::icon_control(lucide_icons::Icon::Settings, "Model settings", Some(Message::ToggleSettings), state.settings_open),
    ]].spacing(4);

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
        messages_col = messages_col.push(column![
            row![
                text(message.role.label()).size(12),
                Space::new().width(Length::Fill),
                crate::icon_control(lucide_icons::Icon::Copy, "Copy message", Some(Message::Copy(message.content.clone())), false),
            ]
            .spacing(6)
            .align_y(iced::Alignment::Center),
            text(message.content.clone()).size(13),
        ]);
    }
    let messages = scrollable(messages_col.spacing(16)).anchor_bottom().height(Length::Fill);

    let mut bottom = column![].spacing(4);
    if let Some(pending) = &state.pending_permission {
        use crate::ai_approval::{Card, Choice, ChoiceKind};
        let (title, details) = pending.label.split_once('(').map(|(name, args)| {
            let details = serde_json::from_str::<serde_json::Value>(args.strip_suffix(')').unwrap_or(args))
                .map(|value| crate::ai_approval::format_input(&value)).unwrap_or_else(|_| args.to_string());
            (name.to_string(), details)
        }).unwrap_or_else(|| ("Review tool request".into(), pending.label.clone()));
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

            if !request_permission(&tx, &label) {
                let _ = tx.send(Event::ToolStart(format!("{label}  [denied]")));
                messages.push(serde_json::json!({
                    "role": "tool",
                    "tool_call_id": call.id,
                    "content": "error: user denied permission for this tool call",
                }));
                continue;
            }

            let _ = tx.send(Event::ToolStart(label));
            let result = execute_tool(&cwd, &call.name, &call.arguments);
            if call.name == "write_file" && !result.starts_with("error") {
                let _ = tx.send(Event::FileChanged);
            }
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
fn request_permission(tx: &mpsc::Sender<Event>, label: &str) -> bool {
    let (respond, response) = mpsc::channel();
    let request = PendingPermission { label: label.to_string(), remember: false, respond };
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

/// OpenAI-format `tools` array describing the functions the model may call: enough for the
/// model to read/search its way around the open project, plus `write_file` to create or
/// overwrite files in it.
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
    ])
}

fn execute_tool(cwd: &Path, name: &str, arguments: &str) -> String {
    let args: serde_json::Value = serde_json::from_str(arguments).unwrap_or(serde_json::Value::Null);
    match name {
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
        state.pending_permission = Some(PendingPermission { remember: false, label: "write_file({})".into(), respond });
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
