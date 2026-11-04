//! Workspace tabs: every project gets its own tab under the top bar. This module holds the
//! parts that don't need the running app, so they can be tested on their own: the session
//! file (and how an older single-project one is read), tab labels and widths, where a dragged
//! tab lands, which tab becomes active after a close, where opening a folder goes, and the
//! small spinner a tab shows while its AI assistant is working.

use crate::EditorSession;
use iced::widget::canvas;
use iced::{mouse, Rectangle, Renderer, Theme};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Every workspace tab in strip order, and which one was active.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionFile {
    pub workspaces: Vec<EditorSession>,
    pub active_workspace: usize,
}

impl Default for SessionFile {
    fn default() -> Self {
        Self { workspaces: vec![EditorSession::default()], active_workspace: 0 }
    }
}

/// Reads `session.json`. A file from before workspace tabs held one project, whose folder was
/// kept in the separate `last_project` file (`last_project` here, already checked to be a
/// folder): it becomes a single workspace exactly as startup used to read it, i.e. the saved
/// tabs and unsaved edits only come back when they belong to that same folder.
pub fn read_session(bytes: Option<&[u8]>, last_project: Option<PathBuf>) -> SessionFile {
    if let Some(mut file) = bytes.and_then(|bytes| serde_json::from_slice::<SessionFile>(bytes).ok()) {
        if file.workspaces.is_empty() {
            return SessionFile::default();
        }
        for workspace in &mut file.workspaces {
            // A project folder that was deleted or moved opens as a tab without a folder; its
            // files and unsaved edits are kept.
            if workspace.root.as_deref().is_some_and(|root| !root.is_dir()) {
                workspace.root = None;
            }
        }
        file.active_workspace = file.active_workspace.min(file.workspaces.len() - 1);
        return file;
    }
    let legacy: EditorSession = bytes.and_then(|bytes| serde_json::from_slice(bytes).ok()).unwrap_or_default();
    let workspace = if legacy.root == last_project {
        EditorSession { root: last_project, layout: None, ..legacy }
    } else {
        EditorSession { root: last_project, ..EditorSession::default() }
    };
    SessionFile { workspaces: vec![workspace], active_workspace: 0 }
}

