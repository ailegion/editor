//! Unit tests for [`ItemTree`](super::ItemTree) — RFC 001 acceptance
//! criteria.

use super::*;

// ---- helpers ----

fn leaf(id: u64, data: &str) -> ItemNode<String> {
    ItemNode {
        id: NodeId(id),
        data: data.to_string(),
        children: vec![],
    }
}

fn branch(id: u64, data: &str, children: Vec<ItemNode<String>>) -> ItemNode<String> {
    ItemNode {
        id: NodeId(id),
        data: data.to_string(),
        children,
    }
}

fn simple_tree() -> ItemNode<String> {
    // root(0)
    //   chapter1(1)
    //     section1(2)
    //   chapter2(3)
    branch(
        0,
        "root",
        vec![
            branch(1, "Chapter 1", vec![leaf(2, "Section 1.1")]),
            branch(3, "Chapter 2", vec![leaf(4, "Section 2.1")]),
        ],
    )
}

// ---- basic construction ----

#[test]
fn new_tree_is_empty() {
    let tree: ItemTree<String> = ItemTree::new();
    assert!(!tree.is_searching());
    assert!(tree.selected_ids().is_empty());
    assert!(tree.visible_rows().is_empty());
}

#[test]
fn set_tree_populates_visible_root() {
    let mut tree = ItemTree::new();
    tree.set_tree(simple_tree());
    // Root is always the first visible row.
    let rows = tree.visible_rows();
    assert_eq!(rows[0].node.id, NodeId(0));
    // Children of root are NOT visible yet (root is collapsed).
    assert_eq!(rows.len(), 1);
}

// ---- expand / collapse ----

#[test]
fn toggled_expands_branch_node() {
    let mut tree = ItemTree::new();
    tree.set_tree(simple_tree());
    let _ = tree.update(ItemTreeEvent::Toggled(NodeId(0)));
    let rows = tree.visible_rows();
    // Root + 2 chapters.
    assert_eq!(rows.len(), 3);
}

#[test]
fn toggled_collapses_expanded_branch() {
    let mut tree = ItemTree::new();
    tree.set_tree(simple_tree());
    let _ = tree.update(ItemTreeEvent::Toggled(NodeId(0))); // expand
    let _ = tree.update(ItemTreeEvent::Toggled(NodeId(0))); // collapse
    assert_eq!(tree.visible_rows().len(), 1);
}

#[test]
fn toggled_on_leaf_is_noop() {
    let mut tree = ItemTree::new();
    tree.set_tree(simple_tree());
    let _ = tree.update(ItemTreeEvent::Toggled(NodeId(0))); // expand root
    let rows_before = tree.visible_rows().len();
    let _ = tree.update(ItemTreeEvent::Toggled(NodeId(4))); // leaf
    assert_eq!(tree.visible_rows().len(), rows_before);
}

// ---- selection ----

#[test]
fn replace_sets_exactly_one_id() {
    let mut tree = ItemTree::new();
    tree.set_tree(simple_tree());
    let _ = tree.update(ItemTreeEvent::Selected(NodeId(1), SelectionMode::Replace));
    assert_eq!(tree.selected_ids(), &[NodeId(1)]);
    assert!(tree.is_selected(NodeId(1)));
    assert!(!tree.is_selected(NodeId(3)));
}

#[test]
fn toggle_adds_then_removes() {
    let mut tree = ItemTree::new();
    tree.set_tree(simple_tree());
    let _ = tree.update(ItemTreeEvent::Selected(NodeId(1), SelectionMode::Replace));
    let _ = tree.update(ItemTreeEvent::Selected(NodeId(3), SelectionMode::Toggle));
    assert_eq!(tree.selected_ids().len(), 2);
    let _ = tree.update(ItemTreeEvent::Selected(NodeId(1), SelectionMode::Toggle));
    assert_eq!(tree.selected_ids(), &[NodeId(3)]);
}

#[test]
fn extend_range_covers_visible_rows() {
    let mut tree = ItemTree::new();
    tree.set_tree(simple_tree());
    let _ = tree.update(ItemTreeEvent::Toggled(NodeId(0)));
    // visible: root(0), chapter1(1), chapter2(3)
    let _ = tree.update(ItemTreeEvent::Selected(NodeId(0), SelectionMode::Replace));
    let _ = tree.update(ItemTreeEvent::Selected(
        NodeId(3),
        SelectionMode::ExtendRange,
    ));
    assert_eq!(tree.selected_ids().len(), 3);
    assert!(tree.is_selected(NodeId(0)));
    assert!(tree.is_selected(NodeId(1)));
    assert!(tree.is_selected(NodeId(3)));
}

