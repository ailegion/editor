//! The [`DirectoryTree`] state type — holds the tree's nodes, cache, and
//! configuration, and is the owning handle the parent application keeps
//! across frames.
//!
//! The `update` and `view` methods live in their own submodules
//! ([`update`] and [`view`]) so this file stays focused on construction
//! and configuration.

pub(crate) mod config;
pub(crate) mod drag;
pub(crate) mod error;
pub(crate) mod executor;
pub(crate) mod icon;
pub(crate) mod keyboard;
pub(crate) mod message;
pub(crate) mod node;
pub(crate) mod search;
pub(crate) mod selection;
pub(crate) mod update;
pub(crate) mod view;
pub(crate) mod walker;

use std::path::PathBuf;
use std::sync::Arc;

use self::{
    config::{DirectoryFilter, TreeConfig},
    executor::{ScanExecutor, ThreadExecutor},
    node::{TreeCache, TreeNode},
};

/// A directory tree widget state.
///
/// Hold one `DirectoryTree` per visible tree in your application state.
/// The widget is cheap to construct: [`DirectoryTree::new`] creates only
/// the root node — child folders are scanned lazily when the user
/// expands them.
///
/// ## Lifecycle
///
/// 1. [`DirectoryTree::new`] — build with a root path.
/// 2. Optionally chain [`DirectoryTree::with_filter`] and/or
///    [`DirectoryTree::with_max_depth`] to configure.
/// 3. Call [`DirectoryTree::view`] from your `view` function.
/// 4. Route emitted [`DirectoryTreeEvent`]s through your app's message
///    system and pass them to [`DirectoryTree::update`], which returns an
///    [`iced::Task`] the parent should `.map(..)` back into its own
///    message type.
///
/// [`DirectoryTreeEvent`]: crate::DirectoryTreeEvent
pub struct DirectoryTree {
    /// The root of the tree. Always present even if traversal fails —
    /// failure just surfaces as [`TreeNode::error`] being set on the root.
    pub(crate) root: TreeNode,
    /// Configuration applied uniformly while traversing.
    pub(crate) config: TreeConfig,
    /// Path → already-loaded children cache to avoid re-scanning on
    /// repeated collapse/expand.
    pub(crate) cache: TreeCache,
    /// Monotonically increasing counter used to invalidate stale async
    /// results when the same folder is expanded, collapsed, expanded
    /// again (or when the tree is dropped / replaced).
    pub(crate) generation: u64,
    /// The set of currently-selected paths.
    ///
    /// v0.3 replaces v0.2's single `selected_path: Option<PathBuf>`
    /// with a Vec for multi-select. The order here is **not**
    /// semantically meaningful — treat it as a set. If you need the
    /// "most recently touched" path (for e.g. a detail pane), use
    /// [`DirectoryTree::active_path`]; if you need the pivot for
    /// range extension, use [`DirectoryTree::anchor_path`].
    ///
    /// `TreeNode::is_selected` is a view-layer cache kept in sync
    /// with this set by [`DirectoryTree::sync_selection_flags`].
    pub(crate) selected_paths: Vec<std::path::PathBuf>,
    /// The path the user most recently acted on (click, Space, etc.).
    ///
    /// This is also what [`DirectoryTree::selected_path`] returns,
    /// which preserves v0.2's single-select API semantics for apps
    /// that never used multi-select.
    pub(crate) active_path: Option<std::path::PathBuf>,
    /// The pivot for Shift+click range extension.
    ///
    /// Set by [`SelectionMode::Replace`](crate::SelectionMode) and
    /// [`SelectionMode::Toggle`](crate::SelectionMode);
    /// deliberately **not** updated by
    /// [`SelectionMode::ExtendRange`](crate::SelectionMode) so
    /// successive Shift+clicks all extend from the same origin —
    /// matching Windows Explorer / Finder / VS Code behaviour.
    pub(crate) anchor_path: Option<std::path::PathBuf>,
    /// In-progress drag state, if the user currently has the mouse
    /// button held after pressing on a row. `None` otherwise.
    ///
    /// See [`drag`](crate::directory_tree::drag) for the state
    /// machine that governs this field. The widget itself performs
    /// no filesystem operations; it just tracks the drag and emits
    /// [`DragCompleted`](crate::DirectoryTreeEvent::DragCompleted)
    /// on successful drop.
    pub(crate) drag: Option<drag::DragState>,
    /// **v0.5:** paths for which a prefetch-triggered scan is
    /// currently in flight.
    ///
    /// Populated by the [`update`](crate::directory_tree::update)
    /// dispatcher when it issues prefetch scans following a user
    /// expansion; drained by `on_loaded` when each scan result
    /// arrives. The presence of a path in this set is how the
    /// widget tells "this scan result came from prefetch — don't
    /// re-prefetch its children" apart from "this scan result came
    /// from a user-initiated expand — do prefetch its children".
    /// Prevents the exponential-cascade trap.
    ///
    /// When the user explicitly expands a path that's currently in
    /// this set (rare but possible: they click faster than the
    /// prefetch scan completes), `on_toggled` removes the path so
    /// the eventual user-initiated result triggers its own prefetch
    /// wave normally.
    pub(crate) prefetching_paths: std::collections::HashSet<std::path::PathBuf>,
    /// **v0.6:** incremental-search state.
    ///
    /// `None` when search is inactive (the default). When the app
    /// calls [`DirectoryTree::set_search_query`] with a non-empty
    /// query, this is populated with the query plus a cached set
    /// of visible-under-search paths.
    ///
    /// The rest of the widget — rendering, keyboard nav — consults
    /// this state automatically through [`TreeNode::visible_rows`].
    /// See the [`search`] module docs for the full contract.
    ///
    /// [`DirectoryTree::set_search_query`]: Self::set_search_query
    pub(crate) search: Option<search::SearchState>,
    /// **v0.7:** the icon theme used by the view.
    ///
    /// Defaults to the crate's stock theme for the enabled feature
    /// set ([`icon::LucideTheme`] when `icons` is on,
    /// [`icon::UnicodeTheme`] when off). Applications can install
    /// their own by implementing [`icon::IconTheme`] and calling
    /// [`DirectoryTree::with_icon_theme`].
    ///
    /// Stored as `Arc<dyn IconTheme>` (matching the existing
    /// `Arc<dyn ScanExecutor>` pattern) so `DirectoryTree` stays
    /// trivially cloneable if callers ever need that, and so the
    /// view layer can borrow the theme via `&dyn` without cloning.
    ///
    /// [`DirectoryTree::with_icon_theme`]: Self::with_icon_theme
    pub(crate) icon_theme: Arc<dyn icon::IconTheme>,
    /// Pluggable executor that runs blocking `scan_dir` calls.
    ///
    /// Defaults to [`ThreadExecutor`] (one `std::thread::spawn` per
    /// expansion), which is correct but slightly wasteful for apps
    /// that already run their own blocking-task pool. Swap it via
    /// [`DirectoryTree::with_executor`].
    pub(crate) executor: Arc<dyn ScanExecutor>,
}

