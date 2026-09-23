//! About: the running version and its GitHub release notes, fetched the first time the panel
//! opens. Uses the same overlay presentation as `goto_line`.

use iced::widget::{column, container, markdown, mouse_area, opaque, row, scrollable, text, Space};
use iced::{Element, Length, Task};

#[derive(Default)]
pub struct AboutState {
    pub visible: bool,
    notes: Notes,
}

#[derive(Default)]
enum Notes {
    #[default]
    Idle,
    Loading,
    Loaded(Vec<markdown::Item>),
    Missing,
    Failed(String),
}

#[derive(Debug, Clone)]
pub enum Message {
    Open,
    Close,
    Loaded(Result<Option<String>, String>),
    LinkClicked(markdown::Uri),
}

pub fn update(state: &mut AboutState, message: Message) -> Task<Message> {
    match message {
        Message::Open => {
            state.visible = true;
            // Notes of a published release don't change while the editor runs; retry only failures.
            if matches!(state.notes, Notes::Idle | Notes::Failed(_)) {
                state.notes = Notes::Loading;
                return Task::perform(
                    async {
                        tokio::task::spawn_blocking(crate::updater::release_notes)
                            .await
                            .map_err(|err| err.to_string())
                            .and_then(|result| result)
                    },
                    Message::Loaded,
                );
            }
        }
        Message::Close => state.visible = false,
        Message::Loaded(result) => {
            state.notes = match result {
                Ok(Some(body)) => Notes::Loaded(markdown::parse(&body).collect()),
                Ok(None) => Notes::Missing,
                Err(err) => Notes::Failed(err),
            }
        }
        Message::LinkClicked(url) => crate::open_url(url.as_str()),
    }
    Task::none()
}

pub fn view<'a>(state: &'a AboutState, theme: &iced::Theme) -> Element<'a, Message> {
    let notes: Element<'_, Message> = match &state.notes {
        Notes::Idle | Notes::Loading => text("Loading release notes...").size(12).into(),
        Notes::Missing => text("No release notes are published for this version.").size(12).into(),
        Notes::Failed(err) => text(err).size(12).style(iced::widget::text::danger).into(),
        Notes::Loaded(items) => scrollable(
            markdown::view(items, markdown::Settings::with_text_size(13, theme)).map(Message::LinkClicked),
        )
        .height(Length::Fixed(360.0))
        .into(),
    };

    let panel = container(
        column![
            row![
                text("editor").size(16),
                text(format!("v{}", env!("CARGO_PKG_VERSION"))).size(16).style(iced::widget::text::secondary),
            ]
            .spacing(8),
            text("Release notes").size(13),
            notes,
            text("Esc to close").size(11).style(iced::widget::text::secondary),
        ]
        .spacing(12),
    )
    .padding(16)
    .width(Length::Fill)
    .max_width(640)
    .style(crate::overlay_style);

    let backdrop = container(Space::new().width(Length::Fill).height(Length::Fill)).style(
        |theme: &iced::Theme| iced::widget::container::Style {
            background: Some(
                iced::Color { a: 0.4, ..theme.extended_palette().background.base.color.inverse() }.into(),
            ),
            ..iced::widget::container::Style::default()
        },
    );

    mouse_area(iced::widget::stack![
        backdrop,
        container(opaque(panel))
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(iced::Alignment::Center)
            .padding(iced::Padding { top: 80.0, ..iced::Padding::default() }),
    ])
    .on_press(Message::Close)
    .into()
}

/// True once after the user picks the native macOS "About editor" menu item. Called every tick;
/// the first calls also redirect that item, which winit adds after launch, to this panel.
pub fn native_menu_requested() -> bool {
    #[cfg(target_os = "macos")]
    {
        native::poll()
    }
    #[cfg(not(target_os = "macos"))]
    false
}

#[cfg(target_os = "macos")]
mod native {
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, NSObject};
    use objc2::{define_class, msg_send, sel, MainThreadMarker, MainThreadOnly};
    use objc2_app_kit::NSApplication;
    use std::sync::atomic::{AtomicBool, Ordering};

    static HOOKED: AtomicBool = AtomicBool::new(false);
    static REQUESTED: AtomicBool = AtomicBool::new(false);

    define_class!(
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "EditorAboutMenuTarget"]
        struct Target;

        impl Target {
            #[unsafe(method(showAbout:))]
            fn show_about(&self, _sender: Option<&AnyObject>) {
                REQUESTED.store(true, Ordering::Relaxed);
            }
        }
    );

    pub fn poll() -> bool {
        if !HOOKED.load(Ordering::Relaxed) {
            if let Some(mtm) = MainThreadMarker::new() {
                HOOKED.store(hook(mtm), Ordering::Relaxed);
            }
        }
        REQUESTED.swap(false, Ordering::Relaxed)
    }

    fn hook(mtm: MainThreadMarker) -> bool {
        let app = NSApplication::sharedApplication(mtm);
        let Some(submenu) = app.mainMenu().and_then(|menu| menu.itemAtIndex(0)).and_then(|item| item.submenu())
        else {
            return false;
        };
        let Some(item) = submenu.itemArray().iter().find(|item| item.action() == Some(sel!(orderFrontStandardAboutPanel:)))
        else {
            return false;
        };
        let target: Retained<Target> = unsafe { msg_send![Target::alloc(mtm), init] };
        unsafe {
            item.setTarget(Some(&target));
            item.setAction(Some(sel!(showAbout:)));
        }
        // Menu item targets are weak references; the target lives for the whole app.
        std::mem::forget(target);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loaded_notes_are_kept_and_failures_are_retried() {
        let mut state = AboutState::default();
        let _ = update(&mut state, Message::Open);
        assert!(state.visible && matches!(state.notes, Notes::Loading));

        let _ = update(&mut state, Message::Loaded(Err("offline".into())));
        assert!(matches!(state.notes, Notes::Failed(_)));
        let _ = update(&mut state, Message::Close);
        let _ = update(&mut state, Message::Open);
        assert!(state.visible && matches!(state.notes, Notes::Loading));

        let _ = update(&mut state, Message::Loaded(Ok(Some("## Fixed\n- [link](https://example.com)".into()))));
        assert!(matches!(&state.notes, Notes::Loaded(items) if !items.is_empty()));
        let _ = update(&mut state, Message::Close);
        assert!(!state.visible);
        let _ = update(&mut state, Message::Open);
        assert!(matches!(state.notes, Notes::Loaded(_)));
    }

    #[test]
    fn empty_release_reports_missing_notes() {
        let mut state = AboutState::default();
        let _ = update(&mut state, Message::Loaded(Ok(None)));
        assert!(matches!(state.notes, Notes::Missing));
    }
}
