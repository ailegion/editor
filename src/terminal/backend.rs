//! All potentially blocking PTY operations belong to this worker, never the UI.
use portable_pty::CommandBuilder;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::time::Duration;

#[derive(Debug, PartialEq)]
pub(super) enum Command {
    Input(Vec<u8>),
    Resize(u16, u16),
}

pub(super) enum Event {
    Ready,
    Output(Vec<u8>),
    Exited(String),
    Error(String),
}

pub(super) fn spawn(
    shell: Option<PathBuf>,
    cwd: PathBuf,
    size: (u16, u16),
    commands: Receiver<Command>,
    events: SyncSender<Event>,
) {
    std::thread::spawn(move || {
        if let Err(error) = run(shell, cwd, size, commands, &events) {
            let _ = events.send(Event::Error(format!("Terminal error: {error}")));
        }
    });
}

fn run(
    shell: Option<PathBuf>,
    cwd: PathBuf,
    size: (u16, u16),
    commands: Receiver<Command>,
    events: &SyncSender<Event>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let pair = portable_pty::native_pty_system().openpty(super::pty_size(size.0, size.1))?;
    let mut command = shell.map_or_else(CommandBuilder::new_default_prog, CommandBuilder::new);
    command.cwd(cwd);
    command.env("TERM", "xterm-256color");
    command.env("COLORTERM", "truecolor");
    let mut reader = pair.master.try_clone_reader()?;
    let mut writer = pair.master.take_writer()?;
    let output = events.clone();
    std::thread::spawn(move || {
        let mut buffer = [0; 8192];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    // Keep draining after a tab closes: closing ConPTY can wait
                    // for its output pipe to drain before releasing the console.
                    let _ = output.send(Event::Output(buffer[..n].to_vec()));
                }
            }
        }
    });
    let mut child = pair.slave.spawn_command(command)?;
    drop(pair.slave);
    let result = (|| -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if events.send(Event::Ready).is_err() {
            return Ok(());
        }
        loop {
            match commands.recv_timeout(Duration::from_millis(20)) {
                Ok(Command::Input(bytes)) => {
                    writer.write_all(&bytes)?;
                    writer.flush()?;
                }
                Ok(Command::Resize(rows, cols)) => {
                    pair.master.resize(super::pty_size(rows, cols))?
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            if let Some(exit) = child.try_wait()? {
                let _ = events.send(Event::Exited(format!(
                    "Shell exited ({exit}) · Restart to open a new shell"
                )));
                return Ok(());
            }
        }
        Ok(())
    })();
    // Closing/restarting disconnects the channel, including during slow startup.
    let _ = child.kill();
    let _ = child.wait();
    drop(writer);
    drop(pair.master);
    result
}
