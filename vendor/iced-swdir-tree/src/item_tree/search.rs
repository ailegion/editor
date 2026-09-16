//! Incremental search for [`ItemTree`](super::ItemTree).

use std::collections::HashSet;
use std::fmt::Display;

use super::node::{ItemNodeState, NodeId};

/// Cached search state: the query and the set of visible node IDs.
#[derive(Debug, Clone)]
pub(crate) struct ItemSearchState {
    /// As set by the caller — original casing preserved for display.
    pub(crate) query: String,
    /// Lowercased snapshot for comparisons.
    pub(crate) query_lower: String,
    /// Matches ∪ ancestors-of-matches.
    pub(crate) visible_ids: HashSet<NodeId>,
    /// Count of direct matches only (not ancestor breadcrumbs).
    pub(crate) match_count: usize,
}

impl ItemSearchState {
    pub(crate) fn new(query: String) -> Self {
        let query_lower = query.to_lowercase();
        Self {
            query,
            query_lower,
            visible_ids: HashSet::new(),
            match_count: 0,
        }
    }
}

/// Walk `node` and all loaded descendants, populating `visible` with
/// matches and their ancestors. Returns `true` iff the subtree
/// rooted at `node` contains at least one match.
pub(crate) fn walk_for_search<T: Display>(
    node: &ItemNodeState<T>,
    query_lower: &str,
    visible: &mut HashSet<NodeId>,
    match_count: &mut usize,
) -> bool {
    let mut subtree_has_match = false;
    for child in &node.children {
        if walk_for_search(child, query_lower, visible, match_count) {
            subtree_has_match = true;
        }
    }
    let self_matches = format!("{}", node.data)
        .to_lowercase()
        .contains(query_lower);
    if self_matches {
        *match_count += 1;
    }
    if self_matches || subtree_has_match {
        visible.insert(node.id);
        true
    } else {
        false
    }
}
