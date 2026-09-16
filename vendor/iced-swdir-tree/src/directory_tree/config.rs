//! Configuration types: [`DirectoryFilter`] and [`TreeConfig`].

use std::path::PathBuf;

/// The default set of basenames that [`DirectoryTree`]'s v0.5 prefetch
/// machinery skips, populated into [`TreeConfig::prefetch_skip`] by
/// default.
///
/// These are directories that are commonly present, commonly *enormous*,
/// and commonly *not* what a user browses for in a tree widget:
///
/// * **Version control metadata** — `.git`, `.hg`, `.svn`. A `.git/`
///   directory alone can contain tens of thousands of tiny files
///   under `objects/`; speculatively scanning it on every repo-root
///   expansion is wasteful even on fast SSDs.
/// * **JavaScript dependencies** — `node_modules`. Frequently the
///   single largest directory in a project by both file count and
///   bytes.
/// * **Python caches and virtual environments** — `__pycache__`,
///   `.venv`, `venv`.
/// * **Build artifacts** — `target` (Rust, Java), `build`, `dist`.
///
/// The match is **exact-basename, ASCII case-insensitive**. Substring
/// matches are *not* performed — a folder named `my-target-files/`
/// is *not* skipped by the entry `"target"`.
///
/// # Overriding the default
///
/// This list is a starting point, not a contract. Apps that want to
/// skip additional directories should merge with the default:
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
/// Apps that want to disable skipping entirely — for example a
/// dedicated `.git/` viewer — can pass an empty list:
///
/// ```ignore
/// let tree = DirectoryTree::new(root).with_prefetch_skip(Vec::<String>::new());
/// ```
///
/// # User clicks are never skipped
///
/// This list applies *only* to automatic prefetch. If the user
/// explicitly clicks to expand a skipped folder, the widget scans
/// it normally — their click is an explicit request.
///
/// [`DirectoryTree`]: crate::DirectoryTree
pub const DEFAULT_PREFETCH_SKIP: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    "node_modules",
    "__pycache__",
    ".venv",
    "venv",
    "target",
    "build",
    "dist",
];

/// Controls which entries the widget displays.
///
/// The widget *always* scans every entry of an expanded directory
/// (swdir's `scan_dir` makes no filtering decisions); the filter is
/// applied as we normalize raw entries into [`TreeNode`]s. That means
/// a filter change takes effect on the next view without needing to
/// re-scan the filesystem.
///
/// [`TreeNode`]: crate::TreeNode
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DirectoryFilter {
    /// Show only directories (convenient for "pick a destination folder"
    /// pickers).
    FoldersOnly,
    /// Show both files and directories, but skip hidden entries (the
    /// default — matches most OS file pickers).
    #[default]
    FilesAndFolders,
    /// Show everything, including hidden entries.
    AllIncludingHidden,
}

impl DirectoryFilter {
    /// `true` if the filter suppresses hidden entries.
    pub fn skips_hidden(self) -> bool {
        !matches!(self, Self::AllIncludingHidden)
    }

    /// `true` if the filter suppresses regular files.
    pub fn skips_files(self) -> bool {
        matches!(self, Self::FoldersOnly)
    }
}

/// Per-tree configuration.
///
/// Constructed internally by [`DirectoryTree::new`] and its builder
/// methods; exposed as `pub` so tests and downstream tooling can
/// introspect the configuration.
///
/// [`DirectoryTree::new`]: crate::DirectoryTree::new
#[derive(Debug, Clone)]
pub struct TreeConfig {
    /// The tree's root directory.
    pub root_path: PathBuf,
    /// Active display filter.
    pub filter: DirectoryFilter,
    /// Maximum depth to descend into. `None` = unbounded.
    ///
    /// Depth is measured relative to the root: `Some(0)` means only
    /// root's direct children load, `Some(1)` allows grandchildren,
    /// and so on.
    pub max_depth: Option<u32>,
    /// **v0.5 — parallel pre-expansion of visible descendants.**
    ///
    /// When a user-initiated expansion finishes loading a folder,
    /// eagerly issue background scans for up to this many of the
    /// folder's direct children-that-are-folders, in parallel via
    /// [`ScanExecutor`]. The scans populate the in-memory cache
    /// (`is_loaded = true`) but do **not** automatically expand the
    /// children in the UI — the user still controls what's visible.
    /// When they later click to expand one of those children, the
    /// data is already there: no I/O, no thread spawn, no delay.
    ///
    /// `0` (the default) disables prefetch entirely, matching v0.1–0.4
    /// behaviour exactly. Higher values improve perceived
    /// responsiveness at the cost of background I/O on every
    /// user-initiated expansion. Typical app values: `5`–`25`. A huge
    /// value just means "prefetch every child folder"; the crate
    /// doesn't cap it because apps with fast executors may legitimately
    /// want that.
    ///
    /// Prefetch is **one level deep only** — a folder that loaded via
    /// prefetch does not itself trigger further prefetches of its
    /// children. This is intentional: cascading prefetch is
    /// exponential (`per_parent ^ depth`) and would be surprising as
    /// a default. If you need deeper prefetch, issue further
    /// [`DirectoryTreeEvent::Toggled`](crate::DirectoryTreeEvent::Toggled)
    /// events yourself from your app's update handler, or keep an
    /// eye on a future release — deeper cascade behind an opt-in may
    /// be added in a patch.
    ///
    /// Prefetch respects `max_depth` the same way user-initiated
    /// scans do: a prefetch target past the depth cap is skipped.
    ///
    /// [`ScanExecutor`]: crate::ScanExecutor
    pub prefetch_per_parent: usize,
    /// **v0.6.1 — prefetch safety valve.**
    ///
    /// A list of basenames that [`DirectoryTree`]'s prefetch
    /// machinery refuses to scan. Match is **exact-basename, ASCII
    /// case-insensitive**: the entry `"target"` skips a folder
    /// named `target/` or `Target/` but not `my-target-files/`.
    ///
    /// Defaults to [`DEFAULT_PREFETCH_SKIP`] — a curated list of
    /// common very-large directories (`.git`, `node_modules`,
    /// `target`, …) that are rarely the thing a user is browsing
    /// toward when they click around a tree. Apps can replace the
    /// list entirely via [`with_prefetch_skip`], or disable
    /// skipping by passing an empty list.
    ///
    /// Applies **only** to automatic prefetch scans. A user-
    /// initiated expansion (they clicked it) is never filtered —
    /// their click is an explicit request.
    ///
    /// [`DirectoryTree`]: crate::DirectoryTree
    /// [`with_prefetch_skip`]: crate::DirectoryTree::with_prefetch_skip
    pub prefetch_skip: Vec<String>,
}

#[cfg(test)]
mod tests;