impl DirectoryTree {
    /// Create a new tree rooted at `root`.
    ///
    /// Only the root node is created eagerly; the first level of
    /// children is scanned when the user first expands the root (or,
    /// for convenience, when you call [`DirectoryTree::update`] with a
    /// `Toggled(root)` event yourself).
    ///
    /// Defaults: [`DirectoryFilter::FilesAndFolders`], no depth limit.
    pub fn new(root: PathBuf) -> Self {
        let root_node = TreeNode::new_root(root.clone());
        Self {
            root: root_node,
            config: TreeConfig {
                root_path: root,
                filter: DirectoryFilter::default(),
                max_depth: None,
                prefetch_per_parent: 0,
                prefetch_skip: config::DEFAULT_PREFETCH_SKIP
                    .iter()
                    .map(|&s| s.to_string())
                    .collect(),
            },
            cache: TreeCache::default(),
            generation: 0,
            selected_paths: Vec::new(),
            active_path: None,
            anchor_path: None,
            drag: None,
            prefetching_paths: std::collections::HashSet::new(),
            search: None,
            icon_theme: icon::default_theme(),
            executor: Arc::new(ThreadExecutor),
        }
    }

    /// Set the display filter.
    ///
    /// This is the builder form used at construction. For runtime
    /// filter changes call [`DirectoryTree::set_filter`] — or use this
    /// method with `std::mem::replace` / `std::mem::take`-style moves
    /// if that fits the shape of your state better. Either route
    /// re-derives visible children from the cache, so the tree
    /// updates instantly without re-scanning the filesystem.
    pub fn with_filter(mut self, filter: DirectoryFilter) -> Self {
        self.set_filter(filter);
        self
    }

