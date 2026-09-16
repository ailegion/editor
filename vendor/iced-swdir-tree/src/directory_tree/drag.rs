//! v0.4: drag-and-drop state machine.
//!
//! The widget tracks an in-flight drag through a small state
//! machine and emits a [`DragCompleted`](crate::DirectoryTreeEvent::DragCompleted)
//! event when the user releases over a valid drop target. The
//! widget itself never touches the filesystem — the application is
//! responsible for actually moving / copying / symlinking / etc. the
//! dragged paths. This keeps the widget a pure UI layer that works
//! equally well for local files, network shares, zip archives, or
//! any other hierarchical backend the app exposes.
//!
//! # Valid drop target
//!
//! A path `T` is a valid drop target for a drag carrying source set
//! `S = {s1, s2, ...}` iff:
//!
//! 1. `T` is a directory — you can't drop into a file.
//! 2. `T` is not itself a member of `S` — dropping A onto A is a no-op.
//! 3. `T` is not a descendant of any member of `S` — dropping
//!    `/foo` onto `/foo/bar` would create a circular move.
//!
//! Validity is recomputed on every [`DragMsg::Entered`] event; the
//! view reads `DragState::hover` to paint a highlight on the current
//! drop target.
//!
//! # Deferred selection
//!
//! Because drags start with a mouse-down on a row and the same
//! mouse-down would otherwise collapse a multi-selection to that
//! single row (via v0.3's `SelectionMode::Replace`), the widget
//! uses the standard "deferred selection" pattern: the view emits
//! [`DragMsg::Pressed`] on mouse-down — which does **not** change
//! the selection — and [`DragMsg::Released`] on mouse-up. If the
//! release is on the same row as the press (i.e., the gesture was
//! a click, not a drag), the widget dispatches a
//! [`Selected(_, _, SelectionMode::Replace)`](crate::DirectoryTreeEvent::Selected)
//! event at that point. This matches Windows Explorer / macOS
//! Finder / VS Code behaviour: clicking an already-selected item
//! doesn't clobber the multi-selection until the user lets go
//! without having moved.

use std::path::{Path, PathBuf};

/// Opaque drag-machinery event produced by the widget's internal
/// mouse-area instrumentation.
///
/// Applications should treat these as opaque payloads and route
/// them back to [`DirectoryTree::update`](crate::DirectoryTree::update)
/// unchanged — exactly like
/// [`LoadPayload`](crate::LoadPayload). Apps generally never
/// construct these variants by hand.
#[derive(Debug, Clone)]
pub enum DragMsg {
    /// Mouse button was pressed on a row. The bool indicates
    /// whether the row is a directory (relevant for valid-target
    /// checks later if that row happens to be the release point).
    Pressed(PathBuf, bool),
    /// Cursor entered a row while a drag is in progress. The
    /// widget decides whether the row is a valid drop target.
    Entered(PathBuf),
    /// Cursor left a row while a drag is in progress.
    Exited(PathBuf),
    /// Mouse button was released on a row. The widget inspects its
    /// drag state to decide whether this was a click (same row as
    /// press → emit a delayed `Selected`), a successful drop
    /// (hover target set → emit `DragCompleted`), or a cancelled
    /// drag (release on non-target → quietly clear state).
    Released(PathBuf),
    /// External cancellation signal. Emitted by the widget itself
    /// when the user presses `Escape` while a drag is in flight,
    /// or by the application if it wants to abort a drag for its
    /// own reasons (e.g. a modal opened). Clearing drag state is
    /// idempotent, so this is safe to call speculatively.
    Cancelled,
}

/// In-progress drag state. Crate-internal — held on
/// [`DirectoryTree`](crate::DirectoryTree) and mutated by the
/// update layer.
#[derive(Debug, Clone)]
pub(crate) struct DragState {
    /// The paths being dragged.
    ///
    /// At drag start this is the current selected set if the
    /// pressed row is in the selection, otherwise just the pressed
    /// row on its own. This matches Explorer/Finder behaviour:
    /// pressing on an unselected row always drags only that row,
    /// regardless of what was selected before.
    pub(crate) sources: Vec<PathBuf>,
    /// The path that was actually pressed. Used to tell "click"
    /// (release on same row) from "drag" (release elsewhere).
    pub(crate) primary: PathBuf,
    /// Whether the primary row is a directory. Stashed at press
    /// time so the same-row-release branch can emit a correctly-
    /// typed `Selected` without re-looking-up the node.
    pub(crate) primary_is_dir: bool,
    /// The currently-hovered row, iff it is a valid drop target.
    /// `None` when the cursor is over an invalid target (a file,
    /// a descendant of a source, one of the sources themselves)
    /// or over empty space between rows.
    pub(crate) hover: Option<PathBuf>,
}

impl DragState {
    /// Would `target` (of the given `is_dir`-ness) be a valid drop
    /// destination for the current drag?
    ///
    /// See the [module-level docs](self) for the three rules.
    pub(crate) fn is_valid_target(&self, target: &Path, target_is_dir: bool) -> bool {
        if !target_is_dir {
            return false;
        }
        // Can't drop onto one of the sources.
        if self.sources.iter().any(|s| s == target) {
            return false;
        }
        // Can't drop into a descendant of any source (circular).
        // `starts_with` does component-wise comparison, so
        // `"/a/b".starts_with("/a")` is true but
        // `"/ab".starts_with("/a")` is false — safe.
        if self.sources.iter().any(|s| target.starts_with(s)) {
            return false;
        }
        true
    }
}

#[cfg(test)]
mod tests;
