//! Render a [`DirectoryTree`] as an `iced::Element`.
//!
//! The layout is a vertical scrollable column of rows; each row is a
//! horizontal strip of indentation, caret, icon, and a button that
//! emits the row's click event. The view delegates icon selection to
//! the [`icon`](super::icon) module so the `icons` feature toggle never
//! leaks into view logic.
//!
//! ## Virtualization
//!
//! Only nodes in collapsed ancestors are skipped (the column shrinks
//! when they're closed). For very large loaded trees, iced's
//! `Scrollable` clips off-screen rows at render time — see
//! `iced::widget::scrollable` — so the cost of keeping them in the
//! element tree is limited to the layout pass. This is the best we
//! can do in iced 0.14 without a custom low-level widget, and it
//! matches the spec's "avoid rendering nodes outside the visible area
//! whenever possible" language.

use std::path::Path;

use iced::{
    Alignment, Background, Border, Element, Length, Theme,
    widget::{Space, button, column, container, mouse_area, row, scrollable, text},
};

use super::DirectoryTree;
use super::drag::DragMsg;
use super::icon::{IconRole, IconTheme, render as icon_render};
use super::message::DirectoryTreeEvent;
use super::node::TreeNode;

/// Per-indent-level horizontal padding in logical pixels.
const INDENT_STEP: f32 = 16.0;
/// Horizontal gap between the caret, the icon, and the label, in
/// logical pixels. iced 0.14's `.spacing()` takes `impl Into<Pixels>`;
/// `f32` implements that conversion.
const INTRA_ROW_GAP: f32 = 6.0;

impl DirectoryTree {
    /// Build an `iced::Element` that renders this tree.
    ///
    /// `on_event` is the closure that maps the widget's internal
    /// [`DirectoryTreeEvent`]s into the parent application's own
    /// message type. See the crate-level docs for a worked example.
    pub fn view<'a, Message, F>(&'a self, on_event: F) -> Element<'a, Message>
    where
        Message: Clone + 'a,
        F: Fn(DirectoryTreeEvent) -> Message + Copy + 'a,
    {
        self.view_with_entry(on_event, None)
    }

    /// Render an inline editor beneath a directory, or in place of an existing row.
    pub fn view_with_entry<'a, Message, F>(
        &'a self, on_event: F, mut entry: Option<(&'a Path, bool, Element<'a, Message>)>,
    ) -> Element<'a, Message>
    where Message: Clone + 'a, F: Fn(DirectoryTreeEvent) -> Message + Copy + 'a,
    {
        // Recurse over the tree and collect rows into a single column
        // inside a scrollable. `column` accepts an iterator, but we
        // build a Vec explicitly because the recursion depth can
        // exceed what inference wants to handle for a chained chain.
        let mut rows: Vec<Element<'a, Message>> = Vec::new();
        // Snapshot the current drop target so each row can paint its
        // own highlight if it matches.
        let drop_target = self.drop_target();
        // v0.6: if a search is active, hand render_node the set of
        // visible paths so it can bypass `is_expanded` — a collapsed
        // ancestor of a match should render as if expanded.
        let search_visible = self.search.as_ref().map(|s| &s.visible_paths);
        // v0.7: the icon theme is stored on the tree; hand it
        // through as `&dyn` so render_node / render_row can query
        // it without needing to thread feature flags.
        let icon_theme: &dyn IconTheme = self.icon_theme.as_ref();
        render_node(
            &self.root,
            0,
            drop_target,
            search_visible,
            icon_theme,
            on_event,
            &mut rows,
            &mut entry,
        );

        let list = column(rows).spacing(2).padding(4).width(Length::Fill);

        scrollable(list)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }
}

