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

/// An item picked in the native macOS app menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub enum NativeMenu {
    About,
    CheckForUpdates,
}

/// The native macOS app-menu item picked since the last call, if any. Called every tick; the
/// first calls also redirect "About editor", which winit adds after launch, to this panel and
/// add "Check for Updates…" below it.
pub fn native_menu_requested() -> Option<NativeMenu> {
    #[cfg(target_os = "macos")]
    {
        native::poll()
    }
    #[cfg(not(target_os = "macos"))]
    None
}

#[cfg(target_os = "macos")]
mod native {
    use super::NativeMenu;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, NSObject};
    use objc2::{define_class, msg_send, sel, AnyThread, MainThreadMarker, MainThreadOnly};
    use objc2_app_kit::{NSApplication, NSImage, NSMenuItem};
    use objc2_foundation::{NSData, NSString};
    use std::sync::atomic::{AtomicBool, Ordering};

    static HOOKED: AtomicBool = AtomicBool::new(false);
    static REQUESTED: AtomicBool = AtomicBool::new(false);
    static UPDATE_REQUESTED: AtomicBool = AtomicBool::new(false);

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

            #[unsafe(method(checkForUpdates:))]
            fn check_for_updates(&self, _sender: Option<&AnyObject>) {
                UPDATE_REQUESTED.store(true, Ordering::Relaxed);
            }
        }
    );

    pub fn poll() -> Option<NativeMenu> {
        if !HOOKED.load(Ordering::Relaxed) {
            if let Some(mtm) = MainThreadMarker::new() {
                HOOKED.store(hook(mtm), Ordering::Relaxed);
            }
        }
        if REQUESTED.swap(false, Ordering::Relaxed) {
            Some(NativeMenu::About)
        } else if UPDATE_REQUESTED.swap(false, Ordering::Relaxed) {
            Some(NativeMenu::CheckForUpdates)
        } else {
            None
        }
    }

    fn hook(mtm: MainThreadMarker) -> bool {
        let app = NSApplication::sharedApplication(mtm);
        let Some(submenu) = app.mainMenu().and_then(|menu| menu.itemAtIndex(0)).and_then(|item| item.submenu())
        else {
            return false;
        };
        let Some(index) = submenu.itemArray().iter().position(|item| item.action() == Some(sel!(orderFrontStandardAboutPanel:)))
        else {
            return false;
        };
        let Some(item) = submenu.itemAtIndex(index as isize) else { return false };
        let target: Retained<Target> = unsafe { msg_send![Target::alloc(mtm), init] };
        let updates = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                &NSString::from_str("Check for Updates…"),
                Some(sel!(checkForUpdates:)),
                &NSString::from_str(""),
            )
        };
        unsafe {
            item.setTarget(Some(&target));
            item.setAction(Some(sel!(showAbout:)));
            updates.setTarget(Some(&target));
        }
        submenu.insertItem_atIndex(&updates, index as isize + 1);
        // Menu item targets are weak references; the target lives for the whole app.
        std::mem::forget(target);
        // The Dock icon; also covers runs outside an `.app` bundle (e.g. `cargo run`), where
        // macOS would otherwise show a generic placeholder.
        let data = NSData::with_bytes(include_bytes!("../icon.png"));
        if let Some(image) = NSImage::initWithData(NSImage::alloc(), &data) {
            unsafe { app.setApplicationIconImage(Some(&image)) };
        }
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
