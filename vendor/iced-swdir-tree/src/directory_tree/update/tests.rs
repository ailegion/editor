//! Unit tests for [`super::DirectoryTree::update`] and its
//! internal state-transition handlers.

use crate::DirectoryTree;
use crate::directory_tree::drag::DragMsg;
use crate::directory_tree::message::{DirectoryTreeEvent, LoadPayload};
use crate::directory_tree::node::{LoadedEntry, TreeNode};
use crate::directory_tree::selection::SelectionMode;
use std::path::PathBuf;

#[test]
fn toggled_on_nonexistent_path_is_noop() {
    let mut tree = DirectoryTree::new(PathBuf::from("/definitely/not/there"));
    let _task = tree.update(DirectoryTreeEvent::Toggled(PathBuf::from(
        "/some/unrelated/elsewhere",
    )));
    // Just asserting we didn't panic.
}

#[test]
fn selection_is_single_under_replace_mode() {
    // Plain Replace-mode clicks keep single-select semantics:
    // each new selection wipes the previous one.
    let mut tree = DirectoryTree::new(PathBuf::from("/a"));
    tree.root
        .children
        .push(TreeNode::new_root(PathBuf::from("/a/b")));
    tree.root
        .children
        .push(TreeNode::new_root(PathBuf::from("/a/c")));
    tree.root.is_loaded = true;

    let _ = tree.update(DirectoryTreeEvent::Selected(
        PathBuf::from("/a/b"),
        true,
        SelectionMode::Replace,
    ));
    assert!(
        tree.root
            .find_mut(std::path::Path::new("/a/b"))
            .unwrap()
            .is_selected
    );
    assert_eq!(tree.selected_paths.len(), 1);

    let _ = tree.update(DirectoryTreeEvent::Selected(
        PathBuf::from("/a/c"),
        true,
        SelectionMode::Replace,
    ));
    assert!(
        !tree
            .root
            .find_mut(std::path::Path::new("/a/b"))
            .unwrap()
            .is_selected
    );
    assert!(
        tree.root
            .find_mut(std::path::Path::new("/a/c"))
            .unwrap()
            .is_selected
    );
    assert_eq!(tree.selected_paths.len(), 1);
}

#[test]
fn collapsing_keeps_children() {
    let mut tree = DirectoryTree::new(PathBuf::from("/r"));
    tree.root.is_loaded = true;
    tree.root.is_expanded = true;
    tree.root
        .children
        .push(TreeNode::new_root(PathBuf::from("/r/x")));
    let _ = tree.update(DirectoryTreeEvent::Toggled(PathBuf::from("/r")));
    assert!(!tree.root.is_expanded);
    assert_eq!(
        tree.root.children.len(),
        1,
        "children must survive collapse"
    );
}

#[test]
fn stale_loaded_events_are_dropped() {
    let mut tree = DirectoryTree::new(PathBuf::from("/r"));
    tree.root.is_dir = true;
    tree.root.is_expanded = true;
    tree.generation = 5;

    let stale = LoadPayload {
        path: PathBuf::from("/r"),
        generation: 4, // older than current
        depth: 0,
        result: std::sync::Arc::new(Ok(vec![LoadedEntry {
            path: PathBuf::from("/r/hacked"),
            is_dir: false,
            is_symlink: false,
            is_hidden: false,
        }])),
    };
    let _ = tree.update(DirectoryTreeEvent::Loaded(stale));
    assert!(
        tree.root.children.is_empty(),
        "stale result must be ignored"
    );
}

// -----------------------------------------------------------------
// Multi-select (v0.3) state-machine tests.
//
// These exercise the three modes directly against the state
// struct — no filesystem or iced runtime involved.
// -----------------------------------------------------------------

/// Build a small hand-made tree for selection tests: /r with
/// three expanded loaded siblings.
fn tree_with_three_siblings() -> DirectoryTree {
    let mut tree = DirectoryTree::new(PathBuf::from("/r"));
    tree.root.is_dir = true;
    tree.root.is_expanded = true;
    tree.root.is_loaded = true;
    for name in &["a", "b", "c"] {
        tree.root
            .children
            .push(TreeNode::new_root(PathBuf::from(format!("/r/{}", name))));
    }
    tree
}

