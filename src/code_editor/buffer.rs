use cosmic_text::{
    Affinity, Attrs, AttrsList, Buffer as CosmicBuffer, BufferLine, Change, Cursor, Edit, Editor, FontSystem,
    LayoutGlyph, LineEnding, LineIter, Metrics, Selection, Shaping, Wrap,
};

use super::brackets;
use super::highlight::{self, Highlighter, LineState};
use super::rows::{Row, RowMap};
use syntect::highlighting::{Highlighter as ThemeHighlighter, Theme};

/// Parser state is kept entering every this-many-th line, so an edit resumes highlighting
/// from the snapshot before it and usually stops at the first snapshot after it, once the
/// state there matches what it was: about twice this many lines, whatever the file's size.
const SNAPSHOT_EVERY: usize = 32;

/// What the last `highlight` left behind, so the next one can redo only the lines an edit
/// affected: the lines as it coloured them, and the parser state entering some of them.
struct HighlightCache {
    /// Syntax name and theme the colours are for; anything else means a full pass.
    syntax: String,
    theme: Theme,
    lines: Vec<String>,
    /// `(line, state entering that line)`, ascending by line, always starting at line 0.
    snapshots: Vec<(usize, LineState)>,
    /// Lines the last `highlight` recomputed, for tests and measurements.
    recomputed: usize,
}

/// One line's attributes from its highlighted pieces: spans only where the attributes differ
/// from the defaults, and none for empty pieces.
fn attrs_list_for(pieces: &[(syntect::highlighting::Style, &str)]) -> AttrsList {
    let mut list = AttrsList::new(&Attrs::new());
    let mut end = 0;
    for (style, piece) in pieces {
        let start = end;
        end += piece.len();
        let attrs = highlight::attrs_for(*style);
        if start < end && attrs != list.defaults() {
            list.add_span(start..end, &attrs);
        }
    }
    list
}

/// The laid-out glyphs of `line`: the engine keeps each line as one unwrapped row.
fn glyphs_of(inner: &CosmicBuffer, line: usize) -> &[LayoutGlyph] {
    inner.lines.get(line)
        .and_then(|line| line.layout_opt())
        .and_then(|layout| layout.first())
        .map_or(&[], |layout| layout.glyphs.as_slice())
}

/// Wraps a `cosmic_text::Buffer`, tracking cursor/selection state alongside it.
///
/// Editing goes through a transient `cosmic_text::Editor` (created on demand and dropped
/// immediately after) rather than storing one long-term, since `Editor<'buffer>` borrows the
/// buffer it wraps and a self-referential struct would be needed to hold both.
pub struct Buffer {
    pub inner: CosmicBuffer,
    pub font_system: FontSystem,
    pub cursor: Cursor,
    pub selection: Selection,
    /// Which part of which line every screen row shows; see `rows.rs`. All positions below
    /// are in its terms: a row index, and x relative to where text starts on screen.
    rows: RowMap,
    /// Something `rows` is built from changed -- the text or its styling, the folds, the wrap
    /// setting or width, the metrics -- so the next `sync` rebuilds it. Moving the cursor or
    /// the selection changes none of those.
    rows_stale: bool,
    /// Row and x of the caret; `None` when its line is folded away.
    caret: Option<(usize, f32)>,
    /// `(row, x_start, x_end)` selection highlight rects, one per row the selection touches.
    selection_pixels: Vec<(usize, f32, f32)>,
    /// `(row, x_start, x_end)` rects for the bracket at the cursor and its match, if any
    /// (0 or 2 entries -- same shape as `selection_pixels` so `render.rs` can treat them
    /// alike, just with a different style).
    matched_brackets: Vec<(usize, f32, f32)>,
    /// Word wrap: lines wider than `wrap_width` continue on further rows.
    wrap: bool,
    /// Room for text in the editor widget and the widget's height, as it last reported them.
    wrap_width: f32,
    view_height: f32,
    /// Bumped whenever rows change shape for a reason other than an edit (wrap, width, zoom),
    /// so the widget can keep the same text at the top of the view.
    layout_epoch: u64,
    /// The x that moving up or down aims for, kept while only such moves happen so that
    /// passing through a shorter row doesn't lose the column.
    goal_x: Option<f32>,
    /// Tells this buffer from every other, for the widget's state, which outlives tab switches.
    id: u64,
    /// Width in pixels of the widest laid-out line, so the widget can bound horizontal scroll.
    content_width: f32,
    undo_stack: Vec<Change>,
    redo_stack: Vec<Change>,
    pub folds: std::collections::BTreeMap<usize, usize>,
    pub collapsed: std::collections::BTreeSet<usize>,
    folding_text: String,
    /// Vertical and horizontal scroll as last drawn (`f32` bits), copied from the editor
    /// widget so the app can place popups at the cursor. Atomics keep `Buffer` `Sync`.
    view_scroll: [std::sync::atomic::AtomicU32; 2],
    highlight_cache: Option<HighlightCache>,
}

/// A `FontSystem` as `FontSystem::new` builds it, except that the system fonts are scanned once
/// per process and each buffer starts from a copy of that database.
fn font_system() -> FontSystem {
    static FONTS: std::sync::OnceLock<(String, cosmic_text::fontdb::Database)> = std::sync::OnceLock::new();
    let (locale, db) = FONTS.get_or_init(|| FontSystem::new().into_locale_and_db());
    FontSystem::new_with_locale_and_db(locale.clone(), db.clone())
}

impl Buffer {
    pub fn new(text: &str, metrics: Metrics) -> Self {
        let mut buffer = Self::unshaped(text, metrics);
        buffer.sync();
        buffer
    }

    /// `new` followed by `highlight`, shaping the text once, coloured, rather than once plain
    /// and then again coloured.
    pub fn highlighted(
        text: &str, metrics: Metrics, highlighter: &Highlighter, extension: &str, theme: &syntect::highlighting::Theme,
    ) -> Self {
        let mut buffer = Self::unshaped(text, metrics);
        buffer.highlight(highlighter, extension, theme);
        buffer
    }

    /// The lines of `text` exactly as `cosmic_text::Buffer::set_text` builds them, but not yet
    /// shaped: the engine is given no size (see below), so `set_text` would shape every line
    /// of the file right here. The first `sync` or `highlight` shapes them instead.
    fn unshaped(text: &str, metrics: Metrics) -> Self {
        let mut font_system = font_system();
        let mut inner = CosmicBuffer::new(&mut font_system, metrics);
        // The engine never wraps and is given no size: it lays every line out as one long
        // row, which `rows` slices into screen rows (see `rows.rs`).
        inner.set_wrap(&mut font_system, Wrap::None);
        inner.lines.clear();
        for (range, ending) in LineIter::new(text) {
            inner.lines.push(BufferLine::new(&text[range], ending, AttrsList::new(&Attrs::new()), Shaping::Advanced));
        }
        // Ensure there is an ending line with no line ending, as `set_text` does.
        if inner.lines.last().map(|line| line.ending()).unwrap_or_default() != LineEnding::None {
            inner.lines.push(BufferLine::new("", LineEnding::None, AttrsList::new(&Attrs::new()), Shaping::Advanced));
        }
        Self {
            inner,
            font_system,
            cursor: Cursor::default(),
            selection: Selection::None,
            rows: RowMap::default(),
            rows_stale: true,
            caret: None,
            selection_pixels: Vec::new(),
            matched_brackets: Vec::new(),
            wrap: false,
            wrap_width: 0.0,
            view_height: 0.0,
            layout_epoch: 0,
            goal_x: None,
            id: {
                static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            },
            content_width: 0.0,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            folds: Default::default(), collapsed: Default::default(),
            folding_text: String::new(),
            view_scroll: Default::default(),
            highlight_cache: None,
        }
    }

    /// Number of screen rows.
    pub fn visible_count(&self) -> usize { self.rows.len() }

    /// The first screen row of `line`; `None` when it is folded away.
    pub fn visual_row(&self, line: usize) -> Option<usize> {
        self.rows.first_row(line)
    }

    /// The line shown on screen row `row` (clamped to the last row).
    pub fn source_line(&self, row: usize) -> usize {
        self.rows.line_at(row)
    }

    /// The screen rows `line` occupies: one, several when it wraps, none when folded away.
    pub fn rows_of(&self, line: usize) -> std::ops::Range<usize> {
        self.rows.of_line(line)
    }

    pub fn row(&self, row: usize) -> Option<&Row> {
        self.rows.get(row)
    }

    /// The laid-out glyphs of `line`, which a [`Row`] of that line indexes into.
    pub fn glyphs(&self, line: usize) -> &[LayoutGlyph] {
        glyphs_of(&self.inner, line)
    }

    /// Screen row and x (relative to the text origin) of the caret; `None` when its line is
    /// folded away.
    pub fn caret(&self) -> Option<(usize, f32)> {
        self.caret
    }

