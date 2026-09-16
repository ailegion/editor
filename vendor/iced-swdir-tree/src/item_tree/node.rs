//! Core node types for [`ItemTree`](super::ItemTree).

use std::collections::HashMap;

// ----------------------------------------------------------------
// Public types (caller-facing input)
// ----------------------------------------------------------------

/// An opaque, caller-assigned node identity.
///
/// The widget uses this only for equality checks and does not
/// inspect or modify it. Assign IDs however is convenient for
/// your data model — sequential integers, hashed string labels,
/// database row IDs, etc.
///
/// Prefer calling `NodeId(your_u64)` directly; no constructor
/// method is provided intentionally.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeId(pub u64);

/// A single node in the tree as provided by the caller.
///
/// Build your tree as nested `ItemNode<T>` values and pass the
/// root to [`ItemTree::set_tree`](super::ItemTree::set_tree).
/// The widget stores its own internal copy; you do not need to
/// keep your `ItemNode` alive after the call.
///
/// # Leaves vs. branches
///
/// A node whose `children` is empty is a leaf and never renders
/// a caret. A node with children is a branch and renders a
/// collapsed or expanded caret depending on the widget's internal
/// state.
///
/// # Example
///
/// ```
/// use iced_swdir_tree::{NodeId, ItemNode};
///
/// let root = ItemNode {
///     id: NodeId(0),
///     data: "Root",
///     children: vec![
///         ItemNode { id: NodeId(1), data: "Chapter 1", children: vec![] },
///         ItemNode { id: NodeId(2), data: "Chapter 2", children: vec![] },
///     ],
/// };
/// ```
#[derive(Debug, Clone)]
pub struct ItemNode<T> {
    /// Caller-assigned identity. Must be unique across the whole
    /// tree — duplicate IDs produce unspecified behaviour during
    /// diffing.
    pub id: NodeId,
    /// The data carried by this node. Rendered via `Display` if
    /// search is active; displayed as `format!("{data:?}")` in the
    /// default view.
    pub data: T,
    /// Direct children of this node. Empty slice = leaf node.
    pub children: Vec<ItemNode<T>>,
}

// ----------------------------------------------------------------
// Internal state (widget-owned, not exposed)
// ----------------------------------------------------------------

/// Internal representation of a tree node.
///
/// Distinct from [`ItemNode`] so that caller-supplied data can be
/// diffed against widget-owned state during
/// [`ItemTree::set_tree`](super::ItemTree::set_tree).
#[derive(Debug, Clone)]
pub(crate) struct ItemNodeState<T> {
    pub(crate) id: NodeId,
    pub(crate) data: T,
    pub(crate) children: Vec<ItemNodeState<T>>,
    /// `true` iff the user has expanded this node.
    /// Preserved across `set_tree` calls for surviving keys.
    pub(crate) is_expanded: bool,
    /// View hint: derived from `ItemTree::selected_ids`.
    /// Not authoritative; always re-synced from the set.
    pub(crate) is_selected: bool,
}

impl<T: Clone> ItemNodeState<T> {
    /// Build an `ItemNodeState` tree from caller-supplied input,
    /// transferring expansion and selection state for any node
    /// whose `NodeId` appears in `old_state`.
    pub(crate) fn from_input(node: ItemNode<T>, old_state: &HashMap<NodeId, (bool, bool)>) -> Self {
        let (is_expanded, is_selected) = old_state.get(&node.id).copied().unwrap_or((false, false));
        Self {
            id: node.id,
            data: node.data,
            is_expanded,
            is_selected,
            children: node
                .children
                .into_iter()
                .map(|c| ItemNodeState::from_input(c, old_state))
                .collect(),
        }
    }
}

/// Snapshot of per-node state: `(is_expanded, is_selected)`.
///
/// Built before a `set_tree` call so the new tree can copy state
/// for surviving keys.
pub(crate) fn snapshot_state<T>(node: &ItemNodeState<T>, out: &mut HashMap<NodeId, (bool, bool)>) {
    out.insert(node.id, (node.is_expanded, node.is_selected));
    for child in &node.children {
        snapshot_state(child, out);
    }
}

// ----------------------------------------------------------------
// Visible-row helpers
// ----------------------------------------------------------------

/// A reference to a node and its depth, as used by the view layer
/// and keyboard navigation.
pub(crate) struct VisibleRow<'a, T> {
    pub(crate) node: &'a ItemNodeState<T>,
    #[allow(dead_code)]
    pub(crate) depth: u32,
}

