# iced 0.14 notes

Facts about this project's `iced` version that took real digging (grepping vendored crate
source, since docs.rs for this exact pinned version/feature-set isn't always trustworthy)
to establish. Read this before re-deriving any of it from scratch.

Pinned versions (from `Cargo.lock`): `iced 0.14.0`, `iced_widget 0.14.2`, `iced_core 0.14.0`,
`iced_runtime 0.14.0`, `iced_futures 0.14.0`, `iced_aw 0.14.1`.

## Finding the source fast

Cargo vendors full source for every dependency under the registry cache. Locate it once per
machine (the hash in the path is registry-instance-specific, so don't hardcode it):

```sh
find ~/.cargo/registry/src -maxdepth 1 -iname "iced*"
```

That lists every `iced_*` crate (widget, core, runtime, futures, winit, wgpu, aw, fonts,
highlighter, ...) as a real directory of `.rs` files — grep it directly instead of guessing
from memory or trusting docs.rs (which may show a different version/feature set than what's
actually pinned here). This was faster and more reliable all session than anything else.

## Crate layering (who defines what)

- `iced_core` — foundational types: `Color`, `Border`, `Padding`, `widget::Id`,
  `widget::operation::*` (the low-level `Operation` trait impls: `focusable::focus`,
  `scrollable::snap_to`/`scroll_to`, etc.)
- `iced_runtime` — wraps those raw `Operation`s into ergonomic `Task`-returning functions:
  `iced_runtime::widget::operation::{focus, focus_next, focus_previous, snap_to, scroll_to,
  scroll_by, select_all, ...}`, each `fn(...) -> Task<T>`.
- `iced_widget` — the actual widgets (`text_input`, `scrollable`, `stack`, `mouse_area`,
  `pane_grid`, ...) plus the `column!`/`row!`/`stack!`/`grid!` macros (in `helpers.rs`).
- `iced_futures` — `Subscription` machinery, including `keyboard::listen()` and
  `event::listen`/`listen_with`/`listen_raw` (see "Global key routing" below).
- `iced` (the umbrella crate) — re-exports all of the above under `iced::widget`,
  `iced::event`, `iced::keyboard`, etc. When something doesn't type-check where you'd
  expect, check which sub-crate actually defines it and whether the umbrella re-exports it
  at the path you assumed.

## `iced::widget::operation` — the module to check before writing custom widget-state hacks

Re-exported from `iced_runtime::widget::operation` at `iced::widget::operation::*`. Each
function returns `Task<T>` directly (no manual `Task::widget(...)`/`Action::widget(...)`
wiring needed — that's already done inside):

```rust
iced::widget::operation::focus::<Message>(id: impl Into<widget::Id>) -> Task<Message>
iced::widget::operation::focus_next/focus_previous::<Message>() -> Task<Message>
iced::widget::operation::snap_to::<Message>(id, RelativeOffset) -> Task<Message>
iced::widget::operation::scroll_to::<Message>(id, AbsoluteOffset) -> Task<Message>
iced::widget::operation::scroll_by::<Message>(id, AbsoluteOffset) -> Task<Message>
iced::widget::operation::select_all::<Message>(id) -> Task<Message>
```

Usage pattern (used in `quick_open.rs` / `command_palette.rs` to autofocus a search box on
open, and to keep the selected row scrolled into view on arrow-key nav):

```rust
fn query_input_id() -> iced::widget::Id { iced::widget::Id::new("my-input") }

// give the widget that id:
text_input("...", &query).id(query_input_id())

// then, from update(), to focus it:
iced::widget::operation::focus(query_input_id())   // -> Task<Message>
```

`text_input::Id` / `scrollable::Id` are **not** distinct types — every widget's `.id()`
builder takes `impl Into<iced::widget::Id>` (the one generic id type). Don't go looking for
a per-widget `Id` type.

`RelativeOffset`/`AbsoluteOffset` are re-exported at both `iced::widget::operation::*` and
`iced::widget::scrollable::*` — either path works. `RelativeOffset { x, y }` ranges `0.0`
(start) to `1.0` (end) along that axis; there's no built-in "scroll this exact row into
view" operation, so proportional snapping (`selected as f32 / (len - 1) as f32`) is the
pragmatic approximation this codebase uses (see `scroll_to_selected` in `quick_open.rs`).

## Global key routing: `keyboard::listen()` silently drops captured events

This app dispatches most keys through one global `Subscription` (`subscription()` in
`main.rs`) rather than iced's per-widget focus system, because the custom canvas-based code
editor (`code_editor/`) and the directory tree don't participate in that system (see
`code_editor/input.rs`'s doc comment). This works *only* for keys no other focused widget
captures first.

`iced::keyboard::listen()` (defined in `iced_futures::keyboard`) is implemented as exactly:

```rust
pub fn listen() -> Subscription<Event> {
    subscription::filter_map(Listen, move |event| match event {
        subscription::Event::Interaction { event: core::Event::Keyboard(event), status: Status::Ignored, .. } => Some(event),
        _ => None,
    })
}
```

i.e. it **only forwards events the widget tree didn't capture**. `text_input` captures
`Escape` itself (to blur, see `iced_widget::text_input`'s key-press handling — it sets
`state.is_focused = None` and marks the event `Captured`) — so if you're relying on a global
Escape handler to close an overlay while a `text_input` inside it has focus, the first
Escape only blurs the input; only the *second* press (now nothing left to capture it) reaches
your subscription. This bit us once already (see git history around the quick-open /
command-palette overlays).

Fix: use `iced::event::listen_with(f)` instead, which exposes the `Status` to your filter
function so you can choose to let specific keys through regardless of capture:

```rust
fn handle_raw_key_event(event: iced::Event, status: iced::event::Status, _window: iced::window::Id) -> Option<Message> {
    let iced::Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) = event else { return None };
    let is_escape = key == keyboard::Key::Named(keyboard::key::Named::Escape);
    if status == iced::event::Status::Ignored || is_escape {
        Some(Message::KeyPressed(key, modifiers))
    } else {
        None
    }
}
// subscription(): iced::event::listen_with(handle_raw_key_event)
```

Constraint: `listen_with`'s callback must be a plain `fn` pointer (`fn(Event, Status, window::Id) -> Option<Message>`), not a capturing closure — it can't read app state directly, only the event/status/window it's handed.

## Building a modal/overlay (no `Modal` widget in this iced_aw version)

`iced_aw 0.14.1` has no `Modal`/`modal` widget (checked its `src/widget/` directory
directly — only `context_menu`, `card`, `selection_list`, `drop_down`, etc.). Build overlays
from base `iced`/`iced_widget` primitives instead:

- `iced::widget::stack![base, overlay]` (macro, like `column!`/`row!`) — Z-layers elements;
  later ones render on top. Used in `main.rs`'s `view()` to lay the quick-open/command-palette
  panels over the whole app.
- `iced::widget::mouse_area(content).on_press(Message::Close)` — wraps `content` (typically a
  full-screen dim backdrop + centered panel, itself built with `stack!`) and fires when
  clicked. Clicks on interactive children *inside* `content` (buttons, text inputs) are
  consumed by those widgets first and don't also trigger the backdrop's `on_press` — this is
  what makes "click outside the panel to close it" work without extra bookkeeping.

See `quick_open.rs` / `command_palette.rs` for the full pattern (backdrop + centered fixed-width
panel + `mouse_area` wrapper), both close to copy-paste-able for a third overlay if one's ever
needed.

## Misc gotchas

- **`iced::Padding` has no `From<[T; 4]>`.** Only `From<f32>` (all sides), `From<[f32; 2]>`
  / `From<[u16; 2]>` (`[top/bottom, left/right]`), and `From<u16>`/`From<Pixels>`. For
  asymmetric padding (e.g. "top only"), construct the struct literal directly:
  `iced::Padding { top: 80.0, ..iced::Padding::default() }`.
- **`iced::Border` is a builder**: `Border::default().rounded(8.0).color(c).width(1.0)` —
  all three chain off a `Border` value, not off the container's `Style`.
- **`Color::inverse()`** exists on `iced_core::Color` (flips r/g/b, keeps alpha) — handy for
  a theme-adaptive dim backdrop without hand-picking light/dark constants.
- Recursive `update()` calls are fine and are the idiom this codebase uses for "message A,
  when handled, should also trigger message B" (e.g. a command-palette entry dispatching a
  `FileAction`): just call `update(state, other_message)` from inside a match arm and fold
  the resulting `Task` into the one you return (`Task::batch([...])`).

## Where to look in *this* codebase for more examples

- `src/code_editor/` — canvas-based (`iced::widget::canvas::Program`) custom widget, for
  when you need to go below the standard widget set entirely (custom text rendering via
  `cosmic-text`, manual mouse/keyboard handling).
- `src/quick_open.rs`, `src/command_palette.rs` — overlay/modal pattern, fuzzy search,
  scroll-to-selection, autofocus.
- `src/git.rs`, `src/project_search.rs` — plain sidebar-panel widgets (no overlay), `Task`
  returned from `update()` for async work (`Task::perform`/`spawn_blocking`).
