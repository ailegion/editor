//! State machine for [`DirectoryTree::update`].
//!
//! The dispatcher lives here; each event variant is handled by a
//! dedicated submodule so that any one handler can grow independently
//! without the file crossing the "too long to read" threshold again:
//!
//! | Event                                   | Handler module           |
//! |-----------------------------------------|--------------------------|
//! | [`DirectoryTreeEvent::Toggled`]         | [`on_toggled`]           |
//! | [`DirectoryTreeEvent::Selected`]        | [`on_selected`]          |
//! | [`DirectoryTreeEvent::Drag`]            | [`on_drag`]              |
//! | [`DirectoryTreeEvent::Loaded`]          | [`on_loaded`]            |
//! | [`DirectoryTreeEvent::DragCompleted`]   | inline no-op             |
//!
//! `DragCompleted` is a broadcast event: the widget's state machine in
//! [`on_drag`] has already cleared its state by the time the message
//! reaches the dispatcher, so the dispatcher's job is just to route it
//! back through the app's message plumbing.
//!
//! The returned [`iced::Task`] carries any follow-up the widget needs
//! to emit — a `Loaded` for an in-flight scan, a delayed `Selected`
//! or `DragCompleted` for a completed drag gesture, etc.

use iced::Task;

use super::DirectoryTree;
use super::message::DirectoryTreeEvent;

mod on_drag;
mod on_loaded;
mod on_selected;
mod on_toggled;

impl DirectoryTree {
    /// Feed an event into the widget.
    ///
    /// Returns an `iced::Task` the parent should `.map(..)` back into
    /// its own message type. For `Selected` this is always
    /// [`Task::none()`]; for `Toggled` on an unloaded folder it carries
    /// the pending async scan; for `Loaded` it is again
    /// [`Task::none()`].
    ///
    /// Parent apps typically route every tree-related message here
    /// unconditionally:
    ///
    /// ```ignore
    /// fn update(&mut self, msg: MyMessage) -> Task<MyMessage> {
    ///     match msg {
    ///         MyMessage::Tree(e) => self.tree.update(e).map(MyMessage::Tree),
    ///     }
    /// }
    /// ```
    pub fn update(&mut self, msg: DirectoryTreeEvent) -> Task<DirectoryTreeEvent> {
        match msg {
            DirectoryTreeEvent::Toggled(path) => self.on_toggled(path),
            DirectoryTreeEvent::Refresh(path) => {
                if !self.root.find_mut(&path).is_some_and(|node| node.is_dir) { return Task::none(); }
                self.prefetching_paths.remove(&path);
                self.generation = self.generation.wrapping_add(1);
                let depth = depth_of(&self.config.root_path, &path);
                crate::directory_tree::walker::scan(self.executor.clone(), path, self.generation, depth)
            }
            DirectoryTreeEvent::Selected(path, is_dir, mode) => {
                self.on_selected(path, is_dir, mode);
                Task::none()
            }
            DirectoryTreeEvent::Drag(msg) => self.on_drag(msg),
            DirectoryTreeEvent::DragCompleted { .. } => Task::none(),
            DirectoryTreeEvent::Loaded(payload) => {
                // v0.5: `on_loaded` returns the paths (possibly empty)
                // that the prefetch layer wants scanned next. The
                // dispatcher is the layer that knows about the
                // executor, so converting paths → scan Tasks happens
                // here, not inside the handler.
                let targets = self.on_loaded(payload);
                self.issue_prefetch_scans(targets)
            }
        }
    }

    /// Issue background scans for a batch of prefetch targets.
    ///
    /// Each target is tracked in `prefetching_paths` so that when the
    /// scan result arrives, [`on_loaded`](Self::on_loaded) knows to
    /// drain the flag rather than triggering another cascade of
    /// prefetches. Returns [`Task::none()`] if the input is empty —
    /// the common case when prefetch is disabled or the user is
    /// expanding a folder with no folder-children.
    fn issue_prefetch_scans(
        &mut self,
        targets: Vec<std::path::PathBuf>,
    ) -> Task<DirectoryTreeEvent> {
        if targets.is_empty() {
            return Task::none();
        }
        let tasks: Vec<Task<DirectoryTreeEvent>> = targets
            .into_iter()
            .map(|p| {
                // Each prefetch gets its own generation so a later
                // collapse-and-rescan of the same path invalidates
                // exactly this stale result.
                self.generation = self.generation.wrapping_add(1);
                self.prefetching_paths.insert(p.clone());
                let depth = depth_of(&self.config.root_path, &p);
                super::walker::scan(self.executor.clone(), p, self.generation, depth)
            })
            .collect();
        Task::batch(tasks)
    }
}

