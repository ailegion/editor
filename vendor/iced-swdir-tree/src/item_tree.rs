//! `ItemTree<T>` — a keyboard-navigable, multi-selectable,
//! searchable tree widget for **in-memory, caller-supplied** node
//! data.
//!
//! # Quick start
//!
//! ```no_run
//! use iced_swdir_tree::{ItemTree, ItemNode, NodeId};
//!
//! let mut tree: ItemTree<String> = ItemTree::new();
//! tree.set_tree(ItemNode {
//!     id: NodeId(0),
//!     data: "Root".into(),
//!     children: vec![
//!         ItemNode { id: NodeId(1), data: "Chapter 1".into(), children: vec![] },
//!         ItemNode { id: NodeId(2), data: "Chapter 2".into(), children: vec![] },
//!     ],
//! });
//! ```
//!
//! # Differences from `DirectoryTree`
//!
//! | `DirectoryTree` | `ItemTree<T>` |
//! |---|---|
//! | Node identity: `PathBuf` | Node identity: `NodeId(u64)` |
//! | Data loaded asynchronously via `ScanExecutor` | Data always fully present |
//! | `Toggled` → scan → `Loaded` lifecycle | No loading step |
//! | `generation` counter guards stale scan results | No generation counter |
//! | Drag-and-drop | Opt-in (`with_drag_and_drop`), v0.9.0 (RFC 002) |
//!
//! Navigation, multi-select, search, and icon themes are identical.
use std::collections::HashSet;
use std::fmt::Display;
use std::sync::Arc;

use iced::keyboard::{Key, Modifiers};
use iced::widget::{Space, button, column, container, mouse_area, row, scrollable, text};
use iced::{Alignment, Background, Border, Color, Element, Length, Padding, Task, Theme};

pub(crate) mod drag;
pub(crate) mod node;
pub(crate) mod search;

#[cfg(test)]
mod tests;

use drag::{DropPosition, ItemDragMsg, ItemDragState};
use node::{
    ItemNode, ItemNodeState, NodeId, VisibleRow, build_parent_map, clear_selection,
    collect_all_ids, collect_all_ids_ordered, collect_search_visible, collect_visible, find,
    find_mut, parent_of, snapshot_state,
};
use search::{ItemSearchState, walk_for_search};

use crate::directory_tree::icon::{IconRole, IconTheme, default_theme, render as icon_render};
use crate::directory_tree::selection::SelectionMode;

// ----------------------------------------------------------------
// Event
// ----------------------------------------------------------------

/// Events emitted by [`ItemTree`].
///
/// Route all of these back through
/// [`ItemTree::update`] in your application's `update` function.
#[derive(Debug, Clone)]
pub enum ItemTreeEvent {
    /// The user clicked the caret on a branch node.
    Toggled(NodeId),
    /// The user selected a row.
    Selected(NodeId, SelectionMode),
    /// Internal drag-machinery event (RFC 002).
    ///
    /// Emitted by the built-in view when drag-and-drop is enabled
    /// via [`ItemTree::with_drag_and_drop`]. Applications treat these
    /// as opaque and always route them back to [`ItemTree::update`] —
    /// the state machine may produce a downstream
    /// [`Selected`](Self::Selected) (a click) or
    /// [`DragCompleted`](Self::DragCompleted) (a drop).
    Drag(ItemDragMsg),
    /// The user completed a drag gesture, intending to place
    /// `sources` at `position` relative to `target` (RFC 002).
    ///
    /// The widget mutates **nothing**. The application moves the
    /// `sources` in its own data model, rebuilds its
    /// [`ItemNode`] tree, and calls
    /// [`set_tree`](ItemTree::set_tree) (or
    /// [`set_tree_and_recompute_search`](ItemTree::set_tree_and_recompute_search)).
    /// Key-based diffing then preserves expansion/selection for every
    /// surviving id — including the moved nodes.
    ///
    /// Guarantees: `sources` is non-empty and in tree order; `target`
    /// is live; the `(target, position)` pair satisfied the validity
    /// rules (see [`ItemDragMsg`]).
    DragCompleted {
        /// Node ids being moved, in tree (pre-order) order.
        sources: Vec<NodeId>,
        /// The node the drop is positioned relative to.
        target: NodeId,
        /// Where the sources land relative to `target`.
        position: DropPosition,
    },
}

