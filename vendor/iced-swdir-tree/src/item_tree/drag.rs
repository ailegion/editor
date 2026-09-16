//! Drag-and-drop state machine for [`ItemTree`](super::ItemTree)
//! (RFC 002).
//!
//! The widget tracks an in-flight drag through a small state machine
//! and emits a
//! [`DragCompleted`](super::ItemTreeEvent::DragCompleted) event when
//! the user releases over a valid drop target. The widget mutates
//! **nothing** — exactly like
//! [`DirectoryTree`](crate::DirectoryTree)'s drag (see
//! [`crate::DragMsg`]). The application performs the move in its own
//! data model and feeds the new tree back through
//! [`set_tree`](super::ItemTree::set_tree), whose key-based diffing
//! preserves expansion and selection across the edit.
//!
//! # Drop model
//!
//! Unlike `DirectoryTree`, where a drop is always *into* the hovered
//! folder, an item-tree drop is placed relative to a target node at
//! one of three [`DropPosition`]s — `Before` / `Into` / `After` —
//! which together express both *reorder* (between siblings) and
//! *nest* (into a node).
//!
//! # Valid drop target
//!
//! A drop of sources `S` at `(target, position)` is valid iff:
//!
//! 1. `target` is a live node.
//! 2. `target ∉ S`.
//! 3. For `Before` / `After`, `target` is not the root (the root has
//!    no sibling slot).
//! 4. No cycle: the *effective new parent* (`target` for `Into`, else
//!    `target`'s parent) is neither a source nor a descendant of any
//!    source. This is checked by walking the effective parent's
//!    ancestor chain through a parent-map snapshot taken at drag
//!    start — O(depth), no live-tree access.
//!
//! # Deferred selection
//!
//! Pressing a row emits [`ItemDragMsg::Pressed`] which does **not**
//! change the selection. If the gesture ends on the same row that was
//! pressed, it was a click, and the widget emits a delayed
//! `Selected(_, Replace)` instead of a drop — the same pattern
//! `DirectoryTree` uses so a click on a multi-selected row doesn't
//! collapse the set until release.

use std::collections::HashMap;

use super::node::NodeId;

/// Where a dragged node lands relative to the drop target.
///
/// Together the three positions express both *reordering* (a node
/// becomes a sibling `Before` or `After` the target) and *nesting*
/// (a node becomes a child of the target, `Into` it).
///
/// Each has an unambiguous `(new_parent, insertion_point)` meaning
/// that does not depend on which row is visually adjacent:
///
/// | Position | New parent       | Insertion point                   |
/// | -------- | ---------------- | --------------------------------- |
/// | `Before` | parent of target | just before `target`              |
/// | `Into`   | `target` itself  | end of `target`'s children        |
/// | `After`  | parent of target | just after `target`               |
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropPosition {
    /// Insert as a sibling immediately before `target`.
    Before,
    /// Insert as the last child of `target` (nest).
    Into,
    /// Insert as a sibling immediately after `target`.
    After,
}

/// Opaque drag-machinery event produced by the widget's built-in
/// view when [`with_drag_and_drop(true)`](super::ItemTree::with_drag_and_drop)
/// is set.
///
/// Applications treat these as opaque payloads and route them back to
/// [`ItemTree::update`](super::ItemTree::update) unchanged — exactly
/// like [`crate::DragMsg`] for `DirectoryTree`. Apps generally never
/// construct these variants by hand.
#[derive(Debug, Clone)]
pub enum ItemDragMsg {
    /// Mouse pressed on a row — begins a drag.
    Pressed(NodeId),
    /// Cursor entered a drop zone of `target` at the given position
    /// while a drag is in progress.
    Entered(NodeId, DropPosition),
    /// Cursor left a drop zone of `target` at the given position.
    Exited(NodeId, DropPosition),
    /// Mouse released over a drop zone of `target` at the given
    /// position.
    Released(NodeId, DropPosition),
    /// External cancellation — emitted by the widget when the user
    /// presses `Escape` mid-drag, or by the application to abort a
    /// drag for its own reasons. Idempotent.
    Cancelled,
}

/// In-progress drag state. Crate-internal — held on
/// [`ItemTree`](super::ItemTree) and mutated by `on_drag`.
#[derive(Debug, Clone)]
pub(crate) struct ItemDragState {
    /// The node ids being dragged, in tree (pre-order) order.
    ///
    /// At drag start this is the current selection if the pressed
    /// node is selected, otherwise just the pressed node alone —
    /// matching `DirectoryTree` and Explorer/Finder behaviour.
    pub(crate) sources: Vec<NodeId>,
    /// The node that was actually pressed. Used to tell "click"
    /// (release on same node) from "drag" (release elsewhere).
    pub(crate) primary: NodeId,
    /// Child → parent map snapshot for every live node (`None` for
    /// the root), taken once at press time so ancestry checks during
    /// hover are O(depth) and never borrow the live tree.
    pub(crate) parent: HashMap<NodeId, Option<NodeId>>,
    /// The current valid hover target, or `None` over an invalid
    /// zone. The view paints a drop indicator from this.
    pub(crate) hover: Option<(NodeId, DropPosition)>,
}

impl ItemDragState {
    /// Would a drop of the current `sources` at `(target, position)`
    /// be valid? See the [module-level docs](self) for the four
    /// rules.
    pub(crate) fn is_valid_drop(&self, target: NodeId, position: DropPosition) -> bool {
        // Rule 1: target must be a live node (every live node, root
        // included, is a key in the parent map).
        if !self.parent.contains_key(&target) {
            return false;
        }
        // Rule 2: cannot place relative to / into a moving node.
        if self.sources.contains(&target) {
            return false;
        }
        // Effective new parent.
        let effective_parent = match position {
            DropPosition::Into => target,
            DropPosition::Before | DropPosition::After => {
                match self.parent.get(&target).copied().flatten() {
                    Some(p) => p,
                    // Rule 3: root has no sibling slot.
                    None => return false,
                }
            }
        };
        // Rule 4: no cycle — no source may be the effective parent or
        // an ancestor of it.
        for &s in &self.sources {
            if self.is_ancestor_or_self(s, effective_parent) {
                return false;
            }
        }
        true
    }

    /// Is `maybe_ancestor` equal to `node`, or an ancestor of it,
    /// per the snapshotted parent map? Walks the chain in O(depth).
    fn is_ancestor_or_self(&self, maybe_ancestor: NodeId, node: NodeId) -> bool {
        let mut cur = Some(node);
        while let Some(c) = cur {
            if c == maybe_ancestor {
                return true;
            }
            cur = self.parent.get(&c).copied().flatten();
        }
        false
    }
}

#[cfg(test)]
mod tests;