    /// Limit how deep the widget will load. `depth == 0` means only the
    /// root's direct children are ever loaded; `depth == 1` allows one
    /// more level of descent; and so on. No limit by default.
    pub fn with_max_depth(mut self, depth: u32) -> Self {
        self.config.max_depth = Some(depth);
        self
    }

    /// **v0.5:** configure parallel pre-expansion of visible descendants.
    ///
    /// When a user-initiated expansion finishes loading a folder,
    /// the widget will eagerly issue background scans for up to
    /// `limit` of that folder's direct children-that-are-folders, in
    /// parallel via the widget's [`ScanExecutor`]. Those children's
    /// data is loaded into the in-memory cache (`is_loaded = true`)
    /// but they are **not** automatically expanded in the UI — the
    /// user still controls what's drawn. When they click to expand
    /// a prefetched child, no I/O happens: the expansion is instant.
    ///
    /// Passing `0` (the default) disables prefetch and restores
    /// v0.1–0.4 behaviour exactly. Typical app values: `5`–`25`,
    /// sized to the number of folder-children a user plausibly
    /// targets with their next click. A very high value effectively
    /// means "prefetch every child folder" — the crate doesn't cap
    /// it, because apps with fast executors (tokio / rayon / smol)
    /// can legitimately want that.
    ///
    /// ```ignore
    /// let tree = DirectoryTree::new(root)
    ///     .with_executor(my_tokio_executor)   // fast pool
    ///     .with_prefetch_limit(20);           // up to 20 parallel scans
    /// ```
    ///
    /// Prefetch is **one level deep only**: a folder that loaded via
    /// prefetch does not itself trigger further prefetches. This
    /// avoids the exponential `limit ^ depth` cascade that would
    /// otherwise paper-over I/O costs the user didn't ask for.
    ///
    /// Prefetch respects [`with_max_depth`](Self::with_max_depth)
    /// the same way user-initiated scans do — a prefetch target
    /// past the cap is skipped, not scanned.
    ///
    /// See [`TreeConfig::prefetch_per_parent`] for the full contract.
    pub fn with_prefetch_limit(mut self, limit: usize) -> Self {
        self.config.prefetch_per_parent = limit;
        self
    }

