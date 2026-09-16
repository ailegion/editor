//! # iced-swdir-tree
//!
//! A reusable [`iced`] widget for displaying a directory tree with lazy
//! loading, filtering, selection, and asynchronous traversal.
//!
//! Built on top of [`swdir`]'s `scan_dir` for single-level, non-recursive
//! directory listings — perfect for GUI trees that expand one folder at a
//! time.
//!
//! ## Minimal example
//!
//! ```no_run
//! use std::path::PathBuf;
//! use iced::{Element, Task};
//! use iced_swdir_tree::{DirectoryFilter, DirectoryTree, DirectoryTreeEvent};
//!
//! #[derive(Debug, Clone)]
//! enum Message {
//!     Tree(DirectoryTreeEvent),
//! }
//!
//! struct App {
//!     tree: DirectoryTree,
//! }
//!
//! impl App {
//!     fn new() -> (Self, Task<Message>) {
//!         let tree = DirectoryTree::new(PathBuf::from("."))
//!             .with_filter(DirectoryFilter::FilesAndFolders);
//!         (Self { tree }, Task::none())
//!     }
//!
//!     fn update(&mut self, message: Message) -> Task<Message> {
//!         match message {
//!             Message::Tree(event) => {
//!                 // Observe app-level side effects BEFORE passing to the widget.
//!                 // The third field is the `SelectionMode` (Replace/Toggle/ExtendRange);
//!                 // match it with `_` when you only care about which path was clicked.
//!                 if let DirectoryTreeEvent::Selected(path, is_dir, _) = &event {
//!                     println!("selected {:?} (dir={})", path, is_dir);
//!                 }
//!                 self.tree.update(event).map(Message::Tree)
//!             }
//!         }
//!     }
//!
//!     fn view(&self) -> Element<'_, Message> {
//!         self.tree.view(Message::Tree)
//!     }
//! }
//! # fn main() {}
//! ```
//!
//! ## Multi-select
//!
//! Shift/Ctrl-click support is covered in `examples/multi_select.rs`
//! — the built-in view always emits `SelectionMode::Replace` because
//! iced 0.14's button `on_press` can't observe modifier keys.
//! Applications that want multi-select track modifier state via a
//! keyboard subscription and rewrite the mode in their own update
//! handler using [`SelectionMode::from_modifiers`].
//!
//! ## Drag-and-drop
//!
//! The widget tracks drag gestures internally and emits a
//! [`DragCompleted`](DirectoryTreeEvent::DragCompleted)`{ sources,
//! destination }` event when the user releases the mouse over a
//! valid folder row. The widget performs **no** filesystem
//! operation — the app reacts to `DragCompleted` and does move /
//! copy / symlink / upload however it likes, then re-scans the
//! affected folders by emitting a `Toggled` collapse+expand to
//! refresh. See `examples/drag_drop.rs` for a worked example that
//! performs `fs::rename` on drop.
//!
//! ## Feature flags
//!
//! * **`icons`** (off by default) — when enabled, uses [`lucide-icons`] for
//!   folder/file graphics. When disabled, icons fall back to short text
//!   labels (`▸ `, `▾ `, etc.). The public API is identical either way.
//!
//! [`iced`]: https://docs.rs/iced
//! [`swdir`]: https://docs.rs/swdir
//! [`lucide-icons`]: https://docs.rs/lucide-icons

#![warn(missing_docs)]
#![cfg_attr(docsrs, feature(doc_cfg))]

mod directory_tree;
mod item_tree;

pub use crate::directory_tree::{
    DirectoryTree,
    config::{DEFAULT_PREFETCH_SKIP, DirectoryFilter, TreeConfig},
    drag::DragMsg,
    error::Error,
    executor::{ScanExecutor, ScanFuture, ScanJob, ThreadExecutor},
    icon::{IconRole, IconSpec, IconTheme, UnicodeTheme},
    message::{DirectoryTreeEvent, LoadPayload},
    node::TreeNode,
    selection::SelectionMode,
};

pub use crate::item_tree::{
    ItemTree, ItemTreeEvent,
    drag::{DropPosition, ItemDragMsg},
    node::{ItemNode, NodeId},
};

#[cfg(feature = "icons")]
#[cfg_attr(docsrs, doc(cfg(feature = "icons")))]
pub use crate::directory_tree::icon::LucideTheme;

#[cfg(feature = "icons")]
#[cfg_attr(docsrs, doc(cfg(feature = "icons")))]
pub use lucide_icons::LUCIDE_FONT_BYTES;

/// **Not part of the public API.** Shim exposed for the crate's own
/// integration tests in `tests/`.
///
/// Integration tests need to drive the state machine without running
/// an iced executor, which means they have to build `Loaded` payloads
/// whose fields are crate-private. This module exposes a tiny set of
/// operations that let tests poke at the otherwise-private surface.
/// It is `#[doc(hidden)]` and not covered by SemVer; downstream
/// crates must not depend on it.
#[doc(hidden)]
pub mod __testing {
    use std::path::PathBuf;
    use std::sync::Arc;

    use crate::directory_tree::message::LoadPayload;
    use crate::directory_tree::node::TreeNode;
    use crate::directory_tree::walker;
    use crate::{DirectoryTree, DirectoryTreeEvent};

    /// Toggle `dir` expanded (bumping the generation), synchronously
    /// scan it through `swdir::scan_dir`, and feed the resulting
    /// `Loaded` event back into the tree — exactly like the async
    /// scan task does in production, minus the thread hop.
    ///
    /// The filter stored on the tree is applied during the `Loaded`
    /// event's `update` handling (same as in production), not here.
    pub fn scan_and_feed(tree: &mut DirectoryTree, dir: PathBuf) {
        let _ = tree.update(DirectoryTreeEvent::Toggled(dir.clone()));
        let raw = swdir::scan_dir(&dir);
        let depth = dir
            .strip_prefix(tree.root_path())
            .map(|rel| rel.components().count() as u32)
            .unwrap_or(u32::MAX);
        let result = raw
            .as_ref()
            .map(|entries| walker::normalize_entries(entries))
            .map_err(crate::Error::from);
        let payload = LoadPayload {
            path: dir,
            generation: tree.generation,
            depth,
            result: Arc::new(result),
        };
        let _ = tree.update(DirectoryTreeEvent::Loaded(payload));
    }

    /// Access the root node for read-only inspection in integration
    /// tests. Not useful in production code — the crate's public API
    /// offers everything parent applications need.
    pub fn root(tree: &DirectoryTree) -> &TreeNode {
        &tree.root
    }
}
