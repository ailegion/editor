# Local changes

Source: iced-swdir-tree 0.9.3 from crates.io (Apache-2.0; see LICENSE and NOTICE).

The editor vendors the library to add two backward-compatible IconTheme hooks:
`file_glyph(path)` and `file_color(path)`. The directory view uses these hooks for
regular files while preserving existing folder, error, selection, and drag behavior.
The application owns file mappings in src/file_icons.rs.

The local `DirectoryTreeEvent::Refresh(path)` explicitly rescans a directory.
Unlike toggling, it bypasses the loaded-state cache. Loaded results retain existing
child nodes when their path and kind match, preserving expanded subtrees.

When upgrading, retain these changes or migrate to equivalent upstream functionality.

`view_with_entry` places a caller-owned name editor inside the tree, either below
a parent directory or in place of a row. `Expand(path)` opens a directory without
collapsing one that is already expanded.