/// Collect the currently-visible rows from the tree rooted at
/// `node`, respecting `is_expanded`.
pub(crate) fn collect_visible<'a, T>(
    node: &'a ItemNodeState<T>,
    depth: u32,
    out: &mut Vec<VisibleRow<'a, T>>,
) {
    out.push(VisibleRow { node, depth });
    if node.is_expanded {
        for child in &node.children {
            collect_visible(child, depth + 1, out);
        }
    }
}

/// Collect visible rows under a search filter: include a node iff
/// its `NodeId` is in `visible_ids`, and always descend
/// (bypassing `is_expanded`) so collapsed subtrees can still
/// contain matches.
pub(crate) fn collect_search_visible<'a, T>(
    node: &'a ItemNodeState<T>,
    depth: u32,
    visible_ids: &std::collections::HashSet<NodeId>,
    out: &mut Vec<VisibleRow<'a, T>>,
) {
    if !visible_ids.contains(&node.id) {
        return;
    }
    out.push(VisibleRow { node, depth });
    for child in &node.children {
        collect_search_visible(child, depth + 1, visible_ids, out);
    }
}

// ----------------------------------------------------------------
// Tree traversal helpers
// ----------------------------------------------------------------

/// Find an immutable reference to the node with the given id.
pub(crate) fn find<T>(node: &ItemNodeState<T>, id: NodeId) -> Option<&ItemNodeState<T>> {
    if node.id == id {
        return Some(node);
    }
    node.children.iter().find_map(|c| find(c, id))
}

/// Find a mutable reference to the node with the given id.
pub(crate) fn find_mut<T>(
    node: &mut ItemNodeState<T>,
    id: NodeId,
) -> Option<&mut ItemNodeState<T>> {
    if node.id == id {
        return Some(node);
    }
    node.children.iter_mut().find_map(|c| find_mut(c, id))
}

/// Collect all `NodeId`s in the tree into `out`.
pub(crate) fn collect_all_ids<T>(
    node: &ItemNodeState<T>,
    out: &mut std::collections::HashSet<NodeId>,
) {
    out.insert(node.id);
    for child in &node.children {
        collect_all_ids(child, out);
    }
}

/// Return the parent `NodeId` of `target_id`, if any.
pub(crate) fn parent_of<T>(
    node: &ItemNodeState<T>,
    target_id: NodeId,
    parent_id: Option<NodeId>,
) -> Option<NodeId> {
    if node.id == target_id {
        return parent_id;
    }
    for child in &node.children {
        if let Some(found) = parent_of(child, target_id, Some(node.id)) {
            return Some(found);
        }
    }
    None
}

/// Clear all `is_selected` flags in the subtree rooted at `node`.
pub(crate) fn clear_selection<T>(node: &mut ItemNodeState<T>) {
    node.is_selected = false;
    for child in &mut node.children {
        clear_selection(child);
    }
}

/// Collect all `NodeId`s in pre-order (parents before children,
/// children in declaration order) into `out`.
///
/// Used by the drag machinery to report a multi-node drag's
/// `sources` in deterministic tree order rather than selection
/// order (RFC 002 [D5]).
pub(crate) fn collect_all_ids_ordered<T>(node: &ItemNodeState<T>, out: &mut Vec<NodeId>) {
    out.push(node.id);
    for child in &node.children {
        collect_all_ids_ordered(child, out);
    }
}

/// Build a child → parent map for every live node, with the root
/// mapping to `None`.
///
/// Snapshotted once when a drag begins so that the descendant /
/// cycle validity check can walk a node's ancestor chain in
/// O(depth) without borrowing the live tree (RFC 002 [D6]). This
/// is the `NodeId` analogue of `DirectoryTree`'s free O(1)
/// `PathBuf::starts_with` ancestry test.
pub(crate) fn build_parent_map<T>(
    root: Option<&ItemNodeState<T>>,
) -> std::collections::HashMap<NodeId, Option<NodeId>> {
    let mut map = std::collections::HashMap::new();
    if let Some(r) = root {
        fill_parent_map(r, None, &mut map);
    }
    map
}

fn fill_parent_map<T>(
    node: &ItemNodeState<T>,
    parent: Option<NodeId>,
    out: &mut std::collections::HashMap<NodeId, Option<NodeId>>,
) {
    out.insert(node.id, parent);
    for child in &node.children {
        fill_parent_map(child, Some(node.id), out);
    }
}
