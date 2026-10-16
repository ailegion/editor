//! Codex's advertised `chat-gpt` ACP method delegates browser login to the adapter
//! and the official Codex app-server, as in Zed. Existing CLI login stays authoritative.
use agent_client_protocol::{Agent, ConnectionTo, JsonRpcRequest};
use agent_client_protocol::schema::v1::{AuthenticateRequest, InitializeResponse};
use crate::grok_auth::{Command, Update};
use tokio::sync::mpsc::UnboundedReceiver;

pub async fn session<Req: JsonRpcRequest + Clone>(
    connection: &ConnectionTo<Agent>, init: &InitializeResponse, request: Req,
    commands: Option<&mut UnboundedReceiver<Command>>, notify: impl Fn(Update),
) -> agent_client_protocol::Result<Req::Response> {
    match connection.send_request(request.clone()).block_task().await {
        Ok(session) => return Ok(session),
        Err(err) if err.code == agent_client_protocol::Error::auth_required().code => {}
        Err(err) => return Err(err),
    }
    let Some(commands) = commands else {
        return Err(agent_client_protocol::Error::auth_required().data("Sign in to Codex in the AI chat panel first."));
    };
    if !init.auth_methods.iter().any(|m| m.id().0.as_ref() == "chat-gpt") {
        return Err(agent_client_protocol::Error::auth_required().data("Codex did not offer browser sign-in. Sign in with the Codex CLI instead."));
    }
    notify(Update::Required);
    loop {
        match commands.recv().await {
            Some(Command::SignIn) => {}
            Some(_) => continue,
            None => return Err(agent_client_protocol::Error::auth_required().data("Sign-in cancelled")),
        }
        notify(Update::Waiting);
        let auth = connection.send_request(AuthenticateRequest::new("chat-gpt")).block_task();
        tokio::pin!(auth);
        let result = loop {
            tokio::select! {
                result = &mut auth => break result,
                command = commands.recv() => match command {
                    Some(Command::SignIn) | Some(Command::Submit(_)) => {}
                    Some(Command::Cancel) | None => return Err(agent_client_protocol::Error::auth_required().data("Sign-in cancelled")),
                }
            }
        };
        match result {
            Ok(_) => {
                let session = connection.send_request(request.clone()).block_task().await?;
                notify(Update::SignedIn);
                return Ok(session);
            }
            Err(err) => notify(Update::Failed(crate::agent_launch::describe(&err))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::{Client, Error};
    use agent_client_protocol::schema::{ProtocolVersion, v1::{AuthMethod, AuthMethodAgent, AuthenticateResponse, LoadSessionRequest, LoadSessionResponse, NewSessionRequest, NewSessionResponse}};
    use std::sync::{Arc, Mutex};

    fn init() -> InitializeResponse {
        InitializeResponse::new(ProtocolVersion::V1).auth_methods(vec![AuthMethod::Agent(AuthMethodAgent::new("chat-gpt", "Codex Login"))])
    }

    #[tokio::test]
    async fn saved_credentials_and_api_keys_need_no_browser_login() {
        let agent = Agent.builder()
            .on_receive_request(async |_: NewSessionRequest, responder, _cx| responder.respond(NewSessionResponse::new("existing-auth")), agent_client_protocol::on_receive_request!())
            .on_receive_request(async |_: LoadSessionRequest, responder, _cx| responder.respond(LoadSessionResponse::new()), agent_client_protocol::on_receive_request!())
            .on_receive_request(async |_: AuthenticateRequest, _responder, _cx| {
                panic!("must reuse saved authentication instead of opening a browser");
                #[allow(unreachable_code)]
                _responder.respond(AuthenticateResponse::new())
            }, agent_client_protocol::on_receive_request!());
        Client.builder().connect_with(agent, async |connection: ConnectionTo<Agent>| {
            let response = session(&connection, &init(), NewSessionRequest::new("project"), None, |_| panic!("no sign-in UI needed")).await?;
            assert_eq!(response.session_id.0.as_ref(), "existing-auth");
            session(&connection, &init(), LoadSessionRequest::new("saved-session", "project"), None, |_| panic!("no sign-in UI needed")).await?;
            Ok(())
        }).await.unwrap();
    }

    #[tokio::test]
    async fn sign_in_can_retry_then_resume_the_original_session() {
        let signed_in = Arc::new(Mutex::new(false));
        let session_auth = signed_in.clone();
        let attempts = Arc::new(Mutex::new(0));
        let auth_attempts = attempts.clone();
        let agent = Agent.builder()
            .on_receive_request(async move |request: LoadSessionRequest, responder, _cx| {
                assert_eq!(request.session_id.0.as_ref(), "saved-session");
                if !*session_auth.lock().unwrap() { responder.respond_with_error(Error::auth_required()) }
                else { responder.respond(LoadSessionResponse::new()) }
            }, agent_client_protocol::on_receive_request!())
            .on_receive_request(async move |request: AuthenticateRequest, responder, _cx| {
                assert_eq!(request.method_id.0.as_ref(), "chat-gpt");
                assert!(request.meta.is_none(), "only the advertised ACP method is sent; credentials stay with Codex");
                let mut attempts = auth_attempts.lock().unwrap();
                *attempts += 1;
                if *attempts == 1 { responder.respond_with_error(Error::auth_required().data("Login denied")) }
                else { *signed_in.lock().unwrap() = true; responder.respond(AuthenticateResponse::new()) }
            }, agent_client_protocol::on_receive_request!());
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let updates = Arc::new(Mutex::new(Vec::new()));
        let observed = updates.clone();
        Client.builder().connect_with(agent, async move |connection: ConnectionTo<Agent>| {
            session(&connection, &init(), LoadSessionRequest::new("saved-session", "project"), Some(&mut rx), |update| {
                if matches!(update, Update::Required | Update::Failed(_)) { tx.send(Command::SignIn).unwrap(); }
                observed.lock().unwrap().push(update);
            }).await?;
            Ok(())
        }).await.unwrap();
        assert_eq!(*attempts.lock().unwrap(), 2);
        let updates = updates.lock().unwrap();
        assert_eq!(updates.len(), 5);
        assert_eq!(updates[0], Update::Required);
        assert_eq!(updates[1], Update::Waiting);
        assert!(matches!(&updates[2], Update::Failed(error) if error.contains("Login denied")));
        assert_eq!(updates[3], Update::Waiting);
        assert_eq!(updates[4], Update::SignedIn);
    }

    #[tokio::test]
    async fn one_off_requests_ask_for_panel_login_without_opening_a_browser() {
        let agent = Agent.builder().on_receive_request(async |_: NewSessionRequest, responder, _cx| {
            responder.respond_with_error(Error::auth_required())
        }, agent_client_protocol::on_receive_request!());
        Client.builder().connect_with(agent, async |connection: ConnectionTo<Agent>| {
            let err = session(&connection, &init(), NewSessionRequest::new("project"), None, |_| panic!("no UI")).await.unwrap_err();
            assert!(crate::agent_launch::describe(&err).contains("Sign in to Codex in the AI chat panel"));
            Ok(())
        }).await.unwrap();
    }

    #[tokio::test]
    async fn closing_sign_in_cancels_without_replaying_a_session() {
        let agent = Agent.builder().on_receive_request(async |_: NewSessionRequest, responder, _cx| {
            responder.respond_with_error(Error::auth_required())
        }, agent_client_protocol::on_receive_request!());
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        drop(tx);
        Client.builder().connect_with(agent, async move |connection: ConnectionTo<Agent>| {
            let err = session(&connection, &init(), NewSessionRequest::new("project"), Some(&mut rx), |_| {}).await.unwrap_err();
            assert!(crate::agent_launch::describe(&err).contains("cancelled"));
            Ok(())
        }).await.unwrap();
    }

    #[tokio::test]
    async fn session_errors_do_not_start_login() {
        let agent = Agent.builder().on_receive_request(async |_: NewSessionRequest, responder, _cx| {
            responder.respond_with_error(Error::internal_error().data("Session failed"))
        }, agent_client_protocol::on_receive_request!());
        Client.builder().connect_with(agent, async |connection: ConnectionTo<Agent>| {
            let err = session(&connection, &init(), NewSessionRequest::new("project"), None, |_| panic!("not an auth error")).await.unwrap_err();
            assert!(crate::agent_launch::describe(&err).contains("Session failed"));
            Ok(())
        }).await.unwrap();
    }

    #[tokio::test]
    async fn browser_login_is_never_requested_unless_the_adapter_advertises_it() {
        let agent = Agent.builder().on_receive_request(async |_: NewSessionRequest, responder, _cx| {
            responder.respond_with_error(Error::auth_required())
        }, agent_client_protocol::on_receive_request!());
        let (_tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        Client.builder().connect_with(agent, async move |connection: ConnectionTo<Agent>| {
            let init = InitializeResponse::new(ProtocolVersion::V1)
                .auth_methods(vec![AuthMethod::Agent(AuthMethodAgent::new("api-key", "API key"))]);
            let err = session(&connection, &init, NewSessionRequest::new("project"), Some(&mut rx), |_| panic!("no browser login offered")).await.unwrap_err();
            assert!(crate::agent_launch::describe(&err).contains("Codex CLI"));
            Ok(())
        }).await.unwrap();
    }
}
