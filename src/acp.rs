use iced::widget::{button, column, container, row, scrollable, text, text_input};
use iced::{Element, Length};
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
            ToolStatus::Pending => "...",
            ToolStatus::InProgress => "*",
            ToolStatus::Completed => "v",
            ToolStatus::Failed => "x",
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
    Done,
    Error(String),
}

#[derive(Default)]
pub struct AcpState {
    loaded: bool,
    cwd: Option<PathBuf>,
    threads: Vec<Thread>,
    active_thread: usize,
    entries: Vec<Entry>,
    input: String,
    rx: Option<Receiver<Event>>,
    prompt_tx: Option<tokio::sync::mpsc::UnboundedSender<String>>,
    cancel_tx: Option<tokio::sync::mpsc::UnboundedSender<()>>,
    streaming: bool,
    started: bool,
    thinking_index: Option<usize>,
    assistant_index: Option<usize>,
    usage: Option<Usage>,
    pending_permission: Option<PendingPermission>,
    just_finished: bool,
    thread_menu_open: bool,
}

#[derive(Debug, Clone)]
pub enum Message {
    NewThread,
    SwitchThread(usize),
    ThreadMenuToggle,
    InputChanged(String),
    Send,
    Stop,
    PermissionChosen(String),
}

impl AcpState {
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
                    self.pending_permission = Some(pending);
                }
                Event::ToolCall { id, title, status } => {
                    Self::upsert_tool_call(&mut self.entries, id, Some(title), Some(status));
                }
                Event::ToolCallUpdate { id, title, status } => {
                    Self::upsert_tool_call(&mut self.entries, id, title, status);
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
        if let Some(id) = new_session_id {
            if let Some(thread) = self.threads.get_mut(self.active_thread) {
                thread.session_id = Some(id);
            }
        }
        if finished {
            self.streaming = false;
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
        });
    }

    fn ensure_loaded(&mut self, cwd: &Path) {
        if self.loaded {
            return;
        }
        self.loaded = true;

        if let Some(path) = threads_path(cwd) {
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
        let Some(path) = threads_path(cwd) else { return };
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
        self.started = false;
        self.rx = None;
        self.prompt_tx = None;
        self.cancel_tx = None;
        self.streaming = false;
        self.pending_permission = None;
    }

    fn start(&mut self, cwd: PathBuf, resume_session_id: Option<String>) {
        if self.started {
            return;
        }
        self.started = true;

        let (tx, rx): (Sender<Event>, Receiver<Event>) = mpsc::channel();
        self.rx = Some(rx);
        let (prompt_tx, mut prompt_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        self.prompt_tx = Some(prompt_tx);
        let (cancel_tx, mut cancel_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        self.cancel_tx = Some(cancel_tx);

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
                let agent = match AcpAgent::from_args(["npx", "@agentclientprotocol/claude-agent-acp"])
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
                                    let _ = notify_tx.send(Event::ToolCall {
                                        id: tool_call.tool_call_id.0.to_string(),
                                        title: tool_call.title,
                                        status: convert_status(tool_call.status),
                                    });
                                }
                                SessionUpdate::ToolCallUpdate(update) => {
                                    let _ = notify_tx.send(Event::ToolCallUpdate {
                                        id: update.tool_call_id.0.to_string(),
                                        title: update.fields.title,
                                        status: update.fields.status.map(convert_status),
                                    });
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

                            let (respond_tx, respond_rx) = tokio::sync::oneshot::channel::<String>();
                            let _ = permission_tx.send(Event::Permission(PendingPermission {
                                title,
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

                        while let Some(text) = prompt_rx.recv().await {
                            let result = connection
                                .send_request(PromptRequest::new(
                                    session_id.clone(),
                                    vec![ContentBlock::Text(TextContent::new(text))],
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
        let text = std::mem::take(&mut self.input);
        if text.trim().is_empty() || self.streaming {
            return;
        }
        let resume_session_id = self
            .threads
            .get(self.active_thread)
            .and_then(|t| t.session_id.clone());
        self.start(cwd, resume_session_id);
        self.entries.push(Entry::User {
            content: text.clone(),
        });
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
            let _ = tx.send(text);
        }
    }

    fn stop(&mut self) {
        if let Some(tx) = &self.cancel_tx {
            let _ = tx.send(());
        }
        self.streaming = false;
    }

    fn new_thread(&mut self, cwd: &Path) {
        self.save_current_thread();
        self.persist(cwd);
        self.threads.push(Thread::default());
        self.active_thread = self.threads.len() - 1;
        self.entries.clear();
        self.thinking_index = None;
        self.assistant_index = None;
        self.usage = None;
        self.reset_connection();
    }

    fn switch_thread(&mut self, index: usize, cwd: &Path) {
        if index == self.active_thread {
            return;
        }
        self.save_current_thread();
        self.persist(cwd);
        self.active_thread = index;
        self.entries = self.threads[index].entries.clone();
        self.thinking_index = None;
        self.assistant_index = None;
        self.usage = None;
        self.reset_connection();
    }
}

pub fn update(state: &mut AcpState, message: Message, cwd: PathBuf) {
    state.ensure_loaded(&cwd);
    state.cwd = Some(cwd.clone());
    match message {
        Message::NewThread => state.new_thread(&cwd),
        Message::SwitchThread(i) => {
            state.switch_thread(i, &cwd);
            state.thread_menu_open = false;
        }
        Message::ThreadMenuToggle => state.thread_menu_open = !state.thread_menu_open,
        Message::InputChanged(text) => state.input = text,
        Message::Send => state.send(cwd),
        Message::Stop => state.stop(),
        Message::PermissionChosen(option_id) => {
            if let Some(pending) = state.pending_permission.take() {
                let _ = pending.respond.send(option_id);
            }
        }
    }
}

pub fn view(state: &AcpState, cwd: PathBuf) -> Element<'_, Message> {
    let mut top_bar = row![
        button(text("+")).on_press(Message::NewThread),
        button(text("...")).on_press(Message::ThreadMenuToggle),
    ]
    .spacing(4);
    if state.thread_menu_open {
        let mut menu = row![].spacing(4);
        for (i, thread) in state.threads.iter().enumerate() {
            let title = if thread.title.is_empty() {
                "New thread".to_string()
            } else {
                thread.title.clone()
            };
            menu = menu.push(button(text(title)).on_press(Message::SwitchThread(i)));
        }
        top_bar = top_bar.push(menu);
    }
    let _ = cwd;

    let mut messages = column![].spacing(6);
    let last = state.entries.len().saturating_sub(1);
    for (i, entry) in state.entries.iter().enumerate() {
        if let Entry::Thinking { content } = entry {
            if content.is_empty() {
                continue;
            }
        }
        let card: Element<'_, Message> = match entry {
            Entry::ToolCall { title, status, .. } => {
                row![text(status.icon()), text(title.clone())].spacing(6).into()
            }
            Entry::User { content } => {
                column![text("You"), text(content.clone())].into()
            }
            Entry::Thinking { content } => {
                column![text("Thinking"), text(content.clone())].into()
            }
            Entry::Assistant { content } => {
                if content.is_empty() && i == last && state.streaming {
                    column![text("Claude"), text("...")].into()
                } else {
                    column![text("Claude"), text(content.clone())].into()
                }
            }
        };
        messages = messages.push(container(card).padding(6));
    }
    let messages = scrollable(messages).height(Length::Fill);

    let mut bottom = column![];
    if let Some(pending) = &state.pending_permission {
        bottom = bottom.push(text(pending.title.clone()));
        let mut options = row![].spacing(4);
        for (option_id, name) in &pending.options {
            options = options
                .push(button(text(name.clone())).on_press(Message::PermissionChosen(option_id.clone())));
        }
        bottom = bottom.push(options);
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
        button(text("Stop")).on_press(Message::Stop)
    } else if awaiting_permission {
        button(text("Send"))
    } else {
        button(text("Send")).on_press(Message::Send)
    };
    bottom = bottom.push(
        row![
            text_input("", &state.input).on_input(Message::InputChanged),
            send_button,
        ]
        .spacing(4),
    );

    column![top_bar, messages, bottom].height(Length::Fill).into()
}

fn threads_path(cwd: &Path) -> Option<PathBuf> {
    use std::hash::{Hash, Hasher};
    let home = std::env::var_os("HOME")?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    cwd.hash(&mut hasher);
    let hash = hasher.finish();
    Some(
        PathBuf::from(home)
            .join(".config")
            .join("editor")
            .join("acp_threads")
            .join(format!("{hash:x}.json")),
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
