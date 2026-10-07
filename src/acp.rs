use iced::widget::{button, column, container, row, scrollable, text, text_editor, Space};
use iced::{Element, Length, Task};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::schema::v1::{
    CancelNotification, ContentBlock, InitializeRequest, LoadSessionRequest, NewSessionRequest,
    PermissionOptionId, PermissionOptionKind, PromptRequest, RequestPermissionOutcome, RequestPermissionRequest,
    RequestPermissionResponse, SelectedPermissionOutcome, SessionConfigValueId, SessionId, SessionNotification,
    SessionUpdate, SetSessionConfigOptionRequest, TextContent, ToolCallStatus,
};
use agent_client_protocol::{Agent, ConnectionTo};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
enum ToolStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
}

impl ToolStatus {
    fn icon(self) -> &'static str {
        match self {
            ToolStatus::Pending => "Queued",
            ToolStatus::InProgress => "Running",
            ToolStatus::Completed => "Done",
            ToolStatus::Failed => "Failed",
        }
    }
}

fn convert_status(status: ToolCallStatus) -> ToolStatus {
    match status {
        ToolCallStatus::Pending => ToolStatus::Pending,
        ToolCallStatus::InProgress => ToolStatus::InProgress,
        ToolCallStatus::Completed => ToolStatus::Completed,
        ToolCallStatus::Failed => ToolStatus::Failed,
        _ => ToolStatus::Pending,
    }
}

#[derive(Clone, Serialize, Deserialize)]
enum Entry {
    User { content: String },
    Thinking { content: String },
    Assistant { content: String },
    ToolCall {
        id: String,
        title: String,
        status: ToolStatus,
        #[serde(default)]
        details: String,
        /// File edits the tool made, for "View diff".
        #[serde(default)]
        diffs: Vec<crate::ai_diff::FileDiff>,
    },
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct Thread {
    title: String,
    session_id: Option<String>,
    entries: Vec<Entry>,
    /// Unix seconds of the last prompt, for ordering the history menu.
    #[serde(default)]
    updated: u64,
}

#[derive(Default, Serialize, Deserialize)]
struct ThreadStore {
    threads: Vec<Thread>,
    active: usize,
    /// Model the user last picked; applied to every new or resumed session.
    #[serde(default)]
    model: Option<String>,
    /// Effort level the user last picked; applied when the session's model offers it.
    #[serde(default)]
    effort: Option<String>,
}

/// One of the agent's select-style session config options (model or effort).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ModelOptions {
    pub(crate) config_id: String,
    pub(crate) current: String,
    choices: Vec<(String, String)>,
}

impl ModelOptions {
    fn current_name(&self) -> String {
        self.choices.iter().find(|(id, _)| id == &self.current).map_or_else(|| self.current.clone(), |(_, name)| name.clone())
    }
}

/// The model and effort selectors the agent reports; effort levels depend on the model.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct SessionConfig {
    model: Option<ModelOptions>,
    effort: Option<ModelOptions>,
}