fn sel(p: &str, mode: SelectionMode) -> DirectoryTreeEvent {
    DirectoryTreeEvent::Selected(PathBuf::from(p), false, mode)
}

#[test]
fn replace_clears_previous_selection() {
    let mut tree = tree_with_three_siblings();
    let _ = tree.update(sel("/r/a", SelectionMode::Replace));
    let _ = tree.update(sel("/r/b", SelectionMode::Replace));
    assert_eq!(tree.selected_paths.len(), 1);
    assert_eq!(tree.selected_paths[0], PathBuf::from("/r/b"));
    assert_eq!(
        tree.active_path.as_deref(),
        Some(std::path::Path::new("/r/b"))
    );
    assert_eq!(
        tree.anchor_path.as_deref(),
        Some(std::path::Path::new("/r/b"))
    );
}

#[test]
fn toggle_adds_then_removes() {
    let mut tree = tree_with_three_siblings();
    let _ = tree.update(sel("/r/a", SelectionMode::Replace));
    let _ = tree.update(sel("/r/b", SelectionMode::Toggle));
    let _ = tree.update(sel("/r/c", SelectionMode::Toggle));
    assert_eq!(
        tree.selected_paths.len(),
        3,
        "three paths after two toggles"
    );

    // Toggle /r/b again — should remove it.
    let _ = tree.update(sel("/r/b", SelectionMode::Toggle));
    assert_eq!(tree.selected_paths.len(), 2);
    assert!(
        !tree
            .selected_paths
            .iter()
            .any(|p| p == std::path::Path::new("/r/b"))
    );

    // Anchor always follows the most recent toggled path
    // regardless of add/remove.
    assert_eq!(
        tree.anchor_path.as_deref(),
        Some(std::path::Path::new("/r/b"))
    );
}

#[test]
fn toggle_updates_per_node_flags() {
    let mut tree = tree_with_three_siblings();
    let _ = tree.update(sel("/r/a", SelectionMode::Replace));
    let _ = tree.update(sel("/r/b", SelectionMode::Toggle));
    // Both /r/a and /r/b should have is_selected = true now.
    assert!(
        tree.root
            .find_mut(std::path::Path::new("/r/a"))
            .unwrap()
            .is_selected
    );
    assert!(
        tree.root
            .find_mut(std::path::Path::new("/r/b"))
            .unwrap()
            .is_selected
    );
    // /r/c untouched.
    assert!(
        !tree
            .root
            .find_mut(std::path::Path::new("/r/c"))
            .unwrap()
            .is_selected
    );
}

#[test]
fn extend_range_covers_visible_interval() {
    let mut tree = tree_with_three_siblings();
    // Anchor at /r/a via a plain Replace.
    let _ = tree.update(sel("/r/a", SelectionMode::Replace));
    // Shift-range to /r/c — should pick up /r, /r/a, /r/b, /r/c
    // (in visible-row order).
    let _ = tree.update(sel("/r/c", SelectionMode::ExtendRange));
    // /r is visible as row 0 but the anchor was /r/a, so range
    // runs from /r/a..=/r/c (3 rows).
    assert_eq!(tree.selected_paths.len(), 3);
    let names: Vec<_> = tree
        .selected_paths
        .iter()
        .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .collect();
    assert_eq!(names, vec!["a", "b", "c"]);
    // Anchor must not have moved.
    assert_eq!(
        tree.anchor_path.as_deref(),
        Some(std::path::Path::new("/r/a"))
    );
}

#[test]
fn extend_range_is_symmetric() {
    let mut tree = tree_with_three_siblings();
    // Anchor at /r/c, then extend *backwards* to /r/a.
    let _ = tree.update(sel("/r/c", SelectionMode::Replace));
    let _ = tree.update(sel("/r/a", SelectionMode::ExtendRange));
    assert_eq!(tree.selected_paths.len(), 3);
    // Anchor still on /r/c.
    assert_eq!(
        tree.anchor_path.as_deref(),
        Some(std::path::Path::new("/r/c"))
    );
}