// ----------------------------------------------------------------
// Widget
// ----------------------------------------------------------------

/// A keyboard-navigable, multi-selectable, searchable tree widget
/// for in-memory, caller-supplied node data.
///
/// See the crate-level module documentation for a quick start and
/// a comparison with [`DirectoryTree`](crate::DirectoryTree).
#[derive(Debug)]
pub struct ItemTree<T> {
    root: Option<ItemNodeState<T>>,
    selected_ids: Vec<NodeId>,
    active_id: Option<NodeId>,
    anchor_id: Option<NodeId>,
    search: Option<ItemSearchState>,
    icon_theme: Arc<dyn IconTheme>,
    /// Whether drag-and-drop is enabled (RFC 002 [D2]). Off by
    /// default; the view renders no drag instrumentation when false.
    dnd_enabled: bool,
    /// In-flight drag state, or `None` when idle.
    drag: Option<ItemDragState>,
}

impl<T: Clone + std::fmt::Debug + Send + Sync + 'static> ItemTree<T> {
    // ---- construction ----

    /// Create an empty `ItemTree`. Call `set_tree`
    /// to populate it.
    pub fn new() -> Self {
        Self {
            root: None,
            selected_ids: Vec::new(),
            active_id: None,
            anchor_id: None,
            search: None,
            icon_theme: default_theme(),
            dnd_enabled: false,
            drag: None,
        }
    }

    /// Enable (or disable) drag-and-drop reorder/nest support
    /// (RFC 002).
    ///
    /// Off by default. When enabled, the built-in view switches to a
    /// press / enter / exit / release gesture model and renders
    /// inter-row drop zones; a completed drag emits
    /// [`ItemTreeEvent::DragCompleted`] for the application to act on.
    /// When disabled, rows behave exactly as in v0.8 (a press emits
    /// `Selected(_, Replace)` directly).
    ///
    /// ```
    /// use iced_swdir_tree::ItemTree;
    /// let tree: ItemTree<String> = ItemTree::new().with_drag_and_drop(true);
    /// ```
    pub fn with_drag_and_drop(mut self, enabled: bool) -> Self {
        self.dnd_enabled = enabled;
        if !enabled {
            self.drag = None;
        }
        self
    }

    /// Whether drag-and-drop is enabled on this tree.
    pub fn is_drag_and_drop_enabled(&self) -> bool {
        self.dnd_enabled
    }

    /// Whether a drag gesture is currently in progress.
    pub fn is_dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// The node ids being dragged, in tree order, or an empty slice
    /// when no drag is active.
    pub fn drag_sources(&self) -> &[NodeId] {
        self.drag.as_ref().map_or(&[], |d| d.sources.as_slice())
    }

    /// The current valid drop target — `(target, position)` — or
    /// `None` when no drag is active or the cursor is over an invalid
    /// zone.
    pub fn drop_target(&self) -> Option<(NodeId, DropPosition)> {
        self.drag.as_ref().and_then(|d| d.hover)
    }

    /// Replace the icon theme.
    ///
    /// The crate ships [`UnicodeTheme`](crate::UnicodeTheme) (always
    /// available) and [`LucideTheme`](crate::LucideTheme) (behind the
    /// `icons` feature). Pass any `Arc<dyn IconTheme>` here.
    /// Only [`IconRole::CaretRight`] and [`IconRole::CaretDown`] are
    /// used by `ItemTree`.
    pub fn with_icon_theme(mut self, theme: Arc<dyn IconTheme>) -> Self {
        self.icon_theme = theme;
        self
    }

    // ---- data model ----

    /// Replace the tree contents, diffing the new data against the
    /// current tree to preserve expansion and selection state for
    /// surviving [`NodeId`]s.
    ///
    /// Expansion and selection are **preserved** for any node whose
    /// `id` appears anywhere in the new tree (regardless of position
    /// changes). State is **dropped** for any id that disappears —
    /// selected ids that vanish are silently removed from the
    /// selection set.
    pub fn set_tree(&mut self, root: ItemNode<T>) {
        // If a drag is in progress its parent-map snapshot is built
        // from the pre-call tree; after set_tree the snapshot is stale
        // and any validity check against it would be incorrect.
        self.drag = None;

        // Snapshot current per-node state keyed by NodeId.
        let mut old_state = std::collections::HashMap::new();
        if let Some(existing) = &self.root {
            snapshot_state(existing, &mut old_state);
        }

        // Build the new internal state tree, transferring old state.
        self.root = Some(ItemNodeState::from_input(root, &old_state));

        // Drop selected/active/anchor ids that no longer exist.
        let mut live_ids = HashSet::new();
        if let Some(r) = &self.root {
            collect_all_ids(r, &mut live_ids);
        }
        self.selected_ids.retain(|id| live_ids.contains(id));
        if self.active_id.is_some_and(|id| !live_ids.contains(&id)) {
            self.active_id = None;
        }
        if self.anchor_id.is_some_and(|id| !live_ids.contains(&id)) {
            self.anchor_id = None;
        }

        // Re-sync view flags.
        self.sync_selection_flags();
        // Note: recompute_search_visibility requires T: Display.
        // Call set_tree_and_recompute_search (below) when T: Display
        // and search may be active.
    }

    fn sync_selection_flags(&mut self) {
        let Some(root) = self.root.as_mut() else {
            return;
        };
        clear_selection(root);
        let ids: Vec<NodeId> = self.selected_ids.clone();
        for id in ids {
            if let Some(node) = find_mut(root, id) {
                node.is_selected = true;
            }
        }
    }

    pub(crate) fn visible_rows(&self) -> Vec<VisibleRow<'_, T>> {
        let Some(root) = &self.root else {
            return Vec::new();
        };
        match &self.search {
            None => {
                let mut out = Vec::new();
                collect_visible(root, 0, &mut out);
                out
            }
            Some(state) => {
                let mut out = Vec::new();
                collect_search_visible(root, 0, &state.visible_ids, &mut out);
                out
            }
        }
    }

    /// Apply an [`ItemTreeEvent`], mutate the tree state, and return
    /// a `Task`.
    ///
    /// All arms return `Task::none()` except the drag click/drop
    /// branch, which emits a deferred `Selected` or a `DragCompleted`
    /// for the application to observe.
    pub fn update(&mut self, event: ItemTreeEvent) -> Task<ItemTreeEvent> {
        match event {
            ItemTreeEvent::Toggled(id) => {
                self.on_toggled(id);
                Task::none()
            }
            ItemTreeEvent::Selected(id, mode) => {
                self.on_selected(id, mode);
                Task::none()
            }
            ItemTreeEvent::Drag(msg) => self.on_drag(msg),
            // Broadcast event: the state machine has already cleared
            // its own state by the time this is routed back. The app
            // is responsible for the actual move.
            ItemTreeEvent::DragCompleted { .. } => Task::none(),
        }
    }

    /// Drive the drag state machine (RFC 002).
    ///
    /// * `Pressed(id)` — snapshot sources and the parent map, enter
    ///   the dragging state. Does **not** change selection.
    /// * `Entered(t, pos)` — set `(t, pos)` as the hover iff it is a
    ///   valid drop, else clear the hover.
    /// * `Exited(t, pos)` — clear the hover if it pointed at `(t, pos)`.
    /// * `Released(id, _)` — same node as the press → emit a deferred
    ///   `Selected(Replace)` (it was a click); else a valid hover →
    ///   `DragCompleted`; else quietly return to idle.
    /// * `Cancelled` — return to idle.
    ///
    /// All variants are no-ops when drag-and-drop is disabled or when
    /// no drag is in progress, so stray or out-of-order messages are
    /// harmless.
    fn on_drag(&mut self, msg: ItemDragMsg) -> Task<ItemTreeEvent> {
        if !self.dnd_enabled {
            return Task::none();
        }
        match msg {
            ItemDragMsg::Pressed(id) => {
                let sources = self.sources_for_drag(id);
                let parent = build_parent_map(self.root.as_ref());
                self.drag = Some(ItemDragState {
                    sources,
                    primary: id,
                    parent,
                    hover: None,
                });
                Task::none()
            }
            ItemDragMsg::Entered(target, position) => {
                if let Some(d) = self.drag.as_mut() {
                    if d.is_valid_drop(target, position) {
                        d.hover = Some((target, position));
                    } else {
                        d.hover = None;
                    }
                }
                Task::none()
            }
            ItemDragMsg::Exited(target, position) => {
                if let Some(d) = self.drag.as_mut()
                    && d.hover == Some((target, position))
                {
                    d.hover = None;
                }
                Task::none()
            }
            ItemDragMsg::Released(id, _position) => {
                let Some(d) = self.drag.take() else {
                    return Task::none();
                };
                // Case 1: released on the press row — a click, not a
                // drag. Emit the deferred selection.
                if id == d.primary {
                    return Task::done(ItemTreeEvent::Selected(d.primary, SelectionMode::Replace));
                }
                // Case 2: released over a valid hover — a drop.
                if let Some((target, position)) = d.hover {
                    return Task::done(ItemTreeEvent::DragCompleted {
                        sources: d.sources,
                        target,
                        position,
                    });
                }
                // Case 3: released over nothing valid — cancelled.
                // Selection is deliberately left untouched.
                Task::none()
            }
            ItemDragMsg::Cancelled => {
                self.drag = None;
                Task::none()
            }
        }
    }

    /// The source set for a drag beginning on `id`: the whole
    /// selection (in tree order) if `id` is selected, otherwise just
    /// `id`. Returns `[id]` if the tree is somehow empty.
    fn sources_for_drag(&self, id: NodeId) -> Vec<NodeId> {
        let set: HashSet<NodeId> = if self.selected_ids.contains(&id) {
            self.selected_ids.iter().copied().collect()
        } else {
            std::iter::once(id).collect()
        };
        let mut ordered = Vec::new();
        if let Some(root) = &self.root {
            collect_all_ids_ordered(root, &mut ordered);
        }
        let sources: Vec<NodeId> = ordered.into_iter().filter(|i| set.contains(i)).collect();
        if sources.is_empty() {
            vec![id]
        } else {
            sources
        }
    }

    fn on_toggled(&mut self, id: NodeId) {
        let Some(root) = self.root.as_mut() else {
            return;
        };
        let Some(node) = find_mut(root, id) else {
            return;
        };
        if !node.children.is_empty() {
            node.is_expanded = !node.is_expanded;
        }
    }

    fn on_selected(&mut self, id: NodeId, mode: SelectionMode) {
        let Some(root) = self.root.as_ref() else {
            return;
        };
        if find(root, id).is_none() {
            return;
        }
        self.active_id = Some(id);
        match mode {
            SelectionMode::Replace => {
                self.selected_ids = vec![id];
                self.anchor_id = Some(id);
            }
            SelectionMode::Toggle => {
                if let Some(pos) = self.selected_ids.iter().position(|&x| x == id) {
                    self.selected_ids.remove(pos);
                } else {
                    self.selected_ids.push(id);
                }
                self.anchor_id = Some(id);
            }
            SelectionMode::ExtendRange => {
                let rows = self.visible_rows();
                let anchor = self.anchor_id.unwrap_or(id);
                let ai = rows.iter().position(|r| r.node.id == anchor);
                let bi = rows.iter().position(|r| r.node.id == id);
                if let (Some(a), Some(b)) = (ai, bi) {
                    let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
                    self.selected_ids = rows[lo..=hi].iter().map(|r| r.node.id).collect();
                } else {
                    self.selected_ids = vec![id];
                    self.anchor_id = Some(id);
                }
            }
        }
        self.sync_selection_flags();
    }

    /// Translate a keyboard event into an `ItemTreeEvent`, or `None`.
    pub fn handle_key(&self, key: &Key, modifiers: Modifiers) -> Option<ItemTreeEvent> {
        use iced::keyboard::key::Named;
        // Escape cancels an in-flight drag (RFC 002 [D8]). Bound only
        // while a drag is active, so apps keep Escape otherwise.
        if matches!(key, Key::Named(Named::Escape)) {
            return self
                .drag
                .is_some()
                .then_some(ItemTreeEvent::Drag(ItemDragMsg::Cancelled));
        }
        let rows = self.visible_rows();
        if rows.is_empty() {
            return None;
        }
        let cur = self
            .active_id
            .and_then(|id| rows.iter().position(|r| r.node.id == id))
            .unwrap_or(0);
        let mode = if modifiers.shift() {
            SelectionMode::ExtendRange
        } else {
            SelectionMode::Replace
        };
        match key {
            Key::Named(Named::ArrowUp) => Some(ItemTreeEvent::Selected(
                rows[cur.saturating_sub(1)].node.id,
                mode,
            )),
            Key::Named(Named::ArrowDown) => Some(ItemTreeEvent::Selected(
                rows[(cur + 1).min(rows.len() - 1)].node.id,
                mode,
            )),
            Key::Named(Named::Home) => Some(ItemTreeEvent::Selected(rows[0].node.id, mode)),
            Key::Named(Named::End) => {
                Some(ItemTreeEvent::Selected(rows[rows.len() - 1].node.id, mode))
            }
            Key::Named(Named::Enter) => {
                let r = &rows[cur];
                (!r.node.children.is_empty()).then_some(ItemTreeEvent::Toggled(r.node.id))
            }
            Key::Named(Named::Space) => Some(ItemTreeEvent::Selected(
                rows[cur].node.id,
                SelectionMode::Toggle,
            )),
            Key::Named(Named::ArrowLeft) => {
                let r = &rows[cur];
                if r.node.is_expanded && !r.node.children.is_empty() {
                    Some(ItemTreeEvent::Toggled(r.node.id))
                } else {
                    self.root
                        .as_ref()
                        .and_then(|root| parent_of(root, r.node.id, None))
                        .map(|pid| ItemTreeEvent::Selected(pid, SelectionMode::Replace))
                }
            }
            Key::Named(Named::ArrowRight) => {
                let r = &rows[cur];
                if !r.node.children.is_empty() && !r.node.is_expanded {
                    Some(ItemTreeEvent::Toggled(r.node.id))
                } else if r.node.is_expanded && !r.node.children.is_empty() {
                    Some(ItemTreeEvent::Selected(
                        r.node.children[0].id,
                        SelectionMode::Replace,
                    ))
                } else {
                    None
                }
            }
            _ => None,
        }
    }
}

