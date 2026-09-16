//! Unit tests for [`super::DragState::is_valid_target`].

use super::*;

fn state_with_sources(sources: &[&str]) -> DragState {
    DragState {
        sources: sources.iter().map(PathBuf::from).collect(),
        primary: PathBuf::from(sources[0]),
        primary_is_dir: false,
        hover: None,
    }
}

#[test]
fn files_are_never_valid_targets() {
    let s = state_with_sources(&["/a"]);
    assert!(!s.is_valid_target(Path::new("/b"), false));
}

#[test]
fn self_drop_is_rejected() {
    let s = state_with_sources(&["/a", "/b"]);
    assert!(!s.is_valid_target(Path::new("/a"), true));
    assert!(!s.is_valid_target(Path::new("/b"), true));
}

#[test]
fn descendant_drop_is_rejected() {
    let s = state_with_sources(&["/root/parent"]);
    assert!(!s.is_valid_target(Path::new("/root/parent/child"), true));
    assert!(!s.is_valid_target(Path::new("/root/parent/child/grand"), true));
}

#[test]
fn sibling_drop_is_accepted() {
    let s = state_with_sources(&["/root/a"]);
    assert!(s.is_valid_target(Path::new("/root/b"), true));
}

#[test]
fn parent_drop_is_accepted() {
    // Dropping /root/a back onto /root is a legitimate move (a
    // no-op to the filesystem but the widget shouldn't block
    // it — the application is free to decide not to act).
    let s = state_with_sources(&["/root/a"]);
    assert!(s.is_valid_target(Path::new("/root"), true));
}

#[test]
fn prefix_but_not_ancestor_is_accepted() {
    // "/foobar" is NOT a descendant of "/foo" — it's a
    // differently-named sibling. `Path::starts_with` uses
    // component boundaries, so this correctly returns false.
    let s = state_with_sources(&["/foo"]);
    assert!(s.is_valid_target(Path::new("/foobar"), true));
}