#[test]
fn extend_range_without_anchor_falls_back_to_replace() {
    // Fresh tree, no prior selection/anchor: ExtendRange acts
    // as Replace onto the target.
    let mut tree = tree_with_three_siblings();
    let _ = tree.update(sel("/r/b", SelectionMode::ExtendRange));
    assert_eq!(tree.selected_paths.len(), 1);
    assert_eq!(tree.selected_paths[0], PathBuf::from("/r/b"));
    // Fallback *does* pick a new anchor, so the user can then
    // Shift+click somewhere else and get a meaningful range.
    assert_eq!(
        tree.anchor_path.as_deref(),
        Some(std::path::Path::new("/r/b"))
    );
}

#[test]
fn selection_on_stale_path_is_noop() {
    let mut tree = tree_with_three_siblings();
    let _ = tree.update(sel("/r/a", SelectionMode::Replace));
    // Click on a path that is NOT in the tree — leaves state alone.
    let _ = tree.update(sel("/completely/unrelated", SelectionMode::Replace));
    assert_eq!(tree.selected_paths.len(), 1);
    assert_eq!(tree.selected_paths[0], PathBuf::from("/r/a"));
}

// -----------------------------------------------------------------
// Drag-and-drop (v0.4) state-machine tests.
// -----------------------------------------------------------------

/// Build a tree with two folders /r/x, /r/y and a file /r/f.
/// Used to test "drop onto folder" vs "drop onto file".
fn tree_with_two_folders_and_a_file() -> DirectoryTree {
    let mut tree = DirectoryTree::new(PathBuf::from("/r"));
    tree.root.is_dir = true;
    tree.root.is_expanded = true;
    tree.root.is_loaded = true;
    let mut x = TreeNode::new_root(PathBuf::from("/r/x"));
    x.is_dir = true;
    tree.root.children.push(x);
    let mut y = TreeNode::new_root(PathBuf::from("/r/y"));
    y.is_dir = true;
    tree.root.children.push(y);
    let mut f = TreeNode::new_root(PathBuf::from("/r/f"));
    // `new_root` defaults is_dir=true (roots are always dirs);
    // override for the file node.
    f.is_dir = false;
    tree.root.children.push(f);
    tree
}

#[test]
fn press_without_prior_selection_drags_only_pressed_row() {
    let mut tree = tree_with_two_folders_and_a_file();
    let _ = tree.update(DirectoryTreeEvent::Drag(DragMsg::Pressed(
        PathBuf::from("/r/f"),
        false,
    )));
    assert!(tree.is_dragging());
    assert_eq!(tree.drag_sources(), &[PathBuf::from("/r/f")]);
}

#[test]
fn press_on_selected_row_drags_whole_selection() {
    // Multi-select {/r/f, /r/x}, then press on /r/f — drag the
    // whole set.
    let mut tree = tree_with_two_folders_and_a_file();
    let _ = tree.update(DirectoryTreeEvent::Selected(
        PathBuf::from("/r/f"),
        false,
        SelectionMode::Replace,
    ));
    let _ = tree.update(DirectoryTreeEvent::Selected(
        PathBuf::from("/r/x"),
        true,
        SelectionMode::Toggle,
    ));
    assert_eq!(tree.selected_paths().len(), 2);

    let _ = tree.update(DirectoryTreeEvent::Drag(DragMsg::Pressed(
        PathBuf::from("/r/f"),
        false,
    )));
    assert_eq!(
        tree.drag_sources().len(),
        2,
        "pressing a selected row drags the whole selection"
    );
}

#[test]
fn entered_folder_sets_hover_to_folder() {
    let mut tree = tree_with_two_folders_and_a_file();
    let _ = tree.update(DirectoryTreeEvent::Drag(DragMsg::Pressed(
        PathBuf::from("/r/f"),
        false,
    )));
    let _ = tree.update(DirectoryTreeEvent::Drag(DragMsg::Entered(PathBuf::from(
        "/r/x",
    ))));
    assert_eq!(tree.drop_target(), Some(std::path::Path::new("/r/x")));
}

