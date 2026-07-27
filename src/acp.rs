use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::schema::v1::{
    ContentBlock, InitializeRequest, NewSessionRequest, PromptRequest, RequestPermissionOutcome,
    RequestPermissionRequest, RequestPermissionResponse, SelectedPermissionOutcome,
    SessionNotification, SessionUpdate, TextContent,
};
use agent_client_protocol::{AcpAgent, Agent, ConnectionTo};

#[derive(Clone, Copy, PartialEq)]
enum Role {
    User,
    Assistant,
}

struct Message {
    role: Role,
    content: String,
}

enum Event {
    Delta(String),
    Done,
    Error(String),
}

#[derive(Default)]
pub struct Acp {
    messages: Vec<Message>,
    input: String,
    rx: Option<Receiver<Event>>,
    prompt_tx: Option<tokio::sync::mpsc::UnboundedSender<String>>,
    streaming: bool,
    started: bool,
}

impl Acp {
    pub fn poll(&mut self) {
        let Some(rx) = &self.rx else { return };
        let mut finished = false;
        while let Ok(event) = rx.try_recv() {
            match event {
                Event::Delta(text) => {
                    if let Some(last) = self.messages.last_mut() {
                        last.content.push_str(&text);
                    }
                }
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
        }
    }

    fn start(&mut self, ctx: egui::Context, cwd: PathBuf) {
        if self.started {
            return;
        }
        self.started = true;

        let (tx, rx): (Sender<Event>, Receiver<Event>) = mpsc::channel();
        self.rx = Some(rx);
        let (prompt_tx, mut prompt_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        self.prompt_tx = Some(prompt_tx);

        std::thread::spawn(move || {
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(err) => {
                    let _ = tx.send(Event::Error(err.to_string()));
                    ctx.request_repaint();
                    return;
                }
            };

            runtime.block_on(async move {
                let agent = match AcpAgent::from_args(["npx", "@agentclientprotocol/claude-agent-acp"])
                {
                    Ok(agent) => agent,
                    Err(err) => {
                        let _ = tx.send(Event::Error(err.to_string()));
                        ctx.request_repaint();
                        return;
                    }
                };

                let notify_tx = tx.clone();
                let notify_ctx = ctx.clone();
                let loop_tx = tx.clone();
                let loop_ctx = ctx.clone();

                let result = agent_client_protocol::Client
                    .builder()
                    .on_receive_notification(
                        async move |notification: SessionNotification, _cx| {
                            if let SessionUpdate::AgentMessageChunk(chunk) = notification.update {
                                if let ContentBlock::Text(text) = chunk.content {
                                    let _ = notify_tx.send(Event::Delta(text.text));
                                    notify_ctx.request_repaint();
                                }
                            }
                            Ok(())
                        },
                        agent_client_protocol::on_receive_notification!(),
                    )
                    .on_receive_request(
                        async move |request: RequestPermissionRequest, responder, _connection| {
                            let option_id = request.options.first().map(|opt| opt.option_id.clone());
                            if let Some(id) = option_id {
                                responder.respond(RequestPermissionResponse::new(
                                    RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(
                                        id,
                                    )),
                                ))
                            } else {
                                responder.respond(RequestPermissionResponse::new(
                                    RequestPermissionOutcome::Cancelled,
                                ))
                            }
                        },
                        agent_client_protocol::on_receive_request!(),
                    )
                    .connect_with(agent, move |connection: ConnectionTo<Agent>| async move {
                        connection
                            .send_request(InitializeRequest::new(ProtocolVersion::V1))
                            .block_task()
                            .await?;

                        let session = connection
                            .send_request(NewSessionRequest::new(cwd))
                            .block_task()
                            .await?;
                        let session_id = session.session_id;

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
                            loop_ctx.request_repaint();
                        }
                        Ok(())
                    })
                    .await;

                if let Err(err) = result {
                    let _ = tx.send(Event::Error(err.to_string()));
                    ctx.request_repaint();
                }
            });
        });
    }

    fn send(&mut self, ctx: egui::Context, cwd: PathBuf) {
        let text = std::mem::take(&mut self.input);
        if text.trim().is_empty() || self.streaming {
            return;
        }
        self.start(ctx, cwd);
        self.messages.push(Message {
            role: Role::User,
            content: text.clone(),
        });
        self.messages.push(Message {
            role: Role::Assistant,
            content: String::new(),
        });
        self.streaming = true;
        if let Some(tx) = &self.prompt_tx {
            let _ = tx.send(text);
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, cwd: PathBuf) {
        let scroll_height = (ui.available_height() - 40.0).max(0.0);
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .max_height(scroll_height)
            .show(ui, |ui| {
                for message in &self.messages {
                    let sender = match message.role {
                        Role::User => "You",
                        Role::Assistant => "Claude",
                    };
                    ui.strong(sender);
                    ui.label(&message.content);
                    ui.add_space(6.0);
                }
            });

        ui.separator();

        ui.horizontal(move |ui| {
            let response = ui.add(egui::TextEdit::singleline(&mut self.input));
            let enter_pressed =
                response.lost_focus() && ui.ctx().input(|i| i.key_pressed(egui::Key::Enter));

            if (ui.button("Send").clicked() || enter_pressed) && !self.input.trim().is_empty() {
                let ctx = ui.ctx().clone();
                self.send(ctx, cwd);
            }
        });
    }
}
