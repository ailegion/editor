//! Grok's ACP authentication, shared by chat and one-off requests.
use agent_client_protocol::{Agent, ConnectionTo};
use agent_client_protocol::schema::v1::{AuthenticateRequest, InitializeResponse};
use tokio::sync::mpsc::UnboundedReceiver;

#[derive(Debug, Clone, PartialEq)]
pub enum Update {
    Required,
    Waiting,
    Link { url: String, can_paste: bool },
    Failed(String),
    SignedIn,
}

#[derive(Debug)]
pub enum Command { SignIn, Submit(String), Cancel }

fn method(init: &InitializeResponse, interactive: bool) -> Option<String> {
    let ids = if interactive { &["grok.com", "oidc"][..] } else { &["cached_token", "xai.api_key"][..] };
    let advertised = |id: &str| init.auth_methods.iter().any(|m| m.id().0.as_ref() == id);
    if !interactive {
        if let Some(default) = init.meta.as_ref().and_then(|m| m.get("defaultAuthMethodId")).and_then(|v| v.as_str()) {
            if ids.contains(&default) && advertised(default) { return Some(default.into()); }
        }
    }
    ids.iter().find(|id| advertised(id)).map(|id| (*id).into())
}

pub async fn extension(connection: &ConnectionTo<Agent>, method: &str, params: serde_json::Value) -> agent_client_protocol::Result<serde_json::Value> {
    // The leading underscore is ACP's extension-method wire prefix.
    let request = agent_client_protocol::UntypedMessage::new(&format!("_{method}"), params)?;
    connection.send_request(request).block_task().await
}

pub async fn authenticate(
    connection: &ConnectionTo<Agent>, init: &InitializeResponse,
    commands: Option<&mut UnboundedReceiver<Command>>, notify: impl Fn(Update),
) -> agent_client_protocol::Result<()> {
    if let Some(id) = method(init, false) {
        let request = AuthenticateRequest::new(id).meta(serde_json::json!({"headless": true}).as_object().unwrap().clone());
        match connection.send_request(request).block_task().await {
            Ok(_) => { notify(Update::SignedIn); return Ok(()); }
            Err(err) if err.code == agent_client_protocol::Error::auth_required().code => {}
            Err(err) => return Err(err),
        }
    }
    let Some(commands) = commands else {
        return Err(agent_client_protocol::Error::auth_required().data("Sign in to Grok in the AI chat panel first."));
    };
    let id = method(init, true).ok_or_else(|| agent_client_protocol::Error::auth_required().data("Grok did not offer browser sign-in. Check its authentication configuration."))?;
    notify(Update::Required);
    loop {
        match commands.recv().await {
            Some(Command::SignIn) => {}
            Some(_) => continue,
            None => return Err(agent_client_protocol::Error::auth_required().data("Sign-in cancelled")),
        }
        notify(Update::Waiting);
        match browser_login(connection, &id, commands, &notify).await {
            Ok(true) => { notify(Update::SignedIn); return Ok(()); }
            Ok(false) => notify(Update::Required),
            Err(err) => {
                let _ = extension(connection, "x.ai/auth/cancel", serde_json::json!({})).await;
                notify(Update::Failed(crate::agent_launch::describe(&err)));
            }
        }
    }
}

