//! Unit tests for [`super::DirectoryTree::handle_key`].

use super::*;
use crate::directory_tree::node::LoadedEntry;
use std::path::PathBuf;

/// Build a tiny synthetic tree: /r with children /r/a (dir,
/// expanded, with children /r/a/aa, /r/a/ab) and /r/b (file).
fn make_tree() -> DirectoryTree {
    let mut tree = DirectoryTree::new(PathBuf::from("/r"));
    tree.root.is_expanded = true;
    tree.root.is_loaded = true;
    let mut a = TreeNode::from_entry(&LoadedEntry {
        path: PathBuf::from("/r/a"),
        is_dir: true,
        is_symlink: false,
        is_hidden: false,
    });
    a.is_expanded = true;
    a.is_loaded = true;
    a.children.push(TreeNode::from_entry(&LoadedEntry {
        path: PathBuf::from("/r/a/aa"),
        is_dir: false,
        is_symlink: false,
        is_hidden: false,
    }));
    a.children.push(TreeNode::from_entry(&LoadedEntry {
        path: PathBuf::from("/r/a/ab"),
        is_dir: false,
        is_symlink: false,
        is_hidden: false,
    }));
    tree.root.children.push(a);
    tree.root.children.push(TreeNode::from_entry(&LoadedEntry {
        path: PathBuf::from("/r/b"),
        is_dir: false,
        is_symlink: false,
        is_hidden: false,
    }));
    tree
}

fn press(key: Named) -> iced::keyboard::Key {
    iced::keyboard::Key::Named(key)
}

/// Shorthand: set the tree's active + anchor + selected_paths to
/// `path` as if a Replace-click had just happened. Bypasses the
/// update() machinery for test brevity.
fn put_cursor_at(tree: &mut DirectoryTree, path: PathBuf) {
    tree.active_path = Some(path.clone());
    tree.anchor_path = Some(path.clone());
    tree.selected_paths = vec![path.clone()];
    tree.sync_selection_flags();
}

#[test]
fn arrow_down_from_no_selection_picks_first_row() {
    let tree = make_tree();
    let event = tree.handle_key(&press(Named::ArrowDown), Modifiers::default());
    match event {
        Some(DirectoryTreeEvent::Selected(p, _, mode)) => {
            assert_eq!(p, PathBuf::from("/r"));
            assert_eq!(mode, SelectionMode::Replace);
        }
        other => panic!("expected Selected, got {other:?}"),
    }
}

