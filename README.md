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

## Build a signed macOS app (.app + .dmg)

```sh
cargo bundle --release
cargo codesign macos --app target/release/bundle/osx/editor.app --skip-notarize
```

Output: signed `.app` and `.dmg` in `target/release/bundle/osx/`. Requires `cargo-bundle` and `cargo-codesign` installed, and `sign.toml` set up (see `sign.toml` / `entitlements.plist` in this repo).

## License

PolyForm Strict License 1.0.0 — free to download and use, but no forking or redistributing modified versions.
