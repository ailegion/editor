# Word wrap plan

Plan for adding word wrap to the code editor (`src/code_editor/`) as a core feature. Agreed
with the project owner on 2026-10-02. A session picking this up should read this whole file,
then `AGENTS.md`, then continue from the first unchecked stage under "Progress".

## Progress

Tick a stage only when its work is done **and** the full `cargo test` passes. Add a dated
line under "Log" for anything a later session needs to know (a decision, a surprise, a
deviation from this plan). A deviation from the plan needs the owner's confirmation first.

- [x] Stage 0 — tests pinning today's behaviour
- [x] Stage 1 — row map introduced, every position calculation switched to it
- [x] Stage 2 — line breaker
- [x] Stage 3 — cursor, selection and bracket rectangles per row
- [x] Stage 4 — drawing per row
- [x] Stage 5 — mouse, scrolling, width reporting, scroll anchoring
- [x] Stage 6 — cursor movement by screen row
- [x] Stage 7 — the setting, menu, palette, shortcut
- [x] Stage 8 — large-file timing (see log) and the owner's hand check

### Log

- 2026-10-02: plan written. The owner approved starting, and confirmed the indent cap in
  decision 4.
- 2026-10-02: stages 0-7 built in one session; `cargo test` green on Windows (187 passed).
  **Nobody has yet looked at it on screen.** Drawing, the scrollbar, scroll anchoring and
  the Alt+Z shortcut are untested by hand; the "Definition of done" list is the checklist.
- 2026-10-02: the owner tested it by hand ("as much as I could") and reported it works.
  That supersedes the "nobody has yet looked at it" note above. Not every item on the
  "Definition of done" list is known to have been tried.
- Where things are: `rows.rs` (row map, line breaker, hit testing, all on plain glyph
  extents), `buffer.rs` (`rebuild_rows`, `perform_pointer`, `perform_row_motion`,
  `set_wrap`, `set_viewport`), `render.rs` (draws by row), `mod.rs` (`top_scroll`,
  viewport reporting in the `RedrawRequested` branch), `main.rs` (`toggle_word_wrap`,
  `Message::EditorViewport`, `ViewAction::ToggleWordWrap`).
- Stage 0 tests are `geometry_tests` in `buffer.rs`. They were written against the old code,
  passed there, and passed unedited after the switch to the row map.
- Decision 1 confirmed by tests that failed before the fix: Down through a short line landed
  on column 0 instead of the starting column, and PageDown did not move.
- Mouse actions (`Click`, `Drag`, `DoubleClick`, `TripleClick`) now carry screen-row
  coordinates (x within the row, y = row x line height with scroll added) and are resolved
  by `Buffer::perform_pointer`, not by the engine.
- `Buffer::goto` keeps the engine's default cursor affinity. Changing it broke a completion
  test that compares whole cursors, so at a wrap point `goto` shows the caret at the end of
  the earlier row.
- Rows are rebuilt only when `rows_stale` is set (text or styling, folds, wrap, width,
  metrics). Any new way of changing the laid-out text must set it.
- `Buffer::set_size` was removed: it was unused, and giving the engine a size would break
  the "engine never wraps" design.
- New direct dependency `unicode-segmentation` (already in the tree via cosmic-text), for
  grapheme boundaries in click-to-cursor. `Cargo.lock` changed accordingly.
- Behaviour changes with wrap off, beyond decision 1: Up on the first row goes to the start
  of the text and Down on the last row to its end; Up/Down step over a collapsed fold
  instead of opening it; on zoom the same text stays at the top of the view; the caret and
  selection edges are placed at fractional pixels rather than whole ones.
- Timing, 50,000 lines of ~105 characters, every line wrapping, release build, one Windows
  machine: keystroke 11 ms with wrap off and 50 ms with wrap on; resize 44 ms; cursor move
  3 ms. The wrap-on keystroke cost is the full row rebuild. If that is too slow in use, the
  next step is to rebuild only the lines an edit touched. Measured by the ignored test
  `large_file_timing` in `buffer.rs`.
- Engine-backed wrap tests return early on a machine with no usable font (glyphs without
  width), since nothing can wrap there. The breaking rules themselves are tested without
  fonts in `rows.rs`.

## Goal

Word wrap is a property of the editor, on or off, and every editor feature works the same in
both states. With wrap off the editor behaves exactly as it does today. This is not a
chat-only or preview-only mode and must not be built as a special case beside the existing
code.

