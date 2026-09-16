//! In-memory tree types: [`TreeNode`] and [`TreeCache`].

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::Error;

/// A single node in the directory tree.
///
/// All fields are public so downstream code (tests, custom renderers
/// built on top of the widget's state) can inspect them, but the widget
/// itself drives mutation through [`DirectoryTree::update`].
///
/// [`DirectoryTree::update`]: crate::DirectoryTree::update
#[derive(Debug, Clone)]
pub struct TreeNode {
    /// Full path of the entry.
    pub path: PathBuf,
    /// `true` if the entry is a directory.
    pub is_dir: bool,
    /// `true` if the directory is currently expanded in the UI. Always
    /// `false` for files.
    pub is_expanded: bool,
    /// `true` if this directory has already been scanned at least once,
    /// even if the scan returned zero children. Distinguishes
    /// "scanned and empty" from "not scanned yet" in the view.
    pub is_loaded: bool,
    /// Direct children. Empty until `is_loaded` is `true`.
    pub children: Vec<TreeNode>,
    /// `true` if the user has this node selected.
    pub is_selected: bool,
    /// Populated when a scan of *this directory* failed. For files this
    /// is always `None`. The view layer uses this to render a greyed-out
    /// or error-tinted row.
    pub error: Option<Error>,
}

impl TreeNode {
    /// Build the root node of a freshly-constructed [`DirectoryTree`].
    ///
    /// The root is always treated as a directory (we can't meaningfully
    /// root a tree at a regular file).
    ///
    /// [`DirectoryTree`]: crate::DirectoryTree
    pub(crate) fn new_root(path: PathBuf) -> Self {
        Self {
            path,
            is_dir: true,
            is_expanded: false,
            is_loaded: false,
            children: Vec::new(),
            is_selected: false,
            error: None,
        }
    }

    /// Build a child node from a loaded entry.
    pub(crate) fn from_entry(entry: &LoadedEntry) -> Self {
        Self {
            path: entry.path.clone(),
            is_dir: entry.is_dir,
            is_expanded: false,
            is_loaded: false,
            children: Vec::new(),
            is_selected: false,
            error: None,
        }
    }

    /// Find a descendant (including `self`) by path, returning a
    /// mutable reference.
    ///
    /// Uses path-prefix pruning: we only descend into subtrees that
    /// could contain `target`, so the worst case is O(depth) not
    /// O(total nodes).
    pub(crate) fn find_mut(&mut self, target: &Path) -> Option<&mut TreeNode> {
        if self.path == target {
            return Some(self);
        }
        // Only descend if target lives under `self.path`. Without this
        // check we'd walk every sibling subtree on every lookup.
        if !target.starts_with(&self.path) {
            return None;
        }
        for child in &mut self.children {
            if let Some(hit) = child.find_mut(target) {
                return Some(hit);
            }
        }
        None
    }

    /// Clear the selection flag on every node in this subtree.
    ///
    /// Selection is single-select; setting a new selection is a
    /// clear-then-set operation.
    pub(crate) fn clear_selection(&mut self) {
        self.is_selected = false;
        for child in &mut self.children {
            child.clear_selection();
        }
    }

    /// Flat list of rows the view would render, in render order.
    ///
    /// Every ancestor-collapsed subtree is skipped. The returned
    /// order is the same one the user sees on screen, so it is the
    /// right order for keyboard navigation and Shift+click range
    /// extension to reason about. Cost is O(visible nodes).
    pub(crate) fn visible_rows(&self) -> Vec<VisibleRow<'_>> {
        let mut out = Vec::new();
        collect_visible(self, 0, &mut out);
        out
    }
}

/// A single visible row: the node, plus its indentation depth.
///
/// Crate-internal — used by the keyboard handler and by the
/// multi-select range-extension path. Depth is cached on the row
/// so callers don't have to re-walk from the root.
#[derive(Debug)]
pub(crate) struct VisibleRow<'a> {
    pub node: &'a TreeNode,
    pub depth: u32,
}