impl SessionConfig {
    fn from_options(options: &[agent_client_protocol::schema::v1::SessionConfigOption]) -> Self {
        Self { model: model_from_options(options), effort: effort_from_options(options) }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Setting {
    Model,
    Effort,
}

struct Usage {
    used: u64,
    size: u64,
    cost: Option<(f64, String)>,
}

struct PendingPermission {
    title: String,
    details: String,
    allow_once: Option<String>,
    scope: Option<String>,
    options: Vec<(String, String, PermissionOptionKind)>,
    remember: bool,
    respond: tokio::sync::oneshot::Sender<String>,
}

struct PendingPrompt {
    content: Vec<ContentBlock>,
    cancel: tokio::sync::oneshot::Receiver<()>,
}

async fn run_prompt(
    connection: &ConnectionTo<Agent>,
    session_id: &SessionId,
    mut prompt: PendingPrompt,
) -> agent_client_protocol::Result<()> {
    // Stop during initialization must cancel this prompt, not an idle session.
    if !matches!(prompt.cancel.try_recv(), Err(tokio::sync::oneshot::error::TryRecvError::Empty)) {
        return Ok(());
    }
    let response = connection
        .send_request(PromptRequest::new(session_id.clone(), prompt.content))
        .block_task();
    tokio::pin!(response);
    tokio::select! {
        result = &mut response => { result?; }
        _ = &mut prompt.cancel => {
            connection.send_notification(CancelNotification::new(session_id.clone()))?;
            // Keep the turn active until the agent acknowledges its completion.
            response.await?;
        }
    }
    Ok(())
}

enum Event {
    Delta(String),
    ThoughtDelta(String),
    Usage(u64, u64, Option<(f64, String)>),
    Config(SessionConfig),
    SettingChanged(Setting, Result<SessionConfig, String>),
    SessionId(String),
    Permission(PendingPermission),
    ToolCall {
        id: String,
        title: String,
        status: ToolStatus,
    },
    ToolCallUpdate {
        id: String,
        title: Option<String>,
        status: Option<ToolStatus>,
    },
    ToolDetails(String, String, Vec<crate::ai_diff::FileDiff>),
    Done,
    Error(String),
}

pub struct AcpState {
    pub attachments: Vec<crate::ai_context::Attachment>,
    provider: &'static str,
    /// Where conversations are saved; `None` keeps them in memory only (tests).
    store_dir: Option<PathBuf>,
    session_grants: std::collections::HashSet<String>,
    loaded: bool,
    cwd: Option<PathBuf>,
    threads: Vec<Thread>,
    active_thread: usize,
    entries: Vec<Entry>,
    input: text_editor::Content,
    rx: Option<Receiver<Event>>,
    prompt_tx: Option<tokio::sync::mpsc::UnboundedSender<PendingPrompt>>,
    cancel_tx: Option<tokio::sync::oneshot::Sender<()>>,
    model_tx: Option<tokio::sync::mpsc::UnboundedSender<(Setting, String, String)>>,
    streaming: bool,
    started: bool,
    /// The session was already started for display once; see `connect_for_display`.
    auto_connected: bool,
    usage: Option<Usage>,
    models: Option<ModelOptions>,
    models_reported: bool,
    model_switching: bool,
    preferred_model: Option<String>,
    model_menu_open: bool,
    effort: Option<ModelOptions>,
    effort_switching: bool,
    preferred_effort: Option<String>,
    effort_menu_open: bool,
    usage_open: bool,
    pending_permission: Option<PendingPermission>,
    permission_queue: std::collections::VecDeque<PendingPermission>,
    just_finished: bool,
    history: crate::ai_history::Menu,
    stopping: bool,
    expanded_thinking: Vec<usize>,
    /// Selectable copies of `entries`' message text, kept in step by `sync_selectable`.
    selectable: crate::ai_selectable::Cache,
    /// Replies rendered as Markdown, kept in step with `selectable`.
    markdown: crate::ai_markdown::Cache,
}

impl Default for AcpState {
    fn default() -> Self {
        Self {
            attachments: Vec::new(),
            provider: "Claude",
            store_dir: None,
            session_grants: Default::default(),
            loaded: false,
            cwd: None,
            threads: Vec::new(),
            active_thread: 0,
            entries: Vec::new(),
            input: text_editor::Content::new(),
            rx: None,
            prompt_tx: None,
            cancel_tx: None,
            model_tx: None,
            streaming: false,
            started: false,
            auto_connected: false,
            usage: None,
            models: None,
            models_reported: false,
            model_switching: false,
            preferred_model: None,
            model_menu_open: false,
            effort: None,
            effort_switching: false,
            preferred_effort: None,
            effort_menu_open: false,
            usage_open: false,
            pending_permission: None,
            permission_queue: Default::default(),
            just_finished: false,
            history: Default::default(),
            stopping: false,
            expanded_thinking: Vec::new(),
            selectable: Default::default(),
            markdown: Default::default(),
        }
    }
}

#[derive(Debug, Clone)]
pub enum Message {
    Composer(crate::ai_composer::Action),
    ToggleUsage,
    CloseUsage,
    ToggleModelMenu,
    SelectModel(String),
    ToggleEffortMenu,
    SelectEffort(String),
    NewThread,
    History(crate::ai_history::Message),
    ToggleThinking(usize),
    Markdown(crate::ai_markdown::Action),
    /// Open diff `.1` of tool call entry `.0` in the preview pane (handled by the app).
    ViewDiff(usize, usize),
    UsePrompt(String),
    InputChanged(text_editor::Action),
    Send,
    Stop,
    PermissionChosen(String),
    RememberPermission(bool),
    ResetPermissions,
    Copy(String),
    /// Selection or caret movement inside message `usize`'s text.
    Selectable(usize, text_editor::Action),
}

impl AcpState {
    pub fn claude() -> Self { Self { store_dir: crate::config_path("acp_threads"), ..Self::default() } }
    pub fn codex() -> Self { Self { provider: "Codex", ..Self::claude() } }

    pub fn provider(&self) -> &'static str { self.provider }

    /// The model the user last picked, applied to new sessions.
    pub fn preferred_model(&self) -> Option<String> { self.preferred_model.clone() }

    pub fn input_text(&self) -> String { self.input.text() }

    /// Replaces the draft, leaving the caret at the end.
    pub fn set_input(&mut self, text: &str) {
        self.input = text_editor::Content::with_text(text);
        self.input.perform(text_editor::Action::Move(text_editor::Motion::DocumentEnd));
    }

    /// Diff `index` of the tool call at entry `entry`.
    pub fn diff(&self, entry: usize, index: usize) -> Option<&crate::ai_diff::FileDiff> {
        match self.entries.get(entry)? {
            Entry::ToolCall { diffs, .. } => diffs.get(index),
            _ => None,
        }
    }

    /// Keeps the selectable and Markdown text in step with `entries`; called after every change.
    fn sync_selectable(&mut self) {
        self.selectable.sync(self.entries.iter().map(|entry| match entry {
            Entry::User { content } | Entry::Assistant { content } => content.as_str(),
            _ => "",
        }));
        self.markdown.sync(self.entries.iter().map(|entry| match entry {
            Entry::Assistant { content } => content.as_str(),
            _ => "",
        }));
    }

    pub fn poll(&mut self) {
        self.poll_events();
        self.sync_selectable();
    }

    /// Returns true (once) the first time this is called after a turn finishes,
    /// so the caller can react (e.g. reload files the agent may have edited).
    pub fn take_finished(&mut self) -> bool {
        std::mem::take(&mut self.just_finished)
    }

    fn poll_events(&mut self) {
        let Some(rx) = &self.rx else { return };
        let mut finished = false;
        let mut new_session_id = None;
        let mut transcript_changed = false;
        while let Ok(event) = rx.try_recv() {
            transcript_changed |= matches!(&event, Event::Delta(_) | Event::ThoughtDelta(_) | Event::ToolCall { .. } | Event::ToolCallUpdate { .. } | Event::ToolDetails(..) | Event::Error(_));
            match event {
                Event::Delta(text) => Self::append_chunk(&mut self.entries, text, false),
                Event::ThoughtDelta(text) => Self::append_chunk(&mut self.entries, text, true),
                Event::Config(config) => {
                    self.models = config.model;
                    self.effort = config.effort;
                    self.models_reported = true;
                }
                Event::SettingChanged(setting, result) => {
                    match setting {
                        Setting::Model => self.model_switching = false,
                        Setting::Effort => self.effort_switching = false,
                    }
                    transcript_changed = true;
                    match result {
                        Ok(config) => {
                            // Remember only what the user picked, not what the agent derived from it.
                            match setting {
                                Setting::Model => if let Some(models) = &config.model { self.preferred_model = Some(models.current.clone()); },
                                Setting::Effort => if let Some(effort) = &config.effort { self.preferred_effort = Some(effort.current.clone()); },
                            }
                            self.models = config.model;
                            self.effort = config.effort;
                        }
                        Err(err) => {
                            let what = match setting { Setting::Model => "model", Setting::Effort => "effort" };
                            Self::append_chunk(&mut self.entries, format!("\n[error: could not change {what}: {err}]"), false);
                        }
                    }
                }
                Event::Usage(used, size, cost) => {
                    self.usage = Some(Usage { used, size, cost });
                }
                Event::SessionId(id) => new_session_id = Some(id),
                Event::Permission(pending) => {
                    if self.stopping || finished { continue; }
                    if pending.scope.as_ref().is_some_and(|scope| self.session_grants.contains(scope)) && pending.allow_once.is_some() {
                        let _ = pending.respond.send(pending.allow_once.unwrap());
                    } else { self.permission_queue.push_back(pending); }
                }
                Event::ToolCall { id, title, status } => {
                    Self::upsert_tool_call(&mut self.entries, id, Some(title), Some(status));
                }
                Event::ToolCallUpdate { id, title, status } => {
                    Self::upsert_tool_call(&mut self.entries, id, title, status);
                }
                Event::ToolDetails(id, details, diffs) => {
                    if let Some(Entry::ToolCall { details: current, diffs: current_diffs, .. }) = self.entries.iter_mut().find(|entry| matches!(entry, Entry::ToolCall { id: existing, .. } if existing == &id)) {
                        if !details.is_empty() { *current = details; }
                        if !diffs.is_empty() { *current_diffs = diffs; }
                    }
                }
                Event::Done => finished = true,
                Event::Error(err) => {
                    Self::append_chunk(&mut self.entries, format!("\n[error: {err}]"), false);
                    finished = true;
                }
            }
        }
        if self.pending_permission.is_none() { self.pending_permission = self.permission_queue.pop_front(); }
        let disconnected = self.prompt_tx.as_ref().is_some_and(|tx| tx.is_closed());
        if disconnected {
            self.model_switching = false;
            self.effort_switching = false;
            self.session_grants.clear();
            self.started = false;
            self.prompt_tx = None;
            self.cancel_tx = None;
            self.model_tx = None;
            finished = true;
        }
        if let Some(id) = new_session_id {
            if let Some(thread) = self.threads.get_mut(self.active_thread) {
                thread.session_id = Some(id);
            }
        }
        if disconnected { self.rx = None; }
        if finished {
            self.streaming = false;
            self.stopping = false;
            self.cancel_tx = None;
            self.pending_permission = None;
            self.permission_queue.clear();
            self.just_finished = true;
        }
        // A notification dispatched just after the prompt result must also be saved.
        if finished || (!self.streaming && transcript_changed) {
            self.save_current_thread();
            if let Some(cwd) = self.cwd.clone() {
                self.persist(&cwd);
            }
        }
    }

    /// Coalesce only adjacent chunks. Tool activity closes the preceding reply,
    /// so a later summary is appended below it instead of modifying an old bubble.
    fn append_chunk(entries: &mut Vec<Entry>, chunk: String, thinking: bool) {
        if chunk.is_empty() { return; }
        match entries.last_mut() {
            Some(Entry::Assistant { content }) if !thinking => content.push_str(&chunk),
            Some(Entry::Thinking { content }) if thinking => content.push_str(&chunk),
            _ if thinking => entries.push(Entry::Thinking { content: chunk }),
            _ => entries.push(Entry::Assistant { content: chunk }),
        }
    }

    fn upsert_tool_call(
        entries: &mut Vec<Entry>,
        id: String,
        title: Option<String>,
        status: Option<ToolStatus>,
    ) {
        for entry in entries.iter_mut() {
            if let Entry::ToolCall {
                id: existing_id,
                title: t,
                status: s,
                ..
            } = entry
            {
                if *existing_id == id {
                    if let Some(title) = title {
                        *t = title;
                    }
                    if let Some(status) = status {
                        *s = status;
                    }
                    return;
                }
            }
        }
        entries.push(Entry::ToolCall {
            id,
            title: title.unwrap_or_else(|| "Tool call".to_string()),
            status: status.unwrap_or(ToolStatus::Pending),
            details: String::new(),
            diffs: Vec::new(),
        });
    }

    pub fn ensure_loaded(&mut self, cwd: &Path) {
        if self.loaded && self.cwd.as_deref() != Some(cwd) {
            self.save_current_thread();
            if let Some(previous) = self.cwd.clone() { self.persist(&previous); }
            self.stop();
            self.reset_connection();
            self.entries.clear();
            self.input = text_editor::Content::new();
            self.attachments.clear();
            self.usage = None;
            self.loaded = false;
        }
        self.cwd = Some(cwd.to_path_buf());
        if self.loaded {
            return;
        }
        self.loaded = true;

        if let Some(path) = self.threads_path(cwd) {
            if let Ok(text) = std::fs::read_to_string(&path) {
                if let Ok(store) = serde_json::from_str::<ThreadStore>(&text) {
                    self.preferred_model = store.model;
                    self.preferred_effort = store.effort;
                    if !store.threads.is_empty() {
                        self.threads = store.threads;
                        self.active_thread = store.active.min(self.threads.len() - 1);
                        self.entries = self.threads[self.active_thread].entries.clone();
                        return;
                    }
                }
            }
        }
        self.threads = vec![Thread::default()];
        self.active_thread = 0;
    }

    fn save_current_thread(&mut self) {
        if let Some(thread) = self.threads.get_mut(self.active_thread) {
            thread.entries = self.entries.clone();
            if thread.title.is_empty() {
                let first_user = self.entries.iter().find_map(|e| match e {
                    Entry::User { content } => Some(content.clone()),
                    _ => None,
                });
                if let Some(content) = first_user {
                    thread.title = content.chars().take(40).collect();
                }
            }
        }
    }

    fn threads_path(&self, cwd: &Path) -> Option<PathBuf> {
        Some(self.store_dir.as_ref()?.join(threads_file(cwd, self.provider)))
    }

    fn persist(&self, cwd: &Path) {
        let Some(path) = self.threads_path(cwd) else { return };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let store = ThreadStore {
            threads: self.threads.clone(),
            active: self.active_thread,
            model: self.preferred_model.clone(),
            effort: self.preferred_effort.clone(),
        };
        if let Ok(text) = serde_json::to_string(&store) {
            let _ = std::fs::write(path, text);
        }
    }

    fn reset_connection(&mut self) {
        self.session_grants.clear();
        self.models = None;
        self.models_reported = false;
        self.model_switching = false;
        self.model_menu_open = false;
        self.effort = None;
        self.effort_switching = false;
        self.effort_menu_open = false;
        self.model_tx = None;
        self.usage_open = false;
        self.stopping = false;
        self.expanded_thinking.clear();
        self.started = false;
        self.auto_connected = false;
        self.rx = None;
        self.prompt_tx = None;
        self.cancel_tx = None;
        self.streaming = false;
        self.pending_permission = None;
        self.permission_queue.clear();
    }

    fn start(&mut self, cwd: PathBuf, resume_session_id: Option<String>) {
        if self.started {
            return;
        }
        self.started = true;

        let (tx, rx): (Sender<Event>, Receiver<Event>) = mpsc::channel();
        self.rx = Some(rx);
        let (prompt_tx, mut prompt_rx) = tokio::sync::mpsc::unbounded_channel::<PendingPrompt>();
        self.prompt_tx = Some(prompt_tx);
        let (model_tx, mut model_rx) = tokio::sync::mpsc::unbounded_channel::<(Setting, String, String)>();
        self.model_tx = Some(model_tx);

        let provider = self.provider;
        let preferred_model = self.preferred_model.clone();
        let preferred_effort = self.preferred_effort.clone();
        std::thread::spawn(move || {
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(err) => {
                    let _ = tx.send(Event::Error(err.to_string()));
                    return;
                }
            };

            runtime.block_on(async move {
                let agent = match crate::agent_launch::agent(if provider == "Codex" { "@agentclientprotocol/codex-acp" } else { "@agentclientprotocol/claude-agent-acp" })
                {
                    Ok(agent) => agent,
                    Err(err) => {
                        let _ = tx.send(Event::Error(err));
                        return;
                    }
                };

                let notify_tx = tx.clone();
                let loop_tx = tx.clone();
                let permission_tx = tx.clone();
                let accepting = Arc::new(AtomicBool::new(false));
                let notify_accepting = accepting.clone();
                let loop_accepting = accepting.clone();

                let result = agent_client_protocol::Client
                    .builder()
                    .on_receive_notification(
                        async move |notification: SessionNotification, _cx| {
                            if !notify_accepting.load(Ordering::Relaxed) {
                                return Ok(());
                            }
                            match notification.update {
                                SessionUpdate::AgentMessageChunk(chunk) => {
                                    if let ContentBlock::Text(text) = chunk.content {
                                        let _ = notify_tx.send(Event::Delta(text.text));
                                    }
                                }
                                SessionUpdate::AgentThoughtChunk(chunk) => {
                                    if let ContentBlock::Text(text) = chunk.content {
                                        let _ = notify_tx.send(Event::ThoughtDelta(text.text));
                                    }
                                }
                                SessionUpdate::ConfigOptionUpdate(update) => {
                                    let _ = notify_tx.send(Event::Config(SessionConfig::from_options(&update.config_options)));
                                }
                                SessionUpdate::UsageUpdate(usage) => {
                                    let cost = usage.cost.map(|c| (c.amount, c.currency));
                                    let _ =
                                        notify_tx.send(Event::Usage(usage.used, usage.size, cost));
                                }
                                SessionUpdate::ToolCall(tool_call) => {
                                    let details = tool_details(&tool_call.content, tool_call.raw_output.as_ref().or(tool_call.raw_input.as_ref()));
                                    let diffs = tool_diffs(&tool_call.content);
                                    let id = tool_call.tool_call_id.0.to_string();
                                    let _ = notify_tx.send(Event::ToolCall {
                                        id: tool_call.tool_call_id.0.to_string(),
                                        title: tool_call.title,
                                        status: convert_status(tool_call.status),
                                    });
                                    let _ = notify_tx.send(Event::ToolDetails(id, details, diffs));
                                }
                                SessionUpdate::ToolCallUpdate(update) => {
                                    let content = update.fields.content.as_deref().unwrap_or_default();
                                    let details = tool_details(content, update.fields.raw_output.as_ref().or(update.fields.raw_input.as_ref()));
                                    let diffs = tool_diffs(content);
                                    let id = update.tool_call_id.0.to_string();
                                    let _ = notify_tx.send(Event::ToolCallUpdate {
                                        id: update.tool_call_id.0.to_string(),
                                        title: update.fields.title,
                                        status: update.fields.status.map(convert_status),
                                    });
                                    let _ = notify_tx.send(Event::ToolDetails(id, details, diffs));
                                }
                                _ => {}
                            }
                            Ok(())
                        },
                        agent_client_protocol::on_receive_notification!(),
                    )
                    .on_receive_request(
                        async move |request: RequestPermissionRequest, responder, connection| {
                            let title = request
                                .tool_call
                                .fields
                                .title
                                .clone()
                                .unwrap_or_else(|| "Permission requested".to_string());
                            let options: Vec<(String, String, PermissionOptionKind)> = request
                                .options
                                .iter()
                                .map(|opt| (opt.option_id.0.to_string(), opt.name.clone(), opt.kind))
                                .collect();

                            let allow_once = request.options.iter().find(|o| o.kind == agent_client_protocol::schema::v1::PermissionOptionKind::AllowOnce).map(|o| o.option_id.0.to_string());
                            // Reuse approval only for an identical operation in this live session.
                            let scope = request.tool_call.fields.raw_input.as_ref().map(|input| format!("{title}:{input}"));
                            let mut details = tool_details(request.tool_call.fields.content.as_deref().unwrap_or_default(), None);
                            if let Some(input) = &request.tool_call.fields.raw_input {
                                if !details.is_empty() { details.push_str("\n\n"); }
                                details.push_str(&crate::ai_approval::format_input(input));
                            }
                            let (respond_tx, respond_rx) = tokio::sync::oneshot::channel::<String>();
                            let _ = permission_tx.send(Event::Permission(PendingPermission {
                                title,
                                details, allow_once, scope,
                                options,
                                remember: false,
                                respond: respond_tx,
                            }));

                            // Waiting for UI approval must not block protocol messages, including Stop.
                            connection.spawn(async move { match respond_rx.await {
                                Ok(option_id) => responder.respond(RequestPermissionResponse::new(
                                    RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(
                                        PermissionOptionId::new(option_id),
                                    )),
                                )),
                                Err(_) => responder.respond(RequestPermissionResponse::new(
                                    RequestPermissionOutcome::Cancelled,
                                )),
                            } })
                        },
                        agent_client_protocol::on_receive_request!(),
                    )
                    .connect_with(agent, move |connection: ConnectionTo<Agent>| async move {
                        let init_response = connection
                            .send_request(InitializeRequest::new(ProtocolVersion::V1))
                            .block_task()
                            .await?;

                        let mut loaded_session_id = None;
                        let mut config_options = Vec::new();
                        if init_response.agent_capabilities.load_session {
                            if let Some(id) = resume_session_id.clone() {
                                let loaded = connection
                                    .send_request(LoadSessionRequest::new(id.clone(), cwd.clone()))
                                    .block_task()
                                    .await;
                                if let Ok(loaded) = loaded {
                                    config_options = loaded.config_options.unwrap_or_default();
                                    loaded_session_id = Some(SessionId::new(id));
                                }
                            }
                        }

                        let session_id = match loaded_session_id {
                            Some(id) => id,
                            None => {
                                let session = connection
                                    .send_request(NewSessionRequest::new(cwd))
                                    .block_task()
                                    .await?;
                                config_options = session.config_options.unwrap_or_default();
                                session.session_id
                            }
                        };
                        // Apply the remembered model before the first prompt can be sent.
                        let mut config = SessionConfig::from_options(&config_options);
                        if let Some(preferred) = &preferred_model {
                            let _ = loop_tx.send(Event::Config(config.clone()));
                            let options = config.model.as_ref().ok_or_else(|| agent_client_protocol::Error::internal_error().data("Agent did not report a model; saved selection cannot be confirmed"))?;
                            if !is_same_model(preferred, &options.current) {
                                // A saved model that cannot be applied must not block chatting with the agent's current model.
                                let config_id = options.config_id.clone();
                                match set_config_option(&connection, &session_id, Setting::Model, &config_id, preferred).await {
                                    Ok(changed) => config = changed,
                                    Err(err) => { let _ = loop_tx.send(Event::SettingChanged(Setting::Model, Err(err))); }
                                }
                            }
                        }
                        // Effort levels differ per model; a saved level this model lacks keeps the agent's choice.
                        if let (Some(preferred), Some(effort)) = (&preferred_effort, &config.effort) {
                            if &effort.current != preferred && effort.choices.iter().any(|(id, _)| id == preferred) {
                                let config_id = effort.config_id.clone();
                                match set_config_option(&connection, &session_id, Setting::Effort, &config_id, preferred).await {
                                    Ok(changed) => config = changed,
                                    Err(err) => { let _ = loop_tx.send(Event::SettingChanged(Setting::Effort, Err(err))); }
                                }
                            }
                        }
                        let _ = loop_tx.send(Event::Config(config));
                        let _ = loop_tx.send(Event::SessionId(session_id.0.to_string()));
                        loop_accepting.store(true, Ordering::Relaxed);

                        let model_connection = connection.clone();
                        let model_session_id = session_id.clone();
                        let model_event_tx = loop_tx.clone();
                        tokio::spawn(async move {
                            while let Some((setting, config_id, value)) = model_rx.recv().await {
                                let result = set_config_option(&model_connection, &model_session_id, setting, &config_id, &value).await;
                                let _ = model_event_tx.send(Event::SettingChanged(setting, result));
                            }
                        });

                        while let Some(prompt) = prompt_rx.recv().await {
                            if !init_response.agent_capabilities.prompt_capabilities.image && prompt.content.iter().any(|b| matches!(b, ContentBlock::Image(_))) {
                                let _ = loop_tx.send(Event::Error("This agent does not support image prompts. Remove images and try again.".into()));
                                continue;
                            }
                            let result = run_prompt(&connection, &session_id, prompt).await;
                            match result {
                                Ok(_) => {
                                    let _ = loop_tx.send(Event::Done);
                                }
                                Err(err) => {
                                    let _ = loop_tx.send(Event::Error(crate::agent_launch::describe(&err)));
                                }
                            }
                        }
                        Ok(())
                    })
                    .await;

                if let Err(err) = result {
                    let _ = tx.send(Event::Error(crate::agent_launch::describe(&err)));
                }
            });
        });
    }