Why now: it is item 2 under "Next — Editing" in `ROADMAP.md`, and it is the one thing the
editor component lacks before it can show prose (for example AI replies, which today use
iced's Markdown widget and so have no text selection).

## Decisions made by the owner

1. Up/Down keep their horizontal position when passing through a shorter line, and
   PageUp/PageDown move one screenful, "like all other editors". This changes behaviour with
   wrap off too, and that is accepted. Each must first be shown broken by a failing test.
2. The current-line highlight covers all rows of a wrapped line.
3. One global setting, default off.
4. Continuation rows are indented to the line's own indentation, as VS Code and Sublime do.
   The indent is capped at half the text width so a deeply nested line still has room for
   text.

## Design

### One code path

Today every position is computed as `line number x line height`, in roughly 25 places across
`buffer.rs`, `render.rs` and `mod.rs`. Do not add `if wrap { .. }` beside each one. Instead a
single **row map** answers every position question and all of those places ask it:

- which screen row (line, byte index) sits on, and at what x;
- which (line, byte index) is at screen row R, x;
- how many rows there are in total.

With wrap off every line has exactly one row, so the row map returns the same numbers as
today's arithmetic. There is no separate no-wrap path.

Folding already hides lines through `Buffer::visible_lines` / `visual_row` / `source_line`.
The row map replaces that table, so folding and wrapping are one mechanism: a folded-away
line has zero rows, a wrapped line has several.

### The text engine never wraps

cosmic-text stays configured as it is today (`Wrap::None`, no width set). It lays out each
buffer line as one long row. The row map slices that row into screen rows itself:

- take the line's laid-out glyphs (`LayoutLine::glyphs`: `start`, `end`, `x`, `w`);
- fill a row until the available width is used up;
- break at the last whitespace boundary, or inside a word only when a single word is wider
  than the row;
- the first row starts at x = 0; continuation rows start at the line's indentation (the x of
  its first non-whitespace glyph, subject to the cap in decision 4) and so have less width.

A glyph on row k is drawn at `glyph.x - row_start_x + row_indent`.

Consequences: shaping, tab stops and glyph positions are untouched; changing the width only
re-slices (no engine re-layout); nothing depends on the engine's own wrapping, its `hit`, or
its `Home`/`End`/`Up`/`Down` motions.

### Pieces

**A. Row map — new file `src/code_editor/rows.rs`**
- Works on plain data of our own (per row: byte range, start x, indent; per glyph: byte
  range, x, width), extracted from the engine by a thin adapter. No engine types in its API.
  This is what makes it testable without fonts.
- Rebuilt when the text, the fold state, the wrap setting, the wrap width or the metrics
  (zoom) change.

**B. Buffer — `src/code_editor/buffer.rs`**
- `set_wrap(bool)`, `set_wrap_width(f32)`.
- `sync()` builds the row map. Cursor position, selection rectangles and bracket-match
  rectangles become per row: `(row, x0, x1)` instead of `(line, x0, x1)`.
- Selection rectangles are computed straight from the row data. Today `sync()` finds them by
  calling `Editor::cursor_position` per selected line, which walks every layout run each time.
- `visible_count` / `visual_row` / `source_line` are replaced by row-map queries.
- With wrap on, `content_width` is not used (no horizontal scroll).

**C. Drawing — `src/code_editor/render.rs`**
- Text: one pass per row.
- Line number, fold arrow, diagnostic gutter dot, indent guides: the line's first row only.
- Git diff bar and current-line highlight: all rows of the line.
- Diagnostic underline: one segment per row its range touches.
- Blame annotation and the "⋯ n lines" fold marker: after the line's last row.
- Caret: on its row. At a wrap boundary the cursor's affinity decides between the end of one
  row and the start of the next.

**D. Mouse and scrolling — `src/code_editor/mod.rs`**
- `cell_at`, `buffer_position`, `anchor_of`, `caret_anchor`, `scroll_correction`, `track`,
  `thumb`, `max_scroll`, `reveal_row` all go through the row map. Click and drag set the
  cursor from the row map's hit result rather than from `cosmic_text::Action::Click`'s own
  hit test.
- With wrap on: `scroll_x` is 0, no horizontal thumb, `max_scroll_x` is 0.
- Wrap width = canvas width - gutter width - `BAR` (the vertical scrollbar). The canvas
  publishes it on `RedrawRequested` when it differs from the buffer's, the same pattern
  `terminal.rs` uses for `Message::Resize`. See `ICED_NOTES.md`, "canvas::Program: reacting
  to state that changed outside the widget".
- Scroll stays anchored to text: when the row map changes shape (wrap toggled, width or zoom
  changed), the buffer line at the top of the view stays at the top.
- Keep `V_SCROLL_PAD` (space below the last line, clear of the horizontal scrollbar).

**E. Cursor movement — `src/code_editor/input.rs`, `buffer.rs`**
- Up/Down move one screen row, to the position nearest a remembered goal x. The goal x is
  kept on `Buffer` and reset by any horizontal move or edit.
- Home/End go to the start/end of the screen row.
- PageUp/PageDown move by the number of rows visible; `Buffer` learns the view height from
  the same message that carries the wrap width.
- Shift-extend keeps working through `input::handle_key` as today.

