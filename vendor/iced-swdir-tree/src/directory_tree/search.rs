//! v0.6: incremental search with real-time filtering.
//!
//! When an application calls [`DirectoryTree::set_search_query`] with
//! a non-empty query, the widget narrows its visible rows to those
//! whose basename matches the query (case-insensitive substring)
//! **plus** every ancestor of every match — so the user sees both
//! the matching rows and the tree context that leads to them.
//!
//! # How matches are defined
//!
//! The match check is **basename, case-insensitive substring**:
//! `"rEAdMe"` matches both `README.md` and `readme.txt`. The
//! *full* path is not searched — only the file/folder name at
//! that level. This keeps search behaviour predictable (a query
//! for `"src"` doesn't light up every file that happens to live
//! under a `src/` folder).
//!
//! More sophisticated matching (regex, glob, fuzzy, case-sensitive
//! opt-in, full-path mode) is a future extension via a pluggable
//! matcher trait; for v0.6 we pick one predictable default.
//!
//! # Scope: already-loaded nodes only
//!
//! Search operates on **already-loaded nodes only** — it does not
//! trigger filesystem scans. If the user has a folder collapsed and
//! not yet loaded, matches inside it won't appear until the folder
//! is loaded (by user expansion, or by the v0.5 prefetch mechanism).
//!
//! Search *does* descend into loaded-but-collapsed folders, though:
//! if `/foo` has been loaded once (so its children exist in memory)
//! and the user then collapses it, a match deep inside still shows
//! up during search and the ancestor chain is force-rendered
//! regardless of `is_expanded` state.
//!
//! # Integration with other features
//!
//! * **Selection** is orthogonal to search. Selected rows remain
//!   selected when hidden by a search query, and reappear when the
//!   query clears.
//! * **Filter** (`DirectoryFilter`) operates first; search runs over
//!   filter-surviving nodes only. Changing the filter while a
//!   search is active re-runs the search over the new filter
//!   output.
//! * **Expand state (`is_expanded`)** is ignored while search is
//!   active. The widget renders the path-to-match chain even if
//!   the user had the ancestors collapsed. When the search is
//!   cleared, the saved `is_expanded` state takes effect.
//!
//! [`DirectoryTree::set_search_query`]: crate::DirectoryTree::set_search_query

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// The crate-internal search state.
///
/// Held at `DirectoryTree::search: Option<SearchState>` — `None`
/// when search is inactive (the default). Applications don't
/// construct this directly; they drive search through
/// [`DirectoryTree::set_search_query`] /
/// [`DirectoryTree::clear_search`] and observe through
/// [`DirectoryTree::search_query`] /
/// [`DirectoryTree::is_searching`] /
/// [`DirectoryTree::search_match_count`].
///
/// [`DirectoryTree::set_search_query`]: crate::DirectoryTree::set_search_query
/// [`DirectoryTree::clear_search`]: crate::DirectoryTree::clear_search
/// [`DirectoryTree::search_query`]: crate::DirectoryTree::search_query
/// [`DirectoryTree::is_searching`]: crate::DirectoryTree::is_searching
/// [`DirectoryTree::search_match_count`]: crate::DirectoryTree::search_match_count
#[derive(Debug, Clone)]
pub(crate) struct SearchState {
    /// The app-facing query string, exactly as the application
    /// passed it in. Returned by `search_query()` so the app can
    /// render its own feedback ("Searching for 'readme'...")
    /// without round-tripping through the Widget's normalized form.
    pub(crate) query: String,
    /// Lowercased snapshot of `query`, computed once at the point
    /// the query is set. Used for case-insensitive matching
    /// without paying `to_lowercase()` per node per recompute.
    pub(crate) query_lower: String,
    /// The set of paths currently visible under this search:
    /// every match plus every proper ancestor of every match.
    ///
    /// Recomputed from scratch whenever the query, the filter, or
    /// the set of loaded nodes changes.
    pub(crate) visible_paths: HashSet<PathBuf>,
    /// Count of *direct matches* (not ancestors). Separate from
    /// `visible_paths.len()` because that would conflate matches
    /// with the ancestor-breadcrumb rows. Apps typically show this
    /// as "N matches" in their search UI.
    pub(crate) match_count: usize,
}

impl SearchState {
    /// Build a fresh state for a new query. The caller is
    /// responsible for immediately calling
    /// `recompute_search_visibility` on the tree; `visible_paths`
    /// and `match_count` start empty and get populated there.
    pub(crate) fn new(query: String) -> Self {
        let query_lower = query.to_lowercase();
        Self {
            query,
            query_lower,
            visible_paths: HashSet::new(),
            match_count: 0,
        }
    }
}

/// Does `path` match the normalized query?
///
/// Match logic: case-insensitive substring on the basename
/// (`path.file_name()`). A path with no basename (root-only paths
/// like `/` or `C:\`) is compared against its full display form
/// as a fallback — unusual but defensive.
///
/// # Example
///
/// ```ignore
/// use std::path::Path;
/// assert!(matches_query(Path::new("/a/README.md"), "readme"));
/// assert!(matches_query(Path::new("/a/README.md"), "ME.MD"));
/// assert!(!matches_query(Path::new("/a/README.md"), "src"));
/// ```
pub(crate) fn matches_query(path: &Path, query_lower: &str) -> bool {
    if query_lower.is_empty() {
        return true;
    }
    let haystack = match path.file_name() {
        Some(name) => name.to_string_lossy().to_lowercase(),
        None => path.to_string_lossy().to_lowercase(),
    };
    haystack.contains(query_lower)
}

#[cfg(test)]
mod tests;