impl<T: Clone + std::fmt::Debug + std::fmt::Display + Send + Sync + 'static> ItemTree<T> {
    /// Like `set_tree` but also re-runs any active search query.
    /// Use this variant when `T: Display` and search may be active.
    ///
    /// This is the method to call in most real applications, since
    /// search requires `T: Display`. For clarity, `set_tree` is
    /// available on all `ItemTree<T>` (with no `Display` bound) but
    /// will leave a stale search cache if called while search is
    /// active.
    pub fn set_tree_and_recompute_search(&mut self, root: ItemNode<T>) {
        self.set_tree(root);
        self.recompute_search_visibility();
    }

    // ---- accessors ----

    /// The most-recently-touched node id (`active_id`).
    pub fn active_id(&self) -> Option<NodeId> {
        self.active_id
    }

    /// The Shift-range anchor id.
    pub fn anchor_id(&self) -> Option<NodeId> {
        self.anchor_id
    }

    /// All currently selected node ids.
    pub fn selected_ids(&self) -> &[NodeId] {
        &self.selected_ids
    }

    /// Whether `id` is in the selection set.
    pub fn is_selected(&self, id: NodeId) -> bool {
        self.selected_ids.contains(&id)
    }

    /// Whether a search query is active.
    pub fn is_searching(&self) -> bool {
        self.search.is_some()
    }

    /// The current search query as provided by the caller, or
    /// `None` when inactive.
    pub fn search_query(&self) -> Option<&str> {
        self.search.as_ref().map(|s| s.query.as_str())
    }

    /// Count of nodes that directly match the current search
    /// query (ancestors not counted). Returns `0` when inactive.
    pub fn search_match_count(&self) -> usize {
        self.search.as_ref().map_or(0, |s| s.match_count)
    }

    // ---- search ----

    /// Activate or update the incremental search query.
    ///
    /// The tree narrows its visible rows to basename-string matches
    /// plus every ancestor of every match, where "basename string"
    /// is `format!("{}", node.data).to_lowercase()`.
    ///
    /// An empty string clears the search (equivalent to
    /// [`clear_search`](Self::clear_search)).
    pub fn set_search_query(&mut self, query: impl Into<String>)
    where
        T: Display,
    {
        let q: String = query.into();
        if q.is_empty() {
            self.search = None;
            return;
        }
        self.search = Some(ItemSearchState::new(q));
        self.recompute_search_visibility();
    }

    /// Clear the active search query. No-op if already inactive.
    pub fn clear_search(&mut self) {
        self.search = None;
    }

    /// Recompute the search visibility cache. Called automatically
    /// by `set_tree`, `set_search_query`, and any operation that
    /// changes the node graph.
    fn recompute_search_visibility(&mut self)
    where
        T: Display,
    {
        let Some(state) = self.search.as_mut() else {
            return;
        };
        let Some(root) = &self.root else {
            state.visible_ids.clear();
            state.match_count = 0;
            return;
        };
        let mut visible = HashSet::new();
        let mut count = 0usize;
        walk_for_search(root, &state.query_lower.clone(), &mut visible, &mut count);
        state.visible_ids = visible;
        state.match_count = count;
    }

    // ---- view ----

    /// Produce the iced `Element` for this tree. Pass a mapper
    /// that wraps `ItemTreeEvent` into your application's
    /// `Message` type.
    ///
    /// ```ignore
    /// fn view(app: &App) -> Element<'_, Message> {
    ///     app.tree.view(Message::Tree)
    /// }
    /// ```
    pub fn view<'a, Message, F>(&'a self, on_event: F) -> Element<'a, Message>
    where
        Message: Clone + 'a,
        T: Display,
        F: Fn(ItemTreeEvent) -> Message + Copy + 'a,
    {
        let mut rows: Vec<Element<'a, Message>> = Vec::new();
        let theme = self.icon_theme.as_ref();
        // Snapshot the current drop hover so each row/strip can paint
        // its own indicator if it matches.
        let hover = self.drag.as_ref().and_then(|d| d.hover);

        if let Some(root) = &self.root {
            render_node(
                root,
                0,
                &self.search,
                theme,
                self.dnd_enabled,
                hover,
                on_event,
                &mut rows,
            );
        }

        // With drop strips present, the strips themselves separate the
        // rows; drop the inter-row column spacing so they hug.
        let spacing = if self.dnd_enabled { 0 } else { 2 };
        let list = column(rows).spacing(spacing).padding(4).width(Length::Fill);
        scrollable(list)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }
}

