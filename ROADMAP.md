# Feature Roadmap

Comparison of this editor's current state against widely used editors (VS Code, Sublime
Text, Zed, JetBrains IDEs, Neovim), and a prioritized plan to close the gaps that matter
for a "minimal, no-bloat" positioning.

## Current state

- Custom canvas-based code editor widget (`code_editor/`, built on `cosmic-text`, replacing
  `iced::widget::text_editor`): line-number gutter, blinking cursor, click/drag selection,
  undo/redo, select-all, zoom in/out/reset (persisted), syntax highlighting via `syntect`
- Find in file (Cmd+F): next/prev match, replace one/replace all
- Find in Project (Cmd+Shift+F): recursive substring search across the open folder
  (skips `.git`/`target`/`node_modules`/etc.), click a result to jump to file + line
- Quick Open (Cmd+P): fuzzy file finder (`fuzzy-matcher`/skim algorithm) over every file
  under the project root, live filtering, arrow-key navigation, click or Enter to open,
  Escape or backdrop-click to dismiss, auto-focused search box
- Git sidebar panel: current branch, working-tree status list, stage-all + commit, refresh
  — shells out to the `git` CLI
- Sidebar is a collapsible "activity bar": Tree / Git / Search, toggled from status-bar
  icons, click-active-icon-again to collapse
- Tabs with dirty-state tracking, close-with-confirm, per-tab line-ending (LF/CRLF) detection
- Status bar: cursor line/col, line ending, detected language name (via `syntect`), icon
  buttons (lucide icons) for sidebar/git/search/AI toggles
- File tree: create/rename/delete/copy path/copy relative path/reveal in file manager,
  keyboard nav, context menu, manual refresh
- Open file / open folder / close folder / save, Cmd+O / Cmd+S
- Window state persisted: size, position, maximized, sidebar/AI pane split ratios, zoom,
  theme, sidebar/AI visibility — all restored on relaunch; reopens last project folder
- 22 selectable app themes, now persisted (previously reset every launch)
- AI sidebar, two backends:
  - **HTTP** (OpenAI-compatible): multiple saved connections (name/URL/key/model cards),
    Test Connection + `/models` picker, streaming, **agentic tool-calling loop**
    (`read_file`/`list_dir`/`search_project`/`write_file`, sandboxed to the project root,
    with Allow/Deny permission prompts per call, up to 8 tool rounds), multiline input
    (Cmd+Enter to send), copy-message button
  - **ACP** (Claude Code): threads with persistence per project, tool-call display,
    permission prompts, usage/cost, multiline input, copy-message button
- Both AI modes can edit files on disk; the app auto-reloads open unmodified tabs and
  refreshes the tree afterward

## What's still missing relative to mainstream editors

| Category | VS Code / Sublime / Zed / JetBrains / Neovim | This editor |
|---|---|---|
| Command palette | ✅ Ctrl+Shift+P | ❌ none |
| Quick open / fuzzy file switcher | ✅ Ctrl+P | ✅ Cmd+P (done) |
| Go to line | ✅ Ctrl+G | ❌ no dedicated command (project-search jump-to-line exists, but no direct "go to line N" in the open file) |
| Multi-cursor / column select | ✅ | ❌ single cursor only |
| Find in file: regex | ✅ | ❌ plain substring only |
| LSP (autocomplete, hover, diagnostics, go-to-def) | ✅ | ❌ none |
| Diff gutter in editor (vs. git HEAD) | ✅ | ❌ git panel shows file-level status only |
| Stage individual files / hunks | ✅ | ❌ stage-all only |
| Word wrap toggle | ✅ | ❌ none (code editor doesn't wrap) |
| Bracket matching / auto-close | ✅ | ❌ none |
| Code folding | ✅ | ❌ none |
| Minimap | ✅ (VS Code/Zed/Sublime) | ❌ none (low priority for "no-bloat") |
| Split editor panes (multiple editors side by side) | ✅ | ❌ pane_grid only splits sidebar/editor/AI |
| Integrated terminal | ✅ | ❌ none |
| Settings UI / config file | ✅ | ⚠️ many individual flat files under `~/.config/editor/`; no unified settings UI beyond AI connections |
| Extensions / plugins | ✅ | ❌ intentionally out of scope |
| Drag-and-drop to open files | ✅ | ❌ none |
| Recent files list | ✅ | ❌ only last *project* |
| Auto-save | ✅ (optional) | ❌ manual save only |
| Diagnostics/problems panel | ✅ | ❌ none (depends on LSP) |
| Outline / symbols view | ✅ | ❌ none (depends on LSP) |

## Proposed plan

Ordered by (impact on daily editing) ÷ (implementation cost).

### Phase 1 — Navigation (highest remaining impact)
1. ~~**Quick open / fuzzy file finder** (Cmd+P)~~ — done: `src/quick_open.rs`, overlay via
   `iced::widget::stack!`, `fuzzy-matcher` (skim algorithm) over a one-time directory walk.
2. **Command palette** (Cmd+Shift+P) — searchable registry of existing `Message`s
   (theme switch, AI toggle, save, zoom, git refresh, etc.). Can share `quick_open.rs`'s
   fuzzy-match-and-overlay plumbing (`SkimMatcherV2`, the `stack!` + `mouse_area` backdrop
   pattern, the `text_input::focus` auto-focus trick) almost directly.
3. **Go to line** — small modal or extend the existing find bar with a `:line` shortcut,
   reusing `cosmic_text::Motion::GotoLine` (already used by project-search jump-to-line).
4. **Recent files** — small MRU list, feeds into quick open.

### Phase 2 — Editing polish
5. **Word wrap toggle** — per-tab or global, persisted like zoom.
6. **Bracket matching / auto-close** — highlight matching bracket at cursor; auto-insert
   closing `)]}"'`.
7. **Regex mode for find/replace** — opt-in toggle next to the existing find bar.

### Phase 3 — Deeper git integration
8. **Diff gutter in editor** — added/modified/removed line markers vs. HEAD, via
   `git diff` (same shell-out approach as `git.rs`).
9. **Stage individual files** (not just stage-all) — `git.rs` already lists per-file
   status; add per-row stage/unstage buttons before touching hunk-level staging.

### Phase 4 — Language intelligence (highest cost, highest ceiling)
10. **LSP client** — spawn `rust-analyzer` (or similar) per project, wire diagnostics +
    hover + go-to-definition + autocomplete. Scope to one language first (Rust). The ACP
    integration in `acp.rs` (stdio JSON-RPC + tokio + channel-based event forwarding into
    `poll()`) is a directly reusable template for the process/transport plumbing.
11. **Diagnostics panel** — surface LSP diagnostics list, click-to-jump.
12. **Autocomplete popup** — LSP completions wired into `code_editor`.

### Phase 5 — Layout & settings
13. **Split editor panes** — reuse `pane_grid` to allow two `PaneKind::Main`-equivalent
    editors side by side.
14. **Unified settings file** — consolidate the growing set of `~/.config/editor/*` flat
    files (last_project, app_theme, zoom, sidebar_ratio, ai_ratio, window_*, ai_visible,
    sidebar_visible, ai_connections.json, acp_threads/) into one `settings.json`, with a
    minimal settings UI (font size, word wrap default, theme) alongside the existing AI
    connections UI.
15. **Auto-save toggle** — debounce writes to disk.
16. **Drag-and-drop file open**.

### Explicitly not planned (conflicts with "no-bloat" goal)
- Extension/plugin system
- Minimap
- Multiple language servers running simultaneously by default
- Built-in debugger
