//! Keyboard/mouse handling -> cursor movement, selection, edits.

use cosmic_text::{Action, Motion, Selection};
use iced::keyboard::{self, Key, Modifiers};

use super::buffer::Buffer;

/// Applies a key press to the buffer. Returns `true` if the key was handled.
///
/// The code editor is a `canvas`, which doesn't participate in iced's per-widget focus
/// system, so keys are dispatched to it directly from the app-level `Message::KeyPressed`
/// handler -- the same convention `DirectoryTree::handle_key` already uses in `main.rs`.
///
/// `cosmic_text::Action::Motion` never touches the selection (confirmed against
/// `Editor::action`'s source), so shift-extend/collapse is handled here: shift+motion opens
/// a selection anchored at the pre-motion cursor if none exists yet; plain motion with an
/// active selection clears it first rather than jumping from an editor-chosen endpoint.
pub fn handle_key(buffer: &mut Buffer, key: &Key, modifiers: Modifiers) -> bool {
    if modifiers.command() {
        return match key.as_ref() {
            Key::Character("z") if modifiers.shift() => {
                buffer.redo();
                true
            }
            Key::Character("z") => {
                buffer.undo();
                true
            }
            Key::Character("y") => {
                buffer.redo();
                true
            }
            Key::Character("a") => {
                buffer.select_all();
                true
            }
            _ => false,
        };
    }
    if modifiers.alt() {
        return false;
    }

    let motion = match key.as_ref() {
        Key::Named(keyboard::key::Named::ArrowLeft) => Some(Motion::Left),
        Key::Named(keyboard::key::Named::ArrowRight) => Some(Motion::Right),
        Key::Named(keyboard::key::Named::ArrowUp) => Some(Motion::Up),
        Key::Named(keyboard::key::Named::ArrowDown) => Some(Motion::Down),
        Key::Named(keyboard::key::Named::Home) => Some(Motion::Home),
        Key::Named(keyboard::key::Named::End) => Some(Motion::End),
        Key::Named(keyboard::key::Named::PageUp) => Some(Motion::PageUp),
        Key::Named(keyboard::key::Named::PageDown) => Some(Motion::PageDown),
        _ => None,
    };

    if let Some(motion) = motion {
        if modifiers.shift() {
            if buffer.selection == Selection::None {
                buffer.selection = Selection::Normal(buffer.cursor);
            }
        } else if buffer.selection != Selection::None {
            buffer.perform(Action::Escape);
        }
        buffer.perform(Action::Motion(motion));
        return true;
    }

    match key.as_ref() {
        Key::Named(keyboard::key::Named::Backspace) => {
            buffer.perform(Action::Backspace);
            true
        }
        Key::Named(keyboard::key::Named::Delete) => {
            buffer.perform(Action::Delete);
            true
        }
        Key::Named(keyboard::key::Named::Enter) => {
            buffer.perform(Action::Enter);
            true
        }
        Key::Named(keyboard::key::Named::Tab) => {
            if modifiers.shift() {
                buffer.perform(Action::Unindent);
            } else {
                buffer.perform(Action::Indent);
            }
            true
        }
        Key::Named(keyboard::key::Named::Escape) => {
            buffer.perform(Action::Escape);
            true
        }
        // iced reports the spacebar as `Named::Space`, not `Character(" ")`.
        Key::Named(keyboard::key::Named::Space) => {
            buffer.perform(Action::Insert(' '));
            true
        }
        Key::Character(c) => match c.chars().next() {
            Some(ch) => {
                buffer.perform(Action::Insert(ch));
                true
            }
            None => false,
        },
        _ => false,
    }
}
