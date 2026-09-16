//! Handler for [`DirectoryTreeEvent::Drag`] — the drag state machine
//! introduced in v0.4.
//!
//! See [`crate::directory_tree::drag`] for the [`DragMsg`] / [`DragState`]
//! types and the three validity rules enforced on drop targets.

use iced::Task;

use crate::directory_tree::DirectoryTree;
use crate::directory_tree::drag::{DragMsg, DragState};
use crate::directory_tree::message::DirectoryTreeEvent;
use crate::directory_tree::selection::SelectionMode;

impl DirectoryTree {
    /// Drive the drag state machine.
    ///
    /// The five [`DragMsg`] variants drive the lifecycle:
    ///
    /// * `Pressed(p, is_dir)` — enter the Dragging state with
    ///   sources derived from the current selection (if `p` is
    ///   already selected) or from `p` alone (if it isn't).
    /// * `Entered(p)` — if `p` is a valid drop target, set it as
    ///   the hover.
    /// * `Exited(p)` — clear hover if it was pointing at `p`.
    /// * `Released(p)` — finalize the gesture:
    ///   - Same row as press? Emit a delayed `Selected(Replace)`
    ///     so the click behaves the way a v0.2/v0.3 single-click
    ///     would.
    ///   - Different row with a valid hover? Emit `DragCompleted`.
    ///   - Anywhere else? Quietly drop back to Idle.
    /// * `Cancelled` — drop to Idle unconditionally.
    ///
    /// `Released` and `Cancelled` are idempotent: they do nothing
    /// if no drag is in progress. The others are also safe to call
    /// out of order — the state machine silently ignores bogus
    /// sequences rather than panicking, so a stray `Entered` with
    /// no prior `Pressed` is a no-op.
    pub(super) fn on_drag(&mut self, msg: DragMsg) -> Task<DirectoryTreeEvent> {
        match msg {
            DragMsg::Pressed(path, is_dir) => {
                // If the pressed row is already part of the
                // selection, drag the whole selection. Otherwise
                // drag only that row — this matches Explorer /
                // Finder behaviour and avoids accidentally dragging
                // an unrelated set when the user clicks a
                // previously-unselected row.
                let sources: Vec<std::path::PathBuf> = if self.is_selected(&path) {
                    self.selected_paths.clone()
                } else {
                    vec![path.clone()]
                };
                self.drag = Some(DragState {
                    sources,
                    primary: path,
                    primary_is_dir: is_dir,
                    hover: None,
                });
                Task::none()
            }
            DragMsg::Entered(path) => {
                if let Some(d) = self.drag.as_mut() {
                    // Look up whether `path` is a directory. We
                    // have to do this dynamically because the view
                    // doesn't bundle `is_dir` into `Entered`
                    // (keeping the payload small).
                    let is_dir = self.root.find_mut(&path).map(|n| n.is_dir).unwrap_or(false);
                    if d.is_valid_target(&path, is_dir) {
                        d.hover = Some(path);
                    } else {
                        d.hover = None;
                    }
                }
                Task::none()
            }
            DragMsg::Exited(path) => {
                if let Some(d) = self.drag.as_mut()
                    && d.hover.as_deref() == Some(path.as_path())
                {
                    d.hover = None;
                }
                Task::none()
            }
            DragMsg::Released(path) => {
                let Some(d) = self.drag.take() else {
                    return Task::none();
                };
                // Case 1: same-row release. The user pressed and
                // released without ever crossing into another row,
                // i.e., it was a click. Dispatch a delayed
                // `Selected` with Replace mode.
                if path == d.primary {
                    return Task::done(DirectoryTreeEvent::Selected(
                        d.primary,
                        d.primary_is_dir,
                        SelectionMode::Replace,
                    ));
                }
                // Case 2: release over a valid drop target. Emit
                // `DragCompleted` for the app to act on.
                if let Some(dest) = d.hover {
                    return Task::done(DirectoryTreeEvent::DragCompleted {
                        sources: d.sources,
                        destination: dest,
                    });
                }
                // Case 3: release somewhere that wasn't a valid
                // target and wasn't the press row — cancelled drag.
                // Selection is deliberately NOT modified (the user
                // may have been trying to drag the current
                // multi-selection and aborted; preserving state is
                // less surprising than silently collapsing).
                Task::none()
            }
            DragMsg::Cancelled => {
                self.drag = None;
                Task::none()
            }
        }
    }
}
