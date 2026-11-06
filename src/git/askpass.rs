//! SSH key passphrases for fetch, pull and push. git runs ssh without a terminal, so ssh asks
//! the program named by `SSH_ASKPASS`: the editor's own executable. Started that way (see
//! [`run_helper`]) it passes ssh's prompt to the running editor over a localhost connection,
//! which shows a dialog, and prints the answer for ssh.
//!
//! Only "Enter passphrase for key" prompts are answered; any other prompt (host keys, password
//! logins, git's own HTTPS questions) is refused, so it fails as it did without a terminal.
//! Passphrases are remembered in memory only, per key, when the user asks to, until the app
//! closes. A second prompt for the same key from the same ssh means the passphrase was wrong:
//! it is refused, so the operation stops with an error instead of asking again.

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, Shutdown, TcpListener, TcpStream};
use std::sync::{mpsc, Arc, Mutex, OnceLock};

const PORT_VAR: &str = "EDITOR_ASKPASS_PORT";
const TOKEN_VAR: &str = "EDITOR_ASKPASS_TOKEN";

/// A passphrase prompt waiting for the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub id: u64,
    /// The key file, as ssh names it.
    pub key: String,
}

/// The passphrase and whether to remember it for the session, or `None` for Cancel.
pub type Answer = Option<(String, bool)>;

/// What happened to the prompts of one git command.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    /// The key whose passphrase was wrong.
    pub wrong: Option<String>,
    pub cancelled: bool,
}

impl Outcome {
    /// git's error, preceded by why the passphrase prompt stopped it, if it did.
    pub fn explain(&self, error: String) -> String {
        match (&self.wrong, self.cancelled) {
            (Some(key), _) => format!("Wrong passphrase for {key}\n{error}"),
            (None, true) => format!("Passphrase entry cancelled\n{error}"),
            (None, false) => error,
        }
    }
}

#[derive(Default)]
struct RunState {
    /// `(ssh process, key)` pairs already asked for.
    asked: HashSet<(u32, String)>,
    outcome: Outcome,
}

#[derive(Default)]
struct Shared {
    /// Running git commands, by their token.
    runs: HashMap<String, RunState>,
    remembered: HashMap<String, String>,
    pending: HashMap<u64, mpsc::Sender<Answer>>,
    next_id: u64,
}

pub struct Server {
    port: u16,
    shared: Arc<Mutex<Shared>>,
    requests: Mutex<mpsc::Receiver<Request>>,
}

impl Server {
    pub fn start() -> std::io::Result<Server> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let port = listener.local_addr()?.port();
        let shared = Arc::new(Mutex::new(Shared::default()));
        let (sender, requests) = mpsc::channel();
        let accept_shared = shared.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let (shared, sender) = (accept_shared.clone(), sender.clone());
                std::thread::spawn(move || { let _ = serve(stream, &shared, &sender); });
            }
        });
        Ok(Server { port, shared, requests: Mutex::new(requests) })
    }

    /// Registers one git command; its prompts are answered until [`Run::finish`].
    pub fn begin(&self) -> Run<'_> {
        let mut bytes = [0u8; 16];
        let _ = ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut bytes);
        let token: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        self.shared.lock().unwrap().runs.insert(token.clone(), RunState::default());
        Run { server: self, token }
    }

    /// The next prompt for the user, if one arrived.
    pub fn next_request(&self) -> Option<Request> {
        self.requests.lock().unwrap().try_recv().ok()
    }

    pub fn answer(&self, id: u64, answer: Answer) {
        if let Some(sender) = self.shared.lock().unwrap().pending.remove(&id) {
            let _ = sender.send(answer);
        }
    }
}

/// One git command's registration with the [`Server`].
pub struct Run<'a> {
    server: &'a Server,
    token: String,
}

impl Run<'_> {
    /// The environment that makes ssh, started by git, ask this run's prompts.
    pub fn env(&self, exe: OsString) -> Vec<(&'static str, OsString)> {
        vec![
            ("SSH_ASKPASS", exe),
            ("SSH_ASKPASS_REQUIRE", "force".into()),
            (PORT_VAR, self.server.port.to_string().into()),
            (TOKEN_VAR, self.token.clone().into()),
        ]
    }

    pub fn finish(self) -> Outcome {
        self.server.shared.lock().unwrap().runs.remove(&self.token).map(|run| run.outcome).unwrap_or_default()
    }
}