impl<T: Clone + std::fmt::Debug + Send + Sync + 'static> Default for ItemTree<T> {
    fn default() -> Self {
        Self::new()
    }
}

// ---- view helpers ----

/// Per-indent-level horizontal step, in logical pixels.
const INDENT_STEP: u16 = 16;
/// Height of a between-rows drop strip, in logical pixels.
const STRIP_H: f32 = 6.0;

#[allow(clippy::too_many_arguments)]
fn render_node<'a, T, Message, F>(
    node: &'a ItemNodeState<T>,
    depth: u32,
    search: &Option<ItemSearchState>,
    theme: &dyn IconTheme,
    dnd_enabled: bool,
    hover: Option<(NodeId, DropPosition)>,
    on_event: F,
    out: &mut Vec<Element<'a, Message>>,
) where
    T: Clone + std::fmt::Debug + std::fmt::Display + Send + Sync + 'static,
    Message: Clone + 'a,
    F: Fn(ItemTreeEvent) -> Message + Copy + 'a,
{
    // Search filter: skip nodes not in the visible set.
    if search
        .as_ref()
        .is_some_and(|s| !s.visible_ids.contains(&node.id))
    {
        return;
    }

    // Drop zones bracket each row when drag-and-drop is enabled
    // (RFC 002 [D3]): a "before" strip, the row body (which is the
    // "into" zone), and an "after" strip.
    if dnd_enabled {
        out.push(drop_strip(node.id, DropPosition::Before, hover, on_event));
    }
    out.push(render_row(node, depth, theme, dnd_enabled, hover, on_event));
    if dnd_enabled {
        out.push(drop_strip(node.id, DropPosition::After, hover, on_event));
    }

    // Descend: during search always descend; otherwise respect is_expanded.
    let descend = if search.is_some() {
        true
    } else {
        node.is_expanded
    };
    if descend {
        for child in &node.children {
            render_node(
                child,
                depth + 1,
                search,
                theme,
                dnd_enabled,
                hover,
                on_event,
                out,
            );
        }
    }
}

