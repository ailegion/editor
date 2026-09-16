# iced-swdir-tree

[![crates.io](https://img.shields.io/crates/v/iced-swdir-tree?label=rust)](https://crates.io/crates/iced-swdir-tree)
[![License](https://img.shields.io/github/license/nabbisen/iced-swdir-tree)](https://github.com/nabbisen/iced-swdir-tree/blob/main/LICENSE)
[![Documentation](https://docs.rs/iced-swdir-tree/badge.svg?version=latest)](https://docs.rs/iced-swdir-tree)
[![Dependency Status](https://deps.rs/crate/iced-swdir-tree/latest/status.svg)](https://deps.rs/crate/iced-swdir-tree)

> Tree widgets for [iced](https://iced.rs) — lazy-loading directory
> views and generic in-memory trees, with multi-select,
> drag-and-drop, keyboard control, and live search.

## Overview

Two widgets that share one navigation model:

- **`DirectoryTree`** — a lazy, async, cache-backed view of a real
  filesystem directory, built on
  [swdir](https://crates.io/crates/swdir)'s `scan_dir`. It expands one
  folder at a time and never blocks the UI thread on disk I/O.
- **`ItemTree<T>`** — a synchronous, in-memory tree over your own data
  `T`, addressed by stable `NodeId`s, with key-based diffing that
  preserves expansion and selection across edits.

Both own **UI state only** — they never rename, delete, move, or write.
Drag-and-drop reports the user's intent as an event and your app
performs the change.

## When to use it

Reach for `DirectoryTree` when your iced app needs a file/folder
picker, a project-navigator pane, drag-and-drop between folders, or a
searchable directory view. Reach for `ItemTree<T>` when you have your
own in-memory hierarchy — an outline, a category browser, a scene
graph — that needs selection, keyboard control, search, and
reorder/nest drag-and-drop.

If you only need a one-shot "pick a file" dialog, the OS-native
file-chooser is a better fit.

## Quick start

```toml
[dependencies]
iced = "0.14"
iced-swdir-tree = "0.9"
```

```rust,no_run
use std::path::PathBuf;
use iced::{Element, Task};
use iced_swdir_tree::{DirectoryFilter, DirectoryTree, DirectoryTreeEvent};

#[derive(Debug, Clone)]
enum Message {
    Tree(DirectoryTreeEvent),
}

struct App {
    tree: DirectoryTree,
}

impl App {
    fn new() -> (Self, Task<Message>) {
        let tree = DirectoryTree::new(PathBuf::from("."))
            .with_filter(DirectoryFilter::FilesAndFolders);
        (Self { tree }, Task::none())
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Tree(event) => self.tree.update(event).map(Message::Tree),
        }
    }

    fn view(&self) -> Element<'_, Message> {
        self.tree.view(Message::Tree)
    }
}

fn main() -> iced::Result {
    iced::application(App::new, App::update, App::view).run()
}
```

The key line is `self.tree.update(event).map(Message::Tree)` — the
widget returns a `Task` (e.g. to scan a folder off-thread), and you
must return it so results flow back in.

Prefer a generic in-memory tree, or want the item-tree walkthrough?
See [Getting started](docs/src/guide/getting-started.md). Working apps
live in [`examples/`](examples/) — run any with
`cargo run --example <name>`.

## Design notes

- **Lazy, async, cache-backed** (`DirectoryTree`). Only the root is
  eagerly created; each expansion dispatches a scan through a pluggable
  [`ScanExecutor`](docs/src/guide/custom-executor.md) and merges the
  result into a generation-tagged cache. Filter and search re-derive
  from the cache — no re-scan.
- **Key-based diffing** (`ItemTree<T>`). Feed a new tree with
  `set_tree`; expansion and selection are preserved for every stable
  `NodeId`, so you can rebuild on every edit without reconciling UI
  state by hand.
- **Every feature is orthogonal.** Selection survives filter flips,
  subtree reloads, collapse cycles, and search-hidden rows. Drag state
  is separate from selection. Search does not mutate expansion. These
  invariants are enforced by the test suite (213 tests).
- **The widget owns UI state, not data state.** Neither widget writes,
  renames, deletes, or moves. Drag-and-drop emits an event
  (`DragCompleted`) for your app to act on — the widget's job ends at
  "here is what the user asked for."
- **Safety valves where defaults matter.** Prefetch
  (`with_prefetch_limit`) skips `.git`, `node_modules`, `target`, and
  similar by default; `max_depth` caps recursion; generation tags drop
  stale scan results that return after a collapse. See
  [Parallel pre-expansion](docs/src/guide/prefetch.md).

## More detail

Full documentation lives in [`docs/`](docs/) as an
[mdbook](https://rust-lang.github.io/mdBook/), organized by audience:

- **User Guide** — [Getting started](docs/src/guide/getting-started.md)
  · [Directory tree](docs/src/guide/directory-tree.md)
  · [Item tree](docs/src/guide/item-tree.md)
  · [Drag and drop](docs/src/guide/drag-and-drop.md)
  · [Multi-select](docs/src/guide/multi-select.md)
  · [Keyboard navigation](docs/src/guide/keyboard-navigation.md)
  · [Incremental search](docs/src/guide/incremental-search.md)
  · [Icon themes](docs/src/guide/icon-themes.md)
  · [FAQ](docs/src/guide/faq.md)
- **Reference** — [Feature list](docs/src/reference/features.md)
  · [Events](docs/src/reference/events.md)
  · [Feature specifications](docs/src/internals/feature-specs.md)
- **Maintainers & Contributors** —
  [Design principles](docs/src/internals/core-design.md)
  · [Architecture](docs/src/internals/architecture.md)
  · [Data model](docs/src/internals/data-model.md)
  · [State machine](docs/src/internals/state-machine.md)
  · [Porting to other frameworks](docs/src/internals/porting-to-dioxus.md)
  · [Development & testing](docs/src/internals/development.md)
- **Release notes** — [CHANGELOG](CHANGELOG.md) · [ROADMAP](ROADMAP.md)

## License

Licensed under the [Apache License, Version 2.0](LICENSE). See also the
[NOTICE](NOTICE) file.