#[test]
fn entered_file_leaves_hover_unset() {
    let mut tree = tree_with_two_folders_and_a_file();
    let _ = tree.update(DirectoryTreeEvent::Drag(DragMsg::Pressed(
        PathBuf::from("/r/x"),
        true,
    )));
    // Now cursor moves over a file, not a folder. Not a valid target.
    let _ = tree.update(DirectoryTreeEvent::Drag(DragMsg::Entered(PathBuf::from(
        "/r/f",
    ))));
    assert_eq!(tree.drop_target(), None);
}

#[test]
fn entered_source_row_leaves_hover_unset() {
    // Dragging /r/x and cursor re-enters /r/x — can't drop on
    // self. hover stays None.
    let mut tree = tree_with_two_folders_and_a_file();
    let _ = tree.update(DirectoryTreeEvent::Drag(DragMsg::Pressed(
        PathBuf::from("/r/x"),
        true,
    )));
    let _ = tree.update(DirectoryTreeEvent::Drag(DragMsg::Entered(PathBuf::from(
        "/r/x",
    ))));
    assert_eq!(tree.drop_target(), None);
}

#[test]
fn exited_target_clears_hover() {
    let mut tree = tree_with_two_folders_and_a_file();
    let _ = tree.update(DirectoryTreeEvent::Drag(DragMsg::Pressed(
        PathBuf::from("/r/f"),
        false,
    )));
    let _ = tree.update(DirectoryTreeEvent::Drag(DragMsg::Entered(PathBuf::from(
        "/r/x",
    ))));
    assert_eq!(tree.drop_target(), Some(std::path::Path::new("/r/x")));
    let _ = tree.update(DirectoryTreeEvent::Drag(DragMsg::Exited(PathBuf::from(
        "/r/x",
    ))));
    assert_eq!(tree.drop_target(), None);
}

#[test]
fn release_same_row_produces_delayed_selected() {
    // Press on /r/f, release on /r/f → click, not drag.
    // The widget emits a Task<Selected> for the app to
    // observe; the drag state is cleared.
    let mut tree = tree_with_two_folders_and_a_file();
    let _ = tree.update(DirectoryTreeEvent::Drag(DragMsg::Pressed(
        PathBuf::from("/r/f"),
        false,
    )));
    let _task = tree.update(DirectoryTreeEvent::Drag(DragMsg::Released(PathBuf::from(
        "/r/f",
    ))));
    // We can't easily inspect the Task's payload without
    // running an iced runtime, but we *can* confirm the drag
    // state is cleared, which is the state-machine
    // invariant. (The Task<Selected> side effect is exercised
    // end-to-end in the integration tests in tests/tree.rs.)
    assert!(!tree.is_dragging());
}

#[test]
fn release_over_valid_target_clears_drag_state() {
    let mut tree = tree_with_two_folders_and_a_file();
    let _ = tree.update(DirectoryTreeEvent::Drag(DragMsg::Pressed(
        PathBuf::from("/r/f"),
        false,
    )));
    let _ = tree.update(DirectoryTreeEvent::Drag(DragMsg::Entered(PathBuf::from(
        "/r/x",
    ))));
    let _task = tree.update(DirectoryTreeEvent::Drag(DragMsg::Released(PathBuf::from(
        "/r/x",
    ))));
    assert!(!tree.is_dragging());
    // The returned Task carries a DragCompleted; we test that
    // end-to-end in the integration tests.
}

#[test]
fn release_without_hover_is_silent_cancel() {
    // Press on /r/f, release on /r/y WITHOUT having entered any
    // row (Entered was never called). hover is None, so it's
    // treated as cancel — state clears, no event.
    let mut tree = tree_with_two_folders_and_a_file();
    let _ = tree.update(DirectoryTreeEvent::Drag(DragMsg::Pressed(
        PathBuf::from("/r/f"),
        false,
    )));
    let _task = tree.update(DirectoryTreeEvent::Drag(DragMsg::Released(PathBuf::from(
        "/r/y",
    ))));
    assert!(!tree.is_dragging());
    // selection must still be empty (no delayed Selected).
    assert!(tree.selected_paths().is_empty());
}