async fn browser_login(
    connection: &ConnectionTo<Agent>, id: &str,
    commands: &mut UnboundedReceiver<Command>, notify: &impl Fn(Update),
) -> agent_client_protocol::Result<bool> {
    let request = AuthenticateRequest::new(id.to_owned()).meta(serde_json::json!({"headless": false}).as_object().unwrap().clone());
    let auth = connection.send_request(request).block_task();
    tokio::pin!(auth);
    let urls = async {
        loop {
            // get_url can return null before the authenticate handler installs its channel.
            let value = extension(connection, "x.ai/auth/get_url", serde_json::json!({})).await?;
            if let Some(url) = value.get("auth_url").and_then(|v| v.as_str()).filter(|v| !v.is_empty()) {
                let can_paste = value.get("mode").and_then(|v| v.as_str()).map_or(
                    value.get("external_provider").and_then(|v| v.as_bool()) != Some(true),
                    |mode| mode == "loopback",
                );
                notify(Update::Link { url: url.into(), can_paste });
                return Ok::<(), agent_client_protocol::Error>(());
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
    };
    tokio::pin!(urls);
    let mut got_url = false;
    loop {
        tokio::select! {
            biased;
            result = &mut auth => return result.map(|_| true),
            result = &mut urls, if !got_url => { result?; got_url = true; }
            command = commands.recv() => match command {
                Some(Command::Submit(code)) => {
                    extension(connection, "x.ai/auth/submit_code", serde_json::json!({"code": code})).await?;
                }
                Some(Command::SignIn) => {}
                Some(Command::Cancel) | None => {
                    let _ = extension(connection, "x.ai/auth/cancel", serde_json::json!({})).await;
                    return Ok(false);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::{Client, Error};
    use agent_client_protocol::schema::{ProtocolVersion, v1::{
        AuthMethod, AuthMethodAgent, AuthenticateResponse,
        InitializeRequest, NewSessionRequest, NewSessionResponse,
    }};
    use std::sync::{Arc, Mutex};

    fn init(ids: &[&str]) -> InitializeResponse {
        InitializeResponse::new(ProtocolVersion::V1).auth_methods(ids.iter().map(|id| {
            AuthMethod::Agent(AuthMethodAgent::new((*id).to_string(), *id))
        }).collect())
    }

    #[test]
    fn selects_only_advertised_credentials_and_respects_the_default() {
        let mut response = init(&["grok.com", "xai.api_key", "cached_token"]);
        assert_eq!(method(&response, false).as_deref(), Some("cached_token"));
        response.meta = Some(serde_json::json!({"defaultAuthMethodId": "xai.api_key"}).as_object().unwrap().clone());
        assert_eq!(method(&response, false).as_deref(), Some("xai.api_key"));
        assert_eq!(method(&init(&["grok.com"]), false), None);
        assert_eq!(method(&init(&["oidc"]), true).as_deref(), Some("oidc"));
        assert_eq!(method(&init(&["unknown"]), true), None);
        response.auth_methods = init(&["grok.com"]).auth_methods;
        assert_eq!(method(&response, false), None, "an unadvertised default is unusable");
    }

    #[tokio::test]
    async fn saved_credentials_authenticate_before_session_creation() {
        for id in ["cached_token", "xai.api_key"] {
            let signed_in = Arc::new(Mutex::new(false));
            let sign_in = signed_in.clone();
            let session_auth = signed_in.clone();
            let agent = Agent.builder()
                .on_receive_request(async move |_: InitializeRequest, responder, _cx| {
                    responder.respond(init(&[id, "grok.com"]))
                }, agent_client_protocol::on_receive_request!())
                .on_receive_request(async move |request: AuthenticateRequest, responder, _cx| {
                    assert_eq!(request.method_id.0.as_ref(), id);
                    assert_eq!(request.meta.unwrap()["headless"], true);
                    *sign_in.lock().unwrap() = true;
                    responder.respond(AuthenticateResponse::new())
                }, agent_client_protocol::on_receive_request!())
                .on_receive_request(async move |_: NewSessionRequest, responder, _cx| {
                    if !*session_auth.lock().unwrap() { return responder.respond_with_error(Error::auth_required()); }
                    responder.respond(NewSessionResponse::new("authenticated"))
                }, agent_client_protocol::on_receive_request!());
            Client.builder().connect_with(agent, async |connection: ConnectionTo<Agent>| {
                let init = connection.send_request(InitializeRequest::new(ProtocolVersion::V1)).block_task().await?;
                authenticate(&connection, &init, None, |_| {}).await?;
                connection.send_request(NewSessionRequest::new("project")).block_task().await?;
                Ok(())
            }).await.unwrap();
            assert!(*signed_in.lock().unwrap());
        }
    }

    #[tokio::test]
    async fn browser_login_accepts_a_link_and_code_then_continues() {
        let (complete, completed) = tokio::sync::oneshot::channel::<()>();
        let complete = Arc::new(Mutex::new(Some(complete)));
        let completed = Arc::new(Mutex::new(Some(completed)));
        let agent = Agent.builder()
            .on_receive_request(async move |request: AuthenticateRequest, responder, cx| {
                assert_eq!(request.method_id.0.as_ref(), "grok.com");
                assert_eq!(request.meta.unwrap()["headless"], false);
                let completed = completed.lock().unwrap().take().unwrap();
                cx.spawn(async move {
                    completed.await.unwrap();
                    responder.respond(AuthenticateResponse::new())
                })
            }, agent_client_protocol::on_receive_request!())
            .on_receive_request(async move |request: agent_client_protocol::UntypedMessage, responder, _cx| {
                let (name, params) = request.into_parts();
                let value = match name.as_str() {
                    "_x.ai/auth/get_url" => serde_json::json!({"auth_url": "https://example.invalid/login", "mode": "loopback"}),
                    "_x.ai/auth/submit_code" => {
                        assert_eq!(params["code"], "test-code");
                        complete.lock().unwrap().take().unwrap().send(()).unwrap();
                        serde_json::json!({"submitted": true})
                    }
                    other => panic!("unexpected extension: {other}"),
                };
                responder.respond(value)
            }, agent_client_protocol::on_receive_request!());
        let (commands, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let updates = Arc::new(Mutex::new(Vec::new()));
        let observed = updates.clone();
        Client.builder().connect_with(agent, async move |connection: ConnectionTo<Agent>| {
            authenticate(&connection, &init(&["grok.com"]), Some(&mut rx), |update| {
                match &update {
                    Update::Required => commands.send(Command::SignIn).unwrap(),
                    Update::Link { .. } => commands.send(Command::Submit("test-code".into())).unwrap(),
                    _ => {}
                }
                observed.lock().unwrap().push(update);
            }).await
        }).await.unwrap();
        assert_eq!(*updates.lock().unwrap(), [Update::Required, Update::Waiting,
            Update::Link { url: "https://example.invalid/login".into(), can_paste: true }, Update::SignedIn]);
    }

    #[tokio::test]
    async fn one_off_requests_direct_unsigned_users_to_the_panel() {
        Client.builder().connect_with(Agent.builder(), async |connection: ConnectionTo<Agent>| {
            let err = authenticate(&connection, &init(&["grok.com"]), None, |_| panic!("no interactive UI")).await.unwrap_err();
            assert!(crate::agent_launch::describe(&err).contains("AI chat panel"));
            Ok(())
        }).await.unwrap();
    }

    #[tokio::test]
    async fn expired_credentials_allow_browser_retry_after_failure() {
        let attempts = Arc::new(Mutex::new(Vec::new()));
        let calls = attempts.clone();
        let agent = Agent.builder().on_receive_request(async move |request: AuthenticateRequest, responder, _cx| {
            let mut calls = calls.lock().unwrap();
            calls.push(request.method_id.0.to_string());
            if calls.len() < 3 { responder.respond_with_error(Error::auth_required()) }
            else { responder.respond(AuthenticateResponse::new()) }
        }, agent_client_protocol::on_receive_request!())
        .on_receive_request(async |request: agent_client_protocol::UntypedMessage, responder, _cx| {
            let (name, _) = request.into_parts();
            assert!(name == "_x.ai/auth/get_url" || name == "_x.ai/auth/cancel");
            responder.respond(serde_json::json!({"auth_url": null}))
        }, agent_client_protocol::on_receive_request!());
        let (commands, mut rx) = tokio::sync::mpsc::unbounded_channel();
        Client.builder().connect_with(agent, async move |connection: ConnectionTo<Agent>| {
            authenticate(&connection, &init(&["cached_token", "grok.com"]), Some(&mut rx), |update| {
                if matches!(update, Update::Required | Update::Failed(_)) {
                    commands.send(Command::SignIn).unwrap();
                }
            }).await
        }).await.unwrap();
        assert_eq!(*attempts.lock().unwrap(), ["cached_token", "grok.com", "grok.com"]);
    }

    #[tokio::test]
    async fn cancelling_a_browser_attempt_leaves_sign_in_available() {
        let (cancel, cancelled) = tokio::sync::oneshot::channel::<()>();
        let cancel = Arc::new(Mutex::new(Some(cancel)));
        let cancelled = Arc::new(Mutex::new(Some(cancelled)));
        let agent = Agent.builder()
            .on_receive_request(async move |_: AuthenticateRequest, responder, cx| {
                let cancelled = cancelled.lock().unwrap().take().unwrap();
                cx.spawn(async move {
                    cancelled.await.unwrap();
                    responder.respond_with_error(Error::auth_required())
                })
            }, agent_client_protocol::on_receive_request!())
            .on_receive_request(async move |request: agent_client_protocol::UntypedMessage, responder, _cx| {
                let (name, _) = request.into_parts();
                match name.as_str() {
                    "_x.ai/auth/get_url" => responder.respond(serde_json::json!({"auth_url": "https://example.invalid/login", "mode": "device"})),
                    "_x.ai/auth/cancel" => {
                        cancel.lock().unwrap().take().unwrap().send(()).unwrap();
                        responder.respond(serde_json::json!({"cancelled": true}))
                    }
                    other => panic!("unexpected extension: {other}"),
                }
            }, agent_client_protocol::on_receive_request!());
        let (commands, mut rx) = tokio::sync::mpsc::unbounded_channel();
        Client.builder().connect_with(agent, async move |connection: ConnectionTo<Agent>| {
            let updates = Mutex::new(Vec::new());
            let result = browser_login(&connection, "grok.com", &mut rx, &|update| {
                commands.send(Command::Cancel).unwrap();
                updates.lock().unwrap().push(update);
            }).await?;
            assert!(!result);
            assert_eq!(*updates.lock().unwrap(), [Update::Link { url: "https://example.invalid/login".into(), can_paste: false }]);
            Ok(())
        }).await.unwrap();
    }
}
