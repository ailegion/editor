//! A persistent PTY shell with a bounded VT screen and scrollback.
pub mod panel;

mod appearance;
mod backend;
use backend::{Command, Event as PtyEvent};
use iced::widget::{Space, canvas, column, container, row, text};
use iced::{Color, Element, Event, Length, Point, Rectangle, Size, Task, keyboard, mouse};
use portable_pty::PtySize;
use std::path::Path;
use std::sync::mpsc::{self, Receiver, SyncSender};

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
    running: bool,
    input: Option<SyncSender<Command>>,
    output: Option<Receiver<PtyEvent>>,
    pending_size: Option<(u16, u16)>,
    directory: String,
    pub status: String,
    pub focused: bool,
}

impl Default for Terminal {
    fn default() -> Self {
        Self {
            shell: None,
            parser: new_parser(),
            running: false,
            pending_size: None,
            directory: String::new(),
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
        let size = self.parser.screen().size();
        self.shutdown();
        self.parser = new_parser();
        self.parser.screen_mut().set_size(size.0, size.1);
        let (input, commands) = mpsc::sync_channel(64);
        let (events, output) = mpsc::sync_channel(64);
        self.input = Some(input);
        self.output = Some(output);
        self.running = true;
        self.focused = true;
        self.directory = cwd.display().to_string();
        self.status = "Starting shell…".into();
        backend::spawn(
            self.shell.clone(),
            cwd.to_path_buf(),
            size,
            commands,
            events,
        );
    }

    pub fn shutdown(&mut self) {
        // The worker owns process/PTY handles and performs all blocking cleanup.
        self.input = None;
        self.output = None;
        self.pending_size = None;
        self.running = false;
    }

    pub fn poll(&mut self) {
        if let (Some(size), Some(tx)) = (self.pending_size, &self.input) {
            if tx.try_send(Command::Resize(size.0, size.1)).is_ok() {
                self.pending_size = None;
            }
        }
        if let Some(rx) = &self.output {
            // Bound parsing work so noisy background tabs cannot monopolize the UI.
            let deadline = std::time::Instant::now() + std::time::Duration::from_millis(2);
            for _ in 0..8 {
                match rx.try_recv() {
                    Ok(PtyEvent::Ready) => self.status = self.directory.clone(),
                    Ok(PtyEvent::Output(bytes)) => {
                        self.parser.process(&bytes);
                        let replies = std::mem::take(&mut self.parser.callbacks_mut().0);
                        if !replies.is_empty() {
                            if let Some(tx) = &self.input {
                                if let Err(error) = tx.try_send(Command::Input(replies)) {
                                    self.status = format!("Could not send terminal reply: {error}");
                                }
                            }
                        }
                    }
                    Ok(PtyEvent::Exited(status) | PtyEvent::Error(status)) => {
                        self.status = status;
                        self.running = false;
                        self.input = None;
                    }
                    Err(mpsc::TryRecvError::Disconnected) => {
                        if self.running {
                            self.status = "Terminal worker stopped · Restart to try again".into();
                        }
                        self.running = false;
                        self.input = None;
                        break;
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                }
                if std::time::Instant::now() >= deadline {
                    break;
                }
            }
        }
    }

    fn write(&mut self, bytes: Vec<u8>) {
        self.parser.screen_mut().set_scrollback(0);
        if let Some(tx) = &self.input {
            if let Err(error) = tx.try_send(Command::Input(bytes)) {
                self.status = format!("Could not send terminal input: {error}");
            }
        }
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Input(bytes) => self.write(bytes),
            Message::Resize(rows, cols) => {
                self.parser.screen_mut().set_size(rows, cols);
                self.pending_size = Some((rows, cols));
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
                    ((bounds.height - PAD * 2.0) / appearance::get().cell_height).max(1.0) as u16,
                    ((bounds.width - PAD * 2.0) / appearance::get().cell_width).max(2.0) as u16,
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
                    mouse::ScrollDelta::Pixels { y, .. } => *y / appearance::get().cell_height,
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
        let appearance = appearance::get();
        let fg = appearance
            .foreground
            .unwrap_or(palette.background.base.text);
        let bg = appearance
            .background
            .unwrap_or(palette.background.base.color);
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
                let point = Point::new(
                    PAD + col as f32 * appearance::get().cell_width,
                    PAD + row as f32 * appearance::get().cell_height,
                );
                let width = appearance::get().cell_width * if cell.is_wide() { 2.0 } else { 1.0 };
                frame.fill_rectangle(
                    point,
                    Size::new(width, appearance::get().cell_height),
                    background,
                );
                frame.fill_text(canvas::Text {
                    content: cell.contents().to_string(),
                    position: point,
                    color: foreground,
                    size: appearance.font_size.into(),
                    line_height: iced::widget::text::LineHeight::Absolute(
                        appearance.cell_height.into(),
                    ),
                    shaping: iced::widget::text::Shaping::Advanced,
                    font: iced::Font {
                        weight: if cell.bold() {
                            iced::font::Weight::Bold
                        } else {
                            iced::font::Weight::Normal
                        },
                        ..appearance.font
                    },
                    ..Default::default()
                });
                if cell.underline() {
                    frame.fill_rectangle(
                        Point::new(point.x, point.y + appearance::get().cell_height - 2.0),
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
                    PAD + col as f32 * appearance::get().cell_width,
                    PAD + row as f32 * appearance::get().cell_height
                        + appearance::get().cell_height
                        - 2.0,
                ),
                Size::new(appearance::get().cell_width, 2.0),
                appearance.cursor.unwrap_or(fg),
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
                if let Some(color) = appearance::get().ansi[index as usize] {
                    return color;
                }
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
    fn startup_errors_arrive_asynchronously_and_restart_discards_old_events() {
        let mut terminal = Terminal::default();
        terminal.shell = Some(std::env::temp_dir().join("editor-nonexistent-test-shell.exe"));
        terminal.start(&std::env::current_dir().unwrap());
        // No worker event is applied until the UI polls, even for immediate failure.
        assert_eq!(terminal.status, "Starting shell…");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while terminal.running {
            terminal.poll();
            assert!(
                std::time::Instant::now() < deadline,
                "Worker did not report failure"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            terminal.status.starts_with("Terminal error:"),
            "{}",
            terminal.status
        );
        let (events, output) = mpsc::sync_channel(2);
        terminal.output = Some(output);
        terminal.start(&std::env::current_dir().unwrap());
        assert!(
            events
                .send(PtyEvent::Output(b"stale output".to_vec()))
                .is_err()
        );
        terminal.shutdown();
    }

    #[test]
    fn resize_does_not_wait_for_a_busy_worker_and_coalesces_requests() {
        let mut terminal = Terminal::default();
        let (input, commands) = mpsc::sync_channel(1);
        terminal.input = Some(input);
        terminal.write(b"busy".to_vec());
        let _ = terminal.update(Message::Resize(20, 100));
        let _ = terminal.update(Message::Resize(30, 120));
        terminal.poll();
        assert_eq!(terminal.pending_size, Some((30, 120)));
        assert_eq!(
            commands.try_recv().unwrap(),
            Command::Input(b"busy".to_vec())
        );
        terminal.poll();
        assert_eq!(commands.try_recv().unwrap(), Command::Resize(30, 120));
        assert_eq!(terminal.pending_size, None);
    }

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
        assert!(terminal.running, "{}", terminal.status);
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
        while terminal.running {
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
        assert!(terminal.running, "{}", terminal.status);
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
        while terminal.running {
            terminal.poll();
            assert!(std::time::Instant::now() < deadline, "Shell did not exit");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(terminal.status.contains("Shell exited"));
    }
}