    /// **v0.6.1:** replace the prefetch skip list.
    ///
    /// The list holds basenames that [`with_prefetch_limit`](Self::with_prefetch_limit)-
    /// driven scans will refuse to enter. Match is **exact-basename,
    /// ASCII case-insensitive** — `"target"` skips `target/` and
    /// `Target/` but not `my-target-files/`. The list applies
    /// **only** to automatic prefetch; a user click on a skipped
    /// folder still expands it normally.
    ///
    /// Replacing the list drops the default entries (see
    /// [`DEFAULT_PREFETCH_SKIP`]). To add entries while keeping
    /// the defaults, read them and append:
    ///
    /// ```ignore
    /// use iced_swdir_tree::{DirectoryTree, DEFAULT_PREFETCH_SKIP};
    ///
    /// let mut skip: Vec<String> = DEFAULT_PREFETCH_SKIP
    ///     .iter()
    ///     .map(|&s| s.to_string())
    ///     .collect();
    /// skip.push("huge_media_library".into());
    ///
    /// let tree = DirectoryTree::new(root)
    ///     .with_prefetch_limit(10)
    ///     .with_prefetch_skip(skip);
    /// ```
    ///
    /// To disable skipping entirely (dangerous — means `.git/` and
    /// `node_modules/` *will* be prefetched), pass an empty list:
    ///
    /// ```ignore
    /// let tree = DirectoryTree::new(root)
    ///     .with_prefetch_limit(10)
    ///     .with_prefetch_skip(Vec::<String>::new());
    /// ```
    ///
    /// See [`DEFAULT_PREFETCH_SKIP`] for the set populated by
    /// default.
    ///
    /// [`DEFAULT_PREFETCH_SKIP`]: crate::DEFAULT_PREFETCH_SKIP
    pub fn with_prefetch_skip<I, S>(mut self, names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.config.prefetch_skip = names.into_iter().map(Into::into).collect();
        self
    }

    /// Route blocking `scan_dir` calls through a custom executor.
    ///
    /// By default the widget spawns a fresh `std::thread` per
    /// expansion via [`ThreadExecutor`]. Apps that already manage
    /// a blocking-task pool (tokio, smol, rayon, ...) can implement
    /// [`ScanExecutor`] and swap it in here:
    ///
    /// ```ignore
    /// use std::sync::Arc;
    /// let tree = DirectoryTree::new(root).with_executor(Arc::new(MyTokioExecutor));
    /// ```
    ///
    /// Calling this mid-session is allowed (the next scan will use
    /// the new executor); in-flight scans initiated under the old
    /// executor still complete through it.
    ///
    /// [`ScanExecutor`]: crate::ScanExecutor
    /// [`ThreadExecutor`]: crate::ThreadExecutor
    pub fn with_executor(mut self, executor: Arc<dyn ScanExecutor>) -> Self {
        self.executor = executor;
        self
    }

    /// **v0.7:** replace the icon theme.
    ///
    /// Install an [`IconTheme`](crate::IconTheme) implementation to
    /// control which glyph, font, and size the view uses for each
    /// [`IconRole`](crate::IconRole) (folder-closed / folder-open /
    /// file / caret-right / caret-down / error).
    ///
    /// The crate ships two stock themes:
    ///
    /// * [`UnicodeTheme`](crate::UnicodeTheme) — always available,
    ///   renders short Unicode symbols (📁 📂 📄 ⚠ ▸ ▾). Default
    ///   when the `icons` feature is disabled.
    /// * [`LucideTheme`](crate::LucideTheme) — available with the
    ///   `icons` feature, renders real lucide vector glyphs.
    ///   Default when `icons` is enabled.
    ///
    /// You don't need to call this if you're happy with the stock
    /// default — `DirectoryTree::new` picks the right one for your
    /// feature configuration automatically.
    ///
    /// Custom themes implement the [`IconTheme`](crate::IconTheme)
    /// trait. A minimal example:
    ///
    /// ```
    /// use std::sync::Arc;
    /// use iced_swdir_tree::{
    ///     DirectoryTree, IconRole, IconSpec, IconTheme,
    /// };
    ///
    /// #[derive(Debug)]
    /// struct LabelTheme;
    ///
    /// impl IconTheme for LabelTheme {
    ///     fn glyph(&self, role: IconRole) -> IconSpec {
    ///         let label: &'static str = match role {
    ///             IconRole::FolderClosed => "[D]",
    ///             IconRole::FolderOpen => "[O]",
    ///             IconRole::File => "[F]",
    ///             IconRole::Error => "[!]",
    ///             IconRole::CaretRight => ">",
    ///             IconRole::CaretDown => "v",
    ///             _ => "?",
    ///         };
    ///         IconSpec::new(label)
    ///     }
    /// }
    ///
    /// let tree = DirectoryTree::new(".".into())
    ///     .with_icon_theme(Arc::new(LabelTheme));
    /// ```
    ///
    /// Note the `_ =>` fallback: [`IconRole`](crate::IconRole) is
    /// `#[non_exhaustive]` so new variants may be added in future
    /// minor releases.
    pub fn with_icon_theme(mut self, theme: Arc<dyn icon::IconTheme>) -> Self {
        self.icon_theme = theme;
        self
    }

