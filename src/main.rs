mod chat;

use std::path::{Path, PathBuf};
use syntect::easy::HighlightLines;
use syntect::highlighting::{Theme, ThemeSet};
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;

fn main() -> eframe::Result<()> {
    eframe::run_native(
        "editor",
        eframe::NativeOptions::default(),
        Box::new(|_cc| Ok(Box::new(App::new()))),
    )
}

struct App {
    root: Option<PathBuf>,
    file_path: Option<PathBuf>,
    content: String,
    dirty: bool,
    renaming: Option<(PathBuf, String)>,
    rename_focus_pending: bool,
    creating: Option<(PathBuf, bool, String)>,
    create_focus_pending: bool,
    pending_expand: Option<PathBuf>,
    tree_cursor: Option<PathBuf>,
    visible_paths: Vec<PathBuf>,
    syntax_set: SyntaxSet,
    themes: Vec<(String, Theme)>,
    theme_index: usize,
    chat: chat::Chat,
    ai_visible: bool,
}

impl App {
    fn new() -> Self {
        Self {
            root: load_last_project(),
            file_path: None,
            content: String::new(),
            dirty: false,
            renaming: None,
            rename_focus_pending: false,
            creating: None,
            create_focus_pending: false,
            pending_expand: None,
            tree_cursor: None,
            visible_paths: Vec::new(),
            syntax_set: SyntaxSet::load_defaults_newlines(),
            themes: load_themes(),
            theme_index: 0,
            chat: chat::Chat::default(),
            ai_visible: load_ai_visible(),
        }
    }

    /// Free function (not a `&self` method) so callers can borrow only the
    /// fields they need, keeping this disjoint from a simultaneous
    /// `&mut self.content` borrow in the `TextEdit` layouter closure.
    fn highlight(
        syntax_set: &SyntaxSet,
        theme: &Theme,
        extension: Option<&str>,
        text: &str,
    ) -> egui::text::LayoutJob {
        let syntax = extension
            .and_then(|ext| syntax_set.find_syntax_by_extension(ext))
            .unwrap_or_else(|| syntax_set.find_syntax_plain_text());

        let mut highlighter = HighlightLines::new(syntax, theme);
        let mut job = egui::text::LayoutJob::default();
        for line in LinesWithEndings::from(text) {
            let Ok(ranges) = highlighter.highlight_line(line, syntax_set) else {
                continue;
            };
            for (style, piece) in ranges {
                job.append(
                    piece,
                    0.0,
                    egui::TextFormat {
                        font_id: egui::FontId::monospace(14.0),
                        color: egui::Color32::from_rgb(
                            style.foreground.r,
                            style.foreground.g,
                            style.foreground.b,
                        ),
                        ..Default::default()
                    },
                );
            }
        }
        job
    }

    fn open_file_dialog(&mut self) {
        if let Some(path) = rfd::FileDialog::new().pick_file() {
            self.open_path(path);
        }
    }

    fn open_path(&mut self, path: PathBuf) {
        if self.dirty && !confirm("Unsaved changes", "Discard unsaved changes?") {
            return;
        }
        if let Ok(text) = std::fs::read_to_string(&path) {
            self.content = text;
            self.file_path = Some(path);
            self.dirty = false;
        }
    }

    fn open_folder(&mut self) {
        if let Some(path) = rfd::FileDialog::new().pick_folder() {
            save_last_project(&path);
            self.root = Some(path);
        }
    }

    fn close_project(&mut self) {
        self.root = None;
        if let Some(path) = last_project_path() {
            let _ = std::fs::remove_file(path);
        }
    }

    fn save(&mut self) {
        let path = match &self.file_path {
            Some(p) => Some(p.clone()),
            None => rfd::FileDialog::new().save_file(),
        };
        if let Some(path) = path {
            if std::fs::write(&path, &self.content).is_ok() {
                self.file_path = Some(path);
                self.dirty = false;
            }
        }
    }