    fn send(&mut self, cwd: PathBuf) {
        let text = self.input.text();
        if (text.trim().is_empty() && self.attachments.is_empty()) || self.streaming || self.model_switching || self.effort_switching {
            return;
        }
        let text = text.trim_end().to_string();
        self.input = text_editor::Content::new();
        self.connect(cwd);
        let mut prompt = vec![ContentBlock::Text(TextContent::new(text.clone()))];
        let mut display = text;
        for attachment in self.attachments.drain(..) {
            display.push_str(&format!("\n[Attached: {}]", attachment.name));
            prompt.push(attachment.acp());
        }
        self.entries.push(Entry::User { content: display });
        if let Some(thread) = self.threads.get_mut(self.active_thread) { thread.updated = crate::ai_history::now(); }
        self.streaming = true;
        if let Some(tx) = &self.prompt_tx {
            let (cancel_tx, cancel) = tokio::sync::oneshot::channel();
            self.cancel_tx = Some(cancel_tx);
            let _ = tx.send(PendingPrompt { content: prompt, cancel });
        }
    }

    /// Starts (or resumes) the active thread's agent session if it is not running yet.
    fn connect(&mut self, cwd: PathBuf) {
        let resume_session_id = self
            .threads
            .get(self.active_thread)
            .and_then(|t| t.session_id.clone());
        self.start(cwd, resume_session_id);
    }

    /// Starts the session while the panel is merely showing, so the model in use is known
    /// before the first prompt. Done once per conversation: a start that failed is retried
    /// by sending or opening the model menu, not on every tick.
    pub fn connect_for_display(&mut self, cwd: &Path) {
        // Not before `npx` is known to exist: without it the start would only add an error.
        if crate::agent_launch::node_found() && self.claim_auto_connect() {
            self.connect(cwd.to_path_buf());
        }
    }

    fn claim_auto_connect(&mut self) -> bool {
        !self.started && !std::mem::replace(&mut self.auto_connected, true)
    }

