//! Zed-compatible terminal authentication advertised by the Claude ACP adapter.
//! The adapter delegates `--cli` to its bundled, unmodified Claude Code binary.
use agent_client_protocol::schema::v1::{ClientCapabilities, InitializeRequest, InitializeResponse};
use agent_client_protocol::schema::ProtocolVersion;
use serde::Deserialize;

pub type Completion = tokio::sync::oneshot::Sender<Result<(), String>>;

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Login {
    pub command: String,
    pub args: Vec<String>,
    #[serde(default)]
    pub env: std::collections::BTreeMap<String, String>,
    #[serde(skip)]
    pub name: String,
}

pub struct Request {
    pub login: Login,
    pub completion: Completion,
}

pub fn initialize(provider: &str) -> InitializeRequest {
    let request = InitializeRequest::new(ProtocolVersion::V1);
    if provider != "Claude" { return request; }
    // The legacy extension also includes the resolved Node executable and adapter
    // entry point, avoiding guesses about npm cache locations or shell quoting.
    request.client_capabilities(ClientCapabilities::new().meta(
        serde_json::json!({"terminal-auth": true}).as_object().unwrap().clone(),
    ))
}

pub fn methods(response: &InitializeResponse) -> Vec<Login> {
    response.auth_methods.iter().filter_map(|method| {
        if !matches!(method.id().0.as_ref(), "claude-ai-login" | "console-login" | "claude-login") { return None; }
        let mut login: Login = serde_json::from_value(method.meta()?.get("terminal-auth")?.clone()).ok()?;
        if login.command.is_empty() || !login.args.iter().any(|arg| arg == "--cli") { return None; }
        login.name = method.name().into();
        Some(login)
    }).collect()
}

impl Login {
    pub fn command(&self) -> portable_pty::CommandBuilder {
        let mut command = portable_pty::CommandBuilder::new(&self.command);
        command.args(&self.args);
        for (key, value) in &self.env { command.env(key, value); }
        command
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_claude_advertises_terminal_auth() {
        for provider in ["Claude", "Codex", "Grok", "Cursor"] {
            let request = serde_json::to_value(initialize(provider)).unwrap();
            assert_eq!(request["clientCapabilities"]["_meta"]["terminal-auth"].as_bool(), (provider == "Claude").then_some(true));
        }
    }

    #[test]
    fn uses_advertised_cli_command_with_literal_arguments() {
        let response: InitializeResponse = serde_json::from_value(json!({
            "protocolVersion": 1, "agentCapabilities": {}, "authMethods": [
                {"id":"claude-ai-login", "type":"terminal", "name":"Claude Subscription",
                 "_meta":{"terminal-auth":{"command":"node with spaces", "args":["adapter with spaces.js", "--cli", "auth", "login", "--claudeai", "literal $() & ;"], "env":{"EXAMPLE":"literal value"}}}},
                {"id":"console-login", "name":"Anthropic Console", "_meta":{"terminal-auth":{"command":"node", "args":["adapter.js", "--cli", "auth", "login", "--console"]}}},
                {"id":"claude-login", "name":"Remote login", "_meta":{"terminal-auth":{"command":"node", "args":["adapter.js", "--cli"]}}},
                {"id":"gateway", "name":"Gateway"},
                {"id":"claude-ai-login", "name":"Malformed", "_meta":{"terminal-auth":{"command":"", "args":["--cli"]}}},
                {"id":"claude-ai-login", "name":"Missing CLI", "_meta":{"terminal-auth":{"command":"node", "args":[]}}}
            ]
        })).unwrap();
        let methods = methods(&response);
        assert_eq!(methods.len(), 3);
        assert_eq!(methods[0].name, "Claude Subscription");
        let argv: Vec<_> = methods[0].command().get_argv().iter().map(|arg| arg.to_string_lossy().into_owned()).collect();
        assert_eq!(argv, ["node with spaces", "adapter with spaces.js", "--cli", "auth", "login", "--claudeai", "literal $() & ;"]);
        assert_eq!(methods[0].command().get_env("EXAMPLE"), Some(std::ffi::OsStr::new("literal value")));
    }
}