fn collect_visible<'a>(node: &'a TreeNode, depth: u32, out: &mut Vec<VisibleRow<'a>>) {
    out.push(VisibleRow { node, depth });
    if node.is_dir && node.is_expanded && node.is_loaded {
        for child in &node.children {
            collect_visible(child, depth + 1, out);
        }
    }
}

/// Lightweight, owned entry record produced by [`crate::walker`] and
/// consumed by [`super::update`] to build [`TreeNode`]s.
///
/// We don't use `swdir::DirEntry` here directly — keeping swdir types
/// out of the message enum means swdir can be a private dependency
/// from the public API's point of view.
#[derive(Debug, Clone)]
pub struct LoadedEntry {
    /// Full path of the entry.
    pub path: PathBuf,
    /// `true` if the entry is a directory. Symlinks to directories
    /// are treated as files here (the widget never auto-follows them,
    /// to stay robust against cycles).
    pub is_dir: bool,
    /// `true` if the entry itself is a symlink (regardless of target).
    ///
    /// Currently only used for cycle-avoidance diagnostics; kept here
    /// so v0.4+ can render a symlink indicator without having to
    /// re-stat every entry.
    #[allow(dead_code)]
    pub is_symlink: bool,
    /// `true` if the entry is hidden per OS conventions.
    ///
    /// Persisted on the entry (not just consulted in the scan path)
    /// so that a later filter change — e.g. flipping from
    /// `FilesAndFolders` to `AllIncludingHidden` — can be applied
    /// from the cache without another disk scan.
    pub is_hidden: bool,
}

impl LoadedEntry {
    /// `true` if this entry should be visible under `filter`.
    ///
    /// The rules mirror [`DirectoryFilter`](crate::DirectoryFilter)'s
    /// predicates but operate on the per-entry flags we already have
    /// in hand, keeping the decision O(1) rather than touching the
    /// filesystem again.
    pub(crate) fn passes(&self, filter: crate::DirectoryFilter) -> bool {
        if filter.skips_hidden() && self.is_hidden {
            return false;
        }
        if filter.skips_files() && !self.is_dir {
            return false;
        }
        true
    }
}

/// A path → children cache so that collapsing and re-expanding a folder
/// does not re-scan the filesystem.
///
/// The cache stores **unfiltered** children (raw normalised entries).
/// When the filter changes at runtime, [`DirectoryTree::set_filter`]
/// re-derives each already-loaded directory's visible child list from
/// its cached raw entries — no filesystem I/O, no flicker.
///
/// [`DirectoryTree::set_filter`]: crate::DirectoryTree::set_filter
/// (In practice the raw entry set is small — a single directory's
/// listing — so the extra memory cost of keeping both raw and
/// filtered forms is not justified at this scale.)
#[derive(Debug, Default, Clone)]
pub struct TreeCache {
    entries: HashMap<PathBuf, CacheEntry>,
}

/// A single cache line: the raw (unfiltered but normalized) listing
/// of a directory plus the generation number at which it was
/// recorded. Read by
/// [`DirectoryTree::set_filter`](crate::DirectoryTree::set_filter)
/// to re-derive filtered children without another scan.
#[derive(Debug, Clone)]
pub(crate) struct CacheEntry {
    /// Generation this cache line was recorded with. Stale lines are
    /// skipped rather than deleted, to avoid churn on repeat expansions.
    #[allow(dead_code)]
    pub generation: u64,
    /// The raw, unfiltered children of the directory.
    pub raw: Vec<LoadedEntry>,
}

impl TreeCache {
    /// Insert or replace the cached entries for `dir`.
    pub(crate) fn put(&mut self, dir: PathBuf, generation: u64, raw: Vec<LoadedEntry>) {
        self.entries.insert(dir, CacheEntry { generation, raw });
    }

    /// Retrieve the raw entries previously recorded for `dir`, if any.
    pub(crate) fn get(&self, dir: &Path) -> Option<&CacheEntry> {
        self.entries.get(dir)
    }
}

#[cfg(test)]
mod tests;
