//! Starting Claude, Codex and Grok through `npx`. Apps opened from Finder or
//! the Dock inherit launchd's minimal `PATH` (`/usr/bin:/bin:/usr/sbin:/sbin`), which misses
//! Homebrew, nvm and similar, so on macOS and Linux the user's shell is asked for its `PATH`
//! once, when an ACP chat is first opened, and put in front of the inherited one.
use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::sync::{Once, OnceLock};

use agent_client_protocol::AcpAgent;

#[cfg(any(not(windows), test))]
const MARKER: &str = "__EDITOR_SHELL_PATH__";
pub const NODE_MISSING: &str = "Node.js was not found. Claude, Codex and Grok run through Node.js: install it from https://nodejs.org, then restart the editor.";

struct Launch {
    path: OsString,
    npx: Option<PathBuf>,
}

/// Starts the lookup in the background, once, so the first message does not wait for it.
pub fn warm_up() {
    static STARTED: Once = Once::new();
    STARTED.call_once(|| { std::thread::spawn(launch); });
}

pub fn provider_missing(provider: &str) -> Option<&'static str> {
    if provider == "Cursor" { return None; }
    LAUNCH.get().and_then(missing_for)
}

fn missing_for(launch: &Launch) -> Option<&'static str> {
    launch.npx.is_none().then_some(NODE_MISSING)
}

pub fn provider_found(provider: &str) -> bool {
    provider == "Cursor" || LAUNCH.get().is_some_and(|launch| missing_for(launch).is_none())
}

pub fn provider_agent(provider: &str) -> Result<AcpAgent, String> {
    let launch = launch();
    let args = if provider == "Cursor" {
        let package = crate::cursor_agent::ensure()?;
        crate::cursor_agent::args(&package, cfg!(windows), launch.path.to_str().ok_or("PATH is not valid Unicode")?)
    } else { provider_args(launch, provider)? };
    AcpAgent::from_args(args).map_err(|err| describe(&err))
}

fn provider_args(launch: &Launch, provider: &str) -> Result<Vec<String>, String> {
    if let Some(missing) = missing_for(launch) { return Err(missing.into()); }
    let path = launch.path.to_str().ok_or("PATH is not valid Unicode")?;
    let package = match provider {
        "Codex" => "@agentclientprotocol/codex-acp",
        "Grok" => "@xai-official/grok",
        _ => "@agentclientprotocol/claude-agent-acp",
    };
    let mut args = vec![format!("PATH={path}"), launch.npx.as_ref().unwrap().display().to_string(), "--yes".into(), package.into()];
    if provider == "Grok" { args.extend(["agent".into(), "stdio".into()]); }
    Ok(args)
}

/// A readable message for a protocol error. The crate wraps errors from its tasks with the
/// source location they were spawned at, which is a path on the build machine; that is dropped.
pub fn describe(err: &agent_client_protocol::Error) -> String {
    let mut data = err.data.as_ref();
    while let Some(inner) = data.and_then(|value| value.get("spawned_at").and(value.get("data"))) {
        data = Some(inner);
    }
    let message = if err.message.is_empty() { i32::from(err.code).to_string() } else { err.message.clone() };
    match data {
        None | Some(serde_json::Value::Null) => message,
        Some(serde_json::Value::String(detail)) => format!("{message}: {detail}"),
        Some(detail) => format!("{message}: {}", serde_json::to_string_pretty(detail).unwrap_or_default()),
    }
}

static LAUNCH: OnceLock<Launch> = OnceLock::new();

fn launch() -> &'static Launch {
    LAUNCH.get_or_init(|| {
        let inherited = std::env::var_os("PATH").unwrap_or_default();
        let path = match shell_path() {
            Some(shell) => merge_paths(&shell, &inherited),
            None => inherited,
        };
        let npx = find_in(if cfg!(windows) { "npx.cmd" } else { "npx" }, &path);
        Launch { path, npx }
    })
}

/// `front`'s directories followed by those of `back` it does not already have.
fn merge_paths(front: &OsStr, back: &OsStr) -> OsString {
    let mut dirs: Vec<PathBuf> = Vec::new();
    for dir in std::env::split_paths(front).chain(std::env::split_paths(back)) {
        if !dir.as_os_str().is_empty() && !dirs.contains(&dir) { dirs.push(dir); }
    }
    std::env::join_paths(dirs).unwrap_or_else(|_| back.to_os_string())
}

fn find_in(program: &str, path: &OsStr) -> Option<PathBuf> {
    std::env::split_paths(path).map(|dir| dir.join(program)).find(|candidate| candidate.is_file())
}

/// The `PATH` printed between markers, ignoring whatever else the shell's startup files print.
#[cfg(any(not(windows), test))]
fn parse_marked(output: &str) -> Option<OsString> {
    let start = output.find(MARKER)? + MARKER.len();
    let length = output[start..].find(MARKER)?;
    let path = output[start..start + length].trim();
    (!path.is_empty()).then(|| path.into())
}

/// Windows GUI apps already get the user's full `PATH`.
#[cfg(windows)]
fn shell_path() -> Option<OsString> { None }