    /// What the model button shows: the session's model, else the saved choice it will get.
    fn model_label(&self) -> String {
        if self.model_switching { return "Switching…".into(); }
        let name = match (&self.models, &self.preferred_model) {
            (Some(models), _) => models.current_name(),
            (None, Some(preferred)) => preferred.clone(),
            (None, None) if self.started && !self.models_reported => "Connecting…".into(),
            (None, None) => "Model".into(),
        };
        format!("{name} ▾")
    }

    fn select_model(&mut self, value: String) {
        if self.streaming || self.model_switching || self.effort_switching { return; }
        self.model_menu_open = false;
        let Some(models) = &mut self.models else { return };
        if models.current == value { return; }
        if let Some(tx) = &self.model_tx {
            if tx.send((Setting::Model, models.config_id.clone(), value)).is_ok() { self.model_switching = true; }
        }
    }

    fn select_effort(&mut self, value: String) {
        if self.streaming || self.model_switching || self.effort_switching { return; }
        self.effort_menu_open = false;
        let Some(effort) = &self.effort else { return };
        if effort.current == value { return; }
        if let Some(tx) = &self.model_tx {
            if tx.send((Setting::Effort, effort.config_id.clone(), value)).is_ok() { self.effort_switching = true; }
        }
    }

    fn stop(&mut self) {
        if let Some(tx) = self.cancel_tx.take() {
            let _ = tx.send(());
        }
        self.stopping = true;
        self.pending_permission = None;
        self.permission_queue.clear();
    }

    fn new_thread(&mut self, cwd: &Path) {
        if self.streaming { return; }
        self.save_current_thread();
        self.persist(cwd);
        self.threads.push(Thread { updated: crate::ai_history::now(), ..Thread::default() });
        self.active_thread = self.threads.len() - 1;
        self.entries.clear();
        self.input = text_editor::Content::new();
        self.attachments.clear();
        self.usage = None;
        self.reset_connection();
        self.persist(cwd);
    }

    fn switch_thread(&mut self, index: usize, cwd: &Path) {
        if self.streaming || index >= self.threads.len() || index == self.active_thread {
            return;
        }
        self.save_current_thread();
        self.persist(cwd);
        self.active_thread = index;
        self.attachments.clear();
        self.input = text_editor::Content::new();
        self.entries = self.threads[index].entries.clone();
        self.usage = None;
        self.reset_connection();
        self.persist(cwd);
    }

    /// The history menu's view of the threads; message text is included only while searching.
    fn history_items(&self) -> Vec<crate::ai_history::Item> {
        let searching = self.history.open;
        self.threads.iter().enumerate().map(|(index, thread)| {
            let entries = if index == self.active_thread { &self.entries } else { &thread.entries };
            let text = if searching {
                entries.iter().filter_map(|entry| match entry {
                    Entry::User { content } | Entry::Assistant { content } => Some(content.as_str()),
                    _ => None,
                }).collect::<Vec<_>>().join("\n")
            } else { String::new() };
            crate::ai_history::Item { index, title: thread.title.clone(), updated: thread.updated, text }
        }).collect()
    }

    fn rename_thread(&mut self, index: usize, title: String) {
        if let Some(thread) = self.threads.get_mut(index) { thread.title = title; }
    }

    /// Removes a thread; deleting the open one opens the newest remaining (or a fresh) thread.
    fn delete_thread(&mut self, index: usize) {
        if self.streaming || index >= self.threads.len() {
            return;
        }
        self.save_current_thread();
        self.threads.remove(index);
        if index == self.active_thread {
            if self.threads.is_empty() {
                self.threads.push(Thread::default());
            }
            self.active_thread = self.threads.len() - 1;
            self.entries = self.threads[self.active_thread].entries.clone();
            self.attachments.clear();
            self.input = text_editor::Content::new();
            self.usage = None;
            self.reset_connection();
        } else if index < self.active_thread {
            self.active_thread -= 1;
        }
    }
}

pub fn update(state: &mut AcpState, message: Message, cwd: PathBuf) -> Task<Message> {
    state.ensure_loaded(&cwd);
    state.cwd = Some(cwd.clone());
    match message {
        Message::Composer(_) => {},
        Message::ToggleUsage => state.usage_open = !state.usage_open,
        Message::CloseUsage => state.usage_open = false,
        Message::ToggleModelMenu => {
            state.model_menu_open = !state.model_menu_open;
            state.effort_menu_open = false;
            // Models are only reported once a session exists, so connect early to list them.
            if state.model_menu_open { state.connect(cwd); }
        }
        Message::SelectModel(value) => {
            state.select_model(value);
            state.persist(&cwd);
        }
        Message::ToggleEffortMenu => {
            state.effort_menu_open = !state.effort_menu_open;
            state.model_menu_open = false;
        }
        Message::SelectEffort(value) => {
            state.select_effort(value);
            state.persist(&cwd);
        }
        Message::NewThread => state.new_thread(&cwd),
        Message::History(message) => match state.history.update(message) {
            Some(crate::ai_history::Action::Switch(i)) => state.switch_thread(i, &cwd),
            Some(crate::ai_history::Action::Delete(i)) => {
                state.delete_thread(i);
                state.persist(&cwd);
            }
            Some(crate::ai_history::Action::Rename(i, title)) => {
                state.rename_thread(i, title);
                state.persist(&cwd);
            }
            None => {}
        },
        Message::Markdown(crate::ai_markdown::Action::Copy(text)) => return iced::clipboard::write(text),
        Message::Markdown(crate::ai_markdown::Action::Link(url)) => crate::ai_markdown::open_link(&url),
        // Inserting into the editor and opening diffs are the app's to handle.
        Message::Markdown(crate::ai_markdown::Action::Insert(_)) | Message::ViewDiff(..) => {}
        Message::UsePrompt(prompt) => state.set_input(&prompt),
        Message::ToggleThinking(index) => {
            if state.expanded_thinking.contains(&index) {
                state.expanded_thinking.retain(|i| *i != index);
            } else {
                state.expanded_thinking.push(index);
            }
        }
        Message::InputChanged(action) => state.input.perform(action),
        Message::Send => state.send(cwd),
        Message::Stop => state.stop(),
        Message::ResetPermissions => state.session_grants.clear(),
        Message::RememberPermission(remember) => {
            if let Some(pending) = &mut state.pending_permission { pending.remember = remember; }
        }
        Message::PermissionChosen(option_id) => {
            if let Some(pending) = state.pending_permission.take() {
                if pending.remember && pending.allow_once.as_deref() == Some(option_id.as_str()) {
                    if let Some(scope) = pending.scope { state.session_grants.insert(scope); }
                }
                let _ = pending.respond.send(option_id);
            }
        }
        Message::Copy(text) => return iced::clipboard::write(text),
        Message::Selectable(index, action) => state.selectable.perform(index, action),
    }
    state.sync_selectable();
    Task::none()
}

pub fn conversation_controls(state: &AcpState) -> Element<'_, Message> {
    row![
        crate::icon_control(lucide_icons::Icon::Plus, "New conversation", (!state.streaming).then_some(Message::NewThread), false),
        crate::icon_control(lucide_icons::Icon::History, "Conversation history", Some(Message::History(crate::ai_history::Message::Toggle)), state.history.open),
    ]
    .spacing(4)
    .into()
}

