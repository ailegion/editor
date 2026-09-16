//! Message types.
//!
//! [`DirectoryTreeEvent`] is the single message type flowing in and out
//! of the widget. Two variants are user-facing (`Toggled`, `Selected`);
//! a third (`Loaded`) carries async scan results and is opaque — parent
//! applications pass it through `update` without needing to introspect
//! it.

use std::path::PathBuf;
use std::sync::Arc;

use super::drag::DragMsg;
use super::node::LoadedEntry;
use super::selection::SelectionMode;

/// A message emitted by or consumed by the [`DirectoryTree`](crate::DirectoryTree) widget.
///
/// ## For parent applications
///
/// Wrap this in one of your own `Message` variants:
///
/// ```ignore
/// enum MyMessage {
///     Tree(iced_swdir_tree::DirectoryTreeEvent),
///     // ...
/// }
/// ```
///
/// Route every `Tree(event)` to [`DirectoryTree::update`] and map its
/// returned `Task` back. Pattern-match on `Toggled` / `Selected` *before*
/// forwarding if you want app-level side effects (e.g. previewing the
/// selected file):
///
/// ```ignore
/// fn update(&mut self, msg: MyMessage) -> Task<MyMessage> {
///     match msg {
///         MyMessage::Tree(event) => {
///             if let DirectoryTreeEvent::Selected(path, _, _) = &event {
///                 self.preview(path);
///             }
///             self.tree.update(event).map(MyMessage::Tree)
///         }
///     }
/// }
/// ```
///
/// [`DirectoryTree::update`]: crate::DirectoryTree::update
#[derive(Debug, Clone)]
pub enum DirectoryTreeEvent {
    /// A folder was toggled open/closed by the user.
    ///
    /// On first expansion the widget issues an async scan whose result
    /// arrives later as [`DirectoryTreeEvent::Loaded`]. Subsequent
    /// toggles of the same folder are instant — children stay in the
    /// internal cache.
    Toggled(PathBuf),

    /// Rescan a directory without toggling its expansion state.
    Refresh(PathBuf),

    /// A row was selected.
    ///
    /// The `bool` indicates whether the path is a directory (`true`)
    /// or a file (`false`). The [`SelectionMode`] controls how the
    /// click composes with any existing selection — see its docs for
    /// the full matrix.
    ///
    /// The built-in view always emits this with
    /// [`SelectionMode::Replace`] because iced 0.14's button
    /// callbacks cannot observe modifier keys at press time.
    /// Applications that want multi-select track modifier state
    /// themselves (see `examples/multi_select.rs`) and rewrite the
    /// mode before forwarding the event — [`SelectionMode::from_modifiers`]
    /// makes that a one-liner.
    Selected(PathBuf, bool, SelectionMode),

    /// Internal drag-machinery event.
    ///
    /// Emitted by the widget's built-in view as the user presses a
    /// row, moves across others, and releases. Applications should
    /// treat these as opaque and always route them back to
    /// [`DirectoryTree::update`](crate::DirectoryTree::update) —
    /// just like [`Loaded`](Self::Loaded).
    ///
    /// The widget's internal state machine may produce a
    /// [`Selected`](Self::Selected) or a [`DragCompleted`](Self::DragCompleted)
    /// as the downstream effect of a [`Drag`](Self::Drag) message.
    /// Apps observe those downstream events via the usual
    /// `.map(MyMessage::Tree)` routing — no extra plumbing needed.
    Drag(DragMsg),

    /// The user completed a drag gesture with intent to move (or
    /// otherwise transplant) `sources` into `destination`.
    ///
    /// The widget performs **no filesystem operation** on its own.
    /// Applications observe this event, perform whatever action
    /// they wish (move, copy, symlink, upload, ignore), and
    /// re-scan affected folders by emitting `Toggled` events
    /// (collapse then re-expand) to refresh the tree view.
    ///
    /// `destination` is guaranteed to be a directory that is not
    /// itself in `sources` nor a descendant of any source — see
    /// [`DragMsg`](crate::DragMsg) for the validity rules.
    /// `sources` is non-empty.
    DragCompleted {
        /// One or more paths the user started dragging. This is
        /// the [selected set](crate::DirectoryTree::selected_paths)
        /// at drag start if the pressed row was in the selection,
        /// otherwise just the pressed row.
        sources: Vec<PathBuf>,
        /// The folder over which the user released the mouse.
        destination: PathBuf,
    },

    /// Internal: an asynchronous scan completed.
    ///
    /// Parent applications should not construct this variant themselves;
    /// it is produced by `iced::Task`s that [`DirectoryTree::update`]
    /// returns and is routed back to `update` through the app's message
    /// plumbing. Treat it as opaque.
    ///
    /// [`DirectoryTree::update`]: crate::DirectoryTree::update
    Loaded(LoadPayload),
}

/// The payload of [`DirectoryTreeEvent::Loaded`].
///
/// The fields are crate-private so the internal representation can
/// evolve without breaking callers — `Clone` / `Debug` are sufficient
/// for anything a parent application needs to do with the message.
#[derive(Debug, Clone)]
pub struct LoadPayload {
    /// Directory whose scan completed.
    pub(crate) path: PathBuf,
    /// Generation counter snapshot taken when the scan was issued.
    /// Used to drop stale results if the user has since collapsed and
    /// re-expanded the folder.
    pub(crate) generation: u64,
    /// Depth of `path` relative to the tree's root — kept here so the
    /// update layer doesn't have to re-walk the tree to find it, and
    /// reserved for future per-depth UI feedback (e.g. showing a
    /// "reached max depth" indicator on the triggering node).
    #[allow(dead_code)]
    pub(crate) depth: u32,
    /// The scan outcome.
    ///
    /// `Arc` keeps the message cheaply cloneable even when the entry
    /// vector is large — iced passes messages by value and clones them
    /// en route to subscribers.
    pub(crate) result: Arc<Result<Vec<LoadedEntry>, crate::Error>>,
}
