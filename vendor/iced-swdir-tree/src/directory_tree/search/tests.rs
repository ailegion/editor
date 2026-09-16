//! Unit tests for [`super::matches_query`].

use super::*;

#[test]
fn empty_query_matches_everything() {
    assert!(matches_query(Path::new("/a/b/c"), ""));
    assert!(matches_query(Path::new("/"), ""));
}

#[test]
fn basename_substring_match() {
    assert!(matches_query(Path::new("/a/README.md"), "readme"));
    assert!(matches_query(Path::new("/a/README.md"), "ead"));
    assert!(matches_query(Path::new("/a/README.md"), ".md"));
}

#[test]
fn case_insensitive() {
    // `matches_query` takes a pre-lowercased query; its caller
    // (`SearchState::new`) lowercases once so the hot loop doesn't
    // have to. The test covers the two interesting directions:
    // lower query matches upper haystack and vice versa.
    assert!(matches_query(Path::new("/a/README.md"), "readme"));
    assert!(matches_query(Path::new("/a/readme.md"), "readme"));
    assert!(matches_query(Path::new("/a/ReAdMe.md"), "readme"));
    // The full end-to-end case-folding (query mixed case, haystack
    // mixed case) is covered by `SearchState::new` — see the v0.6
    // integration tests in tests/search.rs.
}

#[test]
fn path_components_dont_match() {
    // "src" must not match because its basename is "main.rs", not
    // because /project/src/ is in the path.
    assert!(!matches_query(Path::new("/project/src/main.rs"), "src"));
}

#[test]
fn no_match_returns_false() {
    assert!(!matches_query(Path::new("/a/README.md"), "xyz"));
    assert!(!matches_query(Path::new("/"), "anything"));
}

#[test]
fn query_longer_than_basename_is_no_match() {
    assert!(!matches_query(
        Path::new("/a/short.rs"),
        "a_much_longer_substring"
    ));
}