/// Render a single node and its descendants (if expanded) into `out`.
///
/// When `search_visible` is `Some`, search is active: only paths
/// in that set are rendered, and descent into children happens
/// regardless of `is_expanded`. When `search_visible` is `None`,
/// the normal `is_expanded && is_loaded` descent rule applies.
fn render_node<'a, Message, F>(
    node: &'a TreeNode,
    depth: u32,
    drop_target: Option<&Path>,
    search_visible: Option<&std::collections::HashSet<std::path::PathBuf>>,
    icon_theme: &dyn IconTheme,
    on_event: F,
    out: &mut Vec<Element<'a, Message>>,
    entry: &mut Option<(&'a Path, bool, Element<'a, Message>)>,
) where
    Message: Clone + 'a,
    F: Fn(DirectoryTreeEvent) -> Message + Copy + 'a,
{
    // v0.6 search: skip nodes outside the visible set entirely.
    if let Some(visible) = search_visible
        && !visible.contains(&node.path)
    {
        return;
    }
    let is_drop_target = drop_target == Some(node.path.as_path());
    let is_entry_parent = entry.as_ref().is_some_and(|(path, replace, _)| *path == node.path && !replace);
    let replace = entry.as_ref().is_some_and(|(path, replace, _)| *path == node.path && *replace);
    if replace {
        let (_, _, editor) = entry.take().unwrap();
        out.push(row![Space::new().width(depth as f32 * INDENT_STEP + 26.0), editor].into());
    } else {
        out.push(render_row(node, depth, is_drop_target, icon_theme, on_event));
    }
    if is_entry_parent {
        let (_, _, editor) = entry.take().unwrap();
        out.push(row![Space::new().width((depth + 1) as f32 * INDENT_STEP + 26.0), editor].into());
    }

    // Descent rule:
    //   - Search active: always descend (children are gated by the
    //     `visible` check above, so we correctly skip non-match
    //     siblings while still reaching deeper matches).
    //   - Search inactive: normal is_expanded && is_loaded rule.
    let descend = match search_visible {
        Some(_) => node.is_dir,
        None => node.is_dir && node.is_expanded && node.is_loaded,
    };
    if descend {
        for child in &node.children {
            render_node(
                child,
                depth + 1,
                drop_target,
                search_visible,
                icon_theme,
                on_event,
                out,
                entry,
            );
        }
    }
}

