# editor

A minimal, no-bloat code editor in Rust, built on `egui`/`eframe`.

## Features

- Single-pane text editor with syntax highlighting (`syntect`)
- Left sidebar: project file tree, create/rename/delete, keyboard navigation (arrows to move/open/expand/collapse)
- Right sidebar: AI chat against any OpenAI-compatible endpoint (configurable base URL/key/model), with streaming and a stop button
- Status bar with an AI sidebar visibility toggle
- Reopens the last project folder on startup

## Run

```sh
cargo run
```
