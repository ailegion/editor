//! A persistent PTY shell with a bounded VT screen and scrollback.
pub mod panel;
use iced::widget::{Space, canvas, column, container, row, text};
use iced::{Color, Element, Event, Length, Point, Rectangle, Size, Task, keyboard, mouse};
use portable_pty::{CommandBuilder, MasterPty, PtySize};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::mpsc::{self, Receiver, SyncSender};

const CELL_W: f32 = 8.0;
const CELL_H: f32 = 18.0;
const PAD: f32 = 6.0;

#[derive(Default)]
struct TerminalReplies(Vec<u8>);

impl vt100::Callbacks for TerminalReplies {
    fn unhandled_csi(
        &mut self,
        screen: &mut vt100::Screen,
        i1: Option<u8>,
        i2: Option<u8>,
        params: &[&[u16]],
        command: char,
    ) {
        if i1.is_none() && i2.is_none() && command == 'n' {
            match params {
                [p] if *p == [6] => {
                    // ConPTY waits for this reply before starting the Windows shell.
                    let (row, col) = screen.cursor_position();
                    self.0
                        .extend_from_slice(format!("\x1b[{};{}R", row + 1, col + 1).as_bytes());
                }
                [p] if *p == [5] => self.0.extend_from_slice(b"\x1b[0n"),
                _ => {}
            }
        }
    }
}

fn new_parser() -> vt100::Parser<TerminalReplies> {
    vt100::Parser::new_with_callbacks(24, 80, 5000, TerminalReplies::default())
}

pub struct Terminal {
    shell: Option<std::path::PathBuf>,
    parser: vt100::Parser<TerminalReplies>,
    master: Option<Box<dyn MasterPty + Send>>,
    child: Option<Box<dyn portable_pty::Child + Send + Sync>>,
    input: Option<SyncSender<Vec<u8>>>,
    output: Option<Receiver<Vec<u8>>>,
    pub status: String,
    pub focused: bool,
}