/// Render a single row of the tree.
fn render_row<'a, Message, F>(
    node: &'a TreeNode,
    depth: u32,
    is_drop_target: bool,
    icon_theme: &dyn IconTheme,
    on_event: F,
) -> Element<'a, Message>
where
    Message: Clone + 'a,
    F: Fn(DirectoryTreeEvent) -> Message + Copy + 'a,
{
    // Visible label: the entry's file name, with a fallback to the
    // full path for the root (whose file_name() may be None, e.g.
    // `/` on Unix or `C:\` on Windows).
    let label_str: String = match node.path.file_name() {
        Some(n) => n.to_string_lossy().into_owned(),
        None => node.path.display().to_string(),
    };

    // The folder/file icon.
    let type_icon: Element<'a, Message> = if node.error.is_some() {
        icon_render::<Message>(icon_theme, IconRole::Error)
    } else if node.is_dir {
        if node.is_expanded {
            icon_render::<Message>(icon_theme, IconRole::FolderOpen)
        } else {
            icon_render::<Message>(icon_theme, IconRole::FolderClosed)
        }
    } else {
        super::icon::render_file::<Message>(icon_theme, &node.path)
    };

    // The label itself. Permission-denied rows render in a muted
    // foreground so the user sees at a glance that the node is
    // unreadable rather than merely empty. iced 0.14 doesn't expose
    // a single "dimmed" helper, so we set a literal mid-grey that
    // works acceptably on both light and dark themes.
    let label_widget = {
        let t = text(label_str).size(14);
        if node.error.is_some() {
            t.color(iced::Color::from_rgb(0.55, 0.55, 0.55))
        } else {
            t
        }
    };

    // --- Caret (the fold/unfold affordance) ----------------------
    //
    // We split the row into two click targets *side by side* rather
    // than nesting a caret button inside a selection button: iced's
    // button-inside-button hit-testing is undefined and can swallow
    // the inner press. The caret handles Toggled; the rest of the
    // row (icon + label inside a second button) handles Selected.
    let caret: Element<'a, Message> = if node.is_dir {
        let caret_role = if node.is_expanded {
            IconRole::CaretDown
        } else {
            IconRole::CaretRight
        };
        let path = node.path.clone();
        button(icon_render::<Message>(icon_theme, caret_role))
            .padding(2)
            .style(button::text)
            .on_press(on_event(DirectoryTreeEvent::Toggled(path)))
            .into()
    } else {
        // Files: fixed-size placeholder so the icon column aligns
        // with the directory rows above and below.
        Space::new()
            .width(Length::Fixed(20.0))
            .height(Length::Fixed(20.0))
            .into()
    };

    // --- Selection body (icon + label) ---------------------------
    let selection_body = row![
        type_icon,
        Space::new().width(Length::Fixed(4.0)),
        label_widget,
    ]
    .spacing(INTRA_ROW_GAP)
    .align_y(Alignment::Center);

    // --- Row hitbox (selection + drag-and-drop) ------------------
    //
    // v0.4: we used to wrap `selection_body` in a `button` whose
    // `on_press` emitted `Selected(..., Replace)` directly. That
    // worked for single-click selection but made drag-and-drop
    // impossible, for two reasons:
    //
    //   1. iced 0.14's `button::on_press` fires on mouse-*up*, not
    //      mouse-*down*, so we can't detect the start of a drag
    //      gesture from the button alone.
    //   2. Even if we could, a mouse-down that immediately fires
    //      `Selected(..., Replace)` would collapse any existing
    //      multi-selection down to the pressed row before the drag
    //      state machine had a chance to snapshot the current set
    //      of sources — breaking multi-item drag.
    //
    // The fix is twofold. First, wrap the body in a `mouse_area`,
    // whose four event handlers (press / release / enter / exit)
    // are what the drag state machine in `update::on_drag` needs.
    // Second, defer selection: mouse-down emits `Drag(Pressed)`
    // (not `Selected`), and `on_drag` emits a delayed
    // `Selected(..., Replace)` only if the user releases on the
    // same row — i.e., it was a click, not a drag. See
    // `drag.rs` for the full state machine.
    //
    // Visual style is now provided by a styled `container` wrapper
    // rather than by `button`. We replicate the two states `button`
    // previously gave us — normal and primary (selected) — and add
    // a third for the current drop target during an in-flight
    // drag.
    let is_selected = node.is_selected;
    let path = node.path.clone();
    let is_dir = node.is_dir;
    // Clone once per handler to satisfy Fn borrow semantics.
    let path_for_press = path.clone();
    let path_for_enter = path.clone();
    let path_for_exit = path.clone();
    let path_for_release = path;

    let styled_body = container(selection_body)
        .width(Length::Fill)
        .padding(2)
        .style(move |theme: &Theme| {
            let palette = theme.extended_palette();
            if is_selected {
                container::Style {
                    background: Some(Background::Color(palette.primary.base.color)),
                    text_color: Some(palette.primary.base.text),
                    border: Border {
                        radius: 3.0.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }
            } else if is_drop_target {
                // Drop-target highlight: soft success-coloured
                // fill plus a 1.5-px outline, so even users with
                // weak colour vision can see where the drop will
                // land. Using the theme's `success` palette rather
                // than a hard-coded green keeps dark themes
                // readable.
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

    let select_area = mouse_area(styled_body)
        .on_press(on_event(DirectoryTreeEvent::Drag(DragMsg::Pressed(
            path_for_press,
            is_dir,
        ))))
        .on_enter(on_event(DirectoryTreeEvent::Drag(DragMsg::Entered(
            path_for_enter,
        ))))
        .on_exit(on_event(DirectoryTreeEvent::Drag(DragMsg::Exited(
            path_for_exit,
        ))))
        .on_release(on_event(DirectoryTreeEvent::Drag(DragMsg::Released(
            path_for_release,
        ))));

    // Left indent. Using a Space rather than padding so the selection
    // highlight runs the full visible row width — padding would
    // shrink the highlight by the indent amount.
    let indent_px = INDENT_STEP * depth as f32;
    let indent = Space::new().width(Length::Fixed(indent_px));

    container(
        row![indent, caret, select_area]
            .spacing(INTRA_ROW_GAP)
            .align_y(Alignment::Center),
    )
    .width(Length::Fill)
    .into()
}

/// (Kept for future debugging.) Format a path for display in a row's
/// tooltip.
#[allow(dead_code)]
fn display_path(path: &Path) -> String {
    path.display().to_string()
}