/// Runs the user's shell as a login, interactive shell, the way a terminal would, so both
/// `.zprofile` (Homebrew) and `.zshrc` (nvm) are read. Gives up after a few seconds in case a
/// startup file waits for input.
#[cfg(not(windows))]
fn shell_path() -> Option<OsString> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    let shell = std::env::var_os("SHELL").filter(|shell| !shell.is_empty())
        .unwrap_or_else(|| OsString::from(if cfg!(target_os = "macos") { "/bin/zsh" } else { "/bin/sh" }));
    let script = format!("printf '{MARKER}%s{MARKER}' \"$PATH\"");
    let mut child = Command::new(shell)
        .args(["-l", "-i", "-c", script.as_str()])
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null())
        .spawn().ok()?;
    let mut stdout = child.stdout.take()?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut output = String::new();
        let _ = stdout.read_to_string(&mut output);
        let _ = tx.send(output);
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    while child.try_wait().ok()?.is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    parse_marked(&rx.recv_timeout(Duration::from_secs(1)).ok()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_can_start_without_node_or_npx_discovery() {
        assert!(provider_found("Cursor"));
        assert_eq!(provider_missing("Cursor"), None);
        let launch = Launch { path: "test-path".into(), npx: None };
        assert_eq!(missing_for(&launch), Some(NODE_MISSING));
        let package = PathBuf::from("cached cursor");
        for windows in [false, true] {
            let args = crate::cursor_agent::args(&package, windows, "test-path");
            assert_eq!(args[1], package.join(if windows { "node.exe" } else { "node" }).display().to_string());
            assert_eq!(args[2], package.join("index.js").display().to_string());
            assert_eq!(args[3], "acp");
        }
    }

    #[test]
    fn provider_commands_and_requirements_are_independent() {
        let mut launch = Launch { path: "test-path".into(), npx: None };
        for provider in ["Claude", "Codex", "Grok"] {
            assert_eq!(missing_for(&launch), Some(NODE_MISSING));
            assert_eq!(provider_args(&launch, provider), Err(NODE_MISSING.into()));
        }
        launch.npx = Some(PathBuf::from("npx-test"));
        assert_eq!(missing_for(&launch), None);
        for (provider, package) in [("Claude", "@agentclientprotocol/claude-agent-acp"), ("Codex", "@agentclientprotocol/codex-acp")] {
            assert_eq!(provider_args(&launch, provider).unwrap(), ["PATH=test-path", "npx-test", "--yes", package]);
        }
        assert_eq!(provider_args(&launch, "Grok").unwrap(), ["PATH=test-path", "npx-test", "--yes", "@xai-official/grok", "agent", "stdio"]);
    }

    #[test]
    fn grok_uses_npx_without_a_separately_installed_cli_on_any_platform() {
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("node bin");
        std::fs::create_dir(&bin).unwrap();
        let program = if cfg!(windows) { "npx.cmd" } else { "npx" };
        std::fs::write(bin.join(program), "").unwrap();
        let path = std::env::join_paths([&bin]).unwrap();
        let launch = Launch { npx: find_in(program, &path), path };
        assert!(!bin.join("grok").exists());
        assert!(!bin.join("grok.exe").exists());
        let args = provider_args(&launch, "Grok").unwrap();
        assert_eq!(args[1], bin.join(program).display().to_string());
        assert_eq!(&args[2..], ["--yes", "@xai-official/grok", "agent", "stdio"]);
        assert_eq!(missing_for(&launch), None);
    }

    fn joined(dirs: &[&str]) -> OsString {
        std::env::join_paths(dirs).unwrap()
    }

    #[test]
    fn shell_entries_come_first_without_duplicates() {
        let merged = merge_paths(&joined(&["shell-a", "shared"]), &joined(&["inherited", "shared", ""]));
        let dirs: Vec<PathBuf> = std::env::split_paths(&merged).collect();
        assert_eq!(dirs, ["shell-a", "shared", "inherited"].map(PathBuf::from));
    }

    #[test]
    fn marked_path_ignores_startup_file_noise() {
        let output = format!("Welcome!\n{MARKER}first{MARKER}\nbye");
        assert_eq!(parse_marked(&output), Some("first".into()));
        assert_eq!(parse_marked(&format!("{MARKER}{MARKER}")), None);
        assert_eq!(parse_marked("no markers"), None);
        assert_eq!(parse_marked(&format!("{MARKER}unterminated")), None);
    }

    #[test]
    fn finds_programs_only_in_listed_directories() {
        let root = std::env::temp_dir().join(format!("editor-agent-launch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (empty, bin) = (root.join("empty"), root.join("bin"));
        std::fs::create_dir_all(&empty).unwrap();
        std::fs::create_dir_all(bin.join("npx-dir")).unwrap();
        std::fs::write(bin.join("npx"), "").unwrap();

        let path = std::env::join_paths([&empty, &bin]).unwrap();
        assert_eq!(find_in("npx", &path), Some(bin.join("npx")));
        assert_eq!(find_in("npx-dir", &path), None, "directories are not programs");
        assert_eq!(find_in("npx", &std::env::join_paths([&empty]).unwrap()), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn describe_drops_build_machine_locations() {
        let spawn_failure = agent_client_protocol::Error::internal_error().data(serde_json::json!({
            "spawned_at": "/Users/runner/.cargo/registry/src/index.crates.io/agent-client-protocol-2.0.0/src/jsonrpc.rs:1:1",
            "data": {
                "spawned_at": "/Users/runner/.cargo/registry/src/index.crates.io/agent-client-protocol-2.0.0/src/jsonrpc.rs:2:2",
                "data": "No such file or directory (os error 2)",
            },
        }));
        let message = describe(&spawn_failure);
        assert!(message.ends_with(": No such file or directory (os error 2)"), "{message}");
        assert!(!message.contains(".cargo"), "{message}");

        let plain = agent_client_protocol::Error::internal_error();
        assert_eq!(describe(&plain), plain.message);
        let structured = agent_client_protocol::Error::internal_error().data(serde_json::json!({ "reason": "quota" }));
        assert!(describe(&structured).contains("\"quota\""));
    }
}