impl Drop for Run<'_> {
    /// A command that ended without `finish` (a panic) answers nothing more either.
    fn drop(&mut self) {
        if let Ok(mut shared) = self.server.shared.lock() { shared.runs.remove(&self.token); }
    }
}

/// The key file named in ssh's passphrase prompt; `None` for any other prompt.
fn passphrase_key(prompt: &str) -> Option<String> {
    let rest = prompt.trim().strip_prefix("Enter passphrase for ")?;
    let rest = rest.strip_prefix("key ").unwrap_or(rest);
    let rest = rest.strip_suffix(':').unwrap_or(rest).trim_end();
    let key = rest.trim_matches(|c| c == '\'' || c == '"');
    (!key.is_empty()).then(|| key.to_string())
}

/// Answers one helper connection: `token`, ssh's process id and the prompt, one per line, up
/// to the end of the helper's writing; replies `1` and the passphrase, or `0` to refuse.
fn serve(stream: TcpStream, shared: &Mutex<Shared>, sender: &mpsc::Sender<Request>) -> std::io::Result<()> {
    let mut request = String::new();
    (&stream).take(64 * 1024).read_to_string(&mut request)?;
    let mut lines = request.split('\n').map(|line| line.trim_end_matches('\r').to_string());
    let mut next = || lines.next().unwrap_or_default();
    let (token, ssh, prompt) = (next(), next(), next());
    let ssh: u32 = ssh.parse().unwrap_or(0);
    let reply = |mut stream: &TcpStream, passphrase: Option<String>| -> std::io::Result<()> {
        match passphrase {
            Some(passphrase) => write!(stream, "1\n{passphrase}")?,
            None => write!(stream, "0\n")?,
        }
        stream.flush()?;
        stream.shutdown(Shutdown::Write)
    };
    let Some(key) = passphrase_key(&prompt) else { return reply(&stream, None) };
    let receiver = {
        let mut shared = shared.lock().unwrap();
        let shared = &mut *shared;
        let Some(run) = shared.runs.get_mut(&token) else { return reply(&stream, None) };
        if !run.asked.insert((ssh, key.clone())) {
            run.outcome.wrong = Some(key.clone());
            shared.remembered.remove(&key);
            return reply(&stream, None);
        }
        if let Some(passphrase) = shared.remembered.get(&key) {
            return reply(&stream, Some(passphrase.clone()));
        }
        let id = shared.next_id;
        shared.next_id += 1;
        let (answer, receiver) = mpsc::channel();
        shared.pending.insert(id, answer);
        if sender.send(Request { id, key: key.clone() }).is_err() {
            shared.pending.remove(&id);
            return reply(&stream, None);
        }
        receiver
    };
    let answer = receiver.recv().ok().flatten();
    let mut shared = shared.lock().unwrap();
    match answer {
        Some((passphrase, remember)) => {
            if remember { shared.remembered.insert(key, passphrase.clone()); }
            drop(shared);
            reply(&stream, Some(passphrase))
        }
        None => {
            if let Some(run) = shared.runs.get_mut(&token) { run.outcome.cancelled = true; }
            drop(shared);
            reply(&stream, None)
        }
    }
}

/// The app's server, started by the first fetch, pull or push.
static SERVER: OnceLock<Option<Server>> = OnceLock::new();

/// The next prompt for the user, if the server is running and one arrived. Never starts it.
pub fn next_request() -> Option<Request> {
    SERVER.get()?.as_ref()?.next_request()
}

pub fn answer(id: u64, answer: Answer) {
    if let Some(server) = SERVER.get().and_then(Option::as_ref) { server.answer(id, answer); }
}

/// Runs `command` with ssh's passphrase prompts going to the user, and explains a failure the
/// prompt caused. Without the server or the executable's path, `command` runs as before.
pub fn with_prompts(command: impl FnOnce(&[(&str, OsString)]) -> Result<(), String>) -> Result<(), String> {
    let server = SERVER.get_or_init(|| Server::start().ok()).as_ref();
    let (Some(server), Ok(exe)) = (server, std::env::current_exe()) else { return command(&[]) };
    let run = server.begin();
    let result = command(&run.env(exe.into_os_string()));
    let outcome = run.finish();
    result.map_err(|error| outcome.explain(error))
}