#[test]
fn explicit_cancelled_clears_drag() {
    let mut tree = tree_with_two_folders_and_a_file();
    let _ = tree.update(DirectoryTreeEvent::Drag(DragMsg::Pressed(
        PathBuf::from("/r/f"),
        false,
    )));
    assert!(tree.is_dragging());
    let _ = tree.update(DirectoryTreeEvent::Drag(DragMsg::Cancelled));
    assert!(!tree.is_dragging());
}

#[test]
fn stray_events_without_press_are_noops() {
    let mut tree = tree_with_two_folders_and_a_file();
    // No drag active. These must not panic or create state.
    let _ = tree.update(DirectoryTreeEvent::Drag(DragMsg::Entered(PathBuf::from(
        "/r/x",
    ))));
    let _ = tree.update(DirectoryTreeEvent::Drag(DragMsg::Exited(PathBuf::from(
        "/r/x",
    ))));
    let _ = tree.update(DirectoryTreeEvent::Drag(DragMsg::Released(PathBuf::from(
        "/r/x",
    ))));
    let _ = tree.update(DirectoryTreeEvent::Drag(DragMsg::Cancelled));
    assert!(!tree.is_dragging());
}

// -----------------------------------------------------------------
// v0.5: Prefetch (parallel pre-expansion of visible descendants)
// -----------------------------------------------------------------

/// Build a /r tree whose root is already expanded + loaded, with
/// three folder-children /r/x, /r/y, /r/z and a file /r/f. Lets us
/// test `select_prefetch_targets` without actually scanning.
fn tree_with_mixed_children() -> DirectoryTree {
    let mut tree = DirectoryTree::new(PathBuf::from("/r"));
    tree.root.is_dir = true;
    tree.root.is_expanded = true;
    tree.root.is_loaded = true;
    for name in ["x", "y", "z"] {
        let mut c = TreeNode::new_root(PathBuf::from(format!("/r/{name}")));
        c.is_dir = true;
        tree.root.children.push(c);
    }
    let mut f = TreeNode::new_root(PathBuf::from("/r/f"));
    f.is_dir = false;
    tree.root.children.push(f);
    tree
}

#[test]
fn prefetch_disabled_by_default_returns_no_targets() {
    let tree = tree_with_mixed_children();
    assert_eq!(tree.config.prefetch_per_parent, 0);
    assert!(
        tree.select_prefetch_targets(&PathBuf::from("/r"))
            .is_empty()
    );
}

#[test]
fn prefetch_selects_folder_children_only() {
    let mut tree = tree_with_mixed_children();
    tree.config.prefetch_per_parent = 10;
    let targets = tree.select_prefetch_targets(&PathBuf::from("/r"));
    // Should have x, y, z (folders) but not f (file).
    assert_eq!(targets.len(), 3);
    assert!(targets.iter().any(|p| p == &PathBuf::from("/r/x")));
    assert!(targets.iter().any(|p| p == &PathBuf::from("/r/y")));
    assert!(targets.iter().any(|p| p == &PathBuf::from("/r/z")));
    assert!(!targets.iter().any(|p| p == &PathBuf::from("/r/f")));
}

#[test]
fn prefetch_respects_the_limit() {
    let mut tree = tree_with_mixed_children();
    tree.config.prefetch_per_parent = 2;
    let targets = tree.select_prefetch_targets(&PathBuf::from("/r"));
    assert_eq!(targets.len(), 2);
}

#[test]
fn prefetch_skips_already_loaded_children() {
    // If /r/x is already loaded (cached from a previous expansion),
    // there's nothing to prefetch there. Leave it out of the targets.
    let mut tree = tree_with_mixed_children();
    tree.config.prefetch_per_parent = 10;
    tree.root
        .children
        .iter_mut()
        .find(|c| c.path.as_path() == std::path::Path::new("/r/x"))
        .unwrap()
        .is_loaded = true;
    let targets = tree.select_prefetch_targets(&PathBuf::from("/r"));
    assert_eq!(targets.len(), 2);
    assert!(!targets.iter().any(|p| p == &PathBuf::from("/r/x")));
}