    /// Change the display filter at runtime. The tree re-derives its
    /// visible children from the unfiltered cache, so the change is
    /// instant — no re-scan, no blocking the UI.
    ///
    /// **Selection is preserved.** Selection is kept by path on the
    /// widget, not on the [`TreeNode`]s that this call rebuilds, so
    /// every selected path survives the filter swap. Paths that
    /// become invisible under the new filter are not lost — flipping
    /// the filter back re-reveals them unchanged. This is true for
    /// both single and multi-select.
    ///
    /// **Expansion state is preserved too.** `rebuild_from_cache`
    /// copies the whole child subtree from the old node into its
    /// freshly-built replacement, so directories the user had opened
    /// stay open.
    pub fn set_filter(&mut self, filter: DirectoryFilter) {
        if self.config.filter == filter {
            return;
        }
        self.config.filter = filter;
        rebuild_from_cache(&mut self.root, &self.cache, filter);
        // Re-apply selection onto the new node graph. The `selected_paths`
        // Vec is authoritative; the per-node `is_selected` caches
        // need re-syncing after any mutation that drops and recreates
        // nodes.
        self.sync_selection_flags();
        // v0.6: if a search query is active, re-run it against the
        // post-filter node graph. A node that was a match may have
        // been filtered out (e.g. switching to FoldersOnly while
        // searching "readme.md"), or a newly-visible node may now
        // match.
        self.recompute_search_visibility();
    }

    /// Return the root path.
    pub fn root_path(&self) -> &std::path::Path {
        &self.config.root_path
    }

    /// Return the current filter.
    pub fn filter(&self) -> DirectoryFilter {
        self.config.filter
    }

    /// Return the current max depth, if any.
    pub fn max_depth(&self) -> Option<u32> {
        self.config.max_depth
    }

    /// Return a reference to the currently-active selected path, if any.
    ///
    /// The active path is the path the user most recently acted on —
    /// the last row clicked, the last Space-toggled, the last target
    /// of a Shift-range, etc. For single-select applications this is
    /// exactly the one selected path and matches v0.2 semantics.
    ///
    /// For multi-select, use [`DirectoryTree::selected_paths`] to see
    /// the whole set and [`DirectoryTree::anchor_path`] to read the
    /// pivot for range extension.
    ///
    /// The returned path may point to a node that is currently
    /// invisible (because an ancestor is collapsed, or because the
    /// active filter hides it); the view layer handles that
    /// gracefully.
    pub fn selected_path(&self) -> Option<&std::path::Path> {
        self.active_path.as_deref()
    }

    /// All currently-selected paths.
    ///
    /// Order is not semantically meaningful — treat the slice as a
    /// set. The slice is empty iff nothing is selected. Runs in
    /// O(1) (returns a reference to the internal Vec).
    pub fn selected_paths(&self) -> &[std::path::PathBuf] {
        &self.selected_paths
    }

    /// The anchor used as the pivot for
    /// [`SelectionMode::ExtendRange`](crate::SelectionMode).
    ///
    /// The anchor is set by `Replace` and `Toggle` selections, and
    /// is *not* moved by `ExtendRange` — so two successive
    /// `Shift+click`s from the same starting point select different
    /// ranges with the same origin.
    ///
    /// Returns `None` before the first selection.
    pub fn anchor_path(&self) -> Option<&std::path::Path> {
        self.anchor_path.as_deref()
    }