/// Started by ssh as `SSH_ASKPASS` (the environment says so): asks the editor that ran git,
/// prints the passphrase and exits. Returns `false` when this is a normal start.
pub fn run_helper() -> bool {
    let (Some(port), Some(token)) = (std::env::var_os(PORT_VAR), std::env::var_os(TOKEN_VAR)) else { return false };
    let prompt = std::env::args().nth(1).unwrap_or_default();
    let port = port.to_string_lossy().parse().unwrap_or(0);
    let code = helper(port, &token.to_string_lossy(), parent_id(), &prompt, &mut std::io::stdout());
    std::process::exit(code);
}

/// [`run_helper`]'s work: 0 with the passphrase written to `out`, 1 when refused or unreachable.
fn helper(port: u16, token: &str, ssh: u32, prompt: &str, out: &mut impl Write) -> i32 {
    let ask = || -> std::io::Result<Option<String>> {
        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port))?;
        write!(stream, "{token}\n{ssh}\n{}\n", prompt.replace(['\r', '\n'], " "))?;
        stream.flush()?;
        // The editor reads the request to its end, so neither side closes with data unread.
        stream.shutdown(Shutdown::Write)?;
        let mut response = String::new();
        stream.read_to_string(&mut response)?;
        Ok(response.strip_prefix("1\n").map(str::to_string))
    };
    match ask() {
        Ok(Some(passphrase)) if writeln!(out, "{passphrase}").and_then(|()| out.flush()).is_ok() => 0,
        _ => 1,
    }
}

/// The process that started this one: ssh, when started as its askpass program.
#[cfg(unix)]
fn parent_id() -> u32 {
    std::os::unix::process::parent_id()
}