/// Whether `a` and `b` name the same folder, also when spelled differently (case on Windows,
/// a trailing separator, a symlink).
pub fn same_folder(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Where a folder the user asked to open goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FolderTarget {
    /// It is already open in the tab at this index: switch to it.
    Switch(usize),
    /// The current tab has no folder: open it there.
    Current,
    /// The current tab has a folder: ask whether to replace it or open a new tab.
    Ask,
}

/// `roots` are every tab's folders in strip order; `current` is the active tab's index.
pub fn folder_target(roots: &[Option<&Path>], current: usize, path: &Path) -> FolderTarget {
    if let Some(index) = roots.iter().position(|root| root.is_some_and(|root| same_folder(root, path))) {
        return FolderTarget::Switch(index);
    }
    match roots.get(current) {
        Some(Some(_)) => FolderTarget::Ask,
        _ => FolderTarget::Current,
    }
}

/// The name shown on a tab: its folder's name, "New Tab" without a folder. Tabs whose folders
/// share a name also show the parent folder's name, e.g. `app — work`.
pub fn labels(roots: &[Option<&Path>]) -> Vec<String> {
    let name = |root: &Path| root.file_name().map_or_else(|| root.display().to_string(), |name| name.to_string_lossy().into_owned());
    let names: Vec<Option<String>> = roots.iter().map(|root| root.map(name)).collect();
    roots.iter().zip(&names).map(|(root, own)| match (root, own) {
        (Some(root), Some(own)) => {
            let shared = names.iter().filter(|other| other.as_ref() == Some(own)).count() > 1;
            match root.parent().and_then(Path::file_name).filter(|_| shared) {
                Some(parent) => format!("{own} — {}", parent.to_string_lossy()),
                None => own.clone(),
            }
        }
        _ => "New Tab".to_string(),
    }).collect()
}

/// Tabs are all the same width, like a browser's: they share the strip until they reach
/// [`TAB_MIN`], after which the strip scrolls.
pub const TAB_MIN: f32 = 120.0;
pub const TAB_MAX: f32 = 220.0;

pub fn tab_width(available: f32, count: usize) -> f32 {
    if count == 0 || !available.is_finite() {
        return TAB_MAX;
    }
    (available / count as f32).clamp(TAB_MIN, TAB_MAX)
}

/// The tab index under `x` (measured from the start of the tabs) for tabs `width` wide.
pub fn drag_target(x: f32, width: f32, count: usize) -> usize {
    if count == 0 || width <= 0.0 || !x.is_finite() {
        return 0;
    }
    ((x / width).floor().max(0.0) as usize).min(count - 1)
}

/// The next (or, with `forward` false, previous) tab, wrapping around.
pub fn cycle(active: usize, len: usize, forward: bool) -> usize {
    if len == 0 {
        return 0;
    }
    if forward { (active + 1) % len } else { (active + len - 1) % len }
}

/// Which tabs "Close Others" (`right_only` false) or "Close to the Right" (`true`) closes,
/// highest index first so removing them in order keeps the rest in place.
pub fn to_close(len: usize, anchor: usize, right_only: bool) -> Vec<usize> {
    if anchor >= len {
        return Vec::new();
    }
    (0..len).rev().filter(|&index| if right_only { index > anchor } else { index != anchor }).collect()
}

/// A small ring with a gap that turns while drawn; the app redraws every 50ms, so it spins.
pub struct Spinner;

impl<Message> canvas::Program<Message> for Spinner {
    type State = ();

    fn draw(&self, _: &(), renderer: &Renderer, theme: &Theme, bounds: Rectangle, _: mouse::Cursor) -> Vec<canvas::Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        let millis = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |time| time.as_millis());
        let start = (millis % 1000) as f32 / 1000.0 * std::f32::consts::TAU;
        let radius = bounds.width.min(bounds.height) / 2.0 - 1.5;
        let arc = canvas::Path::new(|path| {
            path.arc(canvas::path::Arc {
                center: frame.center(),
                radius: radius.max(1.0),
                start_angle: iced::Radians(start),
                end_angle: iced::Radians(start + std::f32::consts::TAU * 0.7),
            });
        });
        frame.stroke(&arc, canvas::Stroke::default().with_width(1.5).with_color(theme.extended_palette().primary.base.color));
        vec![frame.into_geometry()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RecoveryBuffer;

    #[test]
    fn a_single_project_session_becomes_one_workspace_with_the_old_rules() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project");
        std::fs::create_dir(&project).unwrap();
        let old = |root: Option<&Path>| serde_json::to_vec(&serde_json::json!({
            "root": root, "tabs": [project.join("a.rs")], "active": project.join("a.rs"),
            "recovery": [{"path": project.join("a.rs"), "text": "unsaved"}],
        })).unwrap();

        // Same folder as `last_project`: tabs and unsaved edits come back.
        let file = read_session(Some(&old(Some(&project))), Some(project.clone()));
        assert_eq!(file.active_workspace, 0);
        assert_eq!(file.workspaces.len(), 1);
        let workspace = &file.workspaces[0];
        assert_eq!(workspace.root.as_deref(), Some(project.as_path()));
        assert_eq!(workspace.tabs, [project.join("a.rs")]);
        assert_eq!(workspace.recovery, [RecoveryBuffer { path: Some(project.join("a.rs")), text: "unsaved".into() }]);
        assert!(workspace.layout.is_none(), "an old session keeps today's startup layout");

        // Another folder: only the folder opens, as before.
        let other = dir.path().join("other");
        std::fs::create_dir(&other).unwrap();
        let file = read_session(Some(&old(Some(&project))), Some(other.clone()));
        assert_eq!(file.workspaces, [EditorSession { root: Some(other), ..Default::default() }]);

        // No folder on either side: loose files come back.
        let file = read_session(Some(&old(None)), None);
        assert_eq!(file.workspaces[0].root, None);
        assert_eq!(file.workspaces[0].tabs.len(), 1);

        // No or unreadable file: just the last folder.
        for bytes in [None, Some(&b"{ not json"[..])] {
            let file = read_session(bytes, Some(project.clone()));
            assert_eq!(file.workspaces, [EditorSession { root: Some(project.clone()), ..Default::default() }]);
        }
        assert_eq!(read_session(None, None), SessionFile::default());
    }

    #[test]
    fn workspace_sessions_round_trip_and_ignore_last_project() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        std::fs::create_dir(&a).unwrap();
        std::fs::create_dir(&b).unwrap();
        let file = SessionFile {
            workspaces: vec![
                EditorSession { root: Some(a.clone()), tabs: vec![a.join("x.rs")], active: Some(a.join("x.rs")), ..Default::default() },
                EditorSession { root: Some(b.clone()), recovery: vec![RecoveryBuffer { path: None, text: "draft".into() }], ..Default::default() },
                EditorSession::default(),
            ],
            active_workspace: 1,
        };
        let bytes = serde_json::to_vec(&file).unwrap();
        assert_eq!(read_session(Some(&bytes), Some(dir.path().to_path_buf())), file);

        // A folder that is gone opens without it; its files and unsaved edits stay.
        std::fs::remove_dir(&b).unwrap();
        let read = read_session(Some(&bytes), None);
        assert_eq!(read.workspaces[1].root, None);
        assert_eq!(read.workspaces[1].recovery.len(), 1);

        // An active index past the end, or no workspaces at all, still gives a usable session.
        let broken = serde_json::to_vec(&SessionFile { workspaces: file.workspaces.clone(), active_workspace: 9 }).unwrap();
        assert_eq!(read_session(Some(&broken), None).active_workspace, 2);
        let empty = serde_json::to_vec(&SessionFile { workspaces: Vec::new(), active_workspace: 0 }).unwrap();
        assert_eq!(read_session(Some(&empty), None), SessionFile::default());
    }

    #[test]
    fn opening_a_folder_switches_fills_an_empty_tab_or_asks() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b, c) = (dir.path().join("a"), dir.path().join("b"), dir.path().join("c"));
        for path in [&a, &b, &c] { std::fs::create_dir(path).unwrap(); }
        let roots = [Some(a.as_path()), None, Some(b.as_path())];
        assert_eq!(folder_target(&roots, 0, &b), FolderTarget::Switch(2));
        assert_eq!(folder_target(&roots, 1, &a), FolderTarget::Switch(0), "already open wins over an empty tab");
        assert_eq!(folder_target(&roots, 1, &c), FolderTarget::Current);
        assert_eq!(folder_target(&roots, 0, &c), FolderTarget::Ask);
        // The same folder spelled with a trailing separator is still the same folder.
        let spelled = PathBuf::from(format!("{}{}", a.display(), std::path::MAIN_SEPARATOR));
        assert_eq!(folder_target(&roots, 2, &spelled), FolderTarget::Switch(0));
        assert!(!same_folder(&a, &dir.path().join("missing")));
    }

    #[test]
    fn labels_use_the_folder_name_and_tell_same_names_apart() {
        let (work, home) = (Path::new("/work/app"), Path::new("/home/app"));
        let other = Path::new("/work/site");
        assert_eq!(labels(&[Some(work), None, Some(other)]), ["app", "New Tab", "site"]);
        assert_eq!(labels(&[Some(work), Some(home), Some(other)]), ["app — work", "app — home", "site"]);
    }

    #[test]
    fn tabs_share_the_strip_between_limits_and_drags_land_on_the_tab_under_the_cursor() {
        assert_eq!(tab_width(1000.0, 2), TAB_MAX);
        assert_eq!(tab_width(900.0, 6), 150.0);
        assert_eq!(tab_width(500.0, 10), TAB_MIN, "too many to fit: the strip scrolls");
        assert_eq!(tab_width(f32::INFINITY, 3), TAB_MAX);
        assert_eq!(drag_target(-5.0, 150.0, 4), 0);
        assert_eq!(drag_target(149.0, 150.0, 4), 0);
        assert_eq!(drag_target(150.0, 150.0, 4), 1);
        assert_eq!(drag_target(10_000.0, 150.0, 4), 3);
        assert_eq!(drag_target(10.0, 150.0, 0), 0);
    }

    #[test]
    fn switching_wraps_and_bulk_close_picks_the_right_tabs() {
        assert_eq!(cycle(2, 3, true), 0);
        assert_eq!(cycle(0, 3, false), 2);
        assert_eq!(cycle(1, 3, true), 2);
        assert_eq!(cycle(0, 0, true), 0);
        assert_eq!(to_close(4, 1, false), [3, 2, 0]);
        assert_eq!(to_close(4, 1, true), [3, 2]);
        assert!(to_close(4, 3, true).is_empty());
        assert!(to_close(2, 5, false).is_empty());
    }
}