    /// Screen row and x of byte `index` on `line`; `None` when the line is folded away.
    pub fn locate(&self, line: usize, index: usize) -> Option<(usize, f32)> {
        let row = self.rows.locate(line, index, false)?;
        Some((row, self.rows.get(row)?.x_of(self.glyphs(line), index)))
    }

    /// The text position at `x` on screen row `row` (clamped to the last row).
    pub fn hit(&self, row: usize, x: f32) -> Cursor {
        let Some(row) = self.rows.get(row.min(self.rows.len().saturating_sub(1))) else { return self.cursor };
        let text = self.inner.lines.get(row.line).map_or("", |line| line.text());
        let (index, at_end) = row.hit(self.glyphs(row.line), text, x);
        // At a wrap point the affinity says which of the two rows the caret shows on.
        Cursor::new_with_affinity(row.line, index, if at_end { Affinity::Before } else { Affinity::After })
    }

    /// `(line, byte index)` under `x` on screen row `row`, when that is over text rather
    /// than the space beside it or below the last row.
    pub fn hit_text(&self, row: usize, x: f32) -> Option<(usize, usize)> {
        let row = self.rows.get(row)?;
        let text = self.inner.lines.get(row.line)?.text();
        let index = row.hit_glyph(self.glyphs(row.line), text, x)?;
        (index < text.len()).then_some((row.line, index))
    }

    pub fn wrapped(&self) -> bool { self.wrap }

    /// Turns word wrap on or off.
    pub fn set_wrap(&mut self, wrap: bool) {
        if self.wrap != wrap {
            self.wrap = wrap;
            self.layout_epoch += 1;
            self.rows_stale = true;
            self.sync();
        }
    }

    /// Records the room the editor widget has for text and its height; with word wrap on,
    /// lines are re-wrapped to a new width.
    pub fn set_viewport(&mut self, width: f32, height: f32) {
        let rewrap = self.wrap && width != self.wrap_width;
        self.wrap_width = width;
        self.view_height = height;
        if rewrap {
            self.layout_epoch += 1;
            self.rows_stale = true;
            self.sync();
        }
    }

    /// `(text width, height)` as last given to `set_viewport`.
    pub fn viewport(&self) -> (f32, f32) {
        (self.wrap_width, self.view_height)
    }

    /// See the `layout_epoch` field.
    pub fn layout_epoch(&self) -> u64 { self.layout_epoch }

    /// See the `id` field.
    pub fn id(&self) -> u64 { self.id }

    pub fn toggle_fold(&mut self, line: usize) {
        if !self.folds.contains_key(&line) { return; }
        if !self.collapsed.remove(&line) {
            // Never leave an insertion point or a selection hidden inside a fold.
            self.goto_line(line);
            self.collapsed.insert(line);
        }
        self.rows_stale = true;
        self.sync();
    }

    /// Applies a new font size/line height (e.g. from a zoom change), re-shaping and
    /// re-syncing cached cursor/selection pixel positions to match.
    pub fn set_metrics(&mut self, metrics: Metrics) {
        self.inner.set_metrics(&mut self.font_system, metrics);
        self.layout_epoch += 1;
        self.rows_stale = true;
        self.sync();
    }

