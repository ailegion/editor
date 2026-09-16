//! Handler for [`DirectoryTreeEvent::Toggled`] — folder expand/collapse.
//!
//! First-time expansion kicks off an async scan via
//! [`walker::scan`](crate::directory_tree::walker::scan). Collapse and
//! re-expansion of already-loaded folders are instant (no I/O, children
//! are kept around).

use iced::Task;

use super::depth_of;
use crate::directory_tree::DirectoryTree;
use crate::directory_tree::message::DirectoryTreeEvent;
use crate::directory_tree::walker;

impl DirectoryTree {
    /// Toggle a directory's expansion state.
    ///
    /// Returns a scan task on first expansion; re-expanding an already
    /// scanned folder is instant (children are kept around), and
    /// collapsing never does I/O.
    pub(super) fn on_toggled(&mut self, path: std::path::PathBuf) -> Task<DirectoryTreeEvent> {
        // Compute depth against the root *first*: we need it for both
        // the depth-limit check and the generated `LoadPayload`. Doing
        // this before the `find_mut` call also avoids holding a
        // mutable borrow of `self.root` while we look at `self.config`.
        let depth = depth_of(&self.config.root_path, &path);
        let Some(node) = self.root.find_mut(&path) else {
            // The toggle refers to a path that no longer exists in
            // the tree — likely a stale message from a previous root.
            // Drop it silently; it's not worth crashing the app.
            return Task::none();
        };
        if !node.is_dir {
            return Task::none();
        }

        // Collapse: just flip the flag, keep children around so we
        // don't re-scan on the next expand.
        if node.is_expanded {
            node.is_expanded = false;
            return Task::none();
        }

        // Expand.
        node.is_expanded = true;

        // Already loaded → no I/O needed.
        if node.is_loaded {
            return Task::none();
        }

        // Depth-cap check: `None` means unbounded; `Some(d)` limits
        // *descent*, i.e. we refuse to scan at depths greater than d.
        if let Some(limit) = self.config.max_depth
            && depth > limit
        {
            node.is_loaded = true; // pretend we loaded — children stay empty
            return Task::none();
        }

        // Bump the generation and issue the scan. The generation is
        // captured by value into the closure so a future re-expansion
        // can invalidate this in-flight result.
        //
        // v0.5: if a prefetch scan was already in flight for this
        // path, "upgrade" this to a user-initiated scan by removing
        // the prefetch flag. The stale prefetch result will arrive
        // with an older generation and be dropped; this scan's result
        // will arrive with the fresh generation and be treated as a
        // user-initiated load (triggering its own prefetch wave).
        self.prefetching_paths.remove(&path);
        self.generation = self.generation.wrapping_add(1);
        walker::scan(self.executor.clone(), path, self.generation, depth)
    }
}
