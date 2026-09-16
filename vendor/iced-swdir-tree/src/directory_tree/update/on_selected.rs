//! Handler for [`DirectoryTreeEvent::Selected`] — click-to-select,
//! with the three selection modes from v0.3.

use crate::directory_tree::DirectoryTree;
use crate::directory_tree::selection::SelectionMode;

impl DirectoryTree {
    /// Apply a selection request.
    ///
    /// Selection state is stored on the widget
    /// ([`DirectoryTree::selected_paths`] + [`DirectoryTree::active_path`] +
    /// [`DirectoryTree::anchor_path`]); the per-node `is_selected`
    /// flags used by the view are synced at the end of every mutation.
    ///
    /// Behaviour by mode:
    ///
    /// * [`SelectionMode::Replace`] — clear everything and select
    ///   just `path`. Updates both anchor and active.
    /// * [`SelectionMode::Toggle`] — add `path` to the set if absent,
    ///   remove it otherwise. Updates anchor and active.
    /// * [`SelectionMode::ExtendRange`] — replace the set with every
    ///   visible row between the anchor and `path`. Does **not**
    ///   move the anchor; only active is updated to `path`. Falls
    ///   back to `Replace` semantics if no anchor is set or if
    ///   either endpoint is not currently visible.
    ///
    /// A request for a `path` that isn't present in the tree at all
    /// (stale click, filtered-out node, etc.) is a **no-op** — it
    /// leaves existing selection untouched, to avoid clobbering the
    /// user's real selection with a stale event.
    pub(super) fn on_selected(
        &mut self,
        path: std::path::PathBuf,
        _is_dir: bool,
        mode: SelectionMode,
    ) {
        // Gate every mode on "target exists in the tree". This is
        // the v0.2 guard that stopped stale clicks from clobbering
        // selection; we keep it for v0.3 across every mode.
        if self.root.find_mut(&path).is_none() {
            return;
        }

        match mode {
            SelectionMode::Replace => {
                self.selected_paths.clear();
                self.selected_paths.push(path.clone());
                self.active_path = Some(path.clone());
                self.anchor_path = Some(path);
            }
            SelectionMode::Toggle => {
                if let Some(pos) = self.selected_paths.iter().position(|p| p == &path) {
                    self.selected_paths.remove(pos);
                } else {
                    self.selected_paths.push(path.clone());
                }
                // Regardless of add-vs-remove, the anchor now tracks
                // the most recently ctrl-clicked item — that's the
                // pivot Windows Explorer and friends use for a
                // subsequent Shift+click.
                self.active_path = Some(path.clone());
                self.anchor_path = Some(path);
            }
            SelectionMode::ExtendRange => {
                let range = self.compute_visible_range(&path);
                match range {
                    Some(paths) => {
                        self.selected_paths = paths;
                        self.active_path = Some(path);
                        // anchor intentionally unchanged
                    }
                    None => {
                        // Fallback: behave as Replace. We don't move
                        // the anchor here either — it wasn't usable,
                        // so the user has not implicitly chosen a
                        // new pivot, and "picking a new pivot
                        // quietly" would be surprising.
                        self.selected_paths.clear();
                        self.selected_paths.push(path.clone());
                        self.active_path = Some(path.clone());
                        self.anchor_path = Some(path);
                    }
                }
            }
        }

        // Refresh per-node view flags.
        self.sync_selection_flags();
    }

    /// Build the list of paths between the anchor and `target`
    /// along the visible-rows order.
    ///
    /// Returns `None` when the operation can't be expressed as a
    /// range — no anchor, or either endpoint not visible. In that
    /// case the caller falls back to `Replace` semantics.
    fn compute_visible_range(&self, target: &std::path::Path) -> Option<Vec<std::path::PathBuf>> {
        let anchor = self.anchor_path.as_deref()?;
        let rows = self.visible_rows();
        let a_idx = rows.iter().position(|r| r.node.path == anchor)?;
        let t_idx = rows.iter().position(|r| r.node.path == target)?;
        let (lo, hi) = if a_idx <= t_idx {
            (a_idx, t_idx)
        } else {
            (t_idx, a_idx)
        };
        Some(rows[lo..=hi].iter().map(|r| r.node.path.clone()).collect())
    }
}