    fn title(&self) -> String {
        let name = self
            .file_path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "untitled".to_string());
        if self.dirty {
            format!("{name} *")
        } else {
            name
        }
    }

    fn sidebar_ui(&mut self, ui: &mut egui::Ui) {
        let Some(root) = self.root.clone() else {
            ui.weak("No folder open (File > Open Folder)");
            return;
        };

        let no_widget_focused = ui.ctx().memory(|m| m.focused().is_none());
        let move_delta = if !no_widget_focused {
            0
        } else if ui.ctx().input(|i| i.key_pressed(egui::Key::ArrowDown)) {
            1
        } else if ui.ctx().input(|i| i.key_pressed(egui::Key::ArrowUp)) {
            -1
        } else {
            0
        };
        let left_pressed =
            no_widget_focused && ui.ctx().input(|i| i.key_pressed(egui::Key::ArrowLeft));
        let right_pressed =
            no_widget_focused && ui.ctx().input(|i| i.key_pressed(egui::Key::ArrowRight));

        self.visible_paths.clear();
        self.visible_paths.push(root.clone());
        let is_root_cursor = self.tree_cursor.as_deref() == Some(root.as_path());

        egui::ScrollArea::vertical().show(ui, |ui| {
            let open_override = self.expand_override(&root, is_root_cursor, left_pressed, right_pressed);
            let root_name = root.file_name().map_or_else(
                || root.to_string_lossy().to_string(),
                |n| n.to_string_lossy().to_string(),
            );
            let mut response = egui::CollapsingHeader::new(root_name)
                .id_salt(&root)
                .default_open(true)
                .open(open_override)
                .show(ui, |ui| {
                    self.render_dir(ui, &root, left_pressed, right_pressed);
                });
            if is_root_cursor {
                response.header_response = response.header_response.highlight();
            }
            if response.header_response.clicked() {
                self.tree_cursor = Some(root.clone());
            }
            response
                .header_response
                .context_menu(|ui| self.entry_context_menu(ui, &root, true, false));
        });

        if move_delta != 0 {
            self.move_tree_cursor(move_delta);
        }
    }

    fn move_tree_cursor(&mut self, delta: isize) {
        if self.visible_paths.is_empty() {
            return;
        }
        let current_index = self
            .tree_cursor
            .as_ref()
            .and_then(|c| self.visible_paths.iter().position(|p| p == c));
        let next_index = match current_index {
            Some(i) => (i as isize + delta).clamp(0, self.visible_paths.len() as isize - 1) as usize,
            None if delta > 0 => 0,
            None => self.visible_paths.len() - 1,
        };
        let path = self.visible_paths[next_index].clone();
        self.tree_cursor = Some(path.clone());
        if path.is_file() && self.file_path.as_deref() != Some(path.as_path()) {
            self.open_path(path);
        }
    }

    fn expand_override(
        &mut self,
        path: &Path,
        is_cursor: bool,
        left_pressed: bool,
        right_pressed: bool,
    ) -> Option<bool> {
        if self.pending_expand.as_deref() == Some(path) {
            self.pending_expand = None;
            return Some(true);
        }
        if is_cursor && left_pressed {
            return Some(false);
        }
        if is_cursor && right_pressed {
            return Some(true);
        }
        None
    }

    fn render_dir(&mut self, ui: &mut egui::Ui, dir: &Path, left_pressed: bool, right_pressed: bool) {
        let Ok(read_dir) = std::fs::read_dir(dir) else {
            return;
        };
        let mut entries: Vec<PathBuf> = read_dir.flatten().map(|e| e.path()).collect();
        entries.sort_by_key(|p| (!p.is_dir(), p.file_name().map(|n| n.to_owned())));

        if self.creating.as_ref().map(|(p, _, _)| p.as_path()) == Some(dir) {
            let (_, is_dir, buf) = self.creating.as_mut().unwrap();
            let hint = if *is_dir { "new folder name" } else { "new file name" };
            let response = ui.add(egui::TextEdit::singleline(buf).hint_text(hint));
            if self.create_focus_pending {
                response.request_focus();
                self.create_focus_pending = false;
            }
            if response.lost_focus() {
                if ui.ctx().input(|i| i.key_pressed(egui::Key::Enter)) {
                    self.commit_create();
                } else {
                    self.creating = None;
                }
            }
        }

        for path in entries {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            self.visible_paths.push(path.clone());
            let is_cursor = self.tree_cursor.as_deref() == Some(path.as_path());

            if self.renaming.as_ref().map(|(p, _)| p) == Some(&path) {
                let (_, buf) = self.renaming.as_mut().unwrap();
                let response = ui.add(egui::TextEdit::singleline(buf));
                if self.rename_focus_pending {
                    response.request_focus();
                    self.rename_focus_pending = false;
                }
                if response.lost_focus() {
                    if ui.ctx().input(|i| i.key_pressed(egui::Key::Enter)) {
                        self.commit_rename();
                    } else {
                        self.renaming = None;
                    }
                }
                continue;
            }

            if path.is_dir() {
                let open_override = self.expand_override(&path, is_cursor, left_pressed, right_pressed);
                let mut response = egui::CollapsingHeader::new(name)
                    .id_salt(&path)
                    .default_open(false)
                    .open(open_override)
                    .show(ui, |ui| {
                        self.render_dir(ui, &path, left_pressed, right_pressed);
                    });
                if is_cursor {
                    response.header_response = response.header_response.highlight();
                }
                if response.header_response.clicked() {
                    self.tree_cursor = Some(path.clone());
                }
                response
                    .header_response
                    .context_menu(|ui| self.entry_context_menu(ui, &path, true, true));
            } else {
                let is_selected = self.file_path.as_deref() == Some(path.as_path());
                let mut response = ui.selectable_label(is_selected, name);
                if is_cursor {
                    response = response.highlight();
                }
                if response.clicked() {
                    self.tree_cursor = Some(path.clone());
                    self.open_path(path.clone());
                }
                response.context_menu(|ui| self.entry_context_menu(ui, &path, false, true));
            }
        }
    }

    fn entry_context_menu(
        &mut self,
        ui: &mut egui::Ui,
        path: &Path,
        is_dir: bool,
        allow_rename_delete: bool,
    ) {
        let target_dir = if is_dir {
            path.to_path_buf()
        } else {
            path.parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("."))
        };

        if ui.button("New File").clicked() {
            self.creating = Some((target_dir.clone(), false, String::new()));
            self.create_focus_pending = true;
            self.pending_expand = Some(target_dir.clone());
            ui.close();
        }
        if ui.button("New Folder").clicked() {
            self.creating = Some((target_dir.clone(), true, String::new()));
            self.create_focus_pending = true;
            self.pending_expand = Some(target_dir);
            ui.close();
        }

        ui.separator();

        if ui.button("Copy Path").clicked() {
            ui.ctx().copy_text(path.display().to_string());
            ui.close();
        }
        if ui.button("Copy Relative Path").clicked() {
            let relative = self
                .root
                .as_ref()
                .and_then(|root| path.strip_prefix(root).ok())
                .unwrap_or(path);
            ui.ctx().copy_text(relative.display().to_string());
            ui.close();
        }
        if ui.button(reveal_label()).clicked() {
            reveal_in_file_manager(path);
            ui.close();
        }

        if allow_rename_delete {
            ui.separator();
            if ui.button("Rename").clicked() {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                self.renaming = Some((path.to_path_buf(), name));
                self.rename_focus_pending = true;
                ui.close();
            }
            if ui.button("Delete").clicked() {
                self.delete_path(path);
                ui.close();
            }
        }
    }

    fn commit_create(&mut self) {
        let Some((dir, is_dir, name)) = self.creating.take() else {
            return;
        };
        if name.is_empty() {
            return;
        }
        let target = dir.join(&name);
        if is_dir {
            let _ = std::fs::create_dir(target);
        } else {
            let _ = std::fs::write(target, "");
        }
    }

    fn delete_path(&mut self, path: &Path) {
        if !confirm(
            "Delete",
            &format!("Delete \"{}\"? This cannot be undone.", path.display()),
        ) {
            return;
        }
        let result = if path.is_dir() {
            std::fs::remove_dir_all(path)
        } else {
            std::fs::remove_file(path)
        };
        if result.is_ok() && self.file_path.as_deref() == Some(path) {
            self.file_path = None;
            self.content.clear();
            self.dirty = false;
        }
    }

    fn commit_rename(&mut self) {
        let Some((old_path, new_name)) = self.renaming.take() else {
            return;
        };
        if new_name.is_empty() {
            return;
        }
        let new_path = old_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(&new_name);
        if std::fs::rename(&old_path, &new_path).is_ok() && self.file_path.as_deref() == Some(&old_path) {
            self.file_path = Some(new_path);
        }
    }
}