#[test]
fn arrow_down_moves_forward_in_visible_order() {
    let mut tree = make_tree();
    put_cursor_at(&mut tree, PathBuf::from("/r"));
    let e = tree.handle_key(&press(Named::ArrowDown), Modifiers::default());
    match e {
        Some(DirectoryTreeEvent::Selected(p, _, mode)) => {
            assert_eq!(p, PathBuf::from("/r/a"));
            assert_eq!(mode, SelectionMode::Replace);
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn shift_arrow_down_emits_extend_range() {
    let mut tree = make_tree();
    put_cursor_at(&mut tree, PathBuf::from("/r"));
    let e = tree.handle_key(&press(Named::ArrowDown), Modifiers::SHIFT);
    match e {
        Some(DirectoryTreeEvent::Selected(p, _, mode)) => {
            assert_eq!(p, PathBuf::from("/r/a"));
            assert_eq!(mode, SelectionMode::ExtendRange);
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn arrow_up_moves_backward() {
    let mut tree = make_tree();
    put_cursor_at(&mut tree, PathBuf::from("/r/a/aa"));
    let e = tree.handle_key(&press(Named::ArrowUp), Modifiers::default());
    match e {
        Some(DirectoryTreeEvent::Selected(p, _, _)) => assert_eq!(p, PathBuf::from("/r/a")),
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn arrow_up_at_top_returns_none() {
    let mut tree = make_tree();
    put_cursor_at(&mut tree, PathBuf::from("/r"));
    assert!(
        tree.handle_key(&press(Named::ArrowUp), Modifiers::default())
            .is_none()
    );
}

#[test]
fn enter_on_folder_toggles() {
    let mut tree = make_tree();
    put_cursor_at(&mut tree, PathBuf::from("/r/a"));
    match tree.handle_key(&press(Named::Enter), Modifiers::default()) {
        Some(DirectoryTreeEvent::Toggled(p)) => assert_eq!(p, PathBuf::from("/r/a")),
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn enter_on_file_is_noop() {
    let mut tree = make_tree();
    put_cursor_at(&mut tree, PathBuf::from("/r/b"));
    assert!(
        tree.handle_key(&press(Named::Enter), Modifiers::default())
            .is_none()
    );
}

#[test]
fn left_on_expanded_folder_collapses() {
    let mut tree = make_tree();
    put_cursor_at(&mut tree, PathBuf::from("/r/a"));
    match tree.handle_key(&press(Named::ArrowLeft), Modifiers::default()) {
        Some(DirectoryTreeEvent::Toggled(p)) => assert_eq!(p, PathBuf::from("/r/a")),
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn left_on_file_moves_to_parent() {
    let mut tree = make_tree();
    put_cursor_at(&mut tree, PathBuf::from("/r/a/aa"));
    match tree.handle_key(&press(Named::ArrowLeft), Modifiers::default()) {
        Some(DirectoryTreeEvent::Selected(p, _, _)) => {
            assert_eq!(p, PathBuf::from("/r/a"))
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn right_on_collapsed_folder_expands() {
    let mut tree = make_tree();
    tree.root.children[0].is_expanded = false;
    put_cursor_at(&mut tree, PathBuf::from("/r/a"));
    match tree.handle_key(&press(Named::ArrowRight), Modifiers::default()) {
        Some(DirectoryTreeEvent::Toggled(p)) => assert_eq!(p, PathBuf::from("/r/a")),
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn right_on_expanded_folder_moves_to_first_child() {
    let mut tree = make_tree();
    put_cursor_at(&mut tree, PathBuf::from("/r/a"));
    match tree.handle_key(&press(Named::ArrowRight), Modifiers::default()) {
        Some(DirectoryTreeEvent::Selected(p, _, _)) => {
            assert_eq!(p, PathBuf::from("/r/a/aa"))
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn home_end_jump_to_boundaries() {
    let mut tree = make_tree();
    put_cursor_at(&mut tree, PathBuf::from("/r/a"));
    match tree.handle_key(&press(Named::Home), Modifiers::default()) {
        Some(DirectoryTreeEvent::Selected(p, _, _)) => assert_eq!(p, PathBuf::from("/r")),
        other => panic!("{other:?}"),
    }
    match tree.handle_key(&press(Named::End), Modifiers::default()) {
        Some(DirectoryTreeEvent::Selected(p, _, _)) => assert_eq!(p, PathBuf::from("/r/b")),
        other => panic!("{other:?}"),
    }
}

#[test]
fn shift_home_end_emits_extend_range() {
    let mut tree = make_tree();
    put_cursor_at(&mut tree, PathBuf::from("/r/a"));
    match tree.handle_key(&press(Named::Home), Modifiers::SHIFT) {
        Some(DirectoryTreeEvent::Selected(p, _, mode)) => {
            assert_eq!(p, PathBuf::from("/r"));
            assert_eq!(mode, SelectionMode::ExtendRange);
        }
        other => panic!("{other:?}"),
    }
    match tree.handle_key(&press(Named::End), Modifiers::SHIFT) {
        Some(DirectoryTreeEvent::Selected(p, _, mode)) => {
            assert_eq!(p, PathBuf::from("/r/b"));
            assert_eq!(mode, SelectionMode::ExtendRange);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn space_toggles_active_path() {
    let mut tree = make_tree();
    put_cursor_at(&mut tree, PathBuf::from("/r/b"));
    match tree.handle_key(&press(Named::Space), Modifiers::default()) {
        Some(DirectoryTreeEvent::Selected(p, is_dir, mode)) => {
            assert_eq!(p, PathBuf::from("/r/b"));
            assert!(!is_dir);
            assert_eq!(mode, SelectionMode::Toggle);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn ctrl_space_also_toggles() {
    let mut tree = make_tree();
    put_cursor_at(&mut tree, PathBuf::from("/r/b"));
    match tree.handle_key(&press(Named::Space), Modifiers::CTRL) {
        Some(DirectoryTreeEvent::Selected(_, _, mode)) => {
            assert_eq!(mode, SelectionMode::Toggle);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn escape_cancels_an_in_flight_drag() {
    let mut tree = make_tree();
    // Start a drag. Escape should now produce a Cancelled event.
    let _ = tree.update(DirectoryTreeEvent::Drag(DragMsg::Pressed(
        PathBuf::from("/r/a"),
        true,
    )));
    assert!(tree.is_dragging());
    match tree.handle_key(&press(Named::Escape), Modifiers::default()) {
        Some(DirectoryTreeEvent::Drag(DragMsg::Cancelled)) => {}
        other => panic!("expected Drag(Cancelled), got {other:?}"),
    }
}

#[test]
fn escape_is_unbound_when_no_drag_is_active() {
    // No drag → Escape stays unbound so the app can still use
    // it for its own dialogs/overlays.
    let tree = make_tree();
    assert!(
        tree.handle_key(&press(Named::Escape), Modifiers::default())
            .is_none()
    );
}

#[test]
fn unbound_keys_return_none() {
    let tree = make_tree();
    // Escape is covered by `escape_is_unbound_when_no_drag_is_active`.
    assert!(
        tree.handle_key(
            &iced::keyboard::Key::Character("x".into()),
            Modifiers::default()
        )
        .is_none()
    );
}