pub fn view<'a>(state: &'a AcpState, cwd: PathBuf, composer: crate::ai_composer::Context<'a>) -> Element<'a, Message> {
    let mut header = column![].spacing(4);
    if state.history.open {
        header = header.push(crate::ai_history::view(&state.history, state.history_items(), state.active_thread, !state.streaming).map(Message::History));
    }
    let project = cwd.file_name().unwrap_or(cwd.as_os_str()).to_string_lossy().into_owned();
    header = header.push(text(format!("Project · {project}")).size(12).style(iced::widget::text::secondary));
    if crate::agent_launch::node_missing() {
        header = header.push(container(text(crate::agent_launch::NODE_MISSING).size(12)).padding(8).width(Length::Fill).style(container::danger));
    }

    let labeled_copyable = |label: &'static str, index: usize, content: &str, markdown: bool| -> Element<'_, Message> {
        // Replies render as Markdown, selectable as rendered. Both that and the selectable
        // text of other entries need their cache to have caught up with this entry; plain
        // text is the fallback.
        let rendered = markdown.then(|| crate::ai_markdown::view(&state.markdown, index, composer.theme, Message::Markdown)).flatten();
        let body: Element<'_, Message> = match (rendered, state.selectable.get(index)) {
            (Some(rendered), _) => rendered,
            (_, Some(selectable)) => crate::ai_selectable::view(selectable, 13.0, move |action| Message::Selectable(index, action)),
            _ => text(content.to_string()).size(13).into(),
        };
        let heading = row![text(label).size(12), Space::new().width(Length::Fill)].spacing(6).align_y(iced::Alignment::Center);
        column![
            heading.push(crate::icon_control(lucide_icons::Icon::Copy, "Copy message", Some(Message::Copy(content.to_string())), false)),
            body,
        ]
        .spacing(4)
        .into()
    };

    let mut messages = column![].spacing(12);
    if state.entries.is_empty() {
        messages = messages.push(container(column![
            text("What would you like to work on?").size(20),
            text(format!("{} can explore your project, edit files, and help you work through a change.", state.provider))
                .size(13).style(iced::widget::text::secondary),
            button("Explain this project").style(crate::flat_button_style)
                .on_press(Message::UsePrompt("Explore this project and explain its structure and main entry points.".into())),
            button("Find and fix a bug").style(crate::flat_button_style)
                .on_press(Message::UsePrompt("Help me investigate a bug in this project: ".into())),
            button("Review my changes").style(crate::flat_button_style)
                .on_press(Message::UsePrompt("Review my current changes for bugs and regressions. Explain your findings before editing.".into())),
        ].spacing(12)).padding([24, 8]));
    }
    for (i, entry) in state.entries.iter().enumerate() {
        if let Entry::Thinking { content } | Entry::Assistant { content } = entry {
            if content.is_empty() {
                continue;
            }
        }
        let card: Element<'_, Message> = match entry {
            Entry::ToolCall { title, status, details, diffs, .. } => {
                let expanded = state.expanded_thinking.contains(&i);
                let status = *status;
                let badge = container(text(status.icon()).size(10))
                    .padding([3, 7])
                    .style(move |theme: &iced::Theme| {
                        let palette = theme.extended_palette();
                        let pair = match status {
                            ToolStatus::Completed => palette.success.weak,
                            ToolStatus::Failed => palette.danger.weak,
                            ToolStatus::InProgress => palette.primary.weak,
                            ToolStatus::Pending => palette.secondary.weak,
                        };
                        iced::widget::container::Style {
                            background: Some(pair.color.into()), text_color: Some(pair.text),
                            border: iced::Border::default().rounded(4), ..Default::default()
                        }
                    });
                let heading = row![
                    text("Tool activity").size(11).style(iced::widget::text::secondary),
                    Space::new().width(Length::Fill), badge,
                    text(if details.is_empty() { "" } else if expanded { "Hide" } else { "Details" }).size(11),
                ].spacing(8).align_y(iced::Alignment::Center);
                let summary = column![heading, text(title.clone()).size(12).width(Length::Fill)].spacing(6);
                let mut card = column![button(summary).padding(0).width(Length::Fill)
                    .style(crate::flat_button_style)
                    .on_press_maybe((!details.is_empty()).then_some(Message::ToggleThinking(i)))].spacing(10);
                for (index, diff) in diffs.iter().enumerate() {
                    let (added, removed) = diff.stats();
                    card = card.push(button(row![
                        text(char::from(lucide_icons::Icon::FileDiff)).font(iced::Font::with_name("lucide")).size(13),
                        text(format!("View diff · {}", diff.file_name())).size(12),
                        text(format!("+{added} −{removed}")).size(11).style(iced::widget::text::secondary),
                    ].spacing(6).align_y(iced::Alignment::Center)).padding([3, 6]).style(crate::flat_button_style)
                        .on_press(Message::ViewDiff(i, index)));
                }
                if expanded && !details.is_empty() {
                    card = card.push(container(text(details.clone()).size(12).font(iced::Font::MONOSPACE)).padding(8).width(Length::Fill).style(container::dark));
                }
                container(card).padding(10).width(Length::Fill).style(|theme: &iced::Theme| {
                    let palette = theme.extended_palette();
                    iced::widget::container::Style {
                        background: Some(palette.background.weak.color.into()),
                        border: iced::Border { color: palette.background.strong.color, width: 1.0, radius: 6.0.into() },
                        ..Default::default()
                    }
                }).into()
            }
            Entry::User { content } => labeled_copyable("You", i, content, false),
            Entry::Thinking { content } => {
                let expanded = state.expanded_thinking.contains(&i);
                let mut section = column![button(if expanded { "Hide thinking" } else { "Show thinking" })
                    .style(crate::flat_button_style).on_press(Message::ToggleThinking(i))];
                if expanded { section = section.push(text(content.clone()).size(13).style(iced::widget::text::secondary)); }
                section.into()
            },
            Entry::Assistant { content } => {
                container(labeled_copyable(state.provider, i, content, true)).padding(12).width(Length::Fill)
                    .style(|theme: &iced::Theme| {
                        let palette = theme.extended_palette();
                        iced::widget::container::Style {
                            background: Some(palette.background.base.color.into()),
                            border: iced::Border { color: palette.primary.weak.color, width: 1.0, radius: 6.0.into() },
                            ..Default::default()
                        }
                    }).into()
            }

        };
        messages = messages.push(container(card).padding(6));
    }
    if state.streaming {
        messages = messages.push(container(text(if state.stopping { "Stopping..." } else { "Assistant is working..." })
            .size(12).style(iced::widget::text::secondary)).padding(12));
    }
    let messages = scrollable(messages).anchor_bottom().height(Length::Fill);

    // Everything above the composer is one column, present only when it has something in it,
    // so the keyed column only ever gains or loses its first child: the one change iced's
    // keyed diff handles while keeping the input's widget state (its focus).
    let mut extras: Vec<Element<'a, Message>> = Vec::new();
    if let Some(pending) = &state.pending_permission {
        use crate::ai_approval::{Card, Choice, ChoiceKind};
        let mut choices: Vec<_> = pending.options.iter().map(|(id, name, kind)| {
            let (label, style) = match kind {
                PermissionOptionKind::AllowOnce => (if pending.remember { "Allow for session" } else { "Allow once" }.to_string(), ChoiceKind::Allow),
                PermissionOptionKind::RejectOnce => ("Reject".to_string(), ChoiceKind::Reject),
                _ => (name.clone(), ChoiceKind::Other),
            };
            Choice { label, kind: style, message: Message::PermissionChosen(id.clone()) }
        }).collect();
        choices.sort_by_key(|choice| match choice.kind { ChoiceKind::Reject => 0, ChoiceKind::Allow => 1, ChoiceKind::Other => 2 });
        extras.push(crate::ai_approval::view(Card {
            title: pending.title.clone(), details: pending.details.clone(), choices,
            remember: (pending.allow_once.is_some() && pending.scope.is_some()).then_some((pending.remember, Message::RememberPermission)),
            copy: Message::Copy(pending.details.clone()),
        }));
    }
    if !state.session_grants.is_empty() {
        extras.push(button("Reset session approvals").style(crate::flat_button_style).on_press(Message::ResetPermissions).into());
    }
    let can_switch = !state.streaming && !state.model_switching && !state.effort_switching;
    let choice_buttons = |options: &'a ModelOptions, select: fn(String) -> Message| {
        options.choices.iter().fold(column![].spacing(2), |menu, (id, name)| {
            let label = if id == &options.current { format!("✓ {name}") } else { name.clone() };
            menu.push(button(text(label).size(12)).width(Length::Fill).padding([4, 8])
                .style(crate::flat_button_style)
                .on_press_maybe(can_switch.then(|| select(id.clone()))))
        })
    };
    let dropdown = |menu: iced::widget::Column<'a, Message>| container(scrollable(menu)).padding(4).max_height(240).style(|theme: &iced::Theme| {
        let palette = theme.extended_palette();
        iced::widget::container::Style {
            background: Some(palette.background.weak.color.into()),
            border: iced::Border::default().rounded(6.0),
            ..iced::widget::container::Style::default()
        }
    });
    if state.model_menu_open {
        let menu = match &state.models {
            Some(models) if !models.choices.is_empty() => choice_buttons(models, Message::SelectModel),
            _ if state.started && !state.models_reported => {
                column![text("Loading models…").size(12).style(iced::widget::text::secondary)]
            }
            _ => {
                column![text(format!("{} did not report any selectable models.", state.provider)).size(12).style(iced::widget::text::secondary)]
            }
        };
        extras.push(dropdown(menu).into());
    }
    // Only the levels the agent reports for the current model; nothing when it reports none.
    let effort = state.effort.as_ref().filter(|effort| !effort.choices.is_empty());
    if let (true, Some(effort)) = (state.effort_menu_open, effort) {
        extras.push(dropdown(choice_buttons(effort, Message::SelectEffort)).into());
    }
    let mut bottom = iced::widget::keyed::Column::new();
    if !extras.is_empty() {
        bottom = bottom.push("extras", iced::widget::column(extras).spacing(6));
    }
    let model_name = state.models.as_ref().map(ModelOptions::current_name);
    let model_button = button(text(state.model_label()).size(12))
        .padding([2, 6]).style(crate::flat_button_style).on_press(Message::ToggleModelMenu);
    let effort_button = effort.map(|effort| {
        button(text(if state.effort_switching { "Switching…".into() } else { format!("{} ▾", effort.current_name()) }).size(12))
            .padding([2, 6]).style(crate::flat_button_style).on_press(Message::ToggleEffortMenu)
    });
    let usage_ring = crate::ai_usage::view(crate::ai_usage::Info {
        provider: format!("{} ACP", state.provider), model: model_name,
        used: state.usage.as_ref().map(|u| u.used), capacity: state.usage.as_ref().map(|u| u.size),
        cost: state.usage.as_ref().and_then(|u| u.cost.clone()),
    }, state.usage_open, Message::ToggleUsage, Message::CloseUsage);

    let awaiting_permission = state.pending_permission.is_some();
    let send_button = if state.streaming {
        crate::icon_control(lucide_icons::Icon::CircleStop, "Stop response", (!state.stopping).then_some(Message::Stop), false)
    } else {
        crate::icon_control(lucide_icons::Icon::SendHorizonal, "Send message (Cmd/Ctrl+Enter)",
            (!state.model_switching && !state.effort_switching && !awaiting_permission && (!state.input.text().trim().is_empty() || !state.attachments.is_empty())).then_some(Message::Send), false)
    };
    bottom = bottom.push("composer", crate::ai_composer::view(
        column![
            text_editor(&state.input)
                .size(13)
                .placeholder("Ask your assistant… (Cmd/Ctrl+Enter to send)")
                .on_action(Message::InputChanged)
                .min_height(50.0).max_height(290.0)
                .key_binding(|key_press| crate::ai_composer::key_binding(key_press, Message::Send, Message::Composer)),
            row![text(if state.stopping { "Stopping…" } else if awaiting_permission { "Waiting for approval" } else if state.streaming { "Working…" } else { "Ready" }).size(12).style(iced::widget::text::secondary), Space::new().width(Length::Fill), model_button].push(effort_button).push(usage_ring).push(send_button).spacing(6).align_y(iced::Alignment::Center),
        ]
        .spacing(4).into(),
        &state.attachments, composer, Message::Composer,
    ));

    container(column![header, messages, bottom.spacing(6)].spacing(8)).padding(8).height(Length::Fill).into()
}

fn tool_details(content: &[agent_client_protocol::schema::v1::ToolCallContent], input: Option<&serde_json::Value>) -> String {
    use agent_client_protocol::schema::v1::ToolCallContent;
    let mut details = String::new();
    for block in content {
        if let ToolCallContent::Content(content) = block {
            if let ContentBlock::Text(text) = &content.content { details.push_str(&text.text); details.push('\n'); }
        }
    }
    // Edits read as hunks rather than two full copies of the file.
    for diff in tool_diffs(content) {
        details.push_str(&diff.review_text());
    }
    if let Some(input) = input { details.push_str(&serde_json::to_string_pretty(input).unwrap_or_default()); }
    details
}

