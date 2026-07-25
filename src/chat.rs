use std::io::{BufRead, BufReader};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;

#[derive(Clone, Copy, PartialEq)]
enum Role {
    User,
    Assistant,
}

impl Role {
    fn as_str(self) -> &'static str {
        match self {
            Role::User => "user",
            Role::Assistant => "assistant",
        }
    }
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
struct Provider {
    base_url: String,
    api_key: String,
    model: String,
}

pub struct Chat {
    provider: Provider,
    messages: Vec<Message>,
    input: String,
    rx: Option<Receiver<Event>>,
    cancel: Option<Arc<AtomicBool>>,
    streaming: bool,
}

impl Default for Chat {
    fn default() -> Self {
        Self {
            provider: Provider {
                base_url: "https://api.openai.com/v1".to_string(),
                api_key: String::new(),
                model: String::new(),
            },
            messages: Vec::new(),
            input: String::new(),
            rx: None,
            cancel: None,
            streaming: false,
        }
    }
}

impl Chat {
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
            self.rx = None;
            self.cancel = None;
        }
    }

    fn send(&mut self, ctx: egui::Context) {
        let text = std::mem::take(&mut self.input);
        if text.trim().is_empty() || self.streaming {
            return;
        }
        self.messages.push(Message {
            role: Role::User,
            content: text,
        });
        self.messages.push(Message {
            role: Role::Assistant,
            content: String::new(),
        });

        let history: Vec<(&'static str, String)> = self.messages[..self.messages.len() - 1]
            .iter()
            .map(|m| (m.role.as_str(), m.content.clone()))
            .collect();

        let base_url = self.provider.base_url.trim_end_matches('/').to_string();
        let api_key = self.provider.api_key.clone();
        let model = self.provider.model.clone();

        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        self.rx = Some(rx);
        self.cancel = Some(cancel.clone());
        self.streaming = true;

        std::thread::spawn(move || {
            run_request(base_url, api_key, model, history, tx, cancel, ctx);
        });
    }

    fn stop(&mut self) {
        if let Some(cancel) = &self.cancel {
            cancel.store(true, Ordering::Relaxed);
        }
        self.streaming = false;
        self.rx = None;
        self.cancel = None;
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        egui::CollapsingHeader::new("Provider")
            .default_open(false)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Base URL");
                    ui.text_edit_singleline(&mut self.provider.base_url);
                });
                ui.horizontal(|ui| {
                    ui.label("API Key");
                    ui.add(egui::TextEdit::singleline(&mut self.provider.api_key).password(true));
                });
                ui.horizontal(|ui| {
                    ui.label("Model");
                    ui.text_edit_singleline(&mut self.provider.model);
                });
            });

        ui.separator();

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for message in &self.messages {
                    let sender = match message.role {
                        Role::User => "You",
                        Role::Assistant => "AI",
                    };
                    ui.strong(sender);
                    ui.label(&message.content);
                    ui.add_space(6.0);
                }
            });

        ui.separator();

        ui.horizontal(|ui| {
            let response = ui.add(egui::TextEdit::singleline(&mut self.input));
            let enter_pressed = response.lost_focus()
                && ui.ctx().input(|i| i.key_pressed(egui::Key::Enter));

            if self.streaming {
                if ui.button("Stop").clicked() {
                    self.stop();
                }
            } else if (ui.button("Send").clicked() || enter_pressed)
                && !self.input.trim().is_empty()
            {
                let ctx = ui.ctx().clone();
                self.send(ctx);
            }
        });
    }
}

fn run_request(
    base_url: String,
    api_key: String,
    model: String,
    history: Vec<(&'static str, String)>,
    tx: mpsc::Sender<Event>,
    cancel: Arc<AtomicBool>,
    ctx: egui::Context,
) {
    let messages: Vec<serde_json::Value> = history
        .iter()
        .map(|(role, content)| serde_json::json!({ "role": role, "content": content }))
        .collect();
    let body = serde_json::json!({
        "model": model,
        "stream": true,
        "messages": messages,
    })
    .to_string();

    let url = format!("{base_url}/chat/completions");
    let result = ureq::post(&url)
        .header("Authorization", &format!("Bearer {api_key}"))
        .header("Content-Type", "application/json")
        .send(body);

    let response = match result {
        Ok(response) => response,
        Err(err) => {
            let _ = tx.send(Event::Error(err.to_string()));
            ctx.request_repaint();
            return;
        }
    };

    let reader = BufReader::new(response.into_body().into_reader());
    for line in reader.lines() {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let Ok(line) = line else { break };
        let Some(data) = line.strip_prefix("data: ") else {
            continue;
        };
        if data == "[DONE]" {
            break;
        }
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(data) {
            if let Some(delta) = value["choices"][0]["delta"]["content"].as_str() {
                let _ = tx.send(Event::Delta(delta.to_string()));
                ctx.request_repaint();
            }
        }
    }
    let _ = tx.send(Event::Done);
    ctx.request_repaint();
}
