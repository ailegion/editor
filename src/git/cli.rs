//! The only place the editor starts the `git` executable. Everything that changes a repository
//! (staging, committing) goes through here so hooks, signing, filters and credential helpers
//! behave exactly as they do in a terminal.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

/// Whether a call only reads the repository. Read-only calls skip git's optional lock-taking
/// (e.g. `status` refreshing the index) so they never contend with a `git` run elsewhere.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Read,
    Write,
}

/// Runs `git <args>` in `cwd` and returns its stdout, or the command's diagnostics on failure.
/// `stdin` is piped to the process when given (e.g. a commit message for `commit -F -`).
pub fn run(cwd: &Path, args: &[&str], stdin: Option<&[u8]>, access: Access) -> Result<Vec<u8>, String> {
    let mut command = Command::new("git");
    command
        .args(args)
        .current_dir(cwd)
        // A GUI process has no terminal to answer credential or pager prompts; fail instead.
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_PAGER", "cat")
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if access == Access::Read {
        command.env("GIT_OPTIONAL_LOCKS", "0");
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // `git.exe` is a console program; without this a GUI parent gets a console window per call.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = command.spawn().map_err(|err| match err.kind() {
        std::io::ErrorKind::NotFound => "git was not found on PATH".to_string(),
        _ => format!("could not start git: {err}"),
    })?;
    if let Some(input) = stdin {
        // Dropping the handle closes the pipe so git sees end of input.
        let mut pipe = child.stdin.take().expect("stdin was piped");
        pipe.write_all(input).map_err(|err| format!("could not write to git: {err}"))?;
    }
    let output = child.wait_with_output().map_err(|err| format!("git did not finish: {err}"))?;
    if output.status.success() {
        return Ok(output.stdout);
    }
    // Hooks and signing programs report on either stream; show both.
    let mut message = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !stdout.trim().is_empty() {
        if !message.is_empty() { message.push('\n'); }
        message.push_str(stdout.trim());
    }
    if message.is_empty() {
        message = format!("git {} failed ({})", args.first().copied().unwrap_or_default(), output.status);
    }
    Err(message)
}
