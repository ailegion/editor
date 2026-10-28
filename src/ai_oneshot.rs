//! Single questions to the selected assistant, for features that need one answer rather than
//! a conversation: commit messages and inline edits. Nothing here touches the panels' threads.
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::schema::v1::{
    ContentBlock, InitializeRequest, NewSessionRequest, PromptRequest, RequestPermissionOutcome,
    RequestPermissionRequest, RequestPermissionResponse, SessionNotification, SessionUpdate, TextContent,
};
use agent_client_protocol::{Agent, ConnectionTo};

#[derive(Debug, Clone)]
pub enum Backend {
    /// An OpenAI-compatible chat completions endpoint.
    Http { base_url: String, api_key: String, model: String },
    /// A fresh ACP agent session; it may read the project but not change it.
    Agent { provider: &'static str, model: Option<String> },
}

pub async fn ask(backend: Backend, cwd: PathBuf, prompt: String) -> Result<String, String> {
    let reply = match backend {
        Backend::Http { base_url, api_key, model } => {
            tokio::task::spawn_blocking(move || ask_http(&base_url, &api_key, &model, &prompt))
                .await.map_err(|err| err.to_string())??
        }
        Backend::Agent { provider, model } => {
            // Agents run on their own runtime, like the panel's sessions.
            let (tx, rx) = tokio::sync::oneshot::channel();
            std::thread::spawn(move || {
                let result = tokio::runtime::Builder::new_current_thread().enable_all().build()
                    .map_err(|err| err.to_string())
                    .and_then(|runtime| runtime.block_on(ask_agent(provider, model, cwd, prompt)));
                let _ = tx.send(result);
            });
            rx.await.map_err(|_| "The agent stopped unexpectedly".to_string())??
        }
    };
    if reply.trim().is_empty() { Err("The assistant returned an empty answer".into()) } else { Ok(reply) }
}

fn ask_http(base_url: &str, api_key: &str, model: &str, prompt: &str) -> Result<String, String> {
    let body = serde_json::json!({
        "model": model,
        "stream": false,
        "messages": [{ "role": "user", "content": prompt }],
    }).to_string();
    let mut request = ureq::post(&format!("{base_url}/chat/completions")).header("Content-Type", "application/json");
    if !api_key.trim().is_empty() {
        request = request.header("Authorization", &format!("Bearer {api_key}"));
    }
    let mut response = request.send(body).map_err(|err| err.to_string())?;
    let text = response.body_mut().read_to_string().map_err(|err| err.to_string())?;
    http_reply(&text)
}

fn http_reply(body: &str) -> Result<String, String> {
    let value: serde_json::Value = serde_json::from_str(body).map_err(|err| format!("invalid response: {err}"))?;
    if let Some(message) = value["error"]["message"].as_str() { return Err(message.to_string()); }
    value["choices"][0]["message"]["content"].as_str().map(str::to_string)
        .ok_or_else(|| "the response had no answer".to_string())
}

async fn ask_agent(provider: &'static str, model: Option<String>, cwd: PathBuf, prompt: String) -> Result<String, String> {
    let agent = crate::agent_launch::provider_agent(provider)?;
    let (agent, diagnostics) = crate::agent_launch::Diagnostics::attach(agent);
    let reply = Arc::new(Mutex::new(String::new()));
    connect(agent, reply.clone(), cwd, prompt, model, provider).await.map_err(|err| diagnostics.describe(&err))?;
    let reply = reply.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone();
    Ok(reply)
}

/// Collects the agent's reply text into `reply`. Tool permission requests are declined: a
/// one-off answer must not edit the project behind the user's back.
async fn connect(
    agent: impl agent_client_protocol::ConnectTo<agent_client_protocol::Client> + 'static,
    reply: Arc<Mutex<String>>, cwd: PathBuf, prompt: String, model: Option<String>, provider: &'static str,
) -> agent_client_protocol::Result<()> {
    agent_client_protocol::Client.builder()
        .on_receive_notification(async move |notification: SessionNotification, _cx| {
            if let SessionUpdate::AgentMessageChunk(chunk) = notification.update {
                if let ContentBlock::Text(text) = chunk.content {
                    reply.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).push_str(&text.text);
                }
            }
            Ok(())
        }, agent_client_protocol::on_receive_notification!())
        .on_receive_request(async move |_request: RequestPermissionRequest, responder, _cx| {
            responder.respond(RequestPermissionResponse::new(RequestPermissionOutcome::Cancelled))
        }, agent_client_protocol::on_receive_request!())
        .connect_with(agent, async move |connection: ConnectionTo<Agent>| prompt_once(&connection, cwd, prompt, model, provider).await)
        .await
}

async fn prompt_once(connection: &ConnectionTo<Agent>, cwd: PathBuf, prompt: String, model: Option<String>, provider: &str) -> agent_client_protocol::Result<()> {
    let init = connection.send_request(InitializeRequest::new(ProtocolVersion::V1)).block_task().await?;
    if provider == "Grok" { crate::grok_auth::authenticate(connection, &init, None, |_| {}).await?; }
    let request = NewSessionRequest::new(cwd);
    let session = if provider == "Cursor" {
        crate::cursor_auth::session(connection, &init, request, None, |_| {}).await?
    } else if provider == "Codex" {
        crate::codex_auth::session(connection, &init, request, None, |_| {}).await?
    } else { connection.send_request(request).block_task().await? };
    // Use the model picked in the panel when the agent offers it; otherwise its default.
    if let (Some(model), Some(options)) = (model, crate::acp::model_from_options(&session.config_options.unwrap_or_default())) {
        if !crate::acp::is_same_model(&model, &options.current) {
            let _ = crate::acp::change_model(connection, &session.session_id, &options.config_id, &model).await;
        }
    }
    connection.send_request(PromptRequest::new(session.session_id, vec![ContentBlock::Text(TextContent::new(prompt))]))
        .block_task().await?;
    Ok(())
}