fn tool_diffs(content: &[agent_client_protocol::schema::v1::ToolCallContent]) -> Vec<crate::ai_diff::FileDiff> {
    use agent_client_protocol::schema::v1::ToolCallContent;
    content.iter().filter_map(|block| match block {
        ToolCallContent::Diff(diff) => Some(crate::ai_diff::FileDiff {
            path: diff.path.display().to_string(), old: diff.old_text.clone(), new: diff.new_text.clone(),
        }),
        _ => None,
    }).collect()
}

/// The per-project conversations file name inside the store directory.
fn threads_file(cwd: &Path, provider: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    cwd.hash(&mut hasher);
    let hash = hasher.finish();
    if provider == "Claude" { format!("{hash:x}.json") } else { format!("{hash:x}-codex.json") }
}

pub(crate) async fn change_model(connection: &ConnectionTo<Agent>, session: &SessionId, config_id: &str, value: &str) -> Result<ModelOptions, String> {
    set_config_option(connection, session, Setting::Model, config_id, value).await?.model.ok_or_else(|| "Agent did not confirm a model".into())
}

/// Sets the model or effort and returns the configuration the agent reports afterwards,
/// which for a model change includes that model's effort levels.
async fn set_config_option(connection: &ConnectionTo<Agent>, session: &SessionId, setting: Setting, config_id: &str, value: &str) -> Result<SessionConfig, String> {
    let response = connection.send_request(SetSessionConfigOptionRequest::new(
        session.clone(), config_id.to_owned(), SessionConfigValueId::new(value),
    )).block_task().await.map_err(|err| err.to_string())?;
    let config = SessionConfig::from_options(&response.config_options);
    confirm_setting(&config, setting, value)?;
    Ok(config)
}

fn confirm_setting(config: &SessionConfig, setting: Setting, value: &str) -> Result<(), String> {
    let (reported, confirmed) = match setting {
        Setting::Model => {
            let models = config.model.as_ref().ok_or("Agent did not confirm a model")?;
            (&models.current, is_same_model(value, &models.current))
        }
        Setting::Effort => {
            let effort = config.effort.as_ref().ok_or("Agent did not confirm an effort level")?;
            (&effort.current, effort.current == value)
        }
    };
    if confirmed { Ok(()) } else { Err(format!("Requested {value}, but agent reported {reported}")) }
}

/// Agents may resolve a model to a variant of it, e.g. `claude-fable-5-1` to `claude-fable-5-1[1m]`.
pub(crate) fn is_same_model(requested: &str, reported: &str) -> bool {
    reported.strip_prefix(requested).is_some_and(|suffix| suffix.is_empty() || (suffix.starts_with('[') && suffix.ends_with(']')))
}

pub(crate) fn model_from_options(options: &[agent_client_protocol::schema::v1::SessionConfigOption]) -> Option<ModelOptions> {
    use agent_client_protocol::schema::v1::SessionConfigOptionCategory;
    options.iter().find_map(|option| {
        if option.category != Some(SessionConfigOptionCategory::Model) && option.id.0.as_ref() != "model" { return None; }
        select_options(option)
    })
}

/// The agent's reasoning effort selector, identified by the ACP `thought_level` category
/// rather than an id, since each agent names it differently.
fn effort_from_options(options: &[agent_client_protocol::schema::v1::SessionConfigOption]) -> Option<ModelOptions> {
    use agent_client_protocol::schema::v1::SessionConfigOptionCategory;
    options.iter()
        .filter(|option| option.category == Some(SessionConfigOptionCategory::ThoughtLevel))
        .find_map(select_options)
}

