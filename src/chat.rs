use iced::widget::{button, column, container, row, scrollable, text, text_input};
use iced::{Element, Length};
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

struct ChatMsg {
    role: Role,
    content: String,
}

enum Event {
    Delta(String),
    Done,
    Error(String),
}

pub struct ChatState {
    base_url: String,
    api_key: String,
    model: String,
    messages: Vec<ChatMsg>,
    input: String,
    rx: Option<Receiver<Event>>,
    cancel: Option<Arc<AtomicBool>>,
    streaming: bool,
}

impl Default for ChatState {
    fn default() -> Self {
        Self {
            base_url: "https://api.openai.com/v1".to_string(),
            api_key: String::new(),
            model: String::new(),
            messages: Vec::new(),
            input: String::new(),
            rx: None,
            cancel: None,
            streaming: false,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Message {
    BaseUrlChanged(String),
    ApiKeyChanged(String),
    ModelChanged(String),
    InputChanged(String),
    Send,
    Stop,
}

impl ChatState {
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

    fn send(&mut self) {
        let text = std::mem::take(&mut self.input);
        if text.trim().is_empty() || self.streaming {
            return;
        }
        self.messages.push(ChatMsg {
            role: Role::User,
            content: text,
        });
        self.messages.push(ChatMsg {
            role: Role::Assistant,
            content: String::new(),
        });

        let history: Vec<(&'static str, String)> = self.messages[..self.messages.len() - 1]
            .iter()
            .map(|m| (m.role.as_str(), m.content.clone()))
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
            run_request(base_url, api_key, model, history, tx, cancel);
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
}

pub fn update(state: &mut ChatState, message: Message) {
    match message {
        Message::BaseUrlChanged(text) => state.base_url = text,
        Message::ApiKeyChanged(text) => state.api_key = text,
        Message::ModelChanged(text) => state.model = text,
        Message::InputChanged(text) => state.input = text,
        Message::Send => state.send(),
        Message::Stop => state.stop(),
    }
}

pub fn view(state: &ChatState) -> Element<'_, Message> {
    let provider = column![
        row![text("Base URL"), text_input("", &state.base_url).on_input(Message::BaseUrlChanged)]
            .spacing(6),
        row![
            text("API Key"),
            text_input("", &state.api_key)
                .on_input(Message::ApiKeyChanged)
                .secure(true)
        ]
        .spacing(6),
        row![text("Model"), text_input("", &state.model).on_input(Message::ModelChanged)]
            .spacing(6),
    ]
    .spacing(4);

    let mut messages = column![].spacing(6);
    for message in &state.messages {
        let sender = match message.role {
            Role::User => "You",
            Role::Assistant => "AI",
        };
        messages = messages.push(column![text(sender), text(message.content.clone())]);
    }
    let messages = scrollable(messages).height(Length::Fill);

    let input_row = row![
        text_input("", &state.input).on_input(Message::InputChanged),
        if state.streaming {
            button(text("Stop")).on_press(Message::Stop)
        } else {
            button(text("Send")).on_press(Message::Send)
        },
    ]
    .spacing(4);

    container(column![provider, messages, input_row].spacing(8))
        .height(Length::Fill)
        .into()
}

fn run_request(
    base_url: String,
    api_key: String,
    model: String,
    history: Vec<(&'static str, String)>,
    tx: mpsc::Sender<Event>,
    cancel: Arc<AtomicBool>,
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
            }
        }
    }
    let _ = tx.send(Event::Done);
}
