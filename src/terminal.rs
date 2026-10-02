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
    /// Where a mouse selection started, as a visible `(row, col)` cell.
    select_anchor: Option<Cell>,
    /// First and last selected cell (inclusive) in reading order, with the text they held
    /// when selected: once the screen shows something else there, the selection is dropped.
    selection: Option<(Cell, Cell, String)>,
}

/// A visible screen cell, `(row, col)`.
type Cell = (u16, u16);

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
            select_anchor: None,
            selection: None,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Message {
    Input(Vec<u8>),
    Resize(u16, u16),
    Scroll(i32),
    Focus(bool),
    /// Mouse pressed on this cell: a selection may start here.
    SelectStart(u16, u16),
    /// Mouse dragged to this cell.
    SelectTo(u16, u16),
    /// Double click: select the word at this cell.
    SelectWord(u16, u16),
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
        self.clear_selection();
    }

    fn clear_selection(&mut self) {
        self.select_anchor = None;
        self.selection = None;
    }

    /// Selects from `a` to `b` (inclusive, either order).
    fn select(&mut self, a: Cell, b: Cell) {
        let (start, end) = if a <= b { (a, b) } else { (b, a) };
        let text = self.text_between(start, end);
        self.selection = Some((start, end, text));
    }

    fn text_between(&self, start: Cell, end: Cell) -> String {
        self.parser.screen().contents_between(start.0, start.1, end.0, end.1 + 1)
    }

    /// Drops the selection once the screen no longer shows the selected text there, e.g.
    /// after output scrolled it away.
    fn drop_stale_selection(&mut self) {
        if let Some((start, end, text)) = &self.selection {
            if self.text_between(*start, *end) != *text {
                self.clear_selection();
            }
        }
    }

    /// The selected cells, first and last (inclusive) in reading order.
    fn selected_cells(&self) -> Option<(Cell, Cell)> {
        self.selection.as_ref().map(|(start, end, _)| (*start, *end))
    }

    /// First and last cell of the run of non-blank cells around `cell`, if it is on one.
    fn word_at(&self, (row, col): Cell) -> Option<(Cell, Cell)> {
        let screen = self.parser.screen();
        let filled = |col: u16| {
            screen.cell(row, col).is_some_and(|cell| {
                cell.is_wide_continuation() || !cell.contents().trim().is_empty()
            })
        };
        if !filled(col) {
            return None;
        }
        let (mut start, mut end) = (col, col);
        while start > 0 && filled(start - 1) {
            start -= 1;
        }
        while end + 1 < screen.size().1 && filled(end + 1) {
            end += 1;
        }
        Some(((row, start), (row, end)))
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
        self.drop_stale_selection();
    }

    fn write(&mut self, bytes: Vec<u8>) {
        self.clear_selection();
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
                self.clear_selection();
            }

            Message::Scroll(delta) => {
                let offset = self.parser.screen().scrollback() as i32;
                self.parser
                    .screen_mut()
                    .set_scrollback((offset + delta).max(0) as usize);
                self.drop_stale_selection();
            }
            Message::Focus(focused) => {
                self.focused = focused;
                if focused {
                    return iced::widget::operation::focus("terminal-canvas");
                }
            }
            Message::SelectStart(row, col) => {
                self.focused = true;
                self.selection = None;
                self.select_anchor = Some((row, col));
                return iced::widget::operation::focus("terminal-canvas");
            }
            Message::SelectTo(row, col) => {
                if let Some(anchor) = self.select_anchor {
                    // Still on the pressed cell: a click, not a selection.
                    if anchor == (row, col) {
                        self.selection = None;
                    } else {
                        self.select(anchor, (row, col));
                    }
                }
            }
            Message::SelectWord(row, col) => {
                self.focused = true;
                self.clear_selection();
                if let Some((start, end)) = self.word_at((row, col)) {
                    self.select(start, end);
                }
                return iced::widget::operation::focus("terminal-canvas");
            }
            Message::Copy => {
                let text = match &self.selection {
                    Some((_, _, text)) => text.clone(),
                    None => self.parser.screen().contents(),
                };
                return iced::clipboard::write(text);
            }
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
            "Copy selection, or visible terminal output",
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

/// The screen cell under `position` (canvas coordinates), clamped to a `rows` x `cols` screen
/// so a drag past an edge still selects up to it.
fn cell_at(position: Point, cell_width: f32, cell_height: f32, (rows, cols): (u16, u16)) -> Cell {
    let index = |offset: f32, cell: f32, count: u16| {
        (((offset - PAD) / cell).floor().max(0.0) as u16).min(count.saturating_sub(1))
    };
    (
        index(position.y, cell_height, rows),
        index(position.x, cell_width, cols),
    )
}

#[derive(Default)]
struct ScreenState {
    /// `(rows, cols)` last requested for the available space.
    size: (u16, u16),
    /// Left button held after a press on the screen, so moving the mouse selects.
    dragging: bool,
    /// Previous left click, so the next press can be recognized as a double click.
    last_click: Option<iced::advanced::mouse::click::Click>,
}