    /// `true` if `path` is in the selected set. O(n) in the set size.
    pub fn is_selected(&self, path: &std::path::Path) -> bool {
        self.selected_paths.iter().any(|p| p == path)
    }

    /// `true` when a drag gesture is in progress.
    ///
    /// Apps can use this to dim unrelated UI or change cursors,
    /// but the widget's own rendering already reflects drag state
    /// via the drop-target highlight.
    pub fn is_dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// Read-only view of the currently-hovered drop target, iff
    /// a drag is in progress and the cursor is over a valid folder.
    ///
    /// Returns `None` when there is no drag, or when the cursor is
    /// over an invalid target (a file, one of the sources, a
    /// descendant of a source, or empty space).
    pub fn drop_target(&self) -> Option<&std::path::Path> {
        self.drag.as_ref().and_then(|d| d.hover.as_deref())
    }

    /// Read-only view of the paths being dragged, iff a drag is in
    /// progress. Empty slice if there's no drag.
    pub fn drag_sources(&self) -> &[std::path::PathBuf] {
        self.drag.as_ref().map_or(&[], |d| d.sources.as_slice())
    }

    /// **v0.6:** set or update the incremental search query.
    ///
    /// Apps typically call this from their `TextInput`'s `on_input`
    /// callback. The widget narrows its visible rows to those
    /// whose **basename matches the query as a case-insensitive
    /// substring** — plus every ancestor of every match, so the
    /// user sees the tree context leading to their matches.
    ///
    /// ```ignore
    /// // In your update handler:
    /// Message::SearchChanged(q) => {
    ///     self.tree.set_search_query(q);
    ///     Task::none()
    /// }
    /// ```
    ///
    /// An **empty string clears the search** — equivalent to
    /// [`clear_search`](Self::clear_search). This is a deliberate
    /// simplification: having three states (none / empty-string /
    /// non-empty-string) tends to produce surprising UI where
    /// clearing the text box leaves the widget in a visually
    /// identical-but-semantically-distinct "searching for
    /// nothing" mode. With this contract there are only two
    /// states.
    ///
    /// Search operates on **already-loaded nodes only**. Matches
    /// inside unloaded folders don't appear until the folder
    /// loads (by user expansion or v0.5 prefetch). It does descend
    /// into loaded-but-collapsed folders, though — collapsed
    /// state doesn't hide content from search.
    ///
    /// Selection (including multi-selection) is **orthogonal** to
    /// search and is fully preserved: a selected row hidden by
    /// the query stays selected, and reappears when the query
    /// clears.
    ///
    /// See the crate-internal `search` module for the full contract
    /// (visible in the source tree at `src/directory_tree/search.rs`).
    pub fn set_search_query(&mut self, query: impl Into<String>) {
        let q: String = query.into();
        if q.is_empty() {
            self.search = None;
            return;
        }
        self.search = Some(search::SearchState::new(q));
        self.recompute_search_visibility();
    }

    /// Clear the active search query, if any. No-op if there is no
    /// active search.
    ///
    /// After this call [`is_searching`](Self::is_searching) returns
    /// `false`, [`search_query`](Self::search_query) returns
    /// `None`, and the widget returns to its normal view where
    /// rows are hidden only by `is_expanded` chain (plus the
    /// ordinary [`DirectoryFilter`]).
    ///
    /// [`DirectoryFilter`]: crate::DirectoryFilter
    pub fn clear_search(&mut self) {
        self.search = None;
    }

    /// The current search query as the application set it
    /// (preserving the app's original case), or `None` when search
    /// is inactive.
    pub fn search_query(&self) -> Option<&str> {
        self.search.as_ref().map(|s| s.query.as_str())
    }