/// A thin between-rows drop zone for `Before` / `After` placement.
/// Paints an insertion bar when it is the active hover target.
fn drop_strip<'a, Message, F>(
    id: NodeId,
    position: DropPosition,
    hover: Option<(NodeId, DropPosition)>,
    on_event: F,
) -> Element<'a, Message>
where
    Message: Clone + 'a,
    F: Fn(ItemTreeEvent) -> Message + Copy + 'a,
{
    let active = hover == Some((id, position));
    let bar = container(
        Space::new()
            .width(Length::Fill)
            .height(Length::Fixed(STRIP_H)),
    )
    .width(Length::Fill)
    .style(move |theme: &Theme| {
        if active {
            container::Style {
                background: Some(Background::Color(
                    theme.extended_palette().primary.base.color,
                )),
                border: Border {
                    radius: 2.0.into(),
                    ..Default::default()
                },
                ..Default::default()
            }
        } else {
            container::Style::default()
        }
    });
    mouse_area(bar)
        .on_enter(on_event(ItemTreeEvent::Drag(ItemDragMsg::Entered(
            id, position,
        ))))
        .on_exit(on_event(ItemTreeEvent::Drag(ItemDragMsg::Exited(
            id, position,
        ))))
        .on_release(on_event(ItemTreeEvent::Drag(ItemDragMsg::Released(
            id, position,
        ))))
        .into()
}