/// The first fenced code block's contents, or the whole reply when it has none. Models often
/// wrap an answer in a fence even when asked not to.
pub fn unfence(reply: &str) -> String {
    let mut lines = reply.lines();
    if lines.by_ref().any(|line| line.trim_start().starts_with("```")) {
        let body: Vec<&str> = lines.take_while(|line| !line.trim_start().starts_with("```")).collect();
        return body.join("\n");
    }
    reply.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::schema::v1::{
        ContentChunk, InitializeResponse, NewSessionResponse, PromptResponse, SessionId, StopReason,
    };

    #[test]
    fn http_answers_and_errors_are_read_from_the_response() {
        assert_eq!(http_reply(r#"{"choices":[{"message":{"content":"Fix typo"}}]}"#), Ok("Fix typo".into()));
        assert_eq!(http_reply(r#"{"error":{"message":"model not found"}}"#), Err("model not found".into()));
        assert!(http_reply(r#"{"choices":[]}"#).is_err());
        assert!(http_reply("not json").is_err());
    }

    #[test]
    fn fenced_answers_are_unwrapped() {
        assert_eq!(unfence("```rust\nfn a() {}\n    b();\n```\nExplanation"), "fn a() {}\n    b();");
        assert_eq!(unfence("Here:\n```\nx\n```"), "x");
        assert_eq!(unfence("  plain answer \n"), "plain answer");
    }

    /// An in-process agent streams the reply in pieces and asks for a permission, which the
    /// one-off request declines; no agent program is started.
    #[tokio::test]
    async fn agent_reply_is_collected_and_permissions_are_declined() {
        use agent_client_protocol::schema::v1::{ToolCallUpdate, ToolCallUpdateFields};
        let declined = Arc::new(Mutex::new(None));
        let saw_decline = declined.clone();
        let agent = Agent.builder()
            .on_receive_request(async |_: InitializeRequest, responder, _cx| {
                responder.respond(InitializeResponse::new(ProtocolVersion::V1))
            }, agent_client_protocol::on_receive_request!())
            .on_receive_request(async |_: NewSessionRequest, responder, _cx| {
                responder.respond(NewSessionResponse::new(SessionId::new("one-off")))
            }, agent_client_protocol::on_receive_request!())
            .on_receive_request(async move |request: PromptRequest, responder, cx| {
                let saw_decline = saw_decline.clone();
                cx.spawn({
                    let cx = cx.clone();
                    async move {
                        for piece in ["Add ", "login ", "check"] {
                            cx.send_notification(SessionNotification::new(request.session_id.clone(), SessionUpdate::AgentMessageChunk(
                                ContentChunk::new(ContentBlock::Text(TextContent::new(piece))))))?;
                        }
                        let permission = RequestPermissionRequest::new(request.session_id.clone(),
                            ToolCallUpdate::new("edit", ToolCallUpdateFields::new()), Vec::new());
                        let outcome = cx.send_request(permission).block_task().await?.outcome;
                        *saw_decline.lock().unwrap() = Some(matches!(outcome, RequestPermissionOutcome::Cancelled));
                        responder.respond(PromptResponse::new(StopReason::EndTurn))
                    }
                })
            }, agent_client_protocol::on_receive_request!());
        let reply = Arc::new(Mutex::new(String::new()));
        let run = connect(agent, reply.clone(), PathBuf::from("."), "Write a commit message".into(), None, "Claude");
        tokio::time::timeout(std::time::Duration::from_secs(5), run).await.unwrap().unwrap();
        assert_eq!(*reply.lock().unwrap(), "Add login check");
        assert_eq!(*declined.lock().unwrap(), Some(true));
    }

    #[tokio::test]
    async fn codex_one_off_uses_saved_login_and_never_starts_interactive_authentication() {
        for signed_in in [true, false] {
            let agent = Agent.builder()
                .on_receive_request(async |_: InitializeRequest, responder, _cx| {
                    use agent_client_protocol::schema::v1::{AuthMethod, AuthMethodAgent};
                    responder.respond(InitializeResponse::new(ProtocolVersion::V1)
                        .auth_methods(vec![AuthMethod::Agent(AuthMethodAgent::new("chat-gpt", "ChatGPT"))]))
                }, agent_client_protocol::on_receive_request!())
                .on_receive_request(async move |_: NewSessionRequest, responder, _cx| {
                    if signed_in { responder.respond(NewSessionResponse::new("saved-login")) }
                    else { responder.respond_with_error(agent_client_protocol::Error::auth_required()) }
                }, agent_client_protocol::on_receive_request!())
                .on_receive_request(async |_: PromptRequest, responder, _cx| {
                    responder.respond(PromptResponse::new(StopReason::EndTurn))
                }, agent_client_protocol::on_receive_request!());
            let result = connect(agent, Arc::new(Mutex::new(String::new())), PathBuf::from("project"), "Explain".into(), None, "Codex").await;
            if signed_in { result.unwrap(); }
            else { assert!(crate::agent_launch::describe(&result.unwrap_err()).contains("Sign in to Codex in the AI chat panel")); }
        }
    }
}