/// Compute the depth of `path` relative to `root`. Returns `0` if they
/// are equal, `1` for an immediate child, etc. If `path` does not
/// start with `root` (shouldn't happen in practice — every known node
/// descends from the root) we return `u32::MAX` so any depth limit
/// will trivially exclude it.
///
/// Shared between [`on_toggled`] (for the max-depth guard) and
/// [`DirectoryTree::__test_expand_blocking`] below. `pub(super)` so
/// submodules can import it via `use super::depth_of`.
pub(super) fn depth_of(root: &std::path::Path, path: &std::path::Path) -> u32 {
    let Ok(rel) = path.strip_prefix(root) else {
        return u32::MAX;
    };
    rel.components().count() as u32
}

impl DirectoryTree {
    /// Synchronously scan `path` and merge the result.
    ///
    /// **Test/helper API.** This duplicates the async `Toggled → scan →
    /// Loaded` round-trip but blocks on the scan, which is what
    /// integration tests need — iced's `Task` runtime machinery is
    /// private (see `iced_runtime::task::into_stream`), so driving a
    /// Task to completion from outside iced requires either standing
    /// up a window (overkill for unit-level tests) or bypassing the
    /// Task. This method does the latter.
    ///
    /// v0.5: if `config.prefetch_per_parent > 0`, this helper also
    /// drains the prefetch wave synchronously — the scans that the
    /// real dispatcher would have dispatched to the executor are
    /// instead run on this thread in sequence. That's slower than
    /// production (serial rather than parallel) but gives tests
    /// deterministic state to assert against without spinning up an
    /// iced runtime.
    ///
    /// Real applications should not call this on the main thread —
    /// `scan_dir` blocks on `readdir` — and should route events
    /// through [`DirectoryTree::update`] instead, which delegates the
    /// scan to a worker thread.
    #[doc(hidden)]
    pub fn __test_expand_blocking(&mut self, path: std::path::PathBuf) {
        // User-initiated leg: scan path, merge, flip is_expanded.
        self.__expand_blocking_impl(path.clone(), /* flip_expanded= */ true);

        // v0.5 prefetch leg: drain any prefetch targets synchronously.
        // We look at `select_prefetch_targets` *after* the merge, at
        // which point the children are populated and the set is
        // well-defined. Each target gets its `prefetching_paths` flag
        // set first (so when its `on_loaded` runs, it correctly
        // identifies itself as a prefetch result and won't cascade).
        let targets = self.select_prefetch_targets(&path);
        for t in targets {
            self.prefetching_paths.insert(t.clone());
            self.__expand_blocking_impl(t, /* flip_expanded= */ false);
        }
    }

    /// Shared core of [`__test_expand_blocking`]: scan, build a
    /// `LoadPayload`, optionally flip `is_expanded`, feed to
    /// `on_loaded`. Does **not** look at prefetch — that's one level
    /// up, in `__test_expand_blocking` proper.
    fn __expand_blocking_impl(&mut self, path: std::path::PathBuf, flip_expanded: bool) {
        use super::message::LoadPayload;
        use std::sync::Arc;

        let depth = depth_of(&self.config.root_path, &path);
        // Skip the walker::scan Task entirely: call the blocking
        // primitive directly and hand-assemble the Loaded payload.
        let result = swdir::scan_dir(&path)
            .as_ref()
            .map(|e| super::walker::normalize_entries(e))
            .map_err(crate::Error::from);

        // Make sure generation matches — bump first, then attach.
        self.generation = self.generation.wrapping_add(1);
        let payload = LoadPayload {
            path: path.clone(),
            generation: self.generation,
            depth,
            result: Arc::new(result),
        };

        if flip_expanded
            && let Some(node) = self.root.find_mut(&path)
            && node.is_dir
        {
            node.is_expanded = true;
        }
        // The return value (prefetch targets) is intentionally
        // discarded here: the caller — `__test_expand_blocking` —
        // computes targets itself from the post-merge tree state.
        let _targets = self.on_loaded(payload);
    }
}

#[cfg(test)]
mod tests;