    pub fn text(&self) -> String {
        self.inner
            .lines
            .iter()
            .map(|line| line.text())
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn line_count(&self) -> usize {
        self.inner.lines.len()
    }

    /// 1-indexed (line, column) of the cursor, for the status bar. Column is a character
    /// count (not a byte offset), so it stays correct on lines with multi-byte characters.
    pub fn cursor_line_col(&self) -> (usize, usize) {
        let col = self
            .inner
            .lines
            .get(self.cursor.line)
            .map(|line| {
                let text = line.text();
                let index = self.cursor.index.min(text.len());
                text[..index].chars().count()
            })
            .unwrap_or(0);
        (self.cursor.line + 1, col + 1)
    }

    /// Selects the entire document, cursor ending at the very end.
    pub fn select_all(&mut self) {
        self.perform(cosmic_text::Action::Motion(cosmic_text::Motion::BufferEnd));
        self.selection = Selection::Normal(Cursor::new(0, 0));
        self.sync();
    }

    /// Moves the cursor to `line` (0-indexed), first collapsing any active selection.
    /// `cosmic_text::Action::Motion` never touches the selection on its own (see
    /// `code_editor::input::handle_key`'s doc comment on that) -- a plain jump like this
    /// (used by Go to Line and Find in Project's jump-to-result) should discard whatever was
    /// selected before it, not extend it from the old anchor to the new line, the way
    /// arrow-key navigation already takes care to avoid.
    pub fn goto_line(&mut self, line: usize) {
        if self.selection != Selection::None {
            self.perform(cosmic_text::Action::Escape);
        }
        self.perform(cosmic_text::Action::Motion(cosmic_text::Motion::GotoLine(line)));
    }

    /// Moves the cursor to (`line`, byte `index`), clamped to the text, dropping any selection.
    /// Used by go-to-definition, which lands on a column rather than a whole line.
    pub fn goto(&mut self, line: usize, index: usize) {
        let line = line.min(self.line_count().saturating_sub(1));
        let index = self.inner.lines.get(line).map_or(0, |l| index.min(l.text().len()));
        self.cursor = Cursor::new(line, index);
        self.selection = Selection::None;
        self.sync();
    }

    /// Records the editor widget's scroll offsets as it draws; see `view_scroll`.
    pub(super) fn set_view_scroll(&self, scroll: f32, scroll_x: f32) {
        use std::sync::atomic::Ordering;
        self.view_scroll[0].store(scroll.to_bits(), Ordering::Relaxed);
        self.view_scroll[1].store(scroll_x.to_bits(), Ordering::Relaxed);
    }

    /// `(vertical, horizontal)` scroll as last drawn.
    pub fn view_scroll(&self) -> (f32, f32) {
        use std::sync::atomic::Ordering;
        (f32::from_bits(self.view_scroll[0].load(Ordering::Relaxed)), f32::from_bits(self.view_scroll[1].load(Ordering::Relaxed)))
    }

    /// `(row, x_start, x_end)` selection highlight rects, one per screen row the selection
    /// touches, x relative to the text origin.
    pub fn selection_pixels(&self) -> &[(usize, f32, f32)] {
        &self.selection_pixels
    }

    /// `(row, x_start, x_end)` rects for the bracket next to the cursor and its match, if
    /// any -- see the `matched_brackets` field doc.
    pub fn matched_brackets(&self) -> &[(usize, f32, f32)] {
        &self.matched_brackets
    }

    /// Pixel width of the widest line (as of the last `sync`).
    pub fn content_width(&self) -> f32 {
        self.content_width
    }

    /// Applies a mouse action, whose `x`/`y` are relative to where text starts on screen
    /// with scrolling added back: `y` counts rows from the first, `x` is within the row.
    /// These go through the row map rather than the engine, which knows nothing of rows.
    fn perform_pointer(&mut self, action: &cosmic_text::Action) -> bool {
        use cosmic_text::Action;
        let (Action::Click { x, y } | Action::DoubleClick { x, y } | Action::TripleClick { x, y } | Action::Drag { x, y }) = *action else {
            return false;
        };
        let row = (y as f32 / self.inner.metrics().line_height).max(0.0) as usize;
        let cursor = self.hit(row, x as f32);
        self.selection = match action {
            // A drag extends from where the press put the cursor, keeping a word or line
            // selection started by a double or triple click.
            Action::Drag { .. } if self.selection == Selection::None => Selection::Normal(self.cursor),
            Action::Drag { .. } => self.selection,
            Action::DoubleClick { .. } => Selection::Word(cursor),
            Action::TripleClick { .. } => Selection::Line(cursor),
            _ => Selection::None,
        };
        self.cursor = cursor;
        self.sync();
        true
    }

    /// Applies the motions that depend on screen rows: up and down by one row or one page,
    /// and to either end of the row. Like the engine's motions they leave the selection alone.
    fn perform_row_motion(&mut self, action: &cosmic_text::Action) -> bool {
        use cosmic_text::{Action, Motion};
        let Action::Motion(motion) = *action else { return false; };
        let Some((row, x)) = self.caret else { return false; };
        let page = ((self.view_height / self.inner.metrics().line_height) as usize).max(1);
        let down = match motion {
            Motion::Up => -1,
            Motion::Down => 1,
            Motion::PageUp => -(page as isize),
            Motion::PageDown => page as isize,
            Motion::Home | Motion::End => {
                let Some(row) = self.rows.get(row) else { return false; };
                self.cursor = if motion == Motion::Home {
                    Cursor::new_with_affinity(row.line, row.start, Affinity::After)
                } else {
                    Cursor::new_with_affinity(row.line, row.end, Affinity::Before)
                };
                self.sync();
                return true;
            }
            _ => return false,
        };
        // Moving through shorter rows and back returns to the x the movement started at.
        let goal = self.goal_x.unwrap_or(x);
        let last = self.rows.len().saturating_sub(1);
        self.cursor = match row.checked_add_signed(down) {
            // Up from the first row is the start of the text, down from the last its end.
            None if down == -1 => Cursor::new(self.rows.line_at(0), 0),
            Some(target) if target > last && down == 1 => {
                let line = self.rows.line_at(last);
                Cursor::new(line, self.inner.lines.get(line).map_or(0, |line| line.text().len()))
            }
            target => self.hit(target.unwrap_or(0).min(last), goal),
        };
        self.sync();
        self.goal_x = Some(goal);
        true
    }

    /// Applies a `cosmic_text::Action` (motion, insert, backspace, click, ...) to the buffer.
    pub fn perform(&mut self, action: cosmic_text::Action) {
        if self.perform_pointer(&action) || self.perform_row_motion(&action) {
            return;
        }
        let mut editor = Editor::new(&mut self.inner);
        editor.set_cursor(self.cursor);
        // A click/drag or a shift-motion can leave an empty selection. Cosmic treats
        // deleting it as a completed edit, swallowing Backspace/Delete. Affinity can
        // differ at syntax span boundaries, so compare text positions only. Other actions
        // keep it: a shift-motion starts from an anchor placed on the cursor.
        let deleting = matches!(action, cosmic_text::Action::Backspace | cosmic_text::Action::Delete);
        let selection = match self.selection {
            Selection::Normal(anchor)
                if deleting && anchor.line == self.cursor.line && anchor.index == self.cursor.index => Selection::None,
            selection => selection,
        };
        editor.set_selection(selection);
        editor.start_change();
        editor.action(&mut self.font_system, action);
        let change = editor.finish_change();
        self.cursor = editor.cursor();
        self.selection = editor.selection();
        self.push_undo(change);
        self.sync();
    }

    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    /// Number of edits recorded for undo. Grows by exactly one per user-visible edit (see
    /// `push_undo`), so comparing it before/after an operation tells the caller whether that
    /// operation actually changed the document -- used by `main.rs` to decide whether to mark
    /// a tab dirty and re-run syntax highlighting.
    pub fn undo_count(&self) -> usize {
        self.undo_stack.len()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    pub fn undo(&mut self) {
        let Some(mut change) = self.undo_stack.pop() else {
            return;
        };
        change.reverse();
        let mut editor = Editor::new(&mut self.inner);
        editor.apply_change(&change);
        self.cursor = editor.cursor();
        self.selection = Selection::None;
        change.reverse(); // back to forward order, for redo
        self.redo_stack.push(change);
        self.sync();
    }

    pub fn redo(&mut self) {
        let Some(change) = self.redo_stack.pop() else {
            return;
        };
        let mut editor = Editor::new(&mut self.inner);
        editor.apply_change(&change);
        self.cursor = editor.cursor();
        self.selection = Selection::None;
        self.undo_stack.push(change);
        self.sync();
    }

    /// Records a completed edit for undo, clearing the redo stack (its changes no longer
    /// apply cleanly once the document has diverged from where they were recorded).
    fn push_undo(&mut self, change: Option<Change>) {
        if let Some(change) = change {
            if !change.items.is_empty() {
                self.undo_stack.push(change);
                self.redo_stack.clear();
            }
        }
    }

    /// Re-derives syntax-highlight colors for the current text and re-applies them, line by
    /// line. Only colours change: the text, the lines and the cursor stay exactly as they
    /// are. After an edit, only the lines around it are redone (see `HighlightCache`); the
    /// first pass, a new theme or a new syntax mean a full pass.
    pub fn highlight(&mut self, highlighter: &Highlighter, extension: &str, theme: &Theme) {
        let syntax = highlighter.language_name(extension);
        if self.highlight_cache.as_ref().is_some_and(|cache| cache.syntax == syntax && cache.theme == *theme) {
            self.highlight_changed_lines(highlighter);
        } else {
            self.highlight_all(highlighter, extension, theme);
        }
        // Every line keeps the default ending, and the engine's own scroll stays at the top,
        // as `set_rich_text` (which used to apply the colours) left them.
        for line in &mut self.inner.lines {
            line.set_ending(LineEnding::default());
        }
        self.inner.set_scroll(cosmic_text::Scroll::default());
        // Styles can change glyph widths (a bold span, say), and with them where lines wrap.
        self.rows_stale = true;
        self.sync();
    }

    /// Highlights every line.
    fn highlight_all(&mut self, highlighter: &Highlighter, extension: &str, theme: &Theme) {
        let styles = ThemeHighlighter::new(theme);
        let mut state = highlighter.start(extension, theme);
        let mut snapshots = Vec::new();
        for (i, line) in self.inner.lines.iter_mut().enumerate() {
            if i % SNAPSHOT_EVERY == 0 {
                snapshots.push((i, state.clone()));
            }
            let attrs = attrs_list_for(&highlighter.highlight_line(&mut state, &styles, line.text()));
            line.set_attrs_list(attrs);
        }
        self.highlight_cache = Some(HighlightCache {
            syntax: highlighter.language_name(extension).to_string(),
            theme: theme.clone(),
            lines: self.inner.lines.iter().map(|line| line.text().to_string()).collect(),
            snapshots,
            recomputed: self.inner.lines.len(),
        });
    }

    /// Re-highlights the lines an edit changed, from the snapshot before the first changed
    /// line until the parser state at a snapshot after the last one matches what it was.
    /// Lines whose colours didn't change keep their shaping.
    fn highlight_changed_lines(&mut self, highlighter: &Highlighter) {
        let cache = self.highlight_cache.as_mut().expect("the caller checked there is a cache");
        let lines = &mut self.inner.lines;
        let (old_len, new_len) = (cache.lines.len(), lines.len());
        let prefix = cache.lines.iter().zip(lines.iter()).take_while(|(old, new)| old.as_str() == new.text()).count();
        let suffix = cache.lines.iter().rev().zip(lines.iter().rev())
            .take(old_len.min(new_len) - prefix)
            .take_while(|(old, new)| old.as_str() == new.text())
            .count();
        if prefix == old_len && old_len == new_len {
            cache.recomputed = 0;
            return;
        }
        let styles = ThemeHighlighter::new(&cache.theme);
        // Snapshots in the unchanged tail stay valid, at their lines' new numbers.
        let shift = new_len as isize - old_len as isize;
        let after_edit: Vec<(usize, LineState)> = cache.snapshots.iter()
            .filter(|(line, _)| *line >= old_len - suffix)
            .map(|(line, state)| ((*line as isize + shift) as usize, state.clone()))
            .collect();
        let (start, mut state) = cache.snapshots.iter().rev()
            .find(|(line, _)| *line <= prefix)
            .map(|(line, state)| (*line, state.clone()))
            .expect("line 0 always has a snapshot");
        cache.snapshots.retain(|(line, _)| *line <= start);
        let mut converged = None;
        for i in start..new_len {
            if i >= new_len - suffix {
                if let Some((_, resumed)) = after_edit.iter().find(|(line, _)| *line == i) {
                    if *resumed == state {
                        converged = Some(i);
                        break;
                    }
                }
            }
            if i > start && i % SNAPSHOT_EVERY == 0 {
                cache.snapshots.push((i, state.clone()));
            }
            let line = &mut lines[i];
            let attrs = attrs_list_for(&highlighter.highlight_line(&mut state, &styles, line.text()));
            line.set_attrs_list(attrs);
        }
        cache.recomputed = converged.unwrap_or(new_len) - start;
        if let Some(from) = converged {
            cache.snapshots.extend(after_edit.into_iter().filter(|(line, _)| *line >= from));
        }
        cache.lines = lines.iter().map(|line| line.text().to_string()).collect();
    }

    /// Selects `start..end`, moving the cursor to `end` (matching how a mouse drag would
    /// leave things). Used by search/replace to highlight a match.
    pub fn select_range(&mut self, start: Cursor, end: Cursor) {
        self.cursor = end;
        self.selection = Selection::Normal(start);
        self.sync();
    }

    /// Replaces `start..end` with `replacement`, moving the cursor to just after it and
    /// clearing the selection.
    pub fn replace_range(&mut self, start: Cursor, end: Cursor, replacement: &str) {
        let mut editor = Editor::new(&mut self.inner);
        editor.start_change();
        editor.delete_range(start, end);
        let new_cursor = editor.insert_at(start, replacement, None);
        editor.set_cursor(new_cursor);
        let change = editor.finish_change();
        self.cursor = editor.cursor();
        self.selection = Selection::None;
        self.push_undo(change);
        self.sync();
    }

    /// Applies non-overlapping `edits` (start, end, replacement) as one undo step, leaving the
    /// cursor just after the replacement of `edits[main]` -- e.g. an accepted completion plus
    /// the import it needs further up. Edits run from the end of the document backwards so
    /// earlier positions stay valid; the cursor is shifted by any edits applied above it.
    pub fn replace_ranges(&mut self, edits: &[(Cursor, Cursor, String)], main: usize) {
        let mut order: Vec<usize> = (0..edits.len()).collect();
        order.sort_by(|a, b| {
            let (a, b) = (edits[*a].0, edits[*b].0);
            (b.line, b.index).cmp(&(a.line, a.index))
        });
        let mut editor = Editor::new(&mut self.inner);
        editor.start_change();
        let mut cursor: Option<Cursor> = None;
        for index in order {
            let (start, end, text) = &edits[index];
            editor.delete_range(*start, *end);
            let after = editor.insert_at(*start, text, None);
            if index == main {
                cursor = Some(after);
            } else if let Some(c) = cursor.as_mut() {
                // This edit lies before the cursor: move it by the lines/bytes it added or
                // removed (signed, since an edit can remove more than it inserts).
                let shift = |value: usize, from: usize, to: usize| (value as isize + to as isize - from as isize).max(0) as usize;
                if end.line < c.line {
                    c.line = shift(c.line, end.line, after.line);
                } else if end.line == c.line {
                    c.line = after.line;
                    c.index = shift(c.index, end.index, after.index);
                }
            }
        }
        if let Some(cursor) = cursor { editor.set_cursor(cursor); }
        let change = editor.finish_change();
        self.cursor = editor.cursor();
        self.selection = Selection::None;
        self.push_undo(change);
        self.sync();
    }

    /// Whether a non-empty range is selected (based on the last `sync`).
    pub fn has_selection(&self) -> bool {
        self.selection_pixels.len() > 1
            || self.selection_pixels.first().is_some_and(|(_, x0, x1)| x1 > x0)
    }

    /// Text of the current selection, or `None` if nothing (or an empty range) is selected.
    pub fn copy_selection(&mut self) -> Option<String> {
        let mut editor = Editor::new(&mut self.inner);
        editor.set_cursor(self.cursor);
        editor.set_selection(self.selection);
        editor.copy_selection().filter(|text| !text.is_empty())
    }

    /// Start and end of a non-empty selection, in document order.
    pub fn selection_bounds(&mut self) -> Option<(Cursor, Cursor)> {
        let mut editor = Editor::new(&mut self.inner);
        editor.set_cursor(self.cursor);
        editor.set_selection(self.selection);
        editor.selection_bounds().filter(|(start, end)| (start.line, start.index) != (end.line, end.index))
    }

    /// The text from `start` to `end` (byte positions within lines), or `None` if either is
    /// outside the document, e.g. because it changed since they were taken.
    pub fn text_between(&self, start: Cursor, end: Cursor) -> Option<String> {
        let lines = &self.inner.lines;
        if (start.line, start.index) > (end.line, end.index) { return None; }
        let (first, last) = (lines.get(start.line)?.text(), lines.get(end.line)?.text());
        if start.line == end.line { return first.get(start.index..end.index).map(str::to_string); }
        let mut text = first.get(start.index..)?.to_string();
        for line in &lines[start.line + 1..end.line] {
            text.push('\n');
            text.push_str(line.text());
        }
        text.push('\n');
        text.push_str(last.get(..end.index)?);
        Some(text)
    }

    /// Copies the selection and deletes it as a single undo step.
    pub fn cut_selection(&mut self) -> Option<String> {
        let text = self.copy_selection()?;
        self.replace_selection("");
        Some(text)
    }

    /// Replaces the selection (or inserts at the cursor if there is none) with `text` as a
    /// single undo step -- used for paste and cut.
    pub fn replace_selection(&mut self, text: &str) {
        let mut editor = Editor::new(&mut self.inner);
        editor.set_cursor(self.cursor);
        editor.set_selection(self.selection);
        editor.start_change();
        if text.is_empty() {
            editor.delete_selection();
        } else {
            editor.insert_string(text, None);
        }
        let change = editor.finish_change();
        self.cursor = editor.cursor();
        self.selection = editor.selection();
        self.push_undo(change);
        self.sync();
    }

    fn sync(&mut self) {
        // Anything that gets here other than a move up or down (which sets this again
        // afterwards) starts a new column to keep.
        self.goal_x = None;
        let text = self.text();
        if text != self.folding_text {
            self.folds = super::folding::ranges(&text);
            self.folding_text = text;
            // Reopen on edits instead of keeping stale line ranges after insertion/deletion.
            self.collapsed.clear();
            self.rows_stale = true;
        }
        let collapsed = self.collapsed.len();
        self.collapsed.retain(|start| !self.folds.get(start).is_some_and(|end| self.cursor.line > *start && self.cursor.line <= *end));
        self.rows_stale |= self.collapsed.len() != collapsed;
        self.inner.shape_until_scroll(&mut self.font_system, false);
        self.content_width = self
            .inner
            .lines
            .iter()
            .filter_map(|line| line.layout_opt().and_then(|lines| lines.first()).map(|line| line.w))
            .fold(0.0, f32::max);
        if std::mem::take(&mut self.rows_stale) {
            self.rebuild_rows();
        }

        let at_end = self.cursor.affinity == Affinity::Before;
        self.caret = self.rows.locate(self.cursor.line, self.cursor.index, at_end).and_then(|row| {
            Some((row, self.rows.get(row)?.x_of(glyphs_of(&self.inner, self.cursor.line), self.cursor.index)))
        });

        let selection_bounds = {
            let mut editor = Editor::new(&mut self.inner);
            editor.set_cursor(self.cursor);
            editor.set_selection(self.selection);
            editor.selection_bounds()
        };

        self.selection_pixels.clear();
        if let Some((start, end)) = selection_bounds {
            for line in start.line..=end.line {
                let glyphs = glyphs_of(&self.inner, line);
                let from = if line == start.line { start.index } else { 0 };
                let to = if line == end.line { end.index } else { self.inner.lines.get(line).map_or(0, |l| l.text().len()) };
                let before = self.selection_pixels.len();
                for index in self.rows.of_line(line) {
                    let Some(row) = self.rows.get(index) else { continue };
                    let (from, to) = (from.max(row.start), to.min(row.end));
                    if from < to {
                        let x0 = row.x_of(glyphs, from);
                        self.selection_pixels.push((index, x0, row.x_of(glyphs, to).max(x0)));
                    }
                }
                // A line the selection only passes through at a point -- an empty line, or
                // one it ends at the very start of -- still gets a rect, drawn as a sliver.
                if self.selection_pixels.len() == before {
                    if let Some(index) = self.rows.locate(line, from, false) {
                        let x = self.rows.get(index).map_or(0.0, |row| row.x_of(glyphs, from));
                        self.selection_pixels.push((index, x, x));
                    }
                }
            }
        }

        self.matched_brackets.clear();
        if self.selection == Selection::None {
            if let Some(positions) = brackets::find_match(&self.inner, self.cursor) {
                for (line, start, end) in positions {
                    let Some(index) = self.rows.locate(line, start, false) else { continue };
                    let Some(row) = self.rows.get(index) else { continue };
                    let glyphs = glyphs_of(&self.inner, line);
                    let x0 = row.x_of(glyphs, start);
                    self.matched_brackets.push((index, x0, row.x_of(glyphs, end).max(x0)));
                }
            }
        }
    }

    /// Rebuilds the row map from the laid-out lines, the folds and the wrap width.
    fn rebuild_rows(&mut self) {
        // No width is known until the widget has been laid out once.
        let width = (self.wrap && self.wrap_width > 0.0).then_some(self.wrap_width);
        self.rows.clear();
        let count = self.inner.lines.len();
        let mut line = 0;
        while line < count {
            self.rows.push_line(glyphs_of(&self.inner, line), self.inner.lines[line].text(), width);
            let last = if self.collapsed.contains(&line) {
                self.folds.get(&line).copied().unwrap_or(line).clamp(line, count - 1)
            } else {
                line
            };
            for _ in line..last {
                self.rows.push_hidden();
            }
            line = last + 1;
        }
    }
}

/// A line's laid-out glyphs as comparable values: byte range, glyph id, x and width.
#[cfg(test)]
fn glyph_layout(line: &BufferLine) -> Option<Vec<(usize, usize, u16, u32, u32)>> {
    line.layout_opt().map(|layout| {
        layout.iter().flat_map(|row| row.glyphs.iter().map(|g| (g.start, g.end, g.glyph_id, g.x.to_bits(), g.w.to_bits()))).collect()
    })
}

#[cfg(test)]
mod construction_tests {
    use super::*;

    const SOURCE: &str = "fn main() {\r\n    let s = \"héllo\";\t// ünïcode -> != ff\n\n}\n\rtrailing\rlast";

    fn glyphs(line: &BufferLine) -> Option<Vec<(usize, usize, u16, u32, u32)>> { glyph_layout(line) }

    /// `highlighted` leaves a buffer in exactly the state `new` + `highlight` did.
    #[test]
    fn highlighted_matches_new_then_highlight() {
        let highlighter = Highlighter::new();
        let theme = crate::theme::EditorTheme::default_dark();
        for (source, wrap) in [(SOURCE, false), (SOURCE, true), ("", false), ("one line, no ending", false), ("\n\n", true)] {
            let mut old = Buffer::new(source, Metrics::new(14.0, 20.0));
            old.set_wrap(wrap);
            old.highlight(&highlighter, "rs", &theme.syntax);
            let mut new = Buffer::highlighted(source, Metrics::new(14.0, 20.0), &highlighter, "rs", &theme.syntax);
            new.set_wrap(wrap);

            assert_eq!(new.text(), old.text());
            assert_eq!(new.inner.lines.len(), old.inner.lines.len());
            for (a, b) in new.inner.lines.iter().zip(&old.inner.lines) {
                assert_eq!(a.text(), b.text());
                assert_eq!(a.ending(), b.ending());
                assert_eq!(a.attrs_list(), b.attrs_list());
                assert_eq!(glyphs(a), glyphs(b));
                assert!(glyphs(a).is_some(), "every line is laid out");
            }
            assert_eq!(new.visible_count(), old.visible_count());
            for row in 0..old.visible_count() {
                assert_eq!(new.row(row), old.row(row));
            }
            assert_eq!(new.content_width().to_bits(), old.content_width().to_bits());
            assert_eq!(new.caret(), old.caret());
            assert_eq!(new.folds, old.folds);
            assert_eq!(new.layout_epoch(), old.layout_epoch());
            assert_eq!(new.inner.lines.iter().any(|line| !line.attrs_list().spans().is_empty()), !source.trim().is_empty(), "the text is coloured");
        }
    }

    /// `highlight` feeds the buffer's lines straight to syntect; the result must be what
    /// highlighting the joined text gave.
    #[test]
    fn highlighting_lines_matches_highlighting_the_joined_text() {
        let highlighter = Highlighter::new();
        let theme = crate::theme::EditorTheme::default_dark();
        let buffer = Buffer::new(SOURCE, Metrics::new(14.0, 20.0));
        let from_lines = highlighter.highlight_lines(buffer.inner.lines.iter().map(|line| line.text()), "rs", &theme.syntax);
        let from_text = highlighter.highlight(&buffer.text(), "rs", &theme.syntax);
        assert_eq!(from_lines, from_text);
    }

    /// The shared font database shapes text exactly as a freshly built `FontSystem` does.
    #[test]
    fn shared_font_database_shapes_like_a_fresh_font_system() {
        let mut fresh = FontSystem::new();
        let mut shared = font_system();
        assert_eq!(shared.locale(), fresh.locale());
        assert_eq!(shared.db().len(), fresh.db().len());
        for family in [cosmic_text::Family::Monospace, cosmic_text::Family::SansSerif, cosmic_text::Family::Serif] {
            assert_eq!(shared.db().family_name(&family), fresh.db().family_name(&family));
        }
        let shape = |font_system: &mut FontSystem| {
            let mut buffer = CosmicBuffer::new(font_system, Metrics::new(14.0, 20.0));
            buffer.set_wrap(font_system, Wrap::None);
            buffer.set_text(font_system, SOURCE, &Attrs::new(), Shaping::Advanced, None);
            buffer.lines.iter().map(glyphs).collect::<Vec<_>>()
        };
        assert_eq!(shape(&mut shared), shape(&mut fresh));
    }

}

#[cfg(test)]
mod incremental_highlight_tests {
    use super::*;
    use cosmic_text::Action;

    fn metrics() -> Metrics { Metrics::new(14.0, 20.0) }

    /// 480 lines of Rust, so edits land well inside the snapshot grid.
    fn source() -> String {
        (0..120)
            .map(|i| format!("fn item{i}(x: u32) -> u32 {{\n    let s = \"text {i}\"; // note\n    x + {i}\n}}"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn type_text(buffer: &mut Buffer, text: &str) {
        for c in text.chars() {
            buffer.perform(if c == '\n' { Action::Enter } else { Action::Insert(c) });
        }
    }

    fn recomputed(buffer: &Buffer) -> usize {
        buffer.highlight_cache.as_ref().expect("highlighted at least once").recomputed
    }

    /// Line for line, `buffer` must be what a fresh full highlight of the same text gives:
    /// text, line ending, attributes and glyph layout.
    fn assert_like_fresh(buffer: &Buffer, highlighter: &Highlighter, extension: &str, theme: &Theme) {
        let fresh = Buffer::highlighted(&buffer.text(), metrics(), highlighter, extension, theme);
        assert_eq!(buffer.inner.lines.len(), fresh.inner.lines.len());
        for (i, (a, b)) in buffer.inner.lines.iter().zip(&fresh.inner.lines).enumerate() {
            assert_eq!(a.text(), b.text(), "line {i}");
            assert_eq!(a.ending(), b.ending(), "line {i}");
            assert_eq!(a.attrs_list(), b.attrs_list(), "line {i}");
            assert_eq!(glyph_layout(a), glyph_layout(b), "line {i}");
        }
        assert_eq!(buffer.visible_count(), fresh.visible_count());
        assert_eq!(buffer.content_width().to_bits(), fresh.content_width().to_bits());
    }

    #[test]
    fn edits_rehighlight_like_a_fresh_pass_and_only_the_lines_around_them() {
        let highlighter = Highlighter::new();
        let dark = crate::theme::EditorTheme::default_dark();
        let mut buffer = Buffer::highlighted(&source(), metrics(), &highlighter, "rs", &dark.syntax);
        let lines = buffer.line_count();
        assert!(lines > 4 * SNAPSHOT_EVERY);

        // Typing inside a line.
        buffer.goto(200, 8);
        type_text(&mut buffer, "yz");
        buffer.highlight(&highlighter, "rs", &dark.syntax);
        assert_like_fresh(&buffer, &highlighter, "rs", &dark.syntax);
        assert!(recomputed(&buffer) <= 2 * SNAPSHOT_EVERY, "{} lines for a keystroke", recomputed(&buffer));

        // Splitting a line moves everything below it down; joining moves it back up.
        buffer.perform(Action::Enter);
        buffer.highlight(&highlighter, "rs", &dark.syntax);
        assert_eq!(buffer.line_count(), lines + 1);
        assert_like_fresh(&buffer, &highlighter, "rs", &dark.syntax);
        assert!(recomputed(&buffer) <= 2 * SNAPSHOT_EVERY, "{} lines for a line split", recomputed(&buffer));
        buffer.perform(Action::Backspace);
        buffer.highlight(&highlighter, "rs", &dark.syntax);
        assert_eq!(buffer.line_count(), lines);
        assert_like_fresh(&buffer, &highlighter, "rs", &dark.syntax);

        // Nothing changed: nothing to redo.
        buffer.highlight(&highlighter, "rs", &dark.syntax);
        assert_eq!(recomputed(&buffer), 0);
        assert_like_fresh(&buffer, &highlighter, "rs", &dark.syntax);

        // Opening a block comment recolours everything after it, to the end of the file.
        buffer.goto(40, 0);
        type_text(&mut buffer, "/*");
        buffer.highlight(&highlighter, "rs", &dark.syntax);
        assert_like_fresh(&buffer, &highlighter, "rs", &dark.syntax);
        assert!(recomputed(&buffer) >= lines - 40, "{} lines for an unclosed comment", recomputed(&buffer));
        // Closing it far below recolours the lines between, and the rest converges.
        buffer.goto(300, 0);
        type_text(&mut buffer, "*/");
        buffer.highlight(&highlighter, "rs", &dark.syntax);
        assert_like_fresh(&buffer, &highlighter, "rs", &dark.syntax);
        assert!(recomputed(&buffer) < lines - 40, "{} lines for a closed comment", recomputed(&buffer));
        // Undoing both.
        for _ in 0..4 { buffer.undo(); }
        buffer.highlight(&highlighter, "rs", &dark.syntax);
        assert_like_fresh(&buffer, &highlighter, "rs", &dark.syntax);
        assert_eq!(buffer.line_count(), lines);

        // Deleting a block of lines, then pasting lines with Windows endings (which the
        // editor's line endings are normalised away from, as before).
        buffer.replace_range(Cursor::new(10, 0), Cursor::new(30, 0), "");
        buffer.highlight(&highlighter, "rs", &dark.syntax);
        assert_eq!(buffer.line_count(), lines - 20);
        assert_like_fresh(&buffer, &highlighter, "rs", &dark.syntax);
        buffer.replace_range(Cursor::new(5, 0), Cursor::new(5, 0), "let a = 1;\r\nlet b = \"two\";\r\n");
        buffer.highlight(&highlighter, "rs", &dark.syntax);
        assert_eq!(buffer.line_count(), lines - 18);
        assert_like_fresh(&buffer, &highlighter, "rs", &dark.syntax);

        // A new theme or syntax is a full pass.
        let light = crate::theme::EditorTheme::default_light();
        buffer.highlight(&highlighter, "rs", &light.syntax);
        assert_eq!(recomputed(&buffer), buffer.line_count());
        assert_like_fresh(&buffer, &highlighter, "rs", &light.syntax);
        buffer.highlight(&highlighter, "txt", &light.syntax);
        assert_eq!(recomputed(&buffer), buffer.line_count());
        assert_like_fresh(&buffer, &highlighter, "txt", &light.syntax);
    }

    /// Colours applied line by line are exactly what applying them through
    /// `cosmic_text::Buffer::set_rich_text` (the previous way) produced.
    #[test]
    fn per_line_colours_match_set_rich_text() {
        let highlighter = Highlighter::new();
        let dark = crate::theme::EditorTheme::default_dark();
        let mixed = "fn main() {\r\n    let s = \"héllo\";\t// ünïcode -> != ff\n\n}\n\rtrailing\rlast";
        for text in [source(), mixed.to_string(), String::new()] {
            let mut old_way = Buffer::new(&text, metrics());
            let spans = highlighter.highlight_lines(old_way.inner.lines.iter().map(|line| line.text()), "rs", &dark.syntax);
            old_way.inner.set_rich_text(&mut old_way.font_system, spans.iter().map(|(chunk, attrs)| (chunk.as_str(), attrs.clone())), &Attrs::new(), Shaping::Advanced, None);
            old_way.rows_stale = true;
            old_way.sync();
            let new_way = Buffer::highlighted(&text, metrics(), &highlighter, "rs", &dark.syntax);
            assert_eq!(new_way.inner.lines.len(), old_way.inner.lines.len());
            for (i, (a, b)) in new_way.inner.lines.iter().zip(&old_way.inner.lines).enumerate() {
                assert_eq!(a.text(), b.text(), "line {i}");
                assert_eq!(a.ending(), b.ending(), "line {i}");
                assert_eq!(a.attrs_list(), b.attrs_list(), "line {i}");
                assert_eq!(glyph_layout(a), glyph_layout(b), "line {i}");
            }
            assert_eq!(new_way.visible_count(), old_way.visible_count());
            assert_eq!(new_way.content_width().to_bits(), old_way.content_width().to_bits());
        }
    }

    /// Pasting a Unicode paragraph separator inside a line: no crash, the character stays
    /// in its line, and the colours are right. (Applying colours through `set_rich_text`
    /// used to split the line there and crash on the cursor left past its end.)
    #[test]
    fn a_paragraph_separator_stays_in_its_line() {
        let highlighter = Highlighter::new();
        let dark = crate::theme::EditorTheme::default_dark();
        let mut buffer = Buffer::highlighted(&source(), metrics(), &highlighter, "rs", &dark.syntax);
        let lines = buffer.line_count();
        buffer.goto(0, 8);
        buffer.perform(Action::Insert('\u{2029}'));
        buffer.highlight(&highlighter, "rs", &dark.syntax);
        assert_eq!(buffer.line_count(), lines);
        assert!(buffer.inner.lines[0].text().contains('\u{2029}'));
        assert_like_fresh(&buffer, &highlighter, "rs", &dark.syntax);
        buffer.perform(Action::Insert('x'));
        buffer.highlight(&highlighter, "rs", &dark.syntax);
        assert_like_fresh(&buffer, &highlighter, "rs", &dark.syntax);
    }
}

/// Speed measurements. These never fail: each prints how long an operation took next to the
/// time expected of it, and flags it as a WARNING when slower, so a slowdown shows up in the
/// test output without blocking anything. The expected times are from a debug build on the
/// maintainer's machine (2026-10-08, Windows 11), with the rest of the suite running alongside;
/// other machines will differ.
#[cfg(test)]
mod timing_tests {
    use super::*;
    use cosmic_text::Action;
    use std::time::{Duration, Instant};

    /// Runs `op` once and reports its time against `expected`. Written straight to the
    /// process's stderr, which the test harness doesn't capture, so it shows in a plain
    /// `cargo test` run.
    fn timed(name: &str, expected: Duration, op: impl FnOnce()) {
        use std::io::Write;
        let start = Instant::now();
        op();
        let took = start.elapsed();
        let line = if took > expected {
            format!("WARNING: {name}: expected {expected:?}, took {took:?} ({:.1}x slower)\n", took.as_secs_f64() / expected.as_secs_f64())
        } else {
            format!("timing: {name}: expected {expected:?}, took {took:?}\n")
        };
        let _ = std::io::stderr().write_all(line.as_bytes());
    }

    const LINE: &str = "    let value = compute(first_argument, second_argument) + another_call(third) * 2; // trailing comment";

    fn source() -> String { vec![LINE; 4_000].join("\n") }

    fn open(text: &str, highlighter: &Highlighter, theme: &crate::theme::EditorTheme) -> Buffer {
        Buffer::highlighted(text, Metrics::new(14.0, 20.0), highlighter, "rs", &theme.syntax)
    }

    #[test]
    fn open_4000_lines() {
        let (text, highlighter, theme) = (source(), Highlighter::new(), crate::theme::EditorTheme::default_dark());
        timed("open 4,000 lines", Duration::from_millis(651), || { let _ = open(&text, &highlighter, &theme); });
    }

    #[test]
    fn keystroke_in_4000_lines() {
        let (text, highlighter, theme) = (source(), Highlighter::new(), crate::theme::EditorTheme::default_dark());
        let mut buffer = open(&text, &highlighter, &theme);
        buffer.set_viewport(400.0, 800.0);
        timed("keystroke in 4,000 lines (edit and layout)", Duration::from_millis(6), || buffer.perform(Action::Insert('x')));
    }

    #[test]
    fn rehighlight_4000_lines_after_a_theme_change() {
        let (text, highlighter, theme) = (source(), Highlighter::new(), crate::theme::EditorTheme::default_dark());
        let mut buffer = open(&text, &highlighter, &theme);
        let light = crate::theme::EditorTheme::default_light();
        timed("re-highlight 4,000 lines (theme change)", Duration::from_millis(571), || buffer.highlight(&highlighter, "rs", &light.syntax));
    }

    #[test]
    fn rehighlight_4000_lines_after_a_keystroke() {
        let (text, highlighter, theme) = (source(), Highlighter::new(), crate::theme::EditorTheme::default_dark());
        let mut buffer = open(&text, &highlighter, &theme);
        buffer.goto(2_000, 4);
        buffer.perform(Action::Insert('x'));
        timed("re-highlight 4,000 lines (after a keystroke)", Duration::from_millis(12), || buffer.highlight(&highlighter, "rs", &theme.syntax));
    }

    #[test]
    fn word_wrap_on_4000_lines() {
        let (text, highlighter, theme) = (source(), Highlighter::new(), crate::theme::EditorTheme::default_dark());
        let mut buffer = open(&text, &highlighter, &theme);
        buffer.set_viewport(400.0, 800.0);
        timed("word wrap on, 4,000 lines", Duration::from_millis(22), || buffer.set_wrap(true));
    }

    #[test]
    fn empty_buffer() {
        timed("empty buffer (font system from the shared database)", Duration::from_millis(23), || { let _ = Buffer::new("", Metrics::new(14.0, 20.0)); });
    }
}

#[cfg(test)]
mod folding_tests {
    use super::*;
    #[test]
    fn folding_maps_rows_and_reveals_cursor_without_changing_text() {
        let source = "{\n  [\n    1\n  ]\n}\nafter";
        let mut buffer = Buffer::new(source, Metrics::new(14.0, 20.0));
        buffer.toggle_fold(1);
        assert_eq!(buffer.visible_count(), 4);
        assert_eq!(buffer.source_line(2), 4);
        assert_eq!(buffer.visual_row(3), None);
        buffer.toggle_fold(0);
        assert_eq!(buffer.visible_count(), 2);
        assert_eq!(buffer.source_line(1), 5);
        buffer.toggle_fold(0);
        assert_eq!(buffer.visible_count(), 4); // nested fold preserved
        buffer.goto_line(2);
        assert_eq!(buffer.visible_count(), 6);
        assert_eq!(buffer.text(), source);
        assert!(!buffer.can_undo());
        buffer.toggle_fold(0);
        buffer.perform(cosmic_text::Action::Insert('x'));
        assert!(buffer.collapsed.is_empty());
        assert!(buffer.can_undo());
    }
}

/// Where things sit on screen. These hold with word wrap off, whatever fonts are installed;
/// they were written before the row map existed and pin the behaviour it must keep.
#[cfg(test)]
mod geometry_tests {
    use super::*;

    const SOURCE: &str = "let x = 1;\n\n    indented\nlast";

    fn buffer() -> Buffer { Buffer::new(SOURCE, Metrics::new(14.0, 20.0)) }

    /// Byte offsets a caret can sit at on `line`.
    fn boundaries(buffer: &Buffer, line: usize) -> Vec<usize> {
        let text = buffer.inner.lines[line].text();
        text.char_indices().map(|(i, _)| i).chain([text.len()]).collect()
    }

    #[test]
    fn rows_follow_lines_one_to_one_without_folds() {
        let buffer = buffer();
        assert_eq!(buffer.visible_count(), 4);
        for line in 0..4 {
            assert_eq!(buffer.visual_row(line), Some(line));
            assert_eq!(buffer.source_line(line), line);
        }
        assert_eq!(buffer.source_line(99), 3, "rows past the end clamp to the last line");
    }

    #[test]
    fn caret_sits_on_its_line_and_moves_right_along_it() {
        let mut buffer = buffer();
        for line in 0..4 {
            let mut previous = 0.0;
            for index in boundaries(&buffer, line) {
                buffer.goto(line, index);
                let (row, x) = buffer.caret().unwrap();
                assert_eq!(row, line);
                if index == 0 { assert_eq!(x, 0.0); }
                assert!(x >= previous, "line {line} index {index}: {x} < {previous}");
                previous = x;
            }
        }
    }

    #[test]
    fn clicking_where_the_caret_is_puts_the_cursor_back_there() {
        let mut buffer = buffer();
        buffer.goto(0, 10);
        // Without a usable font every glyph has no width and positions can't be told apart.
        if buffer.caret().unwrap().1 <= 0.0 { return; }
        for line in [0, 2, 3] {
            for index in boundaries(&buffer, line) {
                buffer.goto(line, index);
                let (row, x) = buffer.caret().unwrap();
                let hit = buffer.hit(row, x);
                assert_eq!((hit.line, hit.index), (line, index));
            }
        }
        // Left of the text is the line's start, right of it the line's end.
        let hit = buffer.hit(0, -50.0);
        assert_eq!((hit.line, hit.index), (0, 0));
        let hit = buffer.hit(2, 100_000.0);
        assert_eq!((hit.line, hit.index), (2, 12));
        // An empty line has one position.
        let hit = buffer.hit(1, 40.0);
        assert_eq!((hit.line, hit.index), (1, 0));
    }

    #[test]
    fn mouse_actions_pick_rows_by_height_and_drag_selects() {
        let mut buffer = buffer();
        // Rows are 20px tall: y 30 is on the second line, y 70 on the fourth.
        buffer.perform(cosmic_text::Action::Click { x: 0, y: 30 });
        assert_eq!((buffer.cursor.line, buffer.cursor.index), (1, 0));
        assert_eq!(buffer.selection, Selection::None);
        buffer.perform(cosmic_text::Action::Drag { x: 0, y: 70 });
        assert_eq!((buffer.cursor.line, buffer.cursor.index), (3, 0));
        assert_eq!(buffer.copy_selection().as_deref(), Some("\n    indented\n"));
        // Below the last row is still the last line.
        buffer.perform(cosmic_text::Action::Click { x: 0, y: 5000 });
        assert_eq!(buffer.cursor.line, 3);
        assert_eq!(buffer.copy_selection(), None);
        assert_eq!(buffer.undo_count(), 0, "mouse actions are not edits");
        buffer.perform(cosmic_text::Action::TripleClick { x: 0, y: 50 });
        assert_eq!(buffer.copy_selection().as_deref(), Some("    indented"));
    }

    #[test]
    fn selection_rects_cover_each_selected_line_once() {
        let mut buffer = buffer();
        buffer.goto(0, 4);
        let start_x = buffer.caret().unwrap().1;
        buffer.goto(2, 6);
        let end_x = buffer.caret().unwrap().1;
        buffer.select_range(Cursor::new(0, 4), Cursor::new(2, 6));
        let rects = buffer.selection_pixels().to_vec();
        assert_eq!(rects.iter().map(|rect| rect.0).collect::<Vec<_>>(), [0, 1, 2]);
        assert_eq!(rects[0].1, start_x);
        assert_eq!((rects[1].1, rects[1].2), (0.0, 0.0), "an empty line has an empty rect");
        assert_eq!((rects[2].1, rects[2].2), (0.0, end_x));
        assert!(rects.iter().all(|rect| rect.2 >= rect.1));
        assert!(buffer.has_selection());
        // A selection that covers nothing is not a selection.
        buffer.select_range(Cursor::new(0, 4), Cursor::new(0, 4));
        assert!(!buffer.has_selection());
    }
}

/// Word wrap through the real text engine. Where a line breaks depends on the fonts a machine
/// has, so these assert only what holds for any font, and the exact breaking is tested on
/// hand-written glyphs in `rows.rs`.
#[cfg(test)]
mod wrap_tests {
    use super::*;
    use cosmic_text::{Action, Motion};

    const LONG: &str = "    let message = \"a line that is long enough to need several rows when the view is narrow\";";

    fn buffer() -> Buffer {
        Buffer::new(&format!("fn main() {{\n{LONG}\n}}\n"), Metrics::new(14.0, 20.0))
    }

    /// A text width about a third of the long line's, or `None` on a machine without a
    /// usable font, where glyphs have no width and nothing can wrap.
    fn narrow(buffer: &mut Buffer) -> Option<f32> {
        buffer.goto(1, LONG.len());
        let width = buffer.caret()?.1;
        buffer.goto(0, 0);
        (width > 0.0).then_some(width / 3.0)
    }

    fn rows(buffer: &Buffer, line: usize) -> Vec<Row> {
        buffer.rows_of(line).map(|row| buffer.row(row).unwrap().clone()).collect()
    }

    #[test]
    fn wrapping_needs_the_setting_and_a_width_and_can_be_undone() {
        let mut buffer = buffer();
        let Some(width) = narrow(&mut buffer) else { return; };
        // A width alone changes nothing, and doesn't count as a new layout.
        buffer.set_viewport(width, 200.0);
        assert_eq!((buffer.visible_count(), buffer.layout_epoch()), (4, 0));
        assert_eq!(buffer.viewport(), (width, 200.0));
        buffer.set_wrap(true);
        assert!(buffer.wrapped());
        assert!(buffer.rows_of(1).len() >= 3, "{:?}", buffer.rows_of(1));
        assert_eq!((buffer.rows_of(0).len(), buffer.rows_of(2).len(), buffer.rows_of(3).len()), (1, 1, 1));
        assert_eq!(buffer.layout_epoch(), 1);
        // A wider view needs fewer rows; the same width again is not a new layout.
        let narrow_rows = buffer.visible_count();
        buffer.set_viewport(width * 2.0, 200.0);
        assert!(buffer.visible_count() < narrow_rows);
        assert_eq!(buffer.layout_epoch(), 2);
        buffer.set_viewport(width * 2.0, 300.0);
        assert_eq!(buffer.layout_epoch(), 2);
        buffer.set_wrap(false);
        assert_eq!(buffer.visible_count(), 4);
        for line in 0..4 { assert_eq!(buffer.visual_row(line), Some(line)); }
    }

    #[test]
    fn wrap_without_a_width_yet_leaves_lines_whole() {
        let mut buffer = buffer();
        buffer.set_wrap(true);
        assert_eq!(buffer.visible_count(), 4);
    }

    #[test]
    fn wrapped_rows_cover_the_line_once_and_every_position_round_trips() {
        let mut buffer = buffer();
        let Some(width) = narrow(&mut buffer) else { return; };
        buffer.set_viewport(width, 200.0);
        buffer.set_wrap(true);
        let long = rows(&buffer, 1);
        assert_eq!((long[0].start, long.last().unwrap().end), (0, LONG.len()));
        assert!(long.windows(2).all(|pair| pair[0].end == pair[1].start));
        // Rows after the first start at the line's indentation.
        assert_eq!(long[0].indent, 0.0);
        assert!(long[1].indent > 0.0);
        assert!(long[1..].iter().all(|row| row.indent == long[1].indent));
        let first_row = buffer.visual_row(1).unwrap();
        for index in 0..=LONG.len() {
            // `goto` leaves the caret on the earlier row where the line wraps at `index`.
            buffer.goto(1, index);
            let (row, x) = buffer.caret().unwrap();
            let shown = &long[row - first_row];
            assert!(shown.start <= index && index <= shown.end, "index {index} on row {row}");
            let hit = buffer.hit(row, x);
            assert_eq!((hit.line, hit.index), (1, index));
            // The same position counted from the row that starts there.
            let (row, x) = buffer.locate(1, index).unwrap();
            let hit = buffer.hit(row, x);
            assert_eq!((hit.line, hit.index), (1, index));
        }
        // The lines after a wrapped one sit below all its rows.
        assert_eq!(buffer.visual_row(2), Some(first_row + long.len()));
        assert_eq!(buffer.source_line(first_row + long.len() - 1), 1);
    }

    #[test]
    fn selection_folding_and_edits_follow_wrapped_rows() {
        let mut buffer = buffer();
        let Some(width) = narrow(&mut buffer) else { return; };
        buffer.set_viewport(width, 200.0);
        buffer.set_wrap(true);
        // Selecting everything puts one rect on every row, in order.
        buffer.select_all();
        let selected: Vec<usize> = buffer.selection_pixels().iter().map(|rect| rect.0).collect();
        assert_eq!(selected, (0..buffer.visible_count()).collect::<Vec<_>>());
        // A selection inside one row of the wrapped line touches only that row.
        let second = rows(&buffer, 1)[1].clone();
        buffer.select_range(Cursor::new(1, second.start), Cursor::new(1, second.start + 3));
        assert_eq!(buffer.selection_pixels().len(), 1);
        assert_eq!(buffer.selection_pixels()[0].0, buffer.visual_row(1).unwrap() + 1);
        assert_eq!(buffer.selection_pixels()[0].1, second.indent);
        // Folding the function hides the wrapped line's rows with it.
        assert!(buffer.folds.contains_key(&0));
        buffer.toggle_fold(0);
        assert_eq!(buffer.rows_of(1), 0..0);
        assert_eq!((buffer.visual_row(1), buffer.visual_row(2)), (None, None));
        assert_eq!(buffer.visible_count(), 2);
        buffer.toggle_fold(0);
        assert!(buffer.rows_of(1).len() >= 3);
        // Typing makes a short line wrap, and undoing puts it back on one row.
        buffer.goto(2, 1);
        for ch in LONG.chars() { buffer.perform(Action::Insert(ch)); }
        assert!(buffer.rows_of(2).len() >= 3);
        assert_eq!(buffer.text(), format!("fn main() {{\n{LONG}\n}}{LONG}\n"));
        while buffer.can_undo() { buffer.undo(); }
        assert_eq!(buffer.rows_of(2).len(), 1);
    }

    /// A measurement, not a check: prints what a 50,000-line file costs with wrap on and off.
    /// `cargo test large_file_timing -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn large_file_timing() {
        use std::time::Instant;
        let line = "    let value = compute(first_argument, second_argument) + another_call(third) * 2; // trailing comment";
        let text = vec![line; 50_000].join("\n");
        let start = Instant::now();
        let mut buffer = Buffer::new(&text, Metrics::new(14.0, 20.0));
        println!("open:                    {:?}", start.elapsed());
        buffer.set_viewport(400.0, 800.0);
        let start = Instant::now();
        buffer.perform(Action::Insert('x'));
        println!("keystroke, wrap off:     {:?}", start.elapsed());
        let start = Instant::now();
        buffer.set_wrap(true);
        println!("wrap on:                 {:?} ({} rows)", start.elapsed(), buffer.visible_count());
        let start = Instant::now();
        buffer.perform(Action::Insert('x'));
        println!("keystroke, wrap on:      {:?}", start.elapsed());
        let start = Instant::now();
        buffer.set_viewport(401.0, 800.0);
        println!("resize, wrap on:         {:?}", start.elapsed());
        let start = Instant::now();
        buffer.perform(Action::Motion(Motion::Down));
        println!("arrow down, wrap on:     {:?}", start.elapsed());
    }

    #[test]
    fn movement_goes_row_by_row_through_a_wrapped_line() {
        let mut buffer = buffer();
        let Some(width) = narrow(&mut buffer) else { return; };
        buffer.set_viewport(width, 200.0);
        buffer.set_wrap(true);
        let long = rows(&buffer, 1);
        let first_row = buffer.visual_row(1).unwrap();
        // Down from the line above lands on each row of the wrapped line in turn.
        buffer.goto(0, 0);
        for step in 0..long.len() {
            buffer.perform(Action::Motion(Motion::Down));
            assert_eq!(buffer.cursor.line, 1);
            assert_eq!(buffer.caret().unwrap().0, first_row + step);
        }
        buffer.perform(Action::Motion(Motion::Down));
        assert_eq!(buffer.cursor.line, 2);
        buffer.perform(Action::Motion(Motion::Up));
        assert_eq!(buffer.caret().unwrap().0, first_row + long.len() - 1);
        // End and Home stay on the row the caret is on.
        buffer.goto(1, long[1].start + 2);
        let row = buffer.caret().unwrap().0;
        buffer.perform(Action::Motion(Motion::End));
        assert_eq!((buffer.cursor.index, buffer.caret().unwrap().0), (long[1].end, row));
        buffer.perform(Action::Motion(Motion::Home));
        assert_eq!((buffer.cursor.index, buffer.caret().unwrap().0), (long[1].start, row));
        // Clicking on a row of the wrapped line puts the cursor in that row's text.
        buffer.perform(Action::Click { x: 100_000, y: ((first_row + 1) * 20 + 10) as i32 });
        assert_eq!((buffer.cursor.line, buffer.cursor.index), (1, long[1].end));
        assert_eq!(buffer.caret().unwrap().0, first_row + 1);
    }
}

#[cfg(test)]
mod clipboard_tests {
    use super::*;
    #[test]
    fn cut_and_paste_are_single_undo_steps() {
        let mut buffer = Buffer::new("hello world", Metrics::new(14.0, 20.0));
        assert_eq!(buffer.copy_selection(), None);
        buffer.select_range(Cursor::new(0, 0), Cursor::new(0, 5));
        assert_eq!(buffer.copy_selection().as_deref(), Some("hello"));
        assert_eq!(buffer.cut_selection().as_deref(), Some("hello"));
        assert_eq!(buffer.text(), " world");
        buffer.replace_selection("hi\nthere");
        assert_eq!(buffer.text(), "hi\nthere world");
        assert_eq!(buffer.undo_count(), 2);
        buffer.undo();
        assert_eq!(buffer.text(), " world");
        buffer.undo();
        assert_eq!(buffer.text(), "hello world");
    }

    #[test]
    fn select_all_selects_the_whole_document_from_anywhere() {
        let source = "first\nsecond\n\nlast";
        let mut buffer = Buffer::new(source, Metrics::new(14.0, 20.0));
        buffer.select_all();
        assert_eq!(buffer.copy_selection().as_deref(), Some(source));
        assert_eq!((buffer.cursor.line, buffer.cursor.index), (3, 4));
        assert!(buffer.has_selection());
        // Again from the middle, over an existing selection.
        buffer.select_range(Cursor::new(1, 1), Cursor::new(1, 3));
        buffer.select_all();
        assert_eq!(buffer.copy_selection().as_deref(), Some(source));
        // Selecting all is not an edit, and typing then replaces everything.
        assert_eq!(buffer.undo_count(), 0);
        buffer.perform(cosmic_text::Action::Insert('x'));
        assert_eq!(buffer.text(), "x");
        let mut empty = Buffer::new("", Metrics::new(14.0, 20.0));
        empty.select_all();
        assert_eq!(empty.copy_selection(), None);
    }

    #[test]
    fn ranges_read_back_what_a_replacement_will_cover() {
        let mut buffer = Buffer::new("fn a() {\n    one();\n}\ntail", Metrics::new(14.0, 20.0));
        assert_eq!(buffer.selection_bounds(), None);
        // Selected backwards: bounds still come out in document order.
        buffer.select_range(Cursor::new(2, 1), Cursor::new(0, 3));
        let (start, end) = buffer.selection_bounds().unwrap();
        assert_eq!(((start.line, start.index), (end.line, end.index)), ((0, 3), (2, 1)));
        assert_eq!(buffer.text_between(start, end).as_deref(), Some("a() {\n    one();\n}"));
        assert_eq!(buffer.text_between(Cursor::new(3, 0), Cursor::new(3, 4)).as_deref(), Some("tail"));
        assert_eq!(buffer.text_between(Cursor::new(3, 0), Cursor::new(3, 9)), None);
        assert_eq!(buffer.text_between(Cursor::new(9, 0), Cursor::new(9, 0)), None);
        assert_eq!(buffer.text_between(end, start), None);
        buffer.replace_range(start, end, "b() {}");
        assert_eq!(buffer.text(), "fn b() {}\ntail");
        assert_eq!(buffer.undo_count(), 1);
    }
}

#[cfg(test)]
mod deletion_tests {
    use super::*;

    #[test]
    fn empty_selection_does_not_swallow_deletion_at_semicolon() {
        for (action, index, expected) in [
            (cosmic_text::Action::Backspace, 2, "x"),
            (cosmic_text::Action::Delete, 1, "x"),
        ] {
            let mut buffer = Buffer::new("x;", Metrics::new(14.0, 20.0));
            buffer.cursor = Cursor::new(0, index);
            let mut anchor = buffer.cursor;
            anchor.affinity = cosmic_text::Affinity::Before;
            buffer.cursor.affinity = cosmic_text::Affinity::After;
            buffer.selection = Selection::Normal(anchor);
            buffer.perform(action);
            assert_eq!(buffer.text(), expected);
            assert_eq!(buffer.undo_count(), 1);
            buffer.undo();
            assert_eq!(buffer.text(), "x;");
        }
    }

    #[test]
    fn backspace_still_deletes_nonempty_selection() {
        let mut buffer = Buffer::new("abc;", Metrics::new(14.0, 20.0));
        buffer.select_range(Cursor::new(0, 1), Cursor::new(0, 4));
        buffer.perform(cosmic_text::Action::Backspace);
        assert_eq!(buffer.text(), "a");
    }
}
