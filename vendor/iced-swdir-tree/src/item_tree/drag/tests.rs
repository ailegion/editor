//! Unit tests for [`super::ItemDragState::is_valid_drop`].
//!
//! These build a synthetic drag state from a fixed parent map, so the
//! validity rules are exercised without constructing a live
//! `ItemTree` (RFC 002 acceptance criterion 3).

use super::*;

/// A fixed test tree:
///
/// ```text
/// 0 (root)
/// ├── 1
/// │   ├── 11
/// │   └── 12
/// └── 2
///     ├── 21
///     │   └── 211
///     └── 22
/// ```
fn parent_map() -> HashMap<NodeId, Option<NodeId>> {
    let edges: &[(u64, Option<u64>)] = &[
        (0, None),
        (1, Some(0)),
        (2, Some(0)),
        (11, Some(1)),
        (12, Some(1)),
        (21, Some(2)),
        (22, Some(2)),
        (211, Some(21)),
    ];
    edges
        .iter()
        .map(|&(c, p)| (NodeId(c), p.map(NodeId)))
        .collect()
}

fn state(sources: &[u64]) -> ItemDragState {
    ItemDragState {
        sources: sources.iter().map(|&s| NodeId(s)).collect(),
        primary: NodeId(sources[0]),
        parent: parent_map(),
        hover: None,
    }
}

use DropPosition::{After, Before, Into};

#[test]
fn self_drop_rejected_all_positions() {
    let s = state(&[1]);
    assert!(!s.is_valid_drop(NodeId(1), Into));
    assert!(!s.is_valid_drop(NodeId(1), Before));
    assert!(!s.is_valid_drop(NodeId(1), After));
}

#[test]
fn nest_into_own_descendant_is_a_cycle() {
    let s = state(&[1]);
    // Into a direct child.
    assert!(!s.is_valid_drop(NodeId(11), Into));
    // Sibling-of a direct child → effective parent is `1` itself.
    assert!(!s.is_valid_drop(NodeId(11), Before));
    assert!(!s.is_valid_drop(NodeId(11), After));
}

#[test]
fn deep_cycle_rejected() {
    // Moving `2` into its own grandchild `211`.
    let s = state(&[2]);
    assert!(!s.is_valid_drop(NodeId(211), Into));
}

#[test]
fn sibling_reorder_accepted() {
    let s = state(&[11]);
    assert!(s.is_valid_drop(NodeId(12), Before));
    assert!(s.is_valid_drop(NodeId(12), After));
}

#[test]
fn nest_into_unrelated_node_accepted() {
    let s = state(&[11]);
    assert!(s.is_valid_drop(NodeId(2), Into));
    assert!(s.is_valid_drop(NodeId(21), Into));
}

#[test]
fn root_has_no_sibling_slot() {
    let s = state(&[1]);
    assert!(!s.is_valid_drop(NodeId(0), Before));
    assert!(!s.is_valid_drop(NodeId(0), After));
}

#[test]
fn nest_into_root_accepted() {
    let s = state(&[1]);
    assert!(s.is_valid_drop(NodeId(0), Into));
}

#[test]
fn drop_into_current_parent_is_allowed() {
    // `211` is already a child of `21`; re-nesting it there is a
    // no-op the widget permits (the app may decline to act).
    let s = state(&[211]);
    assert!(s.is_valid_drop(NodeId(21), Into));
}

#[test]
fn nonexistent_target_rejected() {
    let s = state(&[1]);
    assert!(!s.is_valid_drop(NodeId(999), Into));
    assert!(!s.is_valid_drop(NodeId(999), Before));
}

#[test]
fn multi_source_all_valid_accepted() {
    let s = state(&[11, 12]);
    assert!(s.is_valid_drop(NodeId(2), Into));
    assert!(s.is_valid_drop(NodeId(22), After));
}

#[test]
fn multi_source_one_cycle_rejects_whole_drop() {
    // `2` cannot move under `21` (its own child), even bundled with
    // an innocent `11`.
    let s = state(&[2, 11]);
    assert!(!s.is_valid_drop(NodeId(21), Into));
}

#[test]
fn target_in_multi_source_rejected() {
    let s = state(&[1, 2]);
    assert!(!s.is_valid_drop(NodeId(2), After));
    assert!(!s.is_valid_drop(NodeId(1), Before));
}
