//! Selection-mode type for click and keyboard interactions.
//!
//! [`SelectionMode`] controls how a new selection event composes
//! with the existing [`DirectoryTree`](crate::DirectoryTree)
//! selection state:
//!
//! | Mode                    | Effect on the selected set |
//! |-------------------------|----------------------------|
//! | [`SelectionMode::Replace`]      | Clear everything; the path becomes the single selection. |
//! | [`SelectionMode::Toggle`]       | Add the path if absent; remove it if present. |
//! | [`SelectionMode::ExtendRange`]  | Select every visible row between the anchor and the target, inclusive. The anchor is unchanged. |
//!
//! The built-in view emits clicks as [`SelectionMode::Replace`]
//! because iced 0.14's `button::on_press` callback cannot observe
//! modifier keys at press time. Applications that want multi-select
//! track modifier state themselves (see `examples/multi_select.rs`)
//! and rewrite the mode before forwarding the event to
//! [`DirectoryTree::update`](crate::DirectoryTree::update).
//!
//! [`from_modifiers`](SelectionMode::from_modifiers) exists to make
//! that rewrite trivial.

use iced::keyboard::Modifiers;

/// How an incoming selection event composes with the existing set.
///
/// The three modes cover the three standard click idioms:
///
/// | Mode                    | Effect on the selected set |
/// |-------------------------|----------------------------|
/// | [`SelectionMode::Replace`]     | Clear everything; the path becomes the single selection. |
/// | [`SelectionMode::Toggle`]      | Add the path if absent; remove it if present. |
/// | [`SelectionMode::ExtendRange`] | Select every visible row between the anchor and the target, inclusive. The anchor is unchanged. |
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SelectionMode {
    /// Clear any existing selection; the new path becomes the only
    /// selected entry. This is what a plain left-click produces
    /// from the built-in view.
    #[default]
    Replace,

    /// Add the path to the selection if it isn't already there,
    /// remove it otherwise. The anchor used for range extension is
    /// updated to point at this path regardless of whether it was
    /// added or removed.
    Toggle,

    /// Select every visible row between the current anchor and the
    /// target path, inclusive, using the row order the widget
    /// renders. The anchor itself is **not** moved — successive
    /// range extensions all use the same starting pivot, which
    /// matches how Windows Explorer, macOS Finder, and VS Code
    /// behave.
    ///
    /// If the anchor is unset, or if either the anchor or the
    /// target is not currently visible (filtered out, ancestor
    /// collapsed, not yet loaded), the tree falls back to
    /// [`SelectionMode::Replace`] semantics using just the target.
    ExtendRange,
}

impl SelectionMode {
    /// Pick the mode implied by the active modifier keys.
    ///
    /// The mapping is:
    ///
    /// * `Shift`   → [`SelectionMode::ExtendRange`]
    /// * `Ctrl` or `Logo` (Command on macOS / Windows key elsewhere) → [`SelectionMode::Toggle`]
    /// * otherwise → [`SelectionMode::Replace`]
    ///
    /// `Shift` wins over `Ctrl` / `Logo` when both are held. A
    /// future release may distinguish "extend with toggle" (the
    /// Windows Explorer `Ctrl+Shift+click` behavior) with a fourth
    /// variant; for now `Ctrl+Shift+click` behaves as Shift-click.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use iced::keyboard::Modifiers;
    /// use iced_swdir_tree::SelectionMode;
    ///
    /// // In your update handler, when you've received a click event
    /// // and have tracked the current modifier state:
    /// let mode = SelectionMode::from_modifiers(current_modifiers);
    /// // Forward a rewritten Selected event to the widget.
    /// ```
    pub fn from_modifiers(modifiers: Modifiers) -> Self {
        if modifiers.shift() {
            Self::ExtendRange
        } else if modifiers.control() || modifiers.logo() {
            Self::Toggle
        } else {
            Self::Replace
        }
    }
}

#[cfg(test)]
mod tests;