// ---- set_tree diffing ----

#[test]
fn set_tree_preserves_expansion_for_surviving_keys() {
    let mut tree = ItemTree::new();
    tree.set_tree(simple_tree());
    let _ = tree.update(ItemTreeEvent::Toggled(NodeId(0)));
    assert_eq!(tree.visible_rows().len(), 3); // root + 2 chapters

    // Rebuild with the same keys but new label on one.
    let updated = branch(
        0,
        "root",
        vec![
            branch(1, "Chapter One (renamed)", vec![leaf(2, "Section 1.1")]),
            branch(3, "Chapter 2", vec![leaf(4, "Section 2.1")]),
        ],
    );
    tree.set_tree(updated);

    // Root was expanded before the update — it should stay expanded.
    assert_eq!(tree.visible_rows().len(), 3);
}

#[test]
fn set_tree_preserves_selection_for_surviving_keys() {
    let mut tree = ItemTree::new();
    tree.set_tree(simple_tree());
    let _ = tree.update(ItemTreeEvent::Selected(NodeId(1), SelectionMode::Replace));
    assert!(tree.is_selected(NodeId(1)));

    tree.set_tree(simple_tree()); // same keys
    assert!(
        tree.is_selected(NodeId(1)),
        "selection must survive set_tree"
    );
}

#[test]
fn set_tree_drops_selection_for_removed_keys() {
    let mut tree = ItemTree::new();
    tree.set_tree(simple_tree());
    let _ = tree.update(ItemTreeEvent::Selected(NodeId(3), SelectionMode::Replace));

    // Rebuild without chapter2(3).
    let shrunk = branch(
        0,
        "root",
        vec![branch(1, "Chapter 1", vec![leaf(2, "Section 1.1")])],
    );
    tree.set_tree(shrunk);

    assert!(
        !tree.is_selected(NodeId(3)),
        "disappeared key must drop from selection"
    );
    assert!(tree.selected_ids().is_empty());
}

#[test]
fn set_tree_resets_active_id_on_removal() {
    let mut tree = ItemTree::new();
    tree.set_tree(simple_tree());
    let _ = tree.update(ItemTreeEvent::Selected(NodeId(3), SelectionMode::Replace));
    assert_eq!(tree.active_id(), Some(NodeId(3)));

    let shrunk = branch(
        0,
        "root",
        vec![branch(1, "Chapter 1", vec![leaf(2, "Section 1.1")])],
    );
    tree.set_tree(shrunk);
    assert_eq!(tree.active_id(), None);
}

#[test]
fn set_tree_preserves_expansion_regardless_of_position_change() {
    // A node that moves from one parent to another should retain
    // its expansion state (RFC D4: position changes are preserved).
    let mut tree = ItemTree::new();
    let t1 = branch(
        0,
        "root",
        vec![
            branch(
                1,
                "Parent A",
                vec![branch(2, "Moveable", vec![leaf(5, "sub")])],
            ),
            branch(3, "Parent B", vec![]),
        ],
    );
    tree.set_tree(t1);
    let _ = tree.update(ItemTreeEvent::Toggled(NodeId(0)));
    let _ = tree.update(ItemTreeEvent::Toggled(NodeId(1)));
    let _ = tree.update(ItemTreeEvent::Toggled(NodeId(2))); // expand Moveable

    // Now move node 2 under Parent B instead.
    let t2 = branch(
        0,
        "root",
        vec![
            branch(1, "Parent A", vec![]),
            branch(
                3,
                "Parent B",
                vec![branch(2, "Moveable", vec![leaf(5, "sub")])],
            ),
        ],
    );
    tree.set_tree(t2);

    // Node 2 was expanded; it should still be expanded even though it moved.
    let root_state = tree.root.as_ref().unwrap();
    let parent_b = root_state
        .children
        .iter()
        .find(|c| c.id == NodeId(3))
        .unwrap();
    let moveable = parent_b
        .children
        .iter()
        .find(|c| c.id == NodeId(2))
        .unwrap();
    assert!(
        moveable.is_expanded,
        "expansion state must survive position change"
    );
}

