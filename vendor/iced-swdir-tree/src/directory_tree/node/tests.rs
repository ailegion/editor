//! Unit tests for [`super::TreeNode`] lookup and selection helpers.

use super::*;

#[test]
fn find_mut_self() {
    let mut root = TreeNode::new_root(PathBuf::from("/a"));
    assert!(root.find_mut(Path::new("/a")).is_some());
}

#[test]
fn find_mut_child() {
    let mut root = TreeNode::new_root(PathBuf::from("/a"));
    root.children.push(TreeNode::from_entry(&LoadedEntry {
        path: PathBuf::from("/a/b"),
        is_dir: true,
        is_symlink: false,
        is_hidden: false,
    }));
    root.children[0].is_loaded = true;
    root.children[0]
        .children
        .push(TreeNode::from_entry(&LoadedEntry {
            path: PathBuf::from("/a/b/c"),
            is_dir: false,
            is_symlink: false,
            is_hidden: false,
        }));
    assert!(root.find_mut(Path::new("/a/b/c")).is_some());
}

#[test]
fn find_mut_prunes_unrelated() {
    let mut root = TreeNode::new_root(PathBuf::from("/a"));
    root.children.push(TreeNode::from_entry(&LoadedEntry {
        path: PathBuf::from("/a/b"),
        is_dir: true,
        is_symlink: false,
        is_hidden: false,
    }));
    // target not under /a — should return None without panicking
    assert!(root.find_mut(Path::new("/x/y")).is_none());
}

#[test]
fn clear_selection_recurses() {
    let mut root = TreeNode::new_root(PathBuf::from("/a"));
    root.is_selected = true;
    let mut child = TreeNode::from_entry(&LoadedEntry {
        path: PathBuf::from("/a/b"),
        is_dir: true,
        is_symlink: false,
        is_hidden: false,
    });
    child.is_selected = true;
    root.children.push(child);
    root.clear_selection();
    assert!(!root.is_selected);
    assert!(!root.children[0].is_selected);
}
