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
use agent_client_protocol::{AcpAgent, Agent, ConnectionTo};
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
    },
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct Thread {
    title: String,
    session_id: Option<String>,
    entries: Vec<Entry>,
}

#[derive(Default, Serialize, Deserialize)]
struct ThreadStore {
    threads: Vec<Thread>,
    active: usize,
    /// Model the user last picked; applied to every new or resumed session.
    #[serde(default)]
    model: Option<String>,
}

/// The agent's model selector as reported through session config options.
#[derive(Debug, Clone, PartialEq)]
struct ModelOptions {
    config_id: String,
    current: String,
    choices: Vec<(String, String)>,
}

impl ModelOptions {
    fn current_name(&self) -> String {
        self.choices.iter().find(|(id, _)| id == &self.current).map_or_else(|| self.current.clone(), |(_, name)| name.clone())
    }
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
    Models(Option<ModelOptions>),
    ModelChanged(Result<ModelOptions, String>),
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
    ToolDetails(String, String),
    Done,
    Error(String),
}

pub struct AcpState {
    pub attachments: Vec<crate::ai_context::Attachment>,
    provider: &'static str,
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
    model_tx: Option<tokio::sync::mpsc::UnboundedSender<(String, String)>>,
    streaming: bool,
    started: bool,
    usage: Option<Usage>,
    models: Option<ModelOptions>,
    models_reported: bool,
    model_switching: bool,
    preferred_model: Option<String>,
    model_menu_open: bool,
    usage_open: bool,
    pending_permission: Option<PendingPermission>,
    permission_queue: std::collections::VecDeque<PendingPermission>,
    just_finished: bool,
    thread_menu_open: bool,
    stopping: bool,
    expanded_thinking: Vec<usize>,
}