**F. The setting — `src/main.rs`**
- A global `word_wrap: bool`, saved with `config_path("word_wrap")` like `zoom` is
  (`save_zoom` / `load_zoom`), default off, applied to every tab's buffer and to buffers
  created later (`Buffer::new` call sites).
- A `ViewAction`, an entry in the View menu, a command palette entry, and Alt+Z.

## Stages

Each stage ends with the full `cargo test` green. "Visible change" is what a user notices.

| Stage | What | Visible change |
|---|---|---|
| 0 | Tests pinning today's behaviour: line/row mapping with folds, cursor and selection rectangles, click-to-cursor, scroll range and scroll-to-cursor | None |
| 1 | Row map with one row per line; every position calculation switched to it; `visible_lines` removed | None. Stage 0 tests pass without being edited |
| 2 | Line breaker in the row map: indented rows at a given width | None (not switched on) |
| 3 | Cursor, selection and bracket rectangles per row | None |
| 4 | Drawing per row | None |
| 5 | Mouse, scrolling, scrollbar, width reporting, scroll anchoring | None |
| 6 | Cursor movement by screen row | Decision 1 takes effect |
| 7 | Setting, menu, palette, shortcut | Wrap can be turned on |
| 8 | Timing on a very large file; owner's hand check of the list below | - |

Stage 1 is the risky one: it rewires every position calculation while changing nothing
visible. The stage 0 tests are its guard and must not be weakened to make it pass.

## Definition of done

Each of these works with wrap on, and is unchanged with wrap off:

- Typing, deleting, undo/redo, paste, auto-close brackets
- Arrow keys, Home/End, PageUp/PageDown, with and without Shift
- Click, drag-select, double and triple click
- Select all, cut, copy
- Scrolling, scrollbar drag, scroll-to-cursor, last line clear of the bottom edge
- Line numbers, folding, fold arrows
- Git diff bars, blame
- Diagnostic underlines and gutter dots
- Bracket matching
- Hover popup, go-to-definition, completion popup position
- Find/replace highlight, go to line, Find in Project jump
- Zoom
- Inline AI edit
- Markdown preview toggle on a wrapped file

## Tests

The repository ships no text font, so glyph widths, and therefore where a line wraps, depend
on the fonts installed on each machine. Per `AGENTS.md`, a test must hold on Windows, Linux
and macOS.

- Row map and line breaker: tested on hand-written row and glyph data. No fonts. This covers
  mapping both ways, folding combined with wrapping, indentation and its cap, selection
  rectangles, scroll ranges, vertical movement by goal x.
- Tests that go through the real engine assert only what is true under any font: wrap off
  gives one row per line; a line's rows cover its bytes exactly once and in order; cursor ->
  position -> cursor returns the same place at every character boundary.
- Never assert a specific wrap column or pixel value that came from a real font.
- Drawing cannot be unit-tested here; stage 8 is a hand check by the owner.

## Facts established while planning

Read from the source; re-check if versions change (`Cargo.lock`: cosmic-text 0.15.0).

- `Buffer::new` sets `Wrap::None` and never sets a size, so the engine shapes and lays out
  every line and keeps its own scroll at zero. `Buffer::set_size` exists but is unused.
- `render.rs` and `Buffer::sync` read only the first `LayoutLine` of each buffer line
  (`layout_opt().first()`).
- `LayoutLine::glyphs` gives `start`, `end`, `x`, `w` per glyph; `render::x_at` already maps a
  byte offset to x from them.
- The engine's `Motion::PageUp`/`PageDown` do nothing unless the buffer has a height
  (`height_opt`), which this editor never sets. Confirmed by a failing test; these motions
  no longer reach the engine.
- The engine's `Motion::Up`/`Down` carry the column in `cursor_x_opt` on the `Editor`.
  `Buffer::perform` creates a new `Editor` per action, so that value is lost between
  keypresses. Confirmed by a failing test; these motions no longer reach the engine.
- `Buffer::perform` drops an empty selection only for Backspace/Delete. Shift+arrow selection
  and Select All depend on that; tests in `input.rs` and `buffer.rs` cover it.
- Each `Buffer` owns its own `FontSystem` (`FontSystem::new()` in `Buffer::new`). Not part of
  this plan, but it matters if many small buffers are ever shown at once.

## Not in this plan

- Showing AI replies or the Markdown preview in the editor component. Wrap is a prerequisite
  for that; the work itself is a separate decision.
- Per-file wrap settings, or wrap on by default for Markdown and text files.
- A shared font system across buffers.

## Uncertain

- Size: roughly 900-1,200 lines changed or added. An estimate.
- Cost of rebuilding the row map after each keystroke on a very large file. It is arithmetic
  over the glyphs and expected to be fine; measured in stage 8. If it is slow, rebuild only
  the lines that changed.