// ---- search ----

#[test]
fn search_filters_to_matches_and_ancestors() {
    let mut tree: ItemTree<String> = ItemTree::new();
    tree.set_tree(simple_tree());

    // Expand everything first so all nodes are considered.
    let _ = tree.update(ItemTreeEvent::Toggled(NodeId(0)));
    let _ = tree.update(ItemTreeEvent::Toggled(NodeId(1)));
    let _ = tree.update(ItemTreeEvent::Toggled(NodeId(3)));

    tree.set_search_query("Section");
    assert!(tree.is_searching());
    assert_eq!(tree.search_match_count(), 2); // "Section 1.1", "Section 2.1"

    let visible_ids: Vec<NodeId> = tree.visible_rows().iter().map(|r| r.node.id).collect();
    // Matches + ancestors: root(0), chapter1(1), section1.1(2), chapter2(3), section2.1(4)
    assert!(visible_ids.contains(&NodeId(0)), "root is ancestor");
    assert!(visible_ids.contains(&NodeId(2)), "Section 1.1 is a match");
    assert!(visible_ids.contains(&NodeId(4)), "Section 2.1 is a match");
}

#[test]
fn search_case_insensitive() {
    let mut tree: ItemTree<String> = ItemTree::new();
    tree.set_tree(simple_tree());
    let _ = tree.update(ItemTreeEvent::Toggled(NodeId(0)));

    tree.set_search_query("CHAPTER");
    assert_eq!(tree.search_match_count(), 2);
}

#[test]
fn empty_query_clears_search() {
    let mut tree: ItemTree<String> = ItemTree::new();
    tree.set_tree(simple_tree());
    tree.set_search_query("Chapter");
    assert!(tree.is_searching());
    tree.set_search_query("");
    assert!(!tree.is_searching());
}

#[test]
fn search_selection_survives() {
    let mut tree: ItemTree<String> = ItemTree::new();
    tree.set_tree(simple_tree());
    let _ = tree.update(ItemTreeEvent::Toggled(NodeId(0)));
    let _ = tree.update(ItemTreeEvent::Selected(NodeId(3), SelectionMode::Replace));

    tree.set_search_query("Chapter 1");
    // Chapter 2 (NodeId 3) is not a match and might not be visible,
    // but it must still be selected.
    assert!(tree.is_selected(NodeId(3)), "selection survives search");
}

#[test]
fn clear_search_restores_normal_view() {
    let mut tree: ItemTree<String> = ItemTree::new();
    tree.set_tree(simple_tree());
    let _ = tree.update(ItemTreeEvent::Toggled(NodeId(0)));
    let rows_before = tree.visible_rows().len();

    tree.set_search_query("xxxxxxxx_no_match");
    tree.clear_search();
    assert_eq!(tree.visible_rows().len(), rows_before);
}

// ---- keyboard navigation ----

#[test]
fn arrow_down_moves_selection() {
    use iced::keyboard::Key;
    use iced::keyboard::key::Named;

    let mut tree: ItemTree<String> = ItemTree::new();
    tree.set_tree(simple_tree());
    let _ = tree.update(ItemTreeEvent::Toggled(NodeId(0)));
    let _ = tree.update(ItemTreeEvent::Selected(NodeId(0), SelectionMode::Replace));

    let event = tree.handle_key(
        &Key::Named(Named::ArrowDown),
        iced::keyboard::Modifiers::default(),
    );
    assert!(event.is_some());
    let _ = tree.update(event.unwrap());
    assert_eq!(tree.active_id(), Some(NodeId(1)));
}

#[test]
fn escape_is_unbound_when_no_drag() {
    use iced::keyboard::Key;
    use iced::keyboard::key::Named;

    let tree: ItemTree<String> = ItemTree::new();
    // ItemTree has no drag state; Escape should be unbound.
    let event = tree.handle_key(
        &Key::Named(Named::Escape),
        iced::keyboard::Modifiers::default(),
    );
    assert!(event.is_none());
}

// ---- icon theme ----

#[test]
fn with_icon_theme_accepts_arc_dyn() {
    use std::sync::Arc;
    let theme: Arc<dyn crate::IconTheme> = Arc::new(crate::UnicodeTheme);
    let _tree: ItemTree<String> = ItemTree::new().with_icon_theme(theme);
}