impl Default for Terminal {
    fn default() -> Self {
        Self {
            shell: None,
            parser: new_parser(),
            master: None,
            child: None,
            input: None,
            output: None,
            status: "Not started".into(),
            focused: false,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Message {
    Input(Vec<u8>),
    Resize(u16, u16),
    Scroll(i32),
    Focus(bool),
    Paste,
    Pasted(Option<String>),
    Copy,
    Restart,
}

impl Terminal {
    pub fn start(&mut self, cwd: &Path) {
        self.shutdown();
        self.parser = new_parser();
        match self.spawn(cwd) {
            Ok(()) => self.focused = true,
            Err(error) => self.status = format!("Could not start shell: {error}"),
        }
    }

    fn spawn(&mut self, cwd: &Path) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let pair = portable_pty::native_pty_system().openpty(pty_size(24, 80))?;
        let mut command = match &self.shell {
            Some(shell) => CommandBuilder::new(shell),
            None => CommandBuilder::new_default_prog(),
        };
        command.cwd(cwd);
        command.env("TERM", "xterm-256color");
        command.env("COLORTERM", "truecolor");
        let mut reader = pair.master.try_clone_reader()?;
        let mut writer = pair.master.take_writer()?;
        let child = pair.slave.spawn_command(command)?;
        drop(pair.slave);
        let (output_tx, output_rx) = mpsc::sync_channel(64);
        std::thread::spawn(move || {
            let mut buffer = [0u8; 8192];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if output_tx.send(buffer[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        let (input_tx, input_rx) = mpsc::sync_channel::<Vec<u8>>(64);
        std::thread::spawn(move || {
            while let Ok(bytes) = input_rx.recv() {
                if writer
                    .write_all(&bytes)
                    .and_then(|_| writer.flush())
                    .is_err()
                {
                    break;
                }
            }
        });
        self.master = Some(pair.master);
        self.child = Some(child);
        self.input = Some(input_tx);
        self.output = Some(output_rx);
        self.status = cwd.display().to_string();
        Ok(())
    }

    pub fn shutdown(&mut self) {
        self.input = None;
        self.output = None;
        self.master = None;
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
    }

    pub fn poll(&mut self) {
        if let Some(rx) = &self.output {
            // Keep noisy commands from monopolizing the UI thread.
            for _ in 0..64 {
                let Ok(bytes) = rx.try_recv() else {
                    break;
                };
                self.parser.process(&bytes);
                let replies = std::mem::take(&mut self.parser.callbacks_mut().0);
                if !replies.is_empty() {
                    if let Some(tx) = &self.input {
                        if let Err(error) = tx.try_send(replies) {
                            self.status = format!("Could not send terminal reply: {error}");
                        }
                    }
                }
            }
        }
        if let Some(child) = &mut self.child {
            match child.try_wait() {
                Ok(Some(exit)) => {
                    self.status = format!("Shell exited ({exit}) · Restart to open a new shell");
                    self.child = None;
                    self.input = None;
                }
                Err(error) => self.status = format!("Shell error: {error}"),
                _ => {}
            }
        }
    }

    fn write(&mut self, bytes: Vec<u8>) {
        self.parser.screen_mut().set_scrollback(0);
        if let Some(tx) = &self.input {
            if let Err(error) = tx.try_send(bytes) {
                self.status = format!("Could not send terminal input: {error}");
            }
        }
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Input(bytes) => self.write(bytes),
            Message::Resize(rows, cols) => {
                self.parser.screen_mut().set_size(rows, cols);
                if let Some(master) = &self.master {
                    match master.resize(pty_size(rows, cols)) {
                        Ok(()) => {}
                        Err(error) => self.status = format!("Could not resize terminal: {error}"),
                    }
                }
            }
            Message::Scroll(delta) => {
                let offset = self.parser.screen().scrollback() as i32;
                self.parser
                    .screen_mut()
                    .set_scrollback((offset + delta).max(0) as usize);
            }
            Message::Focus(focused) => {
                self.focused = focused;
                if focused {
                    return iced::widget::operation::focus("terminal-canvas");
                }
            }
            Message::Copy => return iced::clipboard::write(self.parser.screen().contents()),
            Message::Paste => return iced::clipboard::read().map(Message::Pasted),
            Message::Pasted(Some(text)) => {
                let bytes = paste_bytes(&text, self.parser.screen().bracketed_paste());
                self.write(bytes);
            }
            _ => {}
        }
        Task::none()
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn pty_size(rows: u16, cols: u16) -> PtySize {
    PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    }
}

fn paste_bytes(text: &str, bracketed: bool) -> Vec<u8> {
    // Strip ESC so pasted text cannot terminate a bracketed-paste sequence.
    let text = text
        .replace('\x1b', "")
        .replace("\r\n", "\n")
        .replace('\n', "\r");
    if bracketed {
        format!("\x1b[200~{text}\x1b[201~").into_bytes()
    } else {
        text.into_bytes()
    }
}

pub fn view(terminal: &Terminal, enabled: bool) -> Element<'_, Message> {
    use lucide_icons::Icon;
    let header = row![
        text("TERMINAL").size(11),
        text(&terminal.status)
            .size(11)
            .wrapping(iced::widget::text::Wrapping::None)
            .style(iced::widget::text::secondary),
        Space::new().width(Length::Fill),
        crate::icon_control(
            Icon::Copy,
            "Copy visible terminal output",
            Some(Message::Copy),
            false
        ),
        crate::icon_control(
            Icon::Clipboard,
            "Paste into terminal",
            Some(Message::Paste),
            false
        ),
        crate::icon_control(
            Icon::RefreshCw,
            "Restart shell (ends current session)",
            Some(Message::Restart),
            false
        ),
    ]
    .spacing(6)
    .align_y(iced::Alignment::Center)
    .padding([2, 8]);
    column![
        container(header).style(crate::chrome_style),
        canvas::Canvas::new(Screen { terminal, enabled })
            .width(Length::Fill)
            .height(Length::Fill)
    ]
    .height(Length::Fill)
    .into()
}

struct Screen<'a> {
    terminal: &'a Terminal,
    enabled: bool,
}

impl canvas::Program<Message> for Screen<'_> {
    type State = (u16, u16);

    fn update(
        &self,
        size: &mut Self::State,
        event: &Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<canvas::Action<Message>> {
        let publish = |msg| Some(canvas::Action::publish(msg).and_capture());
        match event {
            Event::Window(iced::window::Event::RedrawRequested(_)) => {
                let next = (
                    ((bounds.height - PAD * 2.0) / CELL_H).max(1.0) as u16,
                    ((bounds.width - PAD * 2.0) / CELL_W).max(2.0) as u16,
                );
                if next != *size || next != self.terminal.parser.screen().size() {
                    *size = next;
                    return Some(canvas::Action::publish(Message::Resize(next.0, next.1)));
                }
            }
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) if self.enabled => {
                let focused = cursor.is_over(bounds);
                if focused {
                    return publish(Message::Focus(true));
                }
                if self.terminal.focused {
                    return Some(canvas::Action::publish(Message::Focus(false)));
                }
            }
            Event::Mouse(mouse::Event::WheelScrolled { delta }) if cursor.is_over(bounds) => {
                let lines = match delta {
                    mouse::ScrollDelta::Lines { y, .. } => *y * 3.0,
                    mouse::ScrollDelta::Pixels { y, .. } => *y / CELL_H,
                };
                return publish(Message::Scroll(lines as i32));
            }
            Event::Keyboard(keyboard::Event::KeyPressed {
                key,
                modified_key,
                modifiers,
                text,
                ..
            }) if self.enabled && self.terminal.focused => {
                if modifiers.control() && key.as_ref() == keyboard::Key::Character("`") {
                    return None;
                }
                let clipboard_modifier = if cfg!(target_os = "macos") {
                    modifiers.logo()
                } else {
                    modifiers.control() && modifiers.shift()
                };
                if clipboard_modifier && key.as_ref() == keyboard::Key::Character("v") {
                    return publish(Message::Paste);
                }
                if clipboard_modifier && key.as_ref() == keyboard::Key::Character("c") {
                    return publish(Message::Copy);
                }
                if modifiers.logo() {
                    return None;
                }
                if let Some(bytes) = key_bytes(
                    modified_key,
                    *modifiers,
                    text.as_deref(),
                    self.terminal.parser.screen().application_cursor(),
                ) {
                    return publish(Message::Input(bytes));
                }
                // Do not leak unhandled terminal keys into editor shortcuts.
                return Some(canvas::Action::request_redraw().and_capture());
            }
            _ => {}
        }
        None
    }

    fn draw(
        &self,
        _: &Self::State,
        renderer: &iced::Renderer,
        theme: &iced::Theme,
        bounds: Rectangle,
        _: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        let palette = theme.extended_palette();
        let fg = palette.background.base.text;
        let bg = palette.background.base.color;
        frame.fill_rectangle(Point::ORIGIN, bounds.size(), bg);
        let screen = self.terminal.parser.screen();
        let (rows, cols) = screen.size();
        for row in 0..rows {
            for col in 0..cols {
                let Some(cell) = screen.cell(row, col) else {
                    continue;
                };
                if cell.is_wide_continuation() {
                    continue;
                }
                let mut foreground = color(cell.fgcolor(), fg);
                let mut background = color(cell.bgcolor(), bg);
                if cell.inverse() {
                    std::mem::swap(&mut foreground, &mut background);
                }
                let point = Point::new(PAD + col as f32 * CELL_W, PAD + row as f32 * CELL_H);
                let width = CELL_W * if cell.is_wide() { 2.0 } else { 1.0 };
                frame.fill_rectangle(point, Size::new(width, CELL_H), background);
                frame.fill_text(canvas::Text {
                    content: cell.contents().to_string(),
                    position: point,
                    color: foreground,
                    size: 13.0.into(),
                    font: iced::Font {
                        weight: if cell.bold() {
                            iced::font::Weight::Bold
                        } else {
                            iced::font::Weight::Normal
                        },
                        ..iced::Font::MONOSPACE
                    },
                    ..Default::default()
                });
                if cell.underline() {
                    frame.fill_rectangle(
                        Point::new(point.x, point.y + CELL_H - 2.0),
                        Size::new(width, 1.0),
                        foreground,
                    );
                }
            }
        }
        if self.enabled
            && self.terminal.focused
            && !screen.hide_cursor()
            && screen.scrollback() == 0
        {
            let (row, col) = screen.cursor_position();
            frame.fill_rectangle(
                Point::new(
                    PAD + col as f32 * CELL_W,
                    PAD + row as f32 * CELL_H + CELL_H - 2.0,
                ),
                Size::new(CELL_W, 2.0),
                fg,
            );
        }
        vec![frame.into_geometry()]
    }
}

fn key_bytes(
    key: &keyboard::Key,
    modifiers: keyboard::Modifiers,
    text: Option<&str>,
    application: bool,
) -> Option<Vec<u8>> {
    use keyboard::key::Named;
    if modifiers.control() {
        if let keyboard::Key::Character(c) = key {
            let byte = c.as_bytes().first().copied()?;
            if c.len() == 1
                && (byte.is_ascii_alphabetic() || (b'['..=b'_').contains(&byte) || byte == b' ')
            {
                return Some(vec![byte.to_ascii_uppercase() & 0x1f]);
            }
        }
    }
    let prefix = if application { "\x1bO" } else { "\x1b[" };
    let value = match key {
        keyboard::Key::Named(Named::Enter) => "\r".into(),
        keyboard::Key::Named(Named::Backspace) => "\x7f".into(),
        keyboard::Key::Named(Named::Tab) => if modifiers.shift() { "\x1b[Z" } else { "\t" }.into(),
        keyboard::Key::Named(Named::Escape) => "\x1b".into(),
        keyboard::Key::Named(Named::ArrowUp) => format!("{prefix}A"),
        keyboard::Key::Named(Named::ArrowDown) => format!("{prefix}B"),
        keyboard::Key::Named(Named::ArrowRight) => format!("{prefix}C"),
        keyboard::Key::Named(Named::ArrowLeft) => format!("{prefix}D"),
        keyboard::Key::Named(Named::Home) => format!("{prefix}H"),
        keyboard::Key::Named(Named::End) => format!("{prefix}F"),
        keyboard::Key::Named(Named::Delete) => "\x1b[3~".into(),
        keyboard::Key::Named(Named::PageUp) => "\x1b[5~".into(),
        keyboard::Key::Named(Named::PageDown) => "\x1b[6~".into(),
        keyboard::Key::Named(Named::Insert) => "\x1b[2~".into(),
        keyboard::Key::Named(Named::F1) => "\x1bOP".into(),
        keyboard::Key::Named(Named::F2) => "\x1bOQ".into(),
        keyboard::Key::Named(Named::F3) => "\x1bOR".into(),
        keyboard::Key::Named(Named::F4) => "\x1bOS".into(),
        keyboard::Key::Named(Named::F5) => "\x1b[15~".into(),
        keyboard::Key::Named(Named::F6) => "\x1b[17~".into(),
        keyboard::Key::Named(Named::F7) => "\x1b[18~".into(),
        keyboard::Key::Named(Named::F8) => "\x1b[19~".into(),
        keyboard::Key::Named(Named::F9) => "\x1b[20~".into(),
        keyboard::Key::Named(Named::F10) => "\x1b[21~".into(),
        keyboard::Key::Named(Named::F11) => "\x1b[23~".into(),
        keyboard::Key::Named(Named::F12) => "\x1b[24~".into(),
        keyboard::Key::Character(c) => text.unwrap_or(c).to_string(),
        keyboard::Key::Named(Named::Space) => " ".into(),
        _ => return None,
    };
    Some(if modifiers.alt() {
        format!("\x1b{value}").into_bytes()
    } else {
        value.into_bytes()
    })
}

fn color(value: vt100::Color, default: Color) -> Color {
    match value {
        vt100::Color::Default => default,
        vt100::Color::Rgb(r, g, b) => Color::from_rgb8(r, g, b),
        vt100::Color::Idx(index) => {
            const ANSI: [u32; 16] = [
                0x202020, 0xcd3131, 0x0dbc79, 0xe5e510, 0x2472c8, 0xbc3fbc, 0x11a8cd, 0xe5e5e5,
                0x666666, 0xf14c4c, 0x23d18b, 0xf5f543, 0x3b8eea, 0xd670d6, 0x29b8db, 0xffffff,
            ];
            if index < 16 {
                let rgb = ANSI[index as usize];
                Color::from_rgb8((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
            } else if index >= 232 {
                let gray = 8 + (index - 232) * 10;
                Color::from_rgb8(gray, gray, gray)
            } else {
                let n = index - 16;
                let channel = |n| if n == 0 { 0 } else { 55 + n * 40 };
                Color::from_rgb8(channel(n / 36), channel(n / 6 % 6), channel(n % 6))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_replies_to_fragmented_cursor_queries() {
        let mut parser = new_parser();
        parser.process(b"\x1b[4;9H\x1b[");
        parser.process(b"6n\x1b[5n");
        assert_eq!(parser.callbacks().0, b"\x1b[4;9R\x1b[0n");
    }

    #[cfg(windows)]
    #[test]
    fn windows_shell_starts_accepts_input_and_exits() {
        let mut terminal = Terminal::default();
        let cwd = std::env::current_dir().unwrap();
        terminal.start(&cwd);
        assert!(terminal.child.is_some(), "{}", terminal.status);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        // Wait for the initial prompt before sending input to the shell.
        while !terminal.parser.screen().contents().contains('>') {
            terminal.poll();
            assert!(
                std::time::Instant::now() < deadline,
                "No shell prompt: {}",
                terminal.parser.screen().contents()
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let _ = terminal.update(Message::Resize(20, 100));
        // Expansion ensures echoed input cannot satisfy the output assertion.
        terminal.write(b"set EDITOR_PTY_TEST=ready\recho pty-%EDITOR_PTY_TEST%\rcd\r".to_vec());
        loop {
            terminal.poll();
            let output = terminal.parser.screen().contents();
            if output.contains("pty-ready") && output.contains(&cwd.display().to_string()) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "Shell output: {output}"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        terminal.write(b"exit\r".to_vec());
        while terminal.child.is_some() {
            terminal.poll();
            assert!(std::time::Instant::now() < deadline, "Shell did not exit");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    #[test]
    fn terminal_keys_preserve_control_and_application_modes() {
        let ctrl = keyboard::Modifiers::CTRL;
        assert_eq!(
            key_bytes(&keyboard::Key::Character("c".into()), ctrl, None, false),
            Some(vec![3])
        );
        assert_eq!(
            key_bytes(&keyboard::Key::Character("d".into()), ctrl, None, false),
            Some(vec![4])
        );
        assert_eq!(
            key_bytes(
                &keyboard::Key::Named(keyboard::key::Named::ArrowUp),
                keyboard::Modifiers::empty(),
                None,
                true
            ),
            Some(b"\x1bOA".to_vec())
        );
        assert_eq!(
            key_bytes(
                &keyboard::Key::Character("é".into()),
                keyboard::Modifiers::empty(),
                Some("é"),
                false
            ),
            Some("é".as_bytes().to_vec())
        );
    }

    #[test]
    fn paste_normalizes_lines_and_cannot_escape_brackets() {
        assert_eq!(
            paste_bytes("a\r\nb\x1b[201~", true),
            b"\x1b[200~a\rb[201~\x1b[201~"
        );
    }

    #[test]
    fn screen_handles_color_cursor_updates_and_scrollback() {
        let mut terminal = Terminal::default();
        terminal.parser.process(b"\x1b[31mred\x1b[0m\rOK");
        assert!(terminal.parser.screen().contents().starts_with("OKd"));
        assert_eq!(
            terminal.parser.screen().cell(0, 2).unwrap().fgcolor(),
            vt100::Color::Idx(1)
        );
        for _ in 0..40 {
            terminal.parser.process(b"\r\nline");
        }
        let _ = terminal.update(Message::Scroll(10));
        assert_eq!(terminal.parser.screen().scrollback(), 10);
        terminal.write(b"x".to_vec());
        assert_eq!(terminal.parser.screen().scrollback(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn real_shell_uses_project_directory_and_resized_pty() {
        let mut terminal = Terminal::default();
        let cwd = std::env::current_dir().unwrap();
        terminal.start(&cwd);
        assert!(terminal.child.is_some(), "{}", terminal.status);
        let _ = terminal.update(Message::Resize(20, 100));
        // Split the marker so echoed input cannot satisfy the output assertion.
        terminal.write(b"printf 'pty-%s\\n' ready; pwd; stty size\r".to_vec());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            terminal.poll();
            let output = terminal.parser.screen().contents();
            if output.contains("pty-ready")
                && output.contains("20 100")
                && output.contains(&cwd.display().to_string())
            {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "Shell output: {output}"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        terminal.write(b"exit\r".to_vec());
        while terminal.child.is_some() {
            terminal.poll();
            assert!(std::time::Instant::now() < deadline, "Shell did not exit");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(terminal.status.contains("Shell exited"));
    }
}
