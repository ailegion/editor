# editor

A minimal, no-bloat code editor in Rust, built on `iced`.

## Features

- Single-pane text editor with syntax highlighting (`syntect`)
- Left sidebar: project file tree, create/rename/delete, keyboard navigation (arrows to move/open/expand/collapse)
- Right sidebar: AI chat against any OpenAI-compatible endpoint (configurable base URL/key/model), with streaming and a stop button
- Integrated PTY terminal with ANSI colors, 5,000 lines of scrollback, and a resizable bottom panel
- Status bar with AI and terminal visibility toggles
- Reopens the last project with only the root expanded; restores tabs and the active file
- Recovers unsaved edits from atomic local snapshots (every 500 ms and on normal window close)
- Git panel with staged/unstaged lists, per-file staging, and side-by-side diffs with character highlights
- Clickable breadcrumbs and reveal-in-tree navigation
- Notifications for saves, Git operations, and file-operation failures

## Tab shortcuts

- `Cmd+1`–`Cmd+9`: select a tab
- `Cmd+W`: close the current tab or diff
- `Cmd+Shift+T`: reopen the last closed file

Recovery lives in `~/.config/editor/session.json`. Restored edits remain unsaved until
explicitly saved. A crash can lose edits made since the most recent snapshot (up to
about half a second). Missing saved files are skipped; missing files with recovered
edits remain available as dirty tabs.

## Terminal

Open the terminal with **Ctrl+`**, the status-bar terminal icon, or **View → Toggle Terminal**.
It starts your default shell in the current project folder. Click the terminal to type;
Enter, Tab completion, arrow-key history, Ctrl+C, and Ctrl+D work in the shell.
Drag the panel divider to resize it and use the mouse wheel for scrollback.

Paste with Cmd+V on macOS or Ctrl+Shift+V elsewhere. The copy button copies the visible
screen. Hiding the panel keeps the shell running; Restart ends the current session and
starts a shell in the current project. Closing the editor terminates its shell.
One shell session is supported; mouse selection and terminal mouse reporting are not yet implemented.

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