fn load_themes() -> Vec<(String, Theme)> {
    let mut themes = Vec::new();

    let catppuccin = ThemeSet::load_from_reader(&mut std::io::Cursor::new(
        include_bytes!("themes/catppuccin_mocha.tmTheme").as_slice(),
    ))
    .expect("bundled catppuccin_mocha.tmTheme should parse");
    themes.push(("Catppuccin Mocha".to_string(), catppuccin));

    for (name, theme) in ThemeSet::load_defaults().themes {
        themes.push((name, theme));
    }
    themes
}

fn config_path(name: &str) -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".config").join("editor").join(name))
}

fn last_project_path() -> Option<PathBuf> {
    config_path("last_project")
}

fn save_last_project(root: &Path) {
    let Some(path) = last_project_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, root.to_string_lossy().as_bytes());
}

fn load_last_project() -> Option<PathBuf> {
    let text = std::fs::read_to_string(last_project_path()?).ok()?;
    let path = PathBuf::from(text.trim());
    path.is_dir().then_some(path)
}

fn save_ai_visible(visible: bool) {
    let Some(path) = config_path("ai_visible") else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, if visible { "true" } else { "false" });
}

fn load_ai_visible() -> bool {
    config_path("ai_visible")
        .and_then(|path| std::fs::read_to_string(path).ok())
        .map(|text| text.trim() == "true")
        .unwrap_or(false)
}