    /// `true` iff a search query is currently active.
    ///
    /// Convenience wrapper around [`search_query`](Self::search_query);
    /// apps can use either depending on taste.
    pub fn is_searching(&self) -> bool {
        self.search.is_some()
    }

    /// Count of nodes that directly match the current search query.
    ///
    /// Returns `0` when no search is active. This is distinct from
    /// "visible rows" — the visible set also includes ancestor
    /// breadcrumbs leading down to matches, which are typically
    /// not what the user wants counted in their UI's "X results"
    /// display.
    pub fn search_match_count(&self) -> usize {
        self.search.as_ref().map_or(0, |s| s.match_count)
    }

    /// Recompute the cached set of visible-under-search paths.
    ///
    /// Walks every loaded node in the tree (ignoring `is_expanded`,
    /// since search should find matches even inside collapsed-but-
    /// loaded subtrees). Any node whose basename matches the
    /// current query is a "match" — its path is added to
    /// `visible_paths`, all its proper ancestors are added as
    /// breadcrumbs, and the `match_count` is incremented.
    ///
    /// Called automatically on [`set_search_query`](Self::set_search_query),
    /// [`set_filter`](Self::set_filter), and after every scan
    /// merge in `on_loaded`. Applications don't need to call it
    /// manually.
    pub(crate) fn recompute_search_visibility(&mut self) {
        let Some(state) = self.search.as_mut() else {
            return;
        };
        let mut visible: std::collections::HashSet<std::path::PathBuf> =
            std::collections::HashSet::new();
        let mut match_count: usize = 0;
        let _ = walk_for_search(
            &self.root,
            &state.query_lower,
            &mut visible,
            &mut match_count,
        );
        state.visible_paths = visible;
        state.match_count = match_count;
    }

    /// Search-aware version of [`TreeNode::visible_rows`](crate::directory_tree::node::TreeNode::visible_rows).
    ///
    /// When no search is active, this delegates directly to the
    /// node-level walker (which respects `is_expanded`).
    ///
    /// When a search is active, this walks the tree using the
    /// cached [`SearchState::visible_paths`](crate::directory_tree::search::SearchState)
    /// set instead of `is_expanded` — yielding only matches and
    /// their ancestors, and descending into collapsed subtrees when
    /// they contain matches. Indent depth is preserved so the view
    /// still renders nested rows correctly.
    pub(crate) fn visible_rows(&self) -> Vec<node::VisibleRow<'_>> {
        match &self.search {
            None => self.root.visible_rows(),
            Some(state) => {
                let mut out = Vec::new();
                collect_search_visible(&self.root, 0, &state.visible_paths, &mut out);
                out
            }
        }
    }
}

/// Search-mode equivalent of
/// [`node::collect_visible`](crate::directory_tree::node): walk the
/// tree, yielding rows for nodes in `visible` and descending into
/// them regardless of `is_expanded`.
fn collect_search_visible<'a>(
    node: &'a TreeNode,
    depth: u32,
    visible: &std::collections::HashSet<std::path::PathBuf>,
    out: &mut Vec<node::VisibleRow<'a>>,
) {
    if !visible.contains(&node.path) {
        return;
    }
    out.push(node::VisibleRow { node, depth });
    // Always descend when search is active — ancestors-of-matches
    // force children to render even if `is_expanded == false`.
    // Non-matching siblings are filtered out by the visible check
    // at the top of this function.
    for child in &node.children {
        collect_search_visible(child, depth + 1, visible, out);
    }
}