impl Default for AcpState {
    fn default() -> Self {
        Self {
            attachments: Vec::new(),
            provider: "Claude",
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
            usage: None,
            models: None,
            models_reported: false,
            model_switching: false,
            preferred_model: None,
            model_menu_open: false,
            usage_open: false,
            pending_permission: None,
            permission_queue: Default::default(),
            just_finished: false,
            thread_menu_open: false,
            stopping: false,
            expanded_thinking: Vec::new(),
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
    NewThread,
    SwitchThread(usize),
    ThreadMenuToggle,
    ToggleThinking(usize),
    UsePrompt(String),
    InputChanged(text_editor::Action),
    Send,
    Stop,
    PermissionChosen(String),
    RememberPermission(bool),
    ResetPermissions,
    Copy(String),
}

impl AcpState {
    pub fn codex() -> Self { Self { provider: "Codex", ..Self::default() } }

    /// Returns true (once) the first time this is called after a turn finishes,
    /// so the caller can react (e.g. reload files the agent may have edited).
    pub fn take_finished(&mut self) -> bool {
        std::mem::take(&mut self.just_finished)
    }

    pub fn poll(&mut self) {
        let Some(rx) = &self.rx else { return };
        let mut finished = false;
        let mut new_session_id = None;
        let mut transcript_changed = false;
        while let Ok(event) = rx.try_recv() {
            transcript_changed |= matches!(&event, Event::Delta(_) | Event::ThoughtDelta(_) | Event::ToolCall { .. } | Event::ToolCallUpdate { .. } | Event::ToolDetails(_, _) | Event::Error(_));
            match event {
                Event::Delta(text) => Self::append_chunk(&mut self.entries, text, false),
                Event::ThoughtDelta(text) => Self::append_chunk(&mut self.entries, text, true),
                Event::Models(models) => {
                    self.models = models;
                    self.models_reported = true;
                }
                Event::ModelChanged(result) => {
                    self.model_switching = false;
                    transcript_changed = true;
                    match result {
                        Ok(models) => {
                            self.preferred_model = Some(models.current.clone());
                            self.models = Some(models);
                        }
                        Err(err) => Self::append_chunk(&mut self.entries, format!("\n[error: could not change model: {err}]"), false),
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
                Event::ToolDetails(id, details) => {
                    if let Some(Entry::ToolCall { details: current, .. }) = self.entries.iter_mut().find(|entry| matches!(entry, Entry::ToolCall { id: existing, .. } if existing == &id)) {
                        if !details.is_empty() { *current = details; }
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

        if let Some(path) = threads_path(cwd, self.provider) {
            if let Ok(text) = std::fs::read_to_string(&path) {
                if let Ok(store) = serde_json::from_str::<ThreadStore>(&text) {
                    self.preferred_model = store.model;
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

    fn persist(&self, cwd: &Path) {
        let Some(path) = threads_path(cwd, self.provider) else { return };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let store = ThreadStore {
            threads: self.threads.clone(),
            active: self.active_thread,
            model: self.preferred_model.clone(),
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
        self.model_tx = None;
        self.usage_open = false;
        self.stopping = false;
        self.expanded_thinking.clear();
        self.started = false;
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
        let (model_tx, mut model_rx) = tokio::sync::mpsc::unbounded_channel::<(String, String)>();
        self.model_tx = Some(model_tx);

        let provider = self.provider;
        let preferred_model = self.preferred_model.clone();
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
                let agent = match AcpAgent::from_args([if cfg!(windows) { "npx.cmd" } else { "npx" }, "--yes", if provider == "Codex" { "@agentclientprotocol/codex-acp" } else { "@agentclientprotocol/claude-agent-acp" }])
                {
                    Ok(agent) => agent,
                    Err(err) => {
                        let _ = tx.send(Event::Error(err.to_string()));
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
                                    let _ = notify_tx.send(Event::Models(model_from_options(&update.config_options)));
                                }
                                SessionUpdate::UsageUpdate(usage) => {
                                    let cost = usage.cost.map(|c| (c.amount, c.currency));
                                    let _ =
                                        notify_tx.send(Event::Usage(usage.used, usage.size, cost));
                                }
                                SessionUpdate::ToolCall(tool_call) => {
                                    let details = tool_details(&tool_call.content, tool_call.raw_output.as_ref().or(tool_call.raw_input.as_ref()));
                                    let id = tool_call.tool_call_id.0.to_string();
                                    let _ = notify_tx.send(Event::ToolCall {
                                        id: tool_call.tool_call_id.0.to_string(),
                                        title: tool_call.title,
                                        status: convert_status(tool_call.status),
                                    });
                                    let _ = notify_tx.send(Event::ToolDetails(id, details));
                                }
                                SessionUpdate::ToolCallUpdate(update) => {
                                    let details = tool_details(update.fields.content.as_deref().unwrap_or_default(), update.fields.raw_output.as_ref().or(update.fields.raw_input.as_ref()));
                                    let id = update.tool_call_id.0.to_string();
                                    let _ = notify_tx.send(Event::ToolCallUpdate {
                                        id: update.tool_call_id.0.to_string(),
                                        title: update.fields.title,
                                        status: update.fields.status.map(convert_status),
                                    });
                                    let _ = notify_tx.send(Event::ToolDetails(id, details));
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
                        let mut models = model_from_options(&config_options);
                        if let Some(preferred) = &preferred_model {
                            let _ = loop_tx.send(Event::Models(models.clone()));
                            let options = models.as_ref().ok_or_else(|| agent_client_protocol::Error::internal_error().data("Agent did not report a model; saved selection cannot be confirmed"))?;
                            if &options.current != preferred {
                                models = Some(change_model(&connection, &session_id, &options.config_id, preferred).await
                                    .map_err(|err| agent_client_protocol::Error::internal_error().data(err))?);
                            }
                        }
                        let _ = loop_tx.send(Event::Models(models));
                        let _ = loop_tx.send(Event::SessionId(session_id.0.to_string()));
                        loop_accepting.store(true, Ordering::Relaxed);

                        let model_connection = connection.clone();
                        let model_session_id = session_id.clone();
                        let model_event_tx = loop_tx.clone();
                        tokio::spawn(async move {
                            while let Some((config_id, value)) = model_rx.recv().await {
                                let result = change_model(&model_connection, &model_session_id, &config_id, &value).await;
                                let _ = model_event_tx.send(Event::ModelChanged(result));
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
                                    let _ = loop_tx.send(Event::Error(err.to_string()));
                                }
                            }
                        }
                        Ok(())
                    })
                    .await;

                if let Err(err) = result {
                    let _ = tx.send(Event::Error(err.to_string()));
                }
            });
        });
    }

    fn send(&mut self, cwd: PathBuf) {
        let text = self.input.text();
        if (text.trim().is_empty() && self.attachments.is_empty()) || self.streaming || self.model_switching {
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

    fn select_model(&mut self, value: String) {
        if self.streaming || self.model_switching { return; }
        self.model_menu_open = false;
        let Some(models) = &mut self.models else { return };
        if models.current == value { return; }
        if let Some(tx) = &self.model_tx {
            if tx.send((models.config_id.clone(), value)).is_ok() { self.model_switching = true; }
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
        self.threads.push(Thread::default());
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
            // Models are only reported once a session exists, so connect early to list them.
            if state.model_menu_open { state.connect(cwd); }
        }
        Message::SelectModel(value) => {
            state.select_model(value);
            state.persist(&cwd);
        }
        Message::NewThread => state.new_thread(&cwd),
        Message::SwitchThread(i) => {
            state.switch_thread(i, &cwd);
            state.thread_menu_open = false;
        }
        Message::UsePrompt(prompt) => state.input = text_editor::Content::with_text(&prompt),
        Message::ToggleThinking(index) => {
            if state.expanded_thinking.contains(&index) {
                state.expanded_thinking.retain(|i| *i != index);
            } else {
                state.expanded_thinking.push(index);
            }
        }
        Message::ThreadMenuToggle => state.thread_menu_open = !state.thread_menu_open,
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
    }
    Task::none()
}

pub fn conversation_controls(state: &AcpState) -> Element<'_, Message> {
    row![
        crate::icon_control(lucide_icons::Icon::Plus, "New conversation", (!state.streaming).then_some(Message::NewThread), false),
        crate::icon_control(lucide_icons::Icon::History, "Conversation history", Some(Message::ThreadMenuToggle), state.thread_menu_open),
    ]
    .spacing(4)
    .into()
}

pub fn view<'a>(state: &'a AcpState, cwd: PathBuf, composer: crate::ai_composer::Context<'a>) -> Element<'a, Message> {
    let mut header = column![].spacing(4);
    if state.thread_menu_open {
        let mut menu = column![].spacing(2);
        for (i, thread) in state.threads.iter().enumerate() {
            let title = if thread.title.is_empty() {
                "New thread".to_string()
            } else {
                thread.title.clone()
            };
            menu = menu.push(
                button(text(title))
                    .width(Length::Fill)
                    .padding([4, 8])
                    .style(crate::flat_button_style)
                    .on_press_maybe((!state.streaming).then_some(Message::SwitchThread(i))),
            );
        }
        header = header.push(
            container(menu).padding(4).style(|theme: &iced::Theme| {
                let palette = theme.extended_palette();
                iced::widget::container::Style {
                    background: Some(palette.background.weak.color.into()),
                    border: iced::Border::default().rounded(6.0),
                    ..iced::widget::container::Style::default()
                }
            }),
        );
    }
    let project = cwd.file_name().unwrap_or(cwd.as_os_str()).to_string_lossy().into_owned();
    header = header.push(text(format!("Project · {project}")).size(12).style(iced::widget::text::secondary));

    let labeled_copyable = |label: &'static str, content: &str| -> Element<'_, Message> {
        column![
            row![
                text(label).size(12),
                Space::new().width(Length::Fill),
                crate::icon_control(lucide_icons::Icon::Copy, "Copy message", Some(Message::Copy(content.to_string())), false),
            ]
            .spacing(6)
            .align_y(iced::Alignment::Center),
            text(content.to_string()).size(13),
        ]
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
            Entry::ToolCall { title, status, details, .. } => {
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
            Entry::User { content } => labeled_copyable("You", content),
            Entry::Thinking { content } => {
                let expanded = state.expanded_thinking.contains(&i);
                let mut section = column![button(if expanded { "Hide thinking" } else { "Show thinking" })
                    .style(crate::flat_button_style).on_press(Message::ToggleThinking(i))];
                if expanded { section = section.push(text(content.clone()).size(13).style(iced::widget::text::secondary)); }
                section.into()
            },
            Entry::Assistant { content } => {
                container(labeled_copyable(state.provider, content)).padding(12).width(Length::Fill)
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

    let mut bottom = column![];
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
        bottom = bottom.push(crate::ai_approval::view(Card {
            title: pending.title.clone(), details: pending.details.clone(), choices,
            remember: (pending.allow_once.is_some() && pending.scope.is_some()).then_some((pending.remember, Message::RememberPermission)),
            copy: Message::Copy(pending.details.clone()),
        }));
    }
    if !state.session_grants.is_empty() {
        bottom = bottom.push(button("Reset session approvals").style(crate::flat_button_style).on_press(Message::ResetPermissions));
    }
    if state.model_menu_open {
        let mut menu = column![].spacing(2);
        match &state.models {
            Some(models) if !models.choices.is_empty() => {
                for (id, name) in &models.choices {
                    let label = if id == &models.current { format!("✓ {name}") } else { name.clone() };
                    menu = menu.push(button(text(label).size(12)).width(Length::Fill).padding([4, 8])
                        .style(crate::flat_button_style)
                        .on_press_maybe((!state.streaming && !state.model_switching).then(|| Message::SelectModel(id.clone()))));
                }
            }
            _ if state.started && !state.models_reported => {
                menu = menu.push(text("Loading models…").size(12).style(iced::widget::text::secondary));
            }
            _ => {
                menu = menu.push(text(format!("{} did not report any selectable models.", state.provider)).size(12).style(iced::widget::text::secondary));
            }
        }
        bottom = bottom.push(container(scrollable(menu)).padding(4).max_height(240).style(|theme: &iced::Theme| {
            let palette = theme.extended_palette();
            iced::widget::container::Style {
                background: Some(palette.background.weak.color.into()),
                border: iced::Border::default().rounded(6.0),
                ..iced::widget::container::Style::default()
            }
        }));
    }
    let model_name = state.models.as_ref().map(ModelOptions::current_name);
    let model_button = button(text(if state.model_switching { "Switching…".into() } else { format!("{} ▾", model_name.clone().unwrap_or_else(|| "Model".into())) }).size(12))
        .padding([2, 6]).style(crate::flat_button_style).on_press(Message::ToggleModelMenu);
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
            (!state.model_switching && !awaiting_permission && (!state.input.text().trim().is_empty() || !state.attachments.is_empty())).then_some(Message::Send), false)
    };
    bottom = bottom.push(crate::ai_composer::view(
        column![
            text_editor(&state.input)
                .size(13)
                .placeholder("Ask your assistant… (Cmd/Ctrl+Enter to send)")
                .on_action(Message::InputChanged)
                .height(Length::Fixed(60.0))
                .key_binding(|key_press| {
                    let is_enter =
                        key_press.key == iced::keyboard::Key::Named(iced::keyboard::key::Named::Enter);
                    if is_enter && key_press.modifiers.command() {
                        Some(text_editor::Binding::Custom(Message::Send))
                    } else {
                        text_editor::Binding::from_key_press(key_press)
                    }
                }),
            row![text(if state.stopping { "Stopping…" } else if awaiting_permission { "Waiting for approval" } else if state.streaming { "Working…" } else { "Ready" }).size(12).style(iced::widget::text::secondary), Space::new().width(Length::Fill), model_button, usage_ring, send_button].spacing(6).align_y(iced::Alignment::Center),
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
        match block {
            ToolCallContent::Diff(diff) => {
                details.push_str(&format!("{}\n--- Before\n{}\n+++ After\n{}\n", diff.path.display(), diff.old_text.as_deref().unwrap_or("(new file)"), diff.new_text));
            }
            ToolCallContent::Content(content) => {
                if let ContentBlock::Text(text) = &content.content { details.push_str(&text.text); details.push('\n'); }
            }
            _ => {}
        }
    }
    if let Some(input) = input { details.push_str(&serde_json::to_string_pretty(input).unwrap_or_default()); }
    details
}

fn threads_path(cwd: &Path, provider: &str) -> Option<PathBuf> {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    cwd.hash(&mut hasher);
    let hash = hasher.finish();
    Some(
        crate::home_dir()?
            .join(".config")
            .join("editor")
            .join("acp_threads")
            .join(if provider == "Claude" { format!("{hash:x}.json") } else { format!("{hash:x}-codex.json") }),
    )
}

async fn change_model(connection: &ConnectionTo<Agent>, session: &SessionId, config_id: &str, value: &str) -> Result<ModelOptions, String> {
    let response = connection.send_request(SetSessionConfigOptionRequest::new(
        session.clone(), config_id.to_owned(), SessionConfigValueId::new(value),
    )).block_task().await.map_err(|err| err.to_string())?;
    let models = model_from_options(&response.config_options).ok_or("Agent did not confirm a model")?;
    if models.current != value { return Err(format!("Requested {value}, but agent reported {}", models.current)); }
    Ok(models)
}

fn model_from_options(options: &[agent_client_protocol::schema::v1::SessionConfigOption]) -> Option<ModelOptions> {
    use agent_client_protocol::schema::v1::{SessionConfigKind, SessionConfigOptionCategory, SessionConfigSelectOptions};
    options.iter().find_map(|option| {
        if option.category != Some(SessionConfigOptionCategory::Model) && option.id.0.as_ref() != "model" { return None; }
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
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn selecting_a_model_requests_the_change_and_remembers_it() {
        let mut state = AcpState::default();
        let (model_tx, mut model_rx) = tokio::sync::mpsc::unbounded_channel();
        state.model_tx = Some(model_tx);
        state.models = Some(ModelOptions { config_id: "model".into(), current: "fast".into(), choices: vec![("fast".into(), "Fast".into()), ("deep".into(), "Deep".into())] });
        state.model_menu_open = true;
        state.select_model("deep".into());
        assert_eq!(model_rx.try_recv().unwrap(), ("model".into(), "deep".into()));
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
        tx.send(Event::ModelChanged(Ok(confirmed))).unwrap();
        state.poll();
        assert!(!state.model_switching);
        assert_eq!(state.models.as_ref().unwrap().current, "deep");
        assert_eq!(state.preferred_model.as_deref(), Some("deep"));

        state.select_model("fast".into());
        assert!(state.model_switching);
        tx.send(Event::ModelChanged(Err("rejected".into()))).unwrap();
        state.poll();
        assert!(!state.model_switching);
        assert_eq!(state.models.as_ref().unwrap().current, "deep");
        assert_eq!(state.preferred_model.as_deref(), Some("deep"));
        assert!(state.entries.iter().any(|entry| matches!(entry, Entry::Assistant { content } if content.contains("rejected"))));
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
        let mut state = AcpState::codex();
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

    #[test]
    fn provider_histories_are_isolated() {
        let cwd = Path::new("project");
        assert_ne!(threads_path(cwd, "Claude"), threads_path(cwd, "Codex"));
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