#[test]
fn prefetch_respects_max_depth() {
    // With max_depth=0, no scan of depth > 0 is allowed. /r's
    // children are at depth 1, so even with prefetch enabled the
    // target list is empty.
    let mut tree = tree_with_mixed_children();
    tree.config.prefetch_per_parent = 10;
    tree.config.max_depth = Some(0);
    let targets = tree.select_prefetch_targets(&PathBuf::from("/r"));
    assert!(
        targets.is_empty(),
        "max_depth=0 must suppress all prefetch of /r's children"
    );
}

#[test]
fn on_loaded_drains_prefetching_paths_without_cascade() {
    // Simulate: /r is loaded (user action), then /r/x's prefetch
    // result arrives. The second on_loaded must return empty
    // targets (no cascade into /r/x/*).
    let mut tree = tree_with_mixed_children();
    tree.config.prefetch_per_parent = 10;

    // Mark /r/x as prefetching (this is what issue_prefetch_scans
    // would have done).
    tree.prefetching_paths.insert(PathBuf::from("/r/x"));

    // Simulate a prefetch scan result for /r/x arriving: bump gen,
    // build a payload with empty children, feed to on_loaded.
    let next_gen = tree.generation.wrapping_add(1);
    tree.generation = next_gen;
    let payload = LoadPayload {
        path: PathBuf::from("/r/x"),
        generation: next_gen,
        depth: 1,
        result: std::sync::Arc::new(Ok(Vec::<LoadedEntry>::new())),
    };

    let targets = tree.on_loaded(payload);
    assert!(
        targets.is_empty(),
        "prefetch-triggered on_loaded must not cascade"
    );
    assert!(
        !tree.prefetching_paths.contains(&PathBuf::from("/r/x")),
        "prefetching_paths must be drained on arrival"
    );
}

#[test]
fn toggled_clears_a_pending_prefetch_entry() {
    // If the user clicks to expand /r/x while /r/x's prefetch scan
    // is still in flight, on_toggled must clear the prefetching
    // flag so the eventual user-scan result triggers its own
    // prefetch wave normally.
    let mut tree = tree_with_mixed_children();
    tree.prefetching_paths.insert(PathBuf::from("/r/x"));

    let _ = tree.update(DirectoryTreeEvent::Toggled(PathBuf::from("/r/x")));
    assert!(
        !tree.prefetching_paths.contains(&PathBuf::from("/r/x")),
        "user Toggled must remove the path from prefetching_paths \
         so its eventual scan is treated as user-initiated"
    );
}

// -----------------------------------------------------------------
// v0.6.1: Prefetch safety valve (skip list)
// -----------------------------------------------------------------

/// Build /r with a `.git/`-alongside-real children tree so the
/// skip list has something to exclude.
fn tree_with_git_and_src() -> DirectoryTree {
    let mut tree = DirectoryTree::new(PathBuf::from("/r"));
    tree.root.is_dir = true;
    tree.root.is_expanded = true;
    tree.root.is_loaded = true;
    for name in [".git", "src", "target", "notes"] {
        let mut c = TreeNode::new_root(PathBuf::from(format!("/r/{name}")));
        c.is_dir = true;
        tree.root.children.push(c);
    }
    tree
}

#[test]
fn prefetch_default_skips_dot_git_and_target() {
    // Defaults for prefetch_skip are populated in new(); `.git`
    // and `target` should both be in it out of the box.
    let mut tree = tree_with_git_and_src();
    tree.config.prefetch_per_parent = 10;

    let targets = tree.select_prefetch_targets(&PathBuf::from("/r"));
    let names: Vec<_> = targets
        .iter()
        .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .collect();
    assert!(
        !names.contains(&".git".to_string()),
        "default skip list must exclude .git"
    );
    assert!(
        !names.contains(&"target".to_string()),
        "default skip list must exclude target"
    );
    // Non-skipped folders DO show up.
    assert!(names.contains(&"src".to_string()));
    assert!(names.contains(&"notes".to_string()));
}

#[test]
fn custom_skip_list_replaces_defaults() {
    // Setting the skip list to ["notes"] means .git becomes
    // prefetchable and `notes` becomes skipped — the exact
    // *replace* semantic (not extend).
    let mut tree = tree_with_git_and_src();
    tree.config.prefetch_per_parent = 10;
    tree.config.prefetch_skip = vec!["notes".to_string()];

    let targets = tree.select_prefetch_targets(&PathBuf::from("/r"));
    let names: Vec<_> = targets
        .iter()
        .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .collect();
    assert!(
        names.contains(&".git".to_string()),
        "custom skip list replaces the default — .git is no longer skipped"
    );
    assert!(
        !names.contains(&"notes".to_string()),
        "notes is explicitly in the custom skip list"
    );
}