#[cfg(windows)]
fn parent_id() -> u32 {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
    };
    let me = std::process::id();
    // SAFETY: the snapshot handle is checked and closed; `entry` is a plain struct whose size
    // field is set before use, as the ToolHelp API requires.
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE { return 0; }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut parent = 0;
        let mut more = Process32FirstW(snapshot, &mut entry) != 0;
        while more {
            if entry.th32ProcessID == me {
                parent = entry.th32ParentProcessID;
                break;
            }
            more = Process32NextW(snapshot, &mut entry) != 0;
        }
        CloseHandle(snapshot);
        parent
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs the helper as ssh would (process `ssh`, `prompt`) on its own thread.
    fn ask(server: &Server, token: &str, ssh: u32, prompt: &str) -> std::thread::JoinHandle<(i32, String)> {
        let (port, token, prompt) = (server.port, token.to_string(), prompt.to_string());
        std::thread::spawn(move || {
            let mut out = Vec::new();
            let code = helper(port, &token, ssh, &prompt, &mut out);
            (code, String::from_utf8(out).unwrap())
        })
    }

    fn token_of(run: &Run) -> String {
        run.env(OsString::from("editor")).into_iter().find(|(key, _)| *key == TOKEN_VAR).unwrap().1.into_string().unwrap()
    }

    /// Waits for the prompt to reach the user, as the app's tick does.
    fn wait_request(server: &Server) -> Request {
        server.requests.lock().unwrap().recv().expect("a prompt for the user")
    }

    const PROMPT: &str = "Enter passphrase for key '/home/me/.ssh/id_ed25519': ";

    #[test]
    fn only_key_passphrase_prompts_are_recognised() {
        assert_eq!(passphrase_key(PROMPT).as_deref(), Some("/home/me/.ssh/id_ed25519"));
        assert_eq!(passphrase_key("Enter passphrase for key 'C:\\Users\\me/.ssh/id_rsa':").as_deref(), Some("C:\\Users\\me/.ssh/id_rsa"));
        assert_eq!(passphrase_key("Enter passphrase for \"/k\": ").as_deref(), Some("/k"));
        assert_eq!(passphrase_key("Are you sure you want to continue connecting (yes/no/[fingerprint])? "), None);
        assert_eq!(passphrase_key("me@host's password: "), None);
        assert_eq!(passphrase_key("Username for 'https://github.com': "), None);
        assert_eq!(passphrase_key("Enter passphrase for key '': "), None);
    }

    #[test]
    fn the_answer_reaches_ssh_and_is_remembered_only_when_asked() {
        let server = Server::start().unwrap();
        let run = server.begin();
        let token = token_of(&run);
        let helper = ask(&server, &token, 7, PROMPT);
        let request = wait_request(&server);
        assert_eq!(request.key, "/home/me/.ssh/id_ed25519");
        server.answer(request.id, Some(("secret words".into(), false)));
        assert_eq!(helper.join().unwrap(), (0, "secret words\n".into()));
        assert_eq!(run.finish(), Outcome::default());

        // Not remembered: the next command asks again; this time it is remembered.
        let run = server.begin();
        let token = token_of(&run);
        let helper = ask(&server, &token, 8, PROMPT);
        let request = wait_request(&server);
        server.answer(request.id, Some(("secret words".into(), true)));
        assert_eq!(helper.join().unwrap(), (0, "secret words\n".into()));
        assert_eq!(run.finish(), Outcome::default());

        // Remembered: answered without asking the user, in a later command too.
        let run = server.begin();
        let token = token_of(&run);
        assert_eq!(ask(&server, &token, 9, PROMPT).join().unwrap(), (0, "secret words\n".into()));
        assert_eq!(ask(&server, &token, 10, PROMPT).join().unwrap(), (0, "secret words\n".into()), "another ssh of the same command");
        assert!(server.next_request().is_none());
        assert_eq!(run.finish(), Outcome::default());
    }

    #[test]
    fn a_wrong_passphrase_stops_instead_of_asking_again_and_is_forgotten() {
        let server = Server::start().unwrap();
        let run = server.begin();
        let token = token_of(&run);
        let helper = ask(&server, &token, 7, PROMPT);
        let request = wait_request(&server);
        server.answer(request.id, Some(("wrong".into(), true)));
        assert_eq!(helper.join().unwrap(), (0, "wrong\n".into()));
        // ssh asks again for the same key: the passphrase was wrong.
        assert_eq!(ask(&server, &token, 7, PROMPT).join().unwrap(), (1, String::new()));
        assert!(server.next_request().is_none(), "the user is not asked again");
        let outcome = run.finish();
        assert_eq!(outcome.wrong.as_deref(), Some("/home/me/.ssh/id_ed25519"));
        assert!(outcome.explain("git failed".into()).starts_with("Wrong passphrase for /home/me/.ssh/id_ed25519\n"));

        // Forgotten: the next command asks the user.
        let run = server.begin();
        let token = token_of(&run);
        let helper = ask(&server, &token, 8, PROMPT);
        let request = wait_request(&server);
        server.answer(request.id, None);
        assert_eq!(helper.join().unwrap(), (1, String::new()));
        let outcome = run.finish();
        assert!(outcome.cancelled);
        assert_eq!(outcome.explain("git failed".into()), "Passphrase entry cancelled\ngit failed");
    }

    #[test]
    fn other_prompts_unknown_tokens_and_finished_commands_are_refused() {
        let server = Server::start().unwrap();
        let run = server.begin();
        let token = token_of(&run);
        assert_eq!(ask(&server, &token, 7, "Are you sure you want to continue connecting (yes/no)? ").join().unwrap(), (1, String::new()));
        assert_eq!(ask(&server, "not-the-token", 7, PROMPT).join().unwrap(), (1, String::new()));
        assert!(server.next_request().is_none());
        assert_eq!(run.finish(), Outcome::default());
        assert_eq!(ask(&server, &token, 7, PROMPT).join().unwrap(), (1, String::new()), "after its command finished");
        // Nothing listening: refused too.
        let closed = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = closed.local_addr().unwrap().port();
        drop(closed);
        assert_eq!(helper(port, &token, 7, PROMPT, &mut Vec::new()), 1);
    }

    #[test]
    fn the_environment_points_ssh_at_the_editor() {
        let server = Server::start().unwrap();
        let run = server.begin();
        let env: HashMap<_, _> = run.env(OsString::from("/path/to/editor")).into_iter().collect();
        assert_eq!(env["SSH_ASKPASS"], OsString::from("/path/to/editor"));
        assert_eq!(env["SSH_ASKPASS_REQUIRE"], OsString::from("force"));
        assert_eq!(env[PORT_VAR], OsString::from(server.port.to_string()));
        assert_eq!(env[TOKEN_VAR].len(), 32);
        assert_ne!(token_of(&run), token_of(&server.begin()), "each command has its own token");
    }

    #[test]
    fn the_parent_process_is_found() {
        assert_ne!(parent_id(), 0);
        assert_ne!(parent_id(), std::process::id());
    }

    #[test]
    fn a_normal_start_is_not_the_helper() {
        assert!(std::env::var_os(TOKEN_VAR).is_none());
        assert!(!run_helper());
    }
}