/// Walk `node` and every loaded descendant, collecting matches and
/// their ancestors into `visible`.
///
/// Returns `true` iff the subtree rooted at `node` contains at
/// least one match (including `node` itself). The caller uses
/// that signal to decide whether to add `node`'s own path as an
/// ancestor-breadcrumb — which is the only reason we'd want `node`
/// visible if it isn't itself a match.
///
/// Crucially, this walks **regardless of `is_expanded`**: search
/// sees through collapse. Folders that have been loaded once but
/// are currently collapsed still contribute their matches.
fn walk_for_search(
    node: &TreeNode,
    query_lower: &str,
    visible: &mut std::collections::HashSet<std::path::PathBuf>,
    match_count: &mut usize,
) -> bool {
    let mut subtree_has_match = false;
    for child in &node.children {
        if walk_for_search(child, query_lower, visible, match_count) {
            subtree_has_match = true;
        }
    }
    let self_matches = search::matches_query(&node.path, query_lower);
    if self_matches {
        *match_count += 1;
    }
    if self_matches || subtree_has_match {
        visible.insert(node.path.clone());
        true
    } else {
        false
    }
}

impl DirectoryTree {
    /// Re-apply [`DirectoryTree::selected_paths`] to the per-node
    /// `is_selected` flags used by the view.
    ///
    /// Called after any operation that may have replaced nodes
    /// (e.g. `set_filter`, a fresh `Loaded` payload arriving for a
    /// directory that contains selected children). Clearing every
    /// flag and then re-setting only those in `selected_paths`
    /// keeps the view in lockstep with the authoritative set.
    pub(crate) fn sync_selection_flags(&mut self) {
        self.root.clear_selection();
        // Clone the paths out to avoid a borrow clash; the set is
        // typically small (selected paths, not total nodes).
        let paths: Vec<std::path::PathBuf> = self.selected_paths.clone();
        for p in &paths {
            if let Some(node) = self.root.find_mut(p) {
                node.is_selected = true;
            }
        }
    }
}

/// Re-derive the `children` list at every already-loaded directory
/// in the tree from the unfiltered cache, applying `filter`.
///
/// Used by [`DirectoryTree::set_filter`] so a filter change is
/// instant. Unloaded directories are skipped — their filter will be
/// applied on first load, which is already correct without any help
/// from here.
///
/// Expansion state is preserved: before replacing a directory's
/// children we snapshot the `(path → is_expanded, is_loaded)` map of
/// the *old* children, then apply it to the *new* children built from
/// the raw cache. A directory the user had opened stays open, and a
/// grandchild already loaded stays loaded. Selection is re-applied
/// separately in [`DirectoryTree::set_filter`] via
/// [`DirectoryTree::sync_selection_flag`] because the selection
/// cursor lives on the widget, not on nodes.
fn rebuild_from_cache(node: &mut TreeNode, cache: &node::TreeCache, filter: DirectoryFilter) {
    if node.is_dir && node.is_loaded {
        if let Some(cached) = cache.get(&node.path) {
            // Snapshot old children by path so we can carry their
            // `is_expanded`, `is_loaded`, and transitive `children`
            // subtrees over. Without this, an ancestor's filter
            // change would wipe every descendant's loaded state —
            // even though none of the descendants' filesystem
            // listings actually changed.
            let mut previous: std::collections::HashMap<PathBuf, TreeNode> = node
                .children
                .drain(..)
                .map(|c| (c.path.clone(), c))
                .collect();
            node.children = cached
                .raw
                .iter()
                .filter(|e| e.passes(filter))
                .map(|e| {
                    // If this child already existed in the old tree,
                    // move it over wholesale — that preserves every
                    // flag and every deeper subtree in one step.
                    // Otherwise it's genuinely a new appearance (e.g.
                    // hidden → visible after flipping to
                    // AllIncludingHidden), so we build a fresh node.
                    previous
                        .remove(&e.path)
                        .unwrap_or_else(|| TreeNode::from_entry(e))
                })
                .collect();
        } else {
            // `is_loaded` without a cache line can happen for the
            // error branch (we mark loaded even on failure). Leave
            // the existing `children` slice — the error state is
            // what matters for those.
        }
    }
    for child in &mut node.children {
        rebuild_from_cache(child, cache, filter);
    }
}
