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

## AI assistant

The AI sidebar has one provider picker: **Local / Ollama / API**, **Claude ACP**, and
**Codex ACP**. Each provider keeps its own conversation and draft. Claude and Codex
history is stored separately for each project.

For Ollama, open model settings, choose **Use local Ollama**, and select a discovered
model. Other OpenAI-compatible servers use the same base URL, key, and model settings.
Image prompts require a vision-capable model.

ACP starts `npx --yes @agentclientprotocol/claude-agent-acp` or
`npx --yes @agentclientprotocol/codex-acp`. Node/npm must be on PATH; configure the agent's
credentials before chatting. Codex adapter setup: https://github.com/agentclientprotocol/codex-acp.
The first launch may download the adapter. Connection errors appear in the conversation.

- Drag project-tree files or external files/images onto the AI message composer.
  The composer highlights while dragging; drops elsewhere do not attach files.
  A paperclip in the composer opens the file picker as a fallback.
  Supports UTF-8 text/code and PNG, JPEG, GIF, WebP images; up to 16 attachments,
  8 MiB each. Review or remove attachments before sending.
- The composer's code icon attaches the current selection, or the active editor buffer
  including unsaved changes. Attachments are snapshots, not live links.
- **Cmd/Ctrl+Enter** sends the prompt, including attachment-only prompts.
- The ACP composer's context ring fills with used context. Hover for the percentage;
  click for model, token usage, context limit, remaining space, and reported cost.
  Unknown values are shown as not reported. Escape or clicking outside closes the overlay.
- Tool requests show approval controls and details. ACP tool cards expand to show
  supplied output and before/after edits. Approval applies to the entire tool request.
- Approval cards show decoded request details and separate **Reject** / **Allow once**
  actions. **Remember for this session** remembers only the same tool input after allowing;
  **Reset session approvals** revokes these grants. New conversations, project changes,
  or ACP connection resets clear them. ACP can also offer its own named permission
  choices, whose persistence is controlled by the agent.

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

For ready-to-run downloads, see [GitHub Releases](https://github.com/ailegion/editor/releases)
and the [installation instructions](docs/INSTALL.md). No Rust installation is needed.

To run from source:

```sh
cargo run
```

## Publishing releases

The **Build and release** GitHub Actions workflow tests and builds Windows x86-64,
Linux x86-64, and macOS Apple Silicon and Intel packages.

- Pushing to `main` or opening a pull request runs the builds and uploads downloadable
  workflow artifacts. It does not create a public release.
- You can also run the workflow manually from the Actions tab to check a build.
- To release, set the version in `Cargo.toml`, update `Cargo.lock` with Cargo, and
  commit/push your changes. Create and push a matching tag, for example `v0.1.0`
  for the current version `0.1.0`.
- A tag build creates a **draft release** only after all four packages succeed.
  Open GitHub **Releases**, review the draft and downloads, then click **Publish release**.
- Tags containing a prerelease suffix, such as `v0.2.0-beta.1`, create prereleases.
  Reruns can update draft assets but will not overwrite an already published release.

Enable GitHub Actions for the repository. The workflow uses GitHub's built-in token
and a free update-signing key; configure the `EDITOR_UPDATE_SIGNING_KEY` repository
secret as described in [automatic update setup](docs/UPDATES.md) before pushing a release tag.
No paid signing accounts are required. Standard hosted runners are free
for public repositories; private repositories use your account's Actions allowance.
The first cross-platform run must pass before a release is ready. Download and try
each platform's package before publishing the draft.

Packages contain themes, their original notices, the editor license, generated Rust
dependency licenses, and embedded syntax/theme notices. Windows is unsigned; macOS
uses free ad-hoc signing without Apple notarization. Linux ships as a `.tar.gz` archive.

## Automatic updates

Installed release builds check GitHub for newer stable versions on launch and every six
hours. Verified updates download in the background. Click **Restart to update** in the
status bar to apply one; unsaved edits are persisted through session recovery first.
The status-bar control also checks manually and shows update errors on hover.
See [update setup and recovery](docs/UPDATES.md).

## Local macOS bundling (optional)

```sh
cargo bundle --release
cargo codesign macos --app target/release/bundle/osx/editor.app --skip-notarize
```

Output: signed `.app` and `.dmg` in `target/release/bundle/osx/`. Requires `cargo-bundle` and `cargo-codesign` installed, and `sign.toml` set up (see `sign.toml` / `entitlements.plist` in this repo).

## License

The editor's original code is source-available under the [PolyForm Shield License 1.0.0](LICENSE.md).

You can use the editor for personal projects and at work, including to develop commercial
software. Modifications and redistribution are allowed for purposes permitted by the license;
using this software to provide a competing product is not, even if that product is free.
The full license governs these permissions and restrictions.

Required Notice: Copyright 2026 Ajdin (https://github.com/ailegion/editor)

Preserve this required notice when distributing the editor or its code. Bundled themes,
vendored code, dependencies, and other third-party components retain their own licenses
and copyright notices; PolyForm Shield does not replace those terms. See [NOTICE](NOTICE)
for the license scope and locations of bundled component notices.