#[test]
fn empty_skip_list_disables_skipping() {
    let mut tree = tree_with_git_and_src();
    tree.config.prefetch_per_parent = 10;
    tree.config.prefetch_skip = Vec::new();

    let targets = tree.select_prefetch_targets(&PathBuf::from("/r"));
    let names: Vec<_> = targets
        .iter()
        .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .collect();
    // All four folders, including `.git` and `target`, are now
    // prefetch candidates.
    assert_eq!(names.len(), 4);
    assert!(names.contains(&".git".to_string()));
    assert!(names.contains(&"target".to_string()));
}

#[test]
fn skip_list_match_is_case_insensitive_ascii() {
    // A user on macOS with `.Git/` on HFS+ must have it skipped
    // by the default list's `.git` entry.
    let mut tree = DirectoryTree::new(PathBuf::from("/r"));
    tree.root.is_dir = true;
    tree.root.is_expanded = true;
    tree.root.is_loaded = true;
    for name in [".Git", ".GIT", "Target"] {
        let mut c = TreeNode::new_root(PathBuf::from(format!("/r/{name}")));
        c.is_dir = true;
        tree.root.children.push(c);
    }
    tree.config.prefetch_per_parent = 10;

    let targets = tree.select_prefetch_targets(&PathBuf::from("/r"));
    assert!(
        targets.is_empty(),
        "all three folders should match the default (ASCII-case-insensitive) \
         skip entries .git and target"
    );
}

#[test]
fn skip_list_match_is_exact_basename_not_substring() {
    // A folder called `my-target-files/` must NOT be skipped just
    // because its name contains the substring `target`.
    let mut tree = DirectoryTree::new(PathBuf::from("/r"));
    tree.root.is_dir = true;
    tree.root.is_expanded = true;
    tree.root.is_loaded = true;
    let mut c = TreeNode::new_root(PathBuf::from("/r/my-target-files"));
    c.is_dir = true;
    tree.root.children.push(c);
    tree.config.prefetch_per_parent = 10;

    let targets = tree.select_prefetch_targets(&PathBuf::from("/r"));
    assert_eq!(
        targets.len(),
        1,
        "exact-basename match: 'target' in skip list must not \
         skip 'my-target-files'"
    );
}

#[test]
fn user_click_expands_skipped_folder_normally() {
    // The whole point: skip applies ONLY to prefetch. If the user
    // clicks to expand `.git`, we do issue a scan. The dispatcher
    // returns a non-empty Task (generation bumped, walker invoked),
    // not Task::none.
    let mut tree = tree_with_git_and_src();
    tree.config.prefetch_per_parent = 10; // prefetch on

    let git = PathBuf::from("/r/.git");
    let task = tree.update(DirectoryTreeEvent::Toggled(git.clone()));
    assert_ne!(
        task.units(),
        0,
        "user click on a skip-listed folder must still produce a real \
         scan Task — the skip list governs prefetch only, not user-initiated \
         expansion"
    );
    // And the node's is_expanded flag was flipped.
    let node = tree.root.find_mut(&git).unwrap();
    assert!(node.is_expanded);
}

#[test]
fn default_prefetch_skip_const_matches_default_field() {
    // Wiring test: a freshly-constructed DirectoryTree's
    // prefetch_skip must equal the public DEFAULT_PREFETCH_SKIP
    // constant. If someone edits the const but forgets to update
    // the initializer in new(), this fires.
    use crate::DEFAULT_PREFETCH_SKIP;
    let tree = DirectoryTree::new(PathBuf::from("/r"));
    let got: &[String] = &tree.config.prefetch_skip;
    assert_eq!(got.len(), DEFAULT_PREFETCH_SKIP.len());
    for (a, b) in got.iter().zip(DEFAULT_PREFETCH_SKIP.iter()) {
        assert_eq!(a, b);
    }
}
