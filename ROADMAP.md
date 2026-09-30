# Feature Roadmap

Comparison of this editor's current state against widely used editors (VS Code, Sublime
Text, Zed, JetBrains IDEs, Neovim), and a prioritized plan to close the gaps that matter
for a "minimal, no-bloat" positioning.

## Current state

### Editing
- Custom canvas-based code editor widget (`code_editor/`, built on `cosmic-text`): line-number
  gutter, click/drag/double/triple-click selection, undo/redo (Cmd+Z, Cmd+Shift+Z / Cmd+Y; cut
  and paste are one step each), Tab/Shift+Tab indent, right-click menu
- Syntax highlighting via `syntect` (plus two-face's extra syntaxes), colored by the active theme
- Code folding for `{ [ (` blocks (strings and comments skipped): click the gutter strip on a
  fold-start line; a collapsed block shows `⋯ N lines`
- Bracket matching highlight; auto-close for brackets and quotes, type-over for closers
- Find/replace in file (Cmd+F): case-insensitive, optional regex (`.*`) with `$1`/`$name`
  replacements, match counter, replace one/all; matching is per line
- Zoom 50–250% (Cmd+= / Cmd+- / Cmd+0), persisted
- Git diff markers in the gutter (added/modified/removed vs. the staged version, following
  unsaved edits) and inline blame after the cursor line (`git blame --contents -`, so unsaved
  text counts)
- Status bar: line/column, line ending (LF/CRLF), language, error/warning counts
- Markdown preview toggle for `.md` files (eye icon in the breadcrumb row)

### Files and navigation
- Tabs: dirty marker, close with confirm, close others/left/right/all, reopen closed tab
  (Cmd+Shift+T), middle-click close, Cmd+W, Cmd+1…9
- Breadcrumbs that reveal the path in the file tree
- File tree (hidden files shown, editable exclude list): new file/folder, rename, delete, copy
  path/relative path, reveal in file manager, refresh, drag onto a folder to move (asks
  first, never overwrites; open tabs follow)
- Quick Open (Cmd+P): fuzzy file finder, recently opened files first
- Command Palette (Cmd+Shift+P), Go to Line (Cmd+G)
- Find in Project (Cmd+Shift+F): case-insensitive substring search (up to 500 results),
  skipping `.git`/`target`/`node_modules`/etc.

### Language intelligence (LSP)
- Servers: rust-analyzer (Rust), Ruff (Python), Biome (JS/TS/JSON/CSS), just-lsp (justfiles),
  EmmyLua (Lua); found on `PATH` or installed on request from their GitHub releases (SHA-256
  checked when published)
- Diagnostics (wavy underline, gutter dot, status bar message with "Fix with AI"), hover after
  a short pause, go to definition (F12 or Cmd+click); servers restart after a crash

### Terminal
- Integrated terminal (Ctrl+`) below the editor: multiple tabs, shell profiles (PowerShell,
  Windows PowerShell, CMD, Git Bash on Windows; `$SHELL` and installed shells on Unix), 5000
  lines of scrollback; on Windows it uses Windows Terminal's font and colors

### Git
- Source control sidebar: flat or tree view, per-file and per-folder stage/unstage checkboxes,
  stage/unstage all, discard (with confirmation), commit (Cmd+Enter), AI-written commit messages
- All repository changes run the real `git` (hooks, signing, filters, credential helpers
  apply); status reloads after file-system changes settle; file versions for diffs are read
  in-process (gitoxide)
- Side-by-side diff view with staged/unstaged sections and changed-character highlights
- Branch picker: search, all/local/remote filter, switch, track a remote branch, create
- Fetch, pull and push (first push of a branch with one remote sets its upstream)
- Commit graph in the editor area: all branches, remotes and tags with lanes, 500 commits per
  page, each commit's message and files, and each file's change in the diff view

### AI
- Three backends: OpenAI-compatible HTTP (saved connections, connection test, model list,
  Ollama preset), Claude and Codex through ACP (`npx` adapters)
- HTTP agent tools: `read_file`, `list_dir`, `search_project`, `write_file`, `edit_file`,
  `run_command` (up to 8 tool rounds), each needing approval: once, for the session, or reject
- ACP: model and effort pickers from what the agent reports (remembered per provider),
  resumable sessions, agent permission prompts, thinking sections, context usage ring
- Conversation history with search, rename and delete
- Attachments: files, images (also pasted from the clipboard), `@path` mentions, drag from the
  tree or the OS, editor selection or file
- Replies rendered as Markdown with Copy and Insert-at-cursor on code blocks; AI edits open in
  the diff view; open unmodified tabs reload after the agent edits files
- Inline edit (Cmd+K) with Accept/Reject; Explain / Refactor / Fix problem commands

### App
- 20 themes (Dark+, Light+ and 18 bundled), VS Code theme files from the user themes folder,
  install/uninstall themes from Open VSX
- Window size, position, maximized state, pane ratios, zoom, theme, last project and panel
  visibility persist; unsaved buffers (including untitled ones) are kept in a recovery session
  and restored on the next launch, so closing the window never loses edits
- Signed auto-updates from GitHub releases (checked every 6 hours, "Restart to Update")
- Help menu: Check for Updates, File Bug Report, About (with release notes)
- Windows: themed title bar with the menu on the same row; Windows still handles drag, snap,
  Snap Layouts, double-click maximize and resizing. macOS: About and Check for Updates in the
  native app menu

## What's still missing relative to mainstream editors

| Category | VS Code / Sublime / Zed / JetBrains / Neovim | This editor |
|---|---|---|
| Command palette | ✅ Ctrl+Shift+P | ✅ Cmd+Shift+P (done) |
| Quick open / fuzzy file switcher | ✅ Ctrl+P | ✅ Cmd+P (done) |
| Go to line | ✅ Ctrl+G | ✅ Cmd+G (done) |
| Multi-cursor / column select | ✅ | ❌ single cursor only |
| Find in file: regex | ✅ | ✅ `.*` toggle in the find bar (done) |
| Find in project: regex / `.gitignore` | ✅ | ❌ plain substring; fixed skip list |
| LSP (autocomplete, hover, diagnostics, go-to-def) | ✅ | ⚠️ diagnostics, hover and go-to-definition (F12); no autocomplete |
| Diff gutter in editor (vs. git HEAD) | ✅ | ✅ (done) |
| Stage individual files / hunks | ✅ | ⚠️ per-file staging done; hunk staging ❌ |
| Branch switch / create | ✅ | ✅ branch picker with search and local/remote filter (done) |
| Fetch / pull / push | ✅ | ✅ source control ⋯ menu (done) |
| Commit graph / history | ✅ (Zed, VS Code, JetBrains; extensions elsewhere) | ✅ Git: Show Graph, with commit files and diffs (done) |
| Inline blame | ✅ | ✅ (done) |
| Word wrap toggle | ✅ | ❌ none (code editor doesn't wrap) |
| Bracket matching / auto-close | ✅ | ✅ (done) |
| Code folding | ✅ | ✅ (done) |
| Markdown preview | ✅ | ✅ toggle for `.md` files (done) |
| Minimap | ✅ (VS Code/Zed/Sublime) | ❌ none (low priority for "no-bloat") |
| Split editor panes (multiple editors side by side) | ✅ | ❌ pane_grid only splits sidebar/editor/AI/terminal |
| Integrated terminal | ✅ | ✅ Ctrl+` (done) |
| AI chat / agents | ✅ (VS Code, Zed, JetBrains) | ✅ OpenAI-compatible HTTP, Claude and Codex via ACP (done) |
| Inline AI edit | ✅ (VS Code, Zed, JetBrains) | ✅ Cmd+K (done) |
| Settings UI / config file | ✅ | ⚠️ many individual flat files under `~/.config/editor/`; no unified settings UI beyond AI connections |
| Extensions / plugins | ✅ | ❌ intentionally out of scope |
| Drag-and-drop to open files | ✅ | ❌ dropped files only attach to the AI chat input |
| Move files by dragging in the tree | ✅ | ✅ with confirmation; never overwrites (done) |
| Recent files list | ✅ | ✅ recently opened files in Quick Open (done) |
| Auto-save | ✅ (optional) | ❌ manual save; unsaved edits survive restarts via the recovery session |
| Diagnostics/problems panel | ✅ | ⚠️ no panel; error/warning counts and the current line's message in the status bar |
| Outline / symbols view | ✅ | ❌ none |

## Proposed plan

Ordered by (impact on daily editing) ÷ (implementation cost). Completed phases are kept short.

### Done
- **Navigation:** Quick Open, Command Palette, Go to Line, recent files
  (`quick_open.rs`, `command_palette.rs`, `goto_line.rs`, `recent_files.rs`)
- **Editing polish:** bracket matching/auto-close (`code_editor/brackets.rs`), regex
  find/replace (`code_editor/search.rs`), code folding (`code_editor/folding.rs`)
- **Git:** diff gutter (`git_diff.rs`), per-file staging, side-by-side diffs
  (`git_preview.rs`), inline blame (`git/blame.rs`), branch picker (`git/branches.rs`),
  fetch/pull/push (`git/remote.rs`), commit graph (`git_graph.rs`)
- **LSP basics:** client, server registry and installer, diagnostics, hover, go to
  definition (`lsp/`)
- **Terminal:** tabs and shell profiles (`terminal.rs`, `terminal/`)
- **AI:** HTTP and ACP backends, history, attachments, inline edit (`chat.rs`, `acp.rs`,
  `ai_*.rs`, `inline_edit.rs`)

### Next — Editing
1. **Autocomplete popup** — LSP `textDocument/completion` wired into `code_editor`.
2. **Diagnostics panel** — list of the project's diagnostics, click to jump.
3. **Word wrap toggle** — a renderer change, see the note below.
4. **Multi-cursor** — the buffer has a single cursor and selection today.
5. **Outline / symbols** — LSP `textDocument/documentSymbol`.

**Word wrap note:** the custom canvas renderer (`code_editor/render.rs`) draws one row per
`BufferLine` at a fixed `y = line_index * line_height`, and `Buffer::sync` (cursor/selection
pixel math) assumes one `cosmic_text::LayoutLine` per buffer line -- both explicit,
documented assumptions from when the renderer was built. Real word wrap needs cosmic-text's
own wrap-aware layout iteration (`Buffer::layout_runs()`, which yields one `LayoutRun` per
*visual* row with its own correct `line_top`, already accounting for wrapped continuations)
instead of the current manual `index * line_height` math, plus reworking scrolling (currently
a raw pixel `f32`) to work in visual rows rather than buffer lines, plus making gutter line
numbers only draw on a buffer line's first visual row. That's a renderer-architecture change,
not a toggle -- worth scoping as its own task.

### Next — Git
6. **Hunk staging** — stage/unstage individual hunks from the diff view.

### Next — Files and search
7. **Project search: regex and `.gitignore`** — reuse the find bar's regex handling.
8. **Drag-and-drop to open files** from the OS.

### Next — Layout and settings
9. **Split editor panes** — two `PaneKind::Main`-equivalent editors side by side.
10. **Unified settings file** — consolidate the `~/.config/editor/*` files (`session.json`,
    `last_project`, `app_theme`, `file_tree_exclude`, `zoom`, `sidebar_ratio`, `ai_ratio`,
    `window_*`, `ai_visible`, `sidebar_visible`, `recent_files`, `terminal_shell`,
    `git_tree_view`, `ai_connections.json`, `chat_threads/`, `acp_threads/`, `themes/`,
    `lsp/`) into one `settings.json`, with a minimal settings UI.
11. **Auto-save toggle** — debounce writes to disk.

### To verify
- **CRLF files may be saved as LF:** line endings are detected and shown, but saving writes
  the buffer's `\n`-joined text (`main.rs` `save()`, `code_editor/buffer.rs`). Not yet
  confirmed by saving a CRLF file.

### Explicitly not planned (conflicts with "no-bloat" goal)
- Extension/plugin system
- Minimap
- Multiple language servers running simultaneously by default
- Built-in debugger