impl canvas::Program<Message> for Screen<'_> {
    type State = ScreenState;

    fn update(
        &self,
        state: &mut Self::State,
        event: &Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<canvas::Action<Message>> {
        use iced::advanced::mouse::click::{Click, Kind};
        let publish = |msg| Some(canvas::Action::publish(msg).and_capture());
        let cell = |position: Point| {
            let appearance = appearance::get();
            cell_at(
                position,
                appearance.cell_width,
                appearance.cell_height,
                self.terminal.parser.screen().size(),
            )
        };
        match event {
            Event::Window(iced::window::Event::RedrawRequested(_)) => {
                let next = (
                    ((bounds.height - PAD * 2.0) / appearance::get().cell_height).max(1.0) as u16,
                    ((bounds.width - PAD * 2.0) / appearance::get().cell_width).max(2.0) as u16,
                );
                if next != state.size || next != self.terminal.parser.screen().size() {
                    state.size = next;
                    return Some(canvas::Action::publish(Message::Resize(next.0, next.1)));
                }
            }
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) if self.enabled => {
                if let Some(position) = cursor.position_in(bounds) {
                    let click = Click::new(position, mouse::Button::Left, state.last_click);
                    state.last_click = Some(click);
                    let (row, col) = cell(position);
                    if click.kind() == Kind::Double {
                        return publish(Message::SelectWord(row, col));
                    }
                    state.dragging = true;
                    return publish(Message::SelectStart(row, col));
                }
                if self.terminal.focused {
                    return Some(canvas::Action::publish(Message::Focus(false)));
                }
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) if state.dragging => {
                // Use the raw position so the drag keeps tracking outside the canvas.
                let position = cursor.position()? - iced::Vector::new(bounds.x, bounds.y);
                let (row, col) = cell(position);
                return publish(Message::SelectTo(row, col));
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                state.dragging = false;
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
        let selected = self.terminal.selected_cells();
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
                if selected.is_some_and(|(start, end)| (start..=end).contains(&(row, col))) {
                    foreground = palette.primary.weak.text;
                    background = palette.primary.weak.color;
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

    #[test]
    fn mouse_selection_copies_cells_and_clears_when_they_change() {
        let mut terminal = Terminal::default();
        terminal.parser.process(b"cargo build --release\r\nsecond line\r\nthird");
        let selected = |terminal: &Terminal| terminal.selection.as_ref().map(|(_, _, text)| text.clone());
        // A press without a drag is a click, not a selection.
        let _ = terminal.update(Message::SelectStart(0, 6));
        let _ = terminal.update(Message::SelectTo(0, 6));
        assert!(terminal.focused);
        assert_eq!(selected(&terminal), None);
        // Dragging selects through the cell under the mouse, in either direction.
        let _ = terminal.update(Message::SelectTo(0, 10));
        assert_eq!(selected(&terminal).as_deref(), Some("build"));
        let _ = terminal.update(Message::SelectTo(0, 0));
        assert_eq!(selected(&terminal).as_deref(), Some("cargo b"));
        assert_eq!(terminal.selected_cells(), Some(((0, 0), (0, 6))));
        // Across rows: the rest of the first row, whole rows between, the start of the last.
        let _ = terminal.update(Message::SelectTo(2, 2));
        assert_eq!(selected(&terminal).as_deref(), Some("build --release\nsecond line\nthi"));
        // A double click selects the word, and only when there is one.
        let _ = terminal.update(Message::SelectWord(1, 8));
        assert_eq!(selected(&terminal).as_deref(), Some("line"));
        let _ = terminal.update(Message::SelectWord(1, 6));
        assert_eq!(selected(&terminal), None);
        // A drag after a double click does nothing: no press started it.
        let _ = terminal.update(Message::SelectTo(0, 3));
        assert_eq!(selected(&terminal), None);
        // Output elsewhere keeps the selection; output over it, or typing, drops it.
        let _ = terminal.update(Message::SelectWord(1, 0));
        assert_eq!(selected(&terminal).as_deref(), Some("second"));
        terminal.parser.process(b" row");
        terminal.poll();
        assert_eq!(selected(&terminal).as_deref(), Some("second"));
        terminal.parser.process(b"\x1b[2;1Hchange");
        terminal.poll();
        assert_eq!(selected(&terminal), None);
        let _ = terminal.update(Message::SelectWord(0, 0));
        assert_eq!(selected(&terminal).as_deref(), Some("cargo"));
        terminal.write(b"x".to_vec());
        assert_eq!(selected(&terminal), None);
    }

    #[test]
    fn mouse_positions_map_to_cells_and_clamp_to_the_screen() {
        let size = (24, 80);
        assert_eq!(cell_at(Point::new(PAD, PAD), 8.0, 16.0, size), (0, 0));
        assert_eq!(cell_at(Point::new(PAD + 8.0 * 3.0 + 7.0, PAD + 16.0 * 2.0 + 1.0), 8.0, 16.0, size), (2, 3));
        assert_eq!(cell_at(Point::new(-50.0, -50.0), 8.0, 16.0, size), (0, 0));
        assert_eq!(cell_at(Point::new(9000.0, 9000.0), 8.0, 16.0, size), (23, 79));
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