fn reveal_label() -> &'static str {
    if cfg!(target_os = "macos") {
        "Reveal in Finder"
    } else if cfg!(target_os = "windows") {
        "Show in Explorer"
    } else {
        "Open Containing Folder"
    }
}

fn reveal_in_file_manager(path: &Path) {
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg("-R").arg(path).spawn();
    }
    #[cfg(target_os = "windows")]
    {
        let mut arg = std::ffi::OsString::from("/select,");
        arg.push(path.as_os_str());
        let _ = std::process::Command::new("explorer").arg(arg).spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let target = if path.is_dir() {
            path
        } else {
            path.parent().unwrap_or(path)
        };
        let _ = std::process::Command::new("xdg-open").arg(target).spawn();
    }
}

fn confirm(title: &str, description: &str) -> bool {
    rfd::MessageDialog::new()
        .set_title(title)
        .set_description(description)
        .set_buttons(rfd::MessageButtons::YesNo)
        .show()
        == rfd::MessageDialogResult::Yes
}

/// Derives app-wide `egui::Visuals` from a syntect theme's editor colors, so
/// panels/buttons/menus match the selected syntax theme instead of only the
/// code editor text.
fn visuals_from_theme(theme: &Theme) -> egui::Visuals {
    let to_color =
        |c: syntect::highlighting::Color| egui::Color32::from_rgba_unmultiplied(c.r, c.g, c.b, c.a);
    let settings = &theme.settings;
    let background = settings.background.map(to_color);

    let is_dark = background
        .map(|c| (c.r() as u32 + c.g() as u32 + c.b() as u32) < 384)
        .unwrap_or(true);
    let mut visuals = if is_dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };

    if let Some(bg) = background {
        visuals.panel_fill = bg;
        visuals.window_fill = bg;
        visuals.extreme_bg_color = bg;
        visuals.widgets.noninteractive.bg_fill = bg;
        visuals.widgets.open.bg_fill = bg;
    }
    if let Some(fg) = settings.foreground.map(to_color) {
        visuals.widgets.noninteractive.fg_stroke.color = fg;
        visuals.widgets.inactive.fg_stroke.color = fg;
        visuals.widgets.hovered.fg_stroke.color = fg;
        visuals.widgets.active.fg_stroke.color = fg;
    }
    if let Some(selection) = settings.selection.map(to_color) {
        visuals.selection.bg_fill = selection;
        visuals.widgets.hovered.bg_fill = selection;
        visuals.widgets.active.bg_fill = selection;
    }
    if let Some(line_highlight) = settings.line_highlight.map(to_color) {
        visuals.faint_bg_color = line_highlight;
        visuals.widgets.inactive.bg_fill = line_highlight;
    }
    visuals
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        ctx.set_visuals(visuals_from_theme(&self.themes[self.theme_index].1));
        let save_shortcut =
            egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::S);
        let open_shortcut =
            egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::O);
        if ctx.input_mut(|i| i.consume_shortcut(&save_shortcut)) {
            self.save();
        }
        if ctx.input_mut(|i| i.consume_shortcut(&open_shortcut)) {
            self.open_file_dialog();
        }
        self.chat.poll();

        egui::Panel::top("top_bar").show(ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                ui.menu_button("File", |ui| {
                    if ui.button("Open File (Cmd+O)").clicked() {
                        self.open_file_dialog();
                        ui.close();
                    }
                    if ui.button("Open Folder").clicked() {
                        self.open_folder();
                        ui.close();
                    }
                    if ui
                        .add_enabled(self.root.is_some(), egui::Button::new("Close Folder"))
                        .clicked()
                    {
                        self.close_project();
                        ui.close();
                    }
                    if ui.button("Save (Cmd+S)").clicked() {
                        self.save();
                        ui.close();
                    }
                });
                ui.menu_button("Theme", |ui| {
                    let names: Vec<String> =
                        self.themes.iter().map(|(name, _)| name.clone()).collect();
                    for (i, name) in names.into_iter().enumerate() {
                        if ui.button(name).clicked() {
                            self.theme_index = i;
                            ui.close();
                        }
                    }
                });
                ui.label(self.title());
            });
        });

        egui::Panel::bottom("status_bar").show(ui, |ui| {
            ui.horizontal(|ui| {
                let status = self
                    .file_path
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "No file open".to_string());
                ui.label(status);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let label = if self.ai_visible { "Hide AI" } else { "Show AI" };
                    if ui.button(label).clicked() {
                        self.ai_visible = !self.ai_visible;
                        save_ai_visible(self.ai_visible);
                    }
                });
            });
        });

        egui::Panel::left("sidebar")
            .default_size(220.0)
            .show(ui, |ui| {
                self.sidebar_ui(ui);
            });

        if self.ai_visible {
            egui::Panel::right("ai_sidebar")
                .default_size(320.0)
                .show(ui, |ui| {
                    self.chat.ui(ui);
                });
        }

        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::both()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let mut layouter = |ui: &egui::Ui, buf: &dyn egui::TextBuffer, _wrap_width: f32| {
                        let extension = self
                            .file_path
                            .as_ref()
                            .and_then(|p| p.extension())
                            .and_then(|e| e.to_str());
                        let theme = &self.themes[self.theme_index].1;
                        let mut job = App::highlight(&self.syntax_set, theme, extension, buf.as_str());
                        job.wrap.max_width = f32::INFINITY;
                        ui.fonts_mut(|f| f.layout_job(job))
                    };
                    let response = ui.add(
                        egui::TextEdit::multiline(&mut self.content)
                            .font(egui::TextStyle::Monospace)
                            .code_editor()
                            .desired_width(f32::INFINITY)
                            .layouter(&mut layouter),
                    );
                    if response.changed() {
                        self.dirty = true;
                    }
                });
        });
    }
}
