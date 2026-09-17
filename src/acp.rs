use iced::widget::{button, column, container, row, scrollable, text, text_editor, Space};
use iced::{Element, Length, Task};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::schema::v1::{
    CancelNotification, ContentBlock, InitializeRequest, LoadSessionRequest, NewSessionRequest,
    PermissionOptionId, PromptRequest, RequestPermissionOutcome, RequestPermissionRequest,
    RequestPermissionResponse, SelectedPermissionOutcome, SessionId, SessionNotification,
    SessionUpdate, TextContent, ToolCallStatus,
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
    options: Vec<(String, String)>,
    respond: tokio::sync::oneshot::Sender<String>,
}

enum Event {
    Delta(String),
    ThoughtDelta(String),
    Usage(u64, u64, Option<(f64, String)>),
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
    prompt_tx: Option<tokio::sync::mpsc::UnboundedSender<Vec<ContentBlock>>>,
    cancel_tx: Option<tokio::sync::mpsc::UnboundedSender<()>>,
    streaming: bool,
    started: bool,
    thinking_index: Option<usize>,
    assistant_index: Option<usize>,
    usage: Option<Usage>,
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
            streaming: false,
            started: false,
            thinking_index: None,
            assistant_index: None,
            usage: None,
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
    NewThread,
    SwitchThread(usize),
    ThreadMenuToggle,
    ToggleThinking(usize),
    UsePrompt(String),
    InputChanged(text_editor::Action),
    Send,
    Stop,
    PermissionChosen(String),
    AllowSession,
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
        while let Ok(event) = rx.try_recv() {
            match event {
                Event::Delta(text) => {
                    if let Some(i) = self.assistant_index {
                        if let Some(Entry::Assistant { content }) = self.entries.get_mut(i) {
                            content.push_str(&text);
                        }
                    }
                }
                Event::ThoughtDelta(text) => {
                    if let Some(i) = self.thinking_index {
                        if let Some(Entry::Thinking { content }) = self.entries.get_mut(i) {
                            content.push_str(&text);
                        }
                    }
                }
                Event::Usage(used, size, cost) => {
                    self.usage = Some(Usage { used, size, cost });
                }
                Event::SessionId(id) => new_session_id = Some(id),
                Event::Permission(pending) => {
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
                    if let Some(i) = self.assistant_index {
                        if let Some(Entry::Assistant { content }) = self.entries.get_mut(i) {
                            content.push_str(&format!("\n[error: {err}]"));
                        }
                    }
                    finished = true;
                }
            }
        }
        if self.pending_permission.is_none() { self.pending_permission = self.permission_queue.pop_front(); }
        let disconnected = self.prompt_tx.as_ref().is_some_and(|tx| tx.is_closed());
        if disconnected {
            self.session_grants.clear();
            self.started = false;
            self.prompt_tx = None;
            self.cancel_tx = None;
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
            self.pending_permission = None;
            self.permission_queue.clear();
            self.just_finished = true;
            self.save_current_thread();
            if let Some(cwd) = self.cwd.clone() {
                self.persist(&cwd);
            }
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
        };
        if let Ok(text) = serde_json::to_string(&store) {
            let _ = std::fs::write(path, text);
        }
    }

    fn reset_connection(&mut self) {
        self.session_grants.clear();
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
        let (prompt_tx, mut prompt_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<ContentBlock>>();
        self.prompt_tx = Some(prompt_tx);
        let (cancel_tx, mut cancel_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        self.cancel_tx = Some(cancel_tx);

        let provider = self.provider;
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
                let agent = match AcpAgent::from_args([if cfg!(windows) { "npx.cmd" } else { "npx" }, "--yes", if provider == "Codex" { "@zed-industries/codex-acp" } else { "@agentclientprotocol/claude-agent-acp" }])
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
                        async move |request: RequestPermissionRequest, responder, _connection| {
                            let title = request
                                .tool_call
                                .fields
                                .title
                                .clone()
                                .unwrap_or_else(|| "Permission requested".to_string());
                            let options: Vec<(String, String)> = request
                                .options
                                .iter()
                                .map(|opt| (opt.option_id.0.to_string(), opt.name.clone()))
                                .collect();

                            let allow_once = request.options.iter().find(|o| o.kind == agent_client_protocol::schema::v1::PermissionOptionKind::AllowOnce).map(|o| o.option_id.0.to_string());
                            // Reuse approval only for an identical operation in this live session.
                            let scope = request.tool_call.fields.raw_input.as_ref().map(|input| format!("{title}:{input}"));
                            let details = tool_details(request.tool_call.fields.content.as_deref().unwrap_or_default(), request.tool_call.fields.raw_input.as_ref());
                            let (respond_tx, respond_rx) = tokio::sync::oneshot::channel::<String>();
                            let _ = permission_tx.send(Event::Permission(PendingPermission {
                                title,
                                details, allow_once, scope,
                                options,
                                respond: respond_tx,
                            }));

                            match respond_rx.await {
                                Ok(option_id) => responder.respond(RequestPermissionResponse::new(
                                    RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(
                                        PermissionOptionId::new(option_id),
                                    )),
                                )),
                                Err(_) => responder.respond(RequestPermissionResponse::new(
                                    RequestPermissionOutcome::Cancelled,
                                )),
                            }
                        },
                        agent_client_protocol::on_receive_request!(),
                    )
                    .connect_with(agent, move |connection: ConnectionTo<Agent>| async move {
                        let init_response = connection
                            .send_request(InitializeRequest::new(ProtocolVersion::V1))
                            .block_task()
                            .await?;

                        let mut loaded_session_id = None;
                        if init_response.agent_capabilities.load_session {
                            if let Some(id) = resume_session_id.clone() {
                                let loaded = connection
                                    .send_request(LoadSessionRequest::new(id.clone(), cwd.clone()))
                                    .block_task()
                                    .await;
                                if loaded.is_ok() {
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
                                session.session_id
                            }
                        };
                        let _ = loop_tx.send(Event::SessionId(session_id.0.to_string()));
                        loop_accepting.store(true, Ordering::Relaxed);

                        let cancel_connection = connection.clone();
                        let cancel_session_id = session_id.clone();
                        tokio::spawn(async move {
                            while cancel_rx.recv().await.is_some() {
                                let _ = cancel_connection.send_notification(CancelNotification::new(
                                    cancel_session_id.clone(),
                                ));
                            }
                        });

                        while let Some(prompt) = prompt_rx.recv().await {
                            if !init_response.agent_capabilities.prompt_capabilities.image && prompt.iter().any(|b| matches!(b, ContentBlock::Image(_))) {
                                let _ = loop_tx.send(Event::Error("This agent does not support image prompts. Remove images and try again.".into()));
                                continue;
                            }
                            let result = connection
                                .send_request(PromptRequest::new(
                                    session_id.clone(),
                                    prompt,
                                ))
                                .block_task()
                                .await;
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
        if (text.trim().is_empty() && self.attachments.is_empty()) || self.streaming {
            return;
        }
        let text = text.trim_end().to_string();
        self.input = text_editor::Content::new();
        let resume_session_id = self
            .threads
            .get(self.active_thread)
            .and_then(|t| t.session_id.clone());
        self.start(cwd, resume_session_id);
        let mut prompt = vec![ContentBlock::Text(TextContent::new(text.clone()))];
        let mut display = text;
        for attachment in self.attachments.drain(..) {
            display.push_str(&format!("\n[Attached: {}]", attachment.name));
            prompt.push(attachment.acp());
        }
        self.entries.push(Entry::User { content: display });
        self.thinking_index = Some(self.entries.len());
        self.entries.push(Entry::Thinking {
            content: String::new(),
        });
        self.assistant_index = Some(self.entries.len());
        self.entries.push(Entry::Assistant {
            content: String::new(),
        });
        self.streaming = true;
        if let Some(tx) = &self.prompt_tx {
            let _ = tx.send(prompt);
        }
    }

    fn stop(&mut self) {
        if let Some(tx) = &self.cancel_tx {
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
        self.thinking_index = None;
        self.assistant_index = None;
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
        self.thinking_index = None;
        self.assistant_index = None;
        self.usage = None;
        self.reset_connection();
        self.persist(cwd);
    }
}

pub fn update(state: &mut AcpState, message: Message, cwd: PathBuf) -> Task<Message> {
    state.ensure_loaded(&cwd);
    state.cwd = Some(cwd.clone());
    match message {
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
        Message::AllowSession => {
            if let Some(pending) = state.pending_permission.take() {
                if let (Some(scope), Some(option)) = (pending.scope, pending.allow_once) {
                    state.session_grants.insert(scope);
                    let _ = pending.respond.send(option);
                }
            }
        }
        Message::PermissionChosen(option_id) => {
            if let Some(pending) = state.pending_permission.take() {
                let _ = pending.respond.send(option_id);
            }
        }
        Message::Copy(text) => return iced::clipboard::write(text),
    }
    Task::none()
}

pub fn view(state: &AcpState, cwd: PathBuf) -> Element<'_, Message> {
    let top_bar = row![
        text("Conversation").size(12),
        Space::new().width(Length::Fill),
        crate::icon_control(lucide_icons::Icon::Plus, "New conversation", (!state.streaming).then_some(Message::NewThread), false),
        crate::icon_control(lucide_icons::Icon::History, "Conversation history", Some(Message::ThreadMenuToggle), state.thread_menu_open),
    ]
    .spacing(4)
    .width(Length::Fill);
    let mut header = column![top_bar].spacing(4);
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
    let last = state.entries.len().saturating_sub(1);
    for (i, entry) in state.entries.iter().enumerate() {
        if let Entry::Thinking { content } = entry {
            if content.is_empty() {
                continue;
            }
        }
        let card: Element<'_, Message> = match entry {
            Entry::ToolCall { title, status, details, .. } => {
                let expanded = state.expanded_thinking.contains(&i);
                let mut card = column![row![text(status.icon()).size(12), button(text(title.clone()).size(13)).style(crate::flat_button_style).on_press(Message::ToggleThinking(i))].spacing(6)];
                if expanded && !details.is_empty() { card = card.push(text(details.clone()).size(12)); }
                card.into()
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
                if content.is_empty() && i == last && state.streaming {
                    column![
                        text(state.provider),
                        text("Waiting for response...").style(|theme: &iced::Theme| {
                            let palette = theme.extended_palette();
                            iced::widget::text::Style { color: Some(palette.background.strong.color) }
                        }),
                    ]
                    .into()
                } else {
                    labeled_copyable(state.provider, content)
                }
            }
        };
        messages = messages.push(container(card).padding(6));
    }
    let messages = scrollable(messages).anchor_bottom().height(Length::Fill);

    let mut bottom = column![];
    if let Some(pending) = &state.pending_permission {
        bottom = bottom.push(text("Approval needed").size(14));
        bottom = bottom.push(text(pending.title.clone()).size(13));
        bottom = bottom.push(scrollable(text(pending.details.clone()).size(12)).height(Length::Fixed(140.0)));
        if pending.allow_once.is_some() && pending.scope.is_some() {
            bottom = bottom.push(button("Allow identical operation for this session").on_press(Message::AllowSession));
        }
        let mut options = column![].spacing(4);
        for (option_id, name) in &pending.options {
            options = options
                .push(button(text(name.clone())).on_press(Message::PermissionChosen(option_id.clone())));
        }
        bottom = bottom.push(options);
    }
    if !state.session_grants.is_empty() {
        bottom = bottom.push(button("Reset session approvals").style(crate::flat_button_style).on_press(Message::ResetPermissions));
    }
    if let Some(usage) = &state.usage {
        let percent = if usage.size > 0 {
            usage.used as f64 / usage.size as f64 * 100.0
        } else {
            0.0
        };
        bottom = bottom.push(text(format!(
            "Context: {percent:.0}% ({}/{})",
            format_tokens(usage.used),
            format_tokens(usage.size)
        )));
        if let Some((amount, currency)) = &usage.cost {
            bottom = bottom.push(text(format!("{amount:.2} {currency}")));
        }
    }

    let awaiting_permission = state.pending_permission.is_some();
    let send_button = if state.streaming {
        crate::icon_control(lucide_icons::Icon::CircleStop, "Stop response", (!state.stopping).then_some(Message::Stop), false)
    } else {
        crate::icon_control(lucide_icons::Icon::SendHorizonal, "Send message (Cmd/Ctrl+Enter)",
            (!awaiting_permission && (!state.input.text().trim().is_empty() || !state.attachments.is_empty())).then_some(Message::Send), false)
    };
    bottom = bottom.push(
        column![
            text_editor(&state.input)
                .size(13)
                .placeholder("Ask Claude… (Cmd/Ctrl+Enter to send)")
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
            row![text(if state.stopping { "Stopping…" } else if awaiting_permission { "Waiting for approval" } else if state.streaming { "Working…" } else { "Ready" }).size(12).style(iced::widget::text::secondary), Space::new().width(Length::Fill), send_button],
        ]
        .spacing(4),
    );

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

fn format_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{}k", n / 1_000)
    } else {
        n.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn permission(scope: &str) -> (PendingPermission, tokio::sync::oneshot::Receiver<String>) {
        let (respond, rx) = tokio::sync::oneshot::channel();
        (PendingPermission { title: "Edit file".into(), details: "replacement".into(), scope: Some(scope.into()), allow_once: Some("accept".into()), options: vec![], respond }, rx)
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
            title: "Run command".into(), details: String::new(), allow_once: None, scope: None, options: vec![], respond,
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