fn select_options(option: &agent_client_protocol::schema::v1::SessionConfigOption) -> Option<ModelOptions> {
    use agent_client_protocol::schema::v1::{SessionConfigKind, SessionConfigSelectOptions};
    let SessionConfigKind::Select(select) = &option.kind else { return None };
    let choices: Vec<_> = match &select.options {
        SessionConfigSelectOptions::Ungrouped(options) => options.iter().collect(),
        SessionConfigSelectOptions::Grouped(groups) => groups.iter().flat_map(|group| &group.options).collect(),
        _ => Vec::new(),
    };
    Some(ModelOptions {
        config_id: option.id.0.to_string(),
        current: select.current_value.0.to_string(),
        choices: choices.into_iter().map(|choice| (choice.value.0.to_string(), choice.name.clone())).collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_model_menu_opening_or_closing_keeps_the_input_focused() {
        let theme = iced::Theme::Dark;
        let composer = crate::ai_composer::Context { sources: &[], can_reference: false, theme: &theme, mentions: &[] };
        let plain = AcpState::default();
        let menu = AcpState { model_menu_open: true, ..AcpState::default() };
        assert!(crate::tree_focus::keeps_focus(view(&plain, PathBuf::from("."), composer), view(&menu, PathBuf::from("."), composer)));
        assert!(crate::tree_focus::keeps_focus(view(&menu, PathBuf::from("."), composer), view(&plain, PathBuf::from("."), composer)));
        // With the reset button already shown, the menu opens between it and the composer.
        let mut granted = AcpState::default();
        granted.session_grants.insert("write_file".into());
        let mut granted_menu = AcpState { model_menu_open: true, ..AcpState::default() };
        granted_menu.session_grants.insert("write_file".into());
        assert!(crate::tree_focus::keeps_focus(view(&granted, PathBuf::from("."), composer), view(&granted_menu, PathBuf::from("."), composer)));
        assert!(crate::tree_focus::keeps_focus(view(&granted_menu, PathBuf::from("."), composer), view(&granted, PathBuf::from("."), composer)));
    }

    #[test]
    fn model_metadata_is_read_from_session_configuration() {
        let options = serde_json::from_value::<Vec<agent_client_protocol::schema::v1::SessionConfigOption>>(serde_json::json!([
            {"id":"mode", "name":"Mode", "type":"select", "currentValue":"code", "options":[]},
            {"id":"agent-model", "category":"model", "name":"Model", "type":"select", "currentValue":"fast", "options":[
                {"group":"recommended", "name":"Recommended", "options":[{"value":"fast", "name":"Fast model"}]},
                {"group":"other", "name":"Other", "options":[{"value":"deep", "name":"Deep model"}]}
            ]}
        ])).unwrap();
        let models = model_from_options(&options).unwrap();
        assert_eq!(models.config_id, "agent-model");
        assert_eq!(models.current_name(), "Fast model");
        assert_eq!(models.choices, vec![("fast".into(), "Fast model".into()), ("deep".into(), "Deep model".into())]);
        assert_eq!(model_from_options(&[]), None);
    }

    #[test]
    fn effort_levels_are_whatever_the_agent_reports_for_the_model() {
        // Shapes taken from claude-agent-acp and codex-acp: different ids and levels, same category.
        let parse = |value| serde_json::from_value::<Vec<agent_client_protocol::schema::v1::SessionConfigOption>>(value).unwrap();
        let claude = SessionConfig::from_options(&parse(serde_json::json!([
            {"id":"model", "category":"model", "name":"Model", "type":"select", "currentValue":"opus", "options":[{"value":"opus", "name":"Opus"}]},
            {"id":"effort", "category":"thought_level", "name":"Effort", "type":"select", "currentValue":"default", "options":[
                {"value":"default", "name":"Default"}, {"value":"low", "name":"Low"}, {"value":"x_high", "name":"X High"}
            ]}
        ])));
        let effort = claude.effort.unwrap();
        assert_eq!(effort.config_id, "effort");
        assert_eq!(effort.current_name(), "Default");
        assert_eq!(effort.choices, vec![("default".into(), "Default".into()), ("low".into(), "Low".into()), ("x_high".into(), "X High".into())]);
        assert_eq!(claude.model.unwrap().current, "opus");

        let codex = SessionConfig::from_options(&parse(serde_json::json!([
            {"id":"reasoning_effort", "category":"thought_level", "name":"Reasoning effort", "type":"select", "currentValue":"medium", "options":[
                {"value":"minimal", "name":"Minimal"}, {"value":"medium", "name":"Medium"}
            ]}
        ])));
        assert_eq!(codex.effort.unwrap().config_id, "reasoning_effort");

        // A model without effort support reports no selector, so none is shown.
        let none = SessionConfig::from_options(&parse(serde_json::json!([
            {"id":"model", "category":"model", "name":"Model", "type":"select", "currentValue":"haiku", "options":[]},
            {"id":"effort", "name":"Effort", "type":"select", "currentValue":"low", "options":[]}
        ])));
        assert_eq!(none.effort, None);
    }

    #[test]
    fn effort_change_is_confirmed_only_by_the_exact_level() {
        let config = |current: &str| SessionConfig {
            model: None,
            effort: Some(ModelOptions { config_id: "effort".into(), current: current.into(), choices: vec![] }),
        };
        assert_eq!(confirm_setting(&config("high"), Setting::Effort, "high"), Ok(()));
        assert!(confirm_setting(&config("high"), Setting::Effort, "low").is_err());
        assert!(confirm_setting(&config("high[1m]"), Setting::Effort, "high").is_err());
        assert!(confirm_setting(&SessionConfig::default(), Setting::Effort, "high").is_err());
    }

    #[test]
    fn selecting_an_effort_requests_the_change_and_remembers_it() {
        let mut state = AcpState::default();
        let (model_tx, mut model_rx) = tokio::sync::mpsc::unbounded_channel();
        state.model_tx = Some(model_tx);
        let levels = ModelOptions { config_id: "effort".into(), current: "medium".into(), choices: vec![("medium".into(), "Medium".into()), ("high".into(), "High".into())] };
        state.effort = Some(levels.clone());
        state.effort_menu_open = true;
        state.select_effort("high".into());
        assert_eq!(model_rx.try_recv().unwrap(), (Setting::Effort, "effort".into(), "high".into()));
        assert!(state.effort_switching);
        assert!(!state.effort_menu_open);
        assert_eq!(state.preferred_effort, None);

        // Neither sending nor a model change may race an effort change.
        state.input = text_editor::Content::with_text("keep this draft");
        state.send(PathBuf::from("."));
        assert!(!state.started);
        state.models = Some(ModelOptions { config_id: "model".into(), current: "a".into(), choices: vec![] });
        state.select_model("b".into());
        assert!(model_rx.try_recv().is_err());

        let (tx, rx) = mpsc::channel();
        state.rx = Some(rx);
        tx.send(Event::SettingChanged(Setting::Effort, Ok(SessionConfig { model: None, effort: Some(ModelOptions { current: "high".into(), ..levels }) }))).unwrap();
        state.poll();
        assert!(!state.effort_switching);
        assert_eq!(state.effort.as_ref().unwrap().current_name(), "High");
        assert_eq!(state.preferred_effort.as_deref(), Some("high"));

        // Levels the agent derives from a model change are shown but not remembered as a choice.
        tx.send(Event::Config(SessionConfig { model: None, effort: Some(ModelOptions { config_id: "effort".into(), current: "low".into(), choices: vec![("low".into(), "Low".into())] }) })).unwrap();
        state.poll();
        assert_eq!(state.effort.as_ref().unwrap().current, "low");
        assert_eq!(state.preferred_effort.as_deref(), Some("high"));
    }

    #[test]
    fn saved_effort_round_trips_and_old_files_still_load() {
        let store: ThreadStore = serde_json::from_str(r#"{"threads":[],"active":0,"model":"m"}"#).unwrap();
        assert_eq!(store.effort, None);
        let saved = serde_json::to_string(&ThreadStore { effort: Some("high".into()), ..store }).unwrap();
        assert_eq!(serde_json::from_str::<ThreadStore>(&saved).unwrap().effort.as_deref(), Some("high"));
    }

    #[test]
    fn model_variants_reported_by_the_agent_confirm_the_request() {
        assert!(is_same_model("claude-fable-5-1", "claude-fable-5-1"));
        assert!(is_same_model("claude-fable-5-1", "claude-fable-5-1[1m]"));
        assert!(!is_same_model("claude-fable-5-1", "claude-opus-5-5[1m]"));
        assert!(!is_same_model("claude-fable-5-1", "claude-fable-5-10"));
        assert!(!is_same_model("claude-fable-5-1[1m]", "claude-fable-5-1"));
        assert!(!is_same_model("claude-fable-5-1", "claude-fable-5-1[1m"));
    }

    #[test]
    fn selecting_a_model_requests_the_change_and_remembers_it() {
        let mut state = AcpState::default();
        let (model_tx, mut model_rx) = tokio::sync::mpsc::unbounded_channel();
        state.model_tx = Some(model_tx);
        state.models = Some(ModelOptions { config_id: "model".into(), current: "fast".into(), choices: vec![("fast".into(), "Fast".into()), ("deep".into(), "Deep".into())] });
        state.model_menu_open = true;
        state.select_model("deep".into());
        assert_eq!(model_rx.try_recv().unwrap(), (Setting::Model, "model".into(), "deep".into()));
        assert_eq!(state.models.as_ref().unwrap().current, "fast");
        assert_eq!(state.preferred_model, None);
        assert!(state.model_switching);
        assert!(!state.model_menu_open);
        state.input = text_editor::Content::with_text("keep this draft");
        state.send(PathBuf::from("."));
        assert_eq!(state.input.text(), "keep this draft");
        assert!(!state.started);
        let (tx, rx) = mpsc::channel();
        state.rx = Some(rx);
        let mut confirmed = state.models.clone().unwrap();
        confirmed.current = "deep".into();
        tx.send(Event::SettingChanged(Setting::Model, Ok(SessionConfig { model: Some(confirmed), effort: None }))).unwrap();
        state.poll();
        assert!(!state.model_switching);
        assert_eq!(state.models.as_ref().unwrap().current, "deep");
        assert_eq!(state.preferred_model.as_deref(), Some("deep"));

        state.select_model("fast".into());
        assert!(state.model_switching);
        tx.send(Event::SettingChanged(Setting::Model, Err("rejected".into()))).unwrap();
        state.poll();
        assert!(!state.model_switching);
        assert_eq!(state.models.as_ref().unwrap().current, "deep");
        assert_eq!(state.preferred_model.as_deref(), Some("deep"));
        assert!(state.entries.iter().any(|entry| matches!(entry, Entry::Assistant { content } if content.contains("rejected"))));
    }

    #[test]
    fn model_button_names_the_model_before_and_after_the_agent_reports_it() {
        let mut state = AcpState::default();
        assert_eq!(state.model_label(), "Model ▾");
        // The panel being shown starts the session once; a failed start is not retried each tick.
        assert!(state.claim_auto_connect());
        assert!(!state.claim_auto_connect());
        state.started = true;
        assert_eq!(state.model_label(), "Connecting… ▾");
        state.preferred_model = Some("deep".into());
        assert_eq!(state.model_label(), "deep ▾");
        let (tx, rx) = mpsc::channel();
        state.rx = Some(rx);
        tx.send(Event::Config(SessionConfig { model: Some(ModelOptions { config_id: "model".into(), current: "deep".into(), choices: vec![("deep".into(), "Deep model".into())] }), effort: None })).unwrap();
        state.poll();
        assert_eq!(state.model_label(), "Deep model ▾");
        state.model_switching = true;
        assert_eq!(state.model_label(), "Switching…");
        // A new conversation gets its own session, so it connects again; a running one doesn't.
        state.reset_connection();
        assert!(state.claim_auto_connect());
        state.reset_connection();
        state.started = true;
        assert!(!state.claim_auto_connect());
    }

    #[test]
    fn remember_checkbox_grants_only_when_allow_is_chosen() {
        let cwd = PathBuf::from("approval-test");
        let mut state = AcpState { loaded: true, cwd: Some(cwd.clone()), ..Default::default() };
        let (pending, mut reply) = permission("operation");
        state.pending_permission = Some(pending);
        let _ = update(&mut state, Message::RememberPermission(true), cwd.clone());
        assert!(state.session_grants.is_empty());
        let _ = update(&mut state, Message::PermissionChosen("reject".into()), cwd.clone());
        assert_eq!(reply.try_recv().unwrap(), "reject");
        assert!(state.session_grants.is_empty());
        let (pending, mut reply) = permission("operation");
        state.pending_permission = Some(pending);
        let _ = update(&mut state, Message::RememberPermission(true), cwd.clone());
        let _ = update(&mut state, Message::PermissionChosen("accept".into()), cwd);
        assert_eq!(reply.try_recv().unwrap(), "accept");
        assert!(state.session_grants.contains("operation"));
    }

    #[test]
    fn final_reply_follows_tools_and_survives_late_completion_updates() {
        let mut state = AcpState::default();
        let (tx, rx) = mpsc::channel();
        state.rx = Some(rx);
        state.streaming = true;
        state.threads.push(Thread::default());
        state.entries.push(Entry::User { content: "Review project".into() });
        tx.send(Event::Delta("I will inspect it.".into())).unwrap();
        tx.send(Event::ToolCall { id: "read".into(), title: "Read main.rs".into(), status: ToolStatus::InProgress }).unwrap();
        tx.send(Event::Delta("The project ".into())).unwrap();
        tx.send(Event::ToolCallUpdate { id: "read".into(), title: None, status: Some(ToolStatus::Completed) }).unwrap();
        tx.send(Event::Delta("looks good.".into())).unwrap();
        tx.send(Event::Done).unwrap();
        state.poll();
        assert_eq!(state.entries.len(), 4);
        assert!(matches!(&state.entries[1], Entry::Assistant { content } if content == "I will inspect it."));
        assert!(matches!(&state.entries[2], Entry::ToolCall { status: ToolStatus::Completed, .. }));
        assert!(matches!(&state.entries[3], Entry::Assistant { content } if content == "The project looks good."));
        // Late notifications must not be lost from the saved transcript either.
        tx.send(Event::Delta(" No bugs found.".into())).unwrap();
        state.poll();
        assert!(matches!(state.threads[0].entries.last(), Some(Entry::Assistant { content }) if content == "The project looks good. No bugs found."));
        assert!(!state.streaming);
    }

    #[test]
    fn thinking_and_errors_preserve_timeline_without_empty_bubbles() {
        let mut entries = Vec::new();
        AcpState::append_chunk(&mut entries, String::new(), false);
        assert!(entries.is_empty());
        AcpState::append_chunk(&mut entries, "Plan".into(), true);
        AcpState::append_chunk(&mut entries, "ning".into(), true);
        AcpState::upsert_tool_call(&mut entries, "edit".into(), Some("Edit file".into()), None);
        AcpState::append_chunk(&mut entries, "[error: disconnected]".into(), false);
        assert_eq!(entries.len(), 3);
        assert!(matches!(&entries[0], Entry::Thinking { content } if content == "Planning"));
        assert!(matches!(&entries[2], Entry::Assistant { content } if content.contains("disconnected")));
    }

    fn permission(scope: &str) -> (PendingPermission, tokio::sync::oneshot::Receiver<String>) {
        let (respond, rx) = tokio::sync::oneshot::channel();
        (PendingPermission { title: "Edit file".into(), details: "replacement".into(), scope: Some(scope.into()), allow_once: Some("accept".into()), options: vec![], remember: false, respond }, rx)
    }

    #[test]
    fn concurrent_permissions_are_queued_and_cancelled_on_stop() {
        let mut state = AcpState::default();
        let (tx, rx) = mpsc::channel();
        state.rx = Some(rx);
        let (first, mut first_reply) = permission("first");
        let (second, mut second_reply) = permission("second");
        tx.send(Event::Permission(first)).unwrap();
        tx.send(Event::Permission(second)).unwrap();
        state.poll();
        assert_eq!(state.pending_permission.as_ref().unwrap().scope.as_deref(), Some("first"));
        assert_eq!(state.permission_queue.len(), 1);
        state.stop();
        assert!(matches!(first_reply.try_recv(), Err(tokio::sync::oneshot::error::TryRecvError::Closed)));
        assert!(matches!(second_reply.try_recv(), Err(tokio::sync::oneshot::error::TryRecvError::Closed)));
    }

    #[test]
    fn stop_rejects_late_permissions_but_keeps_session_grants() {
        let mut state = AcpState { provider: "Codex", ..AcpState::default() };
        let (tx, rx) = mpsc::channel();
        state.rx = Some(rx);
        state.streaming = true;
        state.session_grants.insert("same".into());
        state.stop();
        for scope in ["same", "new"] {
            let (pending, mut reply) = permission(scope);
            tx.send(Event::Permission(pending)).unwrap();
            state.poll();
            assert!(matches!(reply.try_recv(), Err(tokio::sync::oneshot::error::TryRecvError::Closed)));
        }
        assert!(state.pending_permission.is_none());
        assert!(state.permission_queue.is_empty());
        tx.send(Event::Done).unwrap();
        state.poll();
        assert!(state.session_grants.contains("same"));
        state.streaming = true;
        let (pending, mut reply) = permission("same");
        tx.send(Event::Permission(pending)).unwrap();
        state.poll();
        assert_eq!(reply.try_recv().unwrap(), "accept");
    }

    #[tokio::test]
    async fn stop_cancels_only_its_prompt_before_and_after_dispatch() {
        use agent_client_protocol::schema::v1::{PromptResponse, StopReason};
        let cancelled = Arc::new(tokio::sync::Notify::new());
        let notify_cancelled = cancelled.clone();
        let (started_tx, mut started_rx) = tokio::sync::mpsc::unbounded_channel();
        let agent = Agent.builder()
            .on_receive_notification(async move |_cancel: CancelNotification, _cx| {
                notify_cancelled.notify_one();
                Ok(())
            }, agent_client_protocol::on_receive_notification!())
            .on_receive_request(async move |request: PromptRequest, responder, cx| {
                let text = match &request.prompt[0] {
                    ContentBlock::Text(text) => text.text.as_str(),
                    _ => panic!("expected text prompt"),
                };
                assert_ne!(text, "stopped during startup");
                if text == "next turn" {
                    return responder.respond(PromptResponse::new(StopReason::EndTurn));
                }
                started_tx.send(()).unwrap();
                let cancelled = cancelled.clone();
                cx.spawn(async move {
                    cancelled.notified().await;
                    responder.respond(PromptResponse::new(StopReason::Cancelled))
                })
            }, agent_client_protocol::on_receive_request!());
        let test = agent_client_protocol::Client.builder().connect_with(agent, async move |connection| {
            let session = SessionId::new("test");
            let (stop, cancel) = tokio::sync::oneshot::channel();
            stop.send(()).unwrap();
            run_prompt(&connection, &session, PendingPrompt {
                content: vec![ContentBlock::Text(TextContent::new("stopped during startup"))], cancel,
            }).await?;
            assert!(started_rx.try_recv().is_err());

            let (stop, cancel) = tokio::sync::oneshot::channel();
            let prompt = run_prompt(&connection, &session, PendingPrompt {
                content: vec![ContentBlock::Text(TextContent::new("working"))], cancel,
            });
            let send_stop = async {
                started_rx.recv().await.unwrap();
                stop.send(()).unwrap();
            };
            let (result, ()) = tokio::join!(prompt, send_stop);
            result?;

            let (_stop, cancel) = tokio::sync::oneshot::channel();
            run_prompt(&connection, &session, PendingPrompt {
                content: vec![ContentBlock::Text(TextContent::new("next turn"))], cancel,
            }).await
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), test).await.unwrap().unwrap();
    }

    #[test]
    fn session_grants_match_only_identical_operations_and_reset() {
        let mut state = AcpState::default();
        state.session_grants.insert("same".into());
        let (tx, rx) = mpsc::channel();
        state.rx = Some(rx);
        let (same, mut accepted) = permission("same");
        let (different, mut waiting) = permission("different");
        tx.send(Event::Permission(same)).unwrap();
        tx.send(Event::Permission(different)).unwrap();
        state.poll();
        assert_eq!(accepted.try_recv().unwrap(), "accept");
        assert!(matches!(waiting.try_recv(), Err(tokio::sync::oneshot::error::TryRecvError::Empty)));
        state.reset_connection();
        assert!(state.session_grants.is_empty());
    }

    fn titled(title: &str) -> Thread {
        Thread { title: title.into(), ..Default::default() }
    }

    #[test]
    fn history_actions_rename_and_persist_per_project() {
        let store = tempfile::tempdir().unwrap();
        let project = store.path().join("project");
        let mut state = AcpState { store_dir: Some(store.path().to_path_buf()), ..Default::default() };
        state.ensure_loaded(&project);
        state.entries.push(Entry::User { content: "first question".into() });
        state.new_thread(&project);
        assert_eq!(state.threads.len(), 2);
        assert_eq!(state.threads[0].title, "first question");
        assert!(state.threads[1].updated > 0);
        use crate::ai_history::Message as History;
        let _ = update(&mut state, Message::History(History::Rename(0, "first question".into())), project.clone());
        let _ = update(&mut state, Message::History(History::RenameInput("Login bug".into())), project.clone());
        let _ = update(&mut state, Message::History(History::RenameDone), project.clone());
        let _ = update(&mut state, Message::History(History::Switch(0)), project.clone());
        assert_eq!(state.active_thread, 0);
        assert!(matches!(&state.entries[..], [Entry::User { content }] if content == "first question"));

        let mut reloaded = AcpState { store_dir: Some(store.path().to_path_buf()), ..Default::default() };
        reloaded.ensure_loaded(&project);
        assert_eq!(reloaded.threads.iter().map(|t| t.title.as_str()).collect::<Vec<_>>(), ["Login bug", ""]);
        assert_eq!(reloaded.active_thread, 0);
        let _ = update(&mut reloaded, Message::History(History::Delete(1)), project.clone());
        let mut again = AcpState { store_dir: Some(store.path().to_path_buf()), ..Default::default() };
        again.ensure_loaded(&project);
        assert_eq!(again.threads.len(), 1);
        // Other projects and providers keep separate histories.
        let mut codex = AcpState { provider: "Codex", store_dir: Some(store.path().to_path_buf()), ..Default::default() };
        codex.ensure_loaded(&project);
        assert!(codex.threads[0].entries.is_empty());
    }

    #[test]
    fn tool_edits_keep_their_diffs_for_review() {
        use agent_client_protocol::schema::v1::{Diff, ToolCallContent};
        let content = vec![ToolCallContent::Diff(Diff::new("src/lib.rs", "new\n").old_text("old\n".to_string()))];
        let diffs = tool_diffs(&content);
        assert_eq!(diffs, [crate::ai_diff::FileDiff { path: "src/lib.rs".into(), old: Some("old\n".into()), new: "new\n".into() }]);
        assert!(tool_details(&content, None).contains("-old\n+new\n"));

        let mut state = AcpState::default();
        let (tx, rx) = mpsc::channel();
        state.rx = Some(rx);
        tx.send(Event::ToolCall { id: "edit".into(), title: "Edit lib.rs".into(), status: ToolStatus::Completed }).unwrap();
        tx.send(Event::ToolDetails("edit".into(), "details".into(), diffs.clone())).unwrap();
        // A later update without content keeps the diff.
        tx.send(Event::ToolDetails("edit".into(), String::new(), Vec::new())).unwrap();
        state.poll();
        assert_eq!(state.diff(0, 0), diffs.first());
        assert_eq!(state.diff(0, 1), None);
    }

    #[test]
    fn deleting_threads_keeps_the_open_conversation_consistent() {
        let mut state = AcpState::default();
        state.threads = vec![titled("a"), titled("b"), titled("c")];
        state.active_thread = 2;
        state.entries = vec![Entry::User { content: "in c".into() }];

        // Deleting an earlier thread keeps the same conversation open.
        state.delete_thread(0);
        assert_eq!(state.threads.iter().map(|t| t.title.as_str()).collect::<Vec<_>>(), ["b", "c"]);
        assert_eq!(state.active_thread, 1);
        assert!(matches!(&state.entries[..], [Entry::User { content }] if content == "in c"));

        // Deleting the open thread opens the newest remaining one.
        state.threads[0].entries = vec![Entry::User { content: "in b".into() }];
        state.delete_thread(1);
        assert_eq!(state.active_thread, 0);
        assert!(matches!(&state.entries[..], [Entry::User { content }] if content == "in b"));

        // Deleting the last thread leaves a fresh empty one.
        state.delete_thread(0);
        assert_eq!(state.threads.len(), 1);
        assert!(state.threads[0].title.is_empty());
        assert!(state.entries.is_empty());

        // Out of range and mid-response deletes are ignored.
        state.delete_thread(5);
        state.threads.push(titled("d"));
        state.streaming = true;
        state.delete_thread(1);
        assert_eq!(state.threads.len(), 2);
    }

    #[test]
    fn provider_histories_are_isolated() {
        let cwd = Path::new("project");
        assert_ne!(threads_file(cwd, "Claude"), threads_file(cwd, "Codex"));
        assert_ne!(threads_file(cwd, "Claude"), threads_file(Path::new("other"), "Claude"));
        assert!(AcpState::default().threads_path(cwd).is_none(), "tests never write to the user's config");
    }

    #[test]
    fn stop_waits_for_completion_and_dismisses_permission() {
        let mut state = AcpState::default();
        let (tx, rx) = mpsc::channel();
        let (respond, _) = tokio::sync::oneshot::channel();
        state.rx = Some(rx);
        state.streaming = true;
        state.pending_permission = Some(PendingPermission {
            title: "Run command".into(), details: String::new(), allow_once: None, scope: None, options: vec![], remember: false, respond,
        });
        state.stop();
        assert!(state.streaming);
        assert!(state.stopping);
        assert!(state.pending_permission.is_none());
        tx.send(Event::Done).unwrap();
        state.poll();
        assert!(!state.streaming);
        assert!(!state.stopping);
    }

    #[test]
    fn closed_agent_connection_can_restart() {
        let mut state = AcpState::default();
        let (_tx, rx) = mpsc::channel();
        let (prompt_tx, prompt_rx) = tokio::sync::mpsc::unbounded_channel();
        drop(prompt_rx);
        state.rx = Some(rx);
        state.prompt_tx = Some(prompt_tx);
        state.started = true;
        state.streaming = true;
        state.poll();
        assert!(!state.started);
        assert!(!state.streaming);
        assert!(state.rx.is_none());
    }
}