fn render_row<'a, T, Message, F>(
    node: &'a ItemNodeState<T>,
    depth: u32,
    theme: &dyn IconTheme,
    dnd_enabled: bool,
    hover: Option<(NodeId, DropPosition)>,
    on_event: F,
) -> Element<'a, Message>
where
    T: Clone + std::fmt::Debug + std::fmt::Display + Send + Sync + 'static,
    Message: Clone + 'a,
    F: Fn(ItemTreeEvent) -> Message + Copy + 'a,
{
    let indent = depth as u16 * INDENT_STEP;
    let id = node.id;

    // Caret (only for branch nodes). Emits Toggled in both modes.
    let caret: Element<'a, Message> = if !node.children.is_empty() {
        let role = if node.is_expanded {
            IconRole::CaretDown
        } else {
            IconRole::CaretRight
        };
        button(icon_render::<Message>(theme, role))
            .on_press(on_event(ItemTreeEvent::Toggled(id)))
            .padding(2)
            .style(iced::widget::button::text)
            .into()
    } else {
        // Invisible spacer so labels align with branch nodes.
        text(" ").size(14).into()
    };

    let label = text(format!("{}", node.data)).size(14);
    let content = row![caret, label].spacing(4).align_y(Alignment::Center);

    let left_pad = (indent + 4) as f32;
    let inner = container(content).padding(Padding {
        top: 2.0,
        right: 4.0,
        bottom: 2.0,
        left: left_pad,
    });

    let is_selected = node.is_selected;
    let into_active = dnd_enabled && hover == Some((id, DropPosition::Into));

    let body = container(inner)
        .width(Length::Fill)
        .style(move |theme: &Theme| {
            let palette = theme.extended_palette();
            if is_selected {
                container::Style {
                    background: Some(Background::Color(Color::from_rgb(0.2, 0.5, 0.8))),
                    ..Default::default()
                }
            } else if into_active {
                // Nest-here highlight: soft success fill + outline, the
                // same affordance DirectoryTree uses for a drop target.
                container::Style {
                    background: Some(Background::Color(palette.success.weak.color)),
                    text_color: Some(palette.success.weak.text),
                    border: Border {
                        color: palette.success.strong.color,
                        width: 1.5,
                        radius: 3.0.into(),
                    },
                    ..Default::default()
                }
            } else {
                container::Style::default()
            }
        });

    if dnd_enabled {
        // Deferred selection: press emits Drag(Pressed), not Selected.
        // The body doubles as the "into" drop zone.
        mouse_area(body)
            .on_press(on_event(ItemTreeEvent::Drag(ItemDragMsg::Pressed(id))))
            .on_enter(on_event(ItemTreeEvent::Drag(ItemDragMsg::Entered(
                id,
                DropPosition::Into,
            ))))
            .on_exit(on_event(ItemTreeEvent::Drag(ItemDragMsg::Exited(
                id,
                DropPosition::Into,
            ))))
            .on_release(on_event(ItemTreeEvent::Drag(ItemDragMsg::Released(
                id,
                DropPosition::Into,
            ))))
            .into()
    } else {
        // v0.8 behaviour: a press selects immediately.
        mouse_area(body)
            .on_press(on_event(ItemTreeEvent::Selected(
                id,
                SelectionMode::Replace,
            )))
            .into()
    }
}
