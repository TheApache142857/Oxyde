//! Several documents at once, each in its own OS window, as Creo opens models: File › Open
//! opens a part or an assembly in a new window, and the Window menu brings any of them
//! forward.
//!
//! All windows live in one process and share one kernel, the preferences and the material
//! library. Each has its own document, selection, undo history, sketch session and panels:
//! a [`DocWindow`]. The main window's state is the one [`OxydeApp`] holds; another window
//! is drawn with its state swapped into the application ([`OxydeApp::swap_window`]), so
//! that every command, written against `self.doc` and the rest, works in every window.

use std::path::{Path, PathBuf};

use eframe::egui;

use crate::app::{DocWindow, OxydeApp};

/// A document window besides the main one.
pub(crate) struct ExtraWindow {
    pub(crate) id: egui::ViewportId,
    pub(crate) state: DocWindow,
    /// A file to open in the window when it is first drawn.
    pub(crate) pending_open: Option<PathBuf>,
}

/// A window as the Window menu lists it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct WindowEntry {
    pub(crate) id: egui::ViewportId,
    pub(crate) title: String,
    /// The part and assembly files it has open.
    pub(crate) paths: Vec<PathBuf>,
}

/// The size a new window opens at, in points.
const NEW_WINDOW_SIZE: [f32; 2] = [1280.0, 800.0];

impl OxydeApp {
    /// The part and assembly files the window state the application holds has open,
    /// those set aside while a component is edited in place included.
    pub(crate) fn open_paths(&self) -> Vec<PathBuf> {
        let mut paths: Vec<PathBuf> = self.doc.path.iter().cloned().collect();
        if let Some(asm) = &self.asm {
            paths.extend(asm.path.iter().cloned());
        }
        paths.extend(self.edit_stack.iter().filter_map(|s| s.asm.path.clone()));
        paths
    }

    /// Whether the window shows nothing of the user's yet: a new, unchanged, unsaved part
    /// without features, and no assembly.
    pub(crate) fn window_is_blank(&self) -> bool {
        self.asm.is_none()
            && self.edit_stack.is_empty()
            && self.doc.path.is_none()
            && !self.any_modified()
            && self.doc.part.definition().features.is_empty()
    }

    /// The window that has `path` open, from the list gathered this frame.
    pub(crate) fn window_with(&self, path: &Path) -> Option<egui::ViewportId> {
        self.window_list
            .iter()
            .find(|w| w.paths.iter().any(|p| same_file(p, path)))
            .map(|w| w.id)
    }

    /// Opens `path`: brings forward the window that has it open already; else opens it in
    /// a new window, as Creo does, unless this window is blank or the preference says to
    /// replace the document.
    pub(crate) fn open_document(&mut self, path: &Path, ctx: &egui::Context) {
        if let Some(id) = self.window_with(path) {
            ctx.send_viewport_cmd_to(id, egui::ViewportCommand::Focus);
            self.status = format!("{} is open already", path.display());
            return;
        }
        if self.opens_in_new_window() {
            self.open_in_new_window(path.to_path_buf());
        } else {
            self.open_path(path);
        }
    }

    /// Whether opening a document goes to a new window rather than replacing this one's.
    pub(crate) fn opens_in_new_window(&self) -> bool {
        self.prefs.open_in_new_window && !self.window_is_blank()
    }

    /// A new window that opens `path` when it is first drawn.
    pub(crate) fn open_in_new_window(&mut self, path: PathBuf) {
        let id = egui::ViewportId::from_hash_of(("oxyde window", self.next_window));
        self.next_window += 1;
        let mut state = DocWindow::fresh();
        state.viewport = self.new_viewport();
        self.status = format!("{} opens in a new window", path.display());
        self.windows.push(ExtraWindow {
            id,
            state,
            pending_open: Some(path),
        });
    }

    /// Every window's title and open files, the main window's first.
    pub(crate) fn gather_windows(&mut self) {
        let mut list = vec![WindowEntry {
            id: egui::ViewportId::ROOT,
            title: self.window_title.clone(),
            paths: self.open_paths(),
        }];
        let mut extras = std::mem::take(&mut self.windows);
        for w in &mut extras {
            self.swap_window(&mut w.state);
            let mut paths = self.open_paths();
            paths.extend(w.pending_open.iter().cloned());
            list.push(WindowEntry {
                id: w.id,
                title: self.window_title.clone(),
                paths,
            });
            self.swap_window(&mut w.state);
        }
        self.windows = extras;
        self.window_list = list;
    }

    /// Draws every window besides the main one, each in its own OS window, with its state
    /// swapped in; a window whose close went through is freed and dropped.
    pub(crate) fn draw_extra_windows(&mut self, ctx: &egui::Context) {
        let mut extras = std::mem::take(&mut self.windows);
        let mut closed = Vec::new();
        for (index, w) in extras.iter_mut().enumerate() {
            self.swap_window(&mut w.state);
            let mut pending = w.pending_open.take();
            let title = if self.window_title.is_empty() {
                "Oxyde".to_owned()
            } else {
                self.window_title.clone()
            };
            let builder = egui::ViewportBuilder::default()
                .with_title(title)
                .with_inner_size(NEW_WINDOW_SIZE);
            let mut closing = false;
            ctx.show_viewport_immediate(w.id, builder, |ctx, _class| {
                if let Some(path) = pending.take() {
                    self.open_path(&path);
                }
                self.guarded_frame(ctx);
                closing = self.allow_close
                    || (ctx.input(|i| i.viewport().close_requested())
                        && !self.any_modified()
                        && self.edit_stack.is_empty());
            });
            if closing {
                self.release_window();
                closed.push(index);
            }
            self.swap_window(&mut w.state);
        }
        for index in closed.into_iter().rev() {
            extras.remove(index);
        }
        // Windows opened while these were drawn come after them.
        extras.append(&mut self.windows);
        self.windows = extras;
    }

    /// Frees what the window state the application holds keeps in the kernel and the
    /// assistant's agent: the window is closing.
    pub(crate) fn release_window(&mut self) {
        if let Some(mut session) = self.assistant.mcp.take()
            && let Some(kernel) = self.kernel.occt_mut()
        {
            session.release_in_window(kernel);
        }
        self.assistant.shutdown();
        self.drop_edit_stack();
        self.leave_assembly();
        self.doc.release(&mut self.kernel);
    }

    /// The main window is asked to close: the other windows close with it, unless one has
    /// unsaved changes, which is brought forward instead. Returns whether closing goes on.
    pub(crate) fn close_other_windows(&mut self, ctx: &egui::Context) -> bool {
        let mut extras = std::mem::take(&mut self.windows);
        let mut unsaved = None;
        for w in &mut extras {
            self.swap_window(&mut w.state);
            if unsaved.is_none() && (self.any_modified() || !self.edit_stack.is_empty()) {
                unsaved = Some((w.id, self.window_title.clone()));
            }
            self.swap_window(&mut w.state);
        }
        if let Some((id, title)) = unsaved {
            self.windows = extras;
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd_to(id, egui::ViewportCommand::Focus);
            self.status = format!("{title} has unsaved changes: save or close it first");
            return false;
        }
        for w in &mut extras {
            self.swap_window(&mut w.state);
            self.release_window();
            self.swap_window(&mut w.state);
        }
        true
    }

    /// Brings forward the window after the one drawn now, in the Window menu's order.
    pub(crate) fn focus_next_window(&self, ctx: &egui::Context) {
        let current = ctx.viewport_id();
        let n = self.window_list.len();
        if n < 2 {
            return;
        }
        let at = self
            .window_list
            .iter()
            .position(|w| w.id == current)
            .unwrap_or(0);
        if let Some(next) = self.window_list.get((at + 1) % n) {
            ctx.send_viewport_cmd_to(next.id, egui::ViewportCommand::Focus);
        }
    }

    /// The Window menu: every window, the one drawn now ticked; a click brings it
    /// forward.
    pub(crate) fn window_menu(&mut self, ui: &mut egui::Ui) {
        let current = ui.ctx().viewport_id();
        for w in self.window_list.clone() {
            let title = if w.title.is_empty() {
                "Oxyde".to_owned()
            } else {
                w.title.clone()
            };
            if ui.selectable_label(w.id == current, title).clicked() {
                ui.ctx()
                    .send_viewport_cmd_to(w.id, egui::ViewportCommand::Focus);
                ui.close_menu();
            }
        }
        ui.separator();
        if ui
            .add_enabled(
                self.window_list.len() > 1,
                egui::Button::new("Next Window").shortcut_text("Ctrl+Tab"),
            )
            .clicked()
        {
            self.focus_next_window(ui.ctx());
            ui.close_menu();
        }
    }
}

/// Whether two paths name one file, compared as the file system resolves them when it
/// can.
fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prefs::Preferences;

    fn app() -> OxydeApp {
        OxydeApp::blank(Preferences::default(), None)
    }

    /// The window state the application holds is that of a window that has changed its
    /// part: one undo step, a feature selected, a status.
    fn change_part(app: &mut OxydeApp, status: &str) {
        app.doc.checkpoint();
        app.doc
            .part
            .add_feature(oxyde_model::FeatureKind::Sketch(oxyde_model::Sketch::new(
                oxyde_model::SketchPlane::Standard(oxyde_model::StandardPlane::Front),
            )));
        app.doc.modified = true;
        app.selected_feature = Some(oxyde_model::FeatureId::from_raw(1));
        app.status = status.to_owned();
    }

    #[test]
    fn each_window_keeps_its_own_document_selection_and_undo() {
        let mut app = app();
        change_part(&mut app, "main");
        app.open_in_new_window(PathBuf::from("other.oxyprt"));
        assert_eq!(app.windows.len(), 1);
        // The window it was opened from says so.
        assert!(
            app.status.contains("opens in a new window"),
            "{}",
            app.status
        );
        app.status = "main".to_owned();
        // The other window, swapped in, has none of the main window's state.
        let mut other = app.windows.remove(0);
        app.swap_window(&mut other.state);
        assert_eq!(app.doc.undo_depth(), 0);
        assert!(app.doc.part.features().is_empty());
        assert_eq!(app.selected_feature, None);
        change_part(&mut app, "other");
        app.doc.checkpoint();
        assert_eq!(app.doc.undo_depth(), 2);
        app.swap_window(&mut other.state);
        // Back to the main window: its own state, and the shared preferences untouched.
        assert_eq!(app.doc.undo_depth(), 1);
        assert_eq!(app.status, "main");
        assert_eq!(other.state.status, "other");
        assert_eq!(other.state.doc.undo_depth(), 2);
        assert_eq!(app.prefs, Preferences::default());
    }

    #[test]
    fn a_document_opens_in_a_new_window_unless_the_window_is_blank() {
        let mut app = app();
        // A window that shows nothing yet is used.
        assert!(app.window_is_blank());
        assert!(!app.opens_in_new_window());
        change_part(&mut app, "main");
        assert!(!app.window_is_blank());
        assert!(app.opens_in_new_window());
        // Unless the preference says to replace the document.
        app.prefs.open_in_new_window = false;
        assert!(!app.opens_in_new_window());
        app.prefs.open_in_new_window = true;
        let path = std::env::temp_dir().join("oxyde-windows-test-part.oxyprt");
        let ctx = egui::Context::default();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            app.open_document(&path, ctx);
        });
        assert_eq!(app.windows.len(), 1);
        assert_eq!(app.windows[0].pending_open.as_deref(), Some(path.as_path()));
        // Asked again, the window that has it is brought forward: no second copy.
        app.gather_windows();
        assert_eq!(app.window_with(&path), Some(app.windows[0].id));
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            app.open_document(&path, ctx);
        });
        assert_eq!(app.windows.len(), 1);
        assert!(app.status.contains("open already"), "{}", app.status);
        // The Window menu lists both, the main window first.
        let ids: Vec<_> = app.window_list.iter().map(|w| w.id).collect();
        assert_eq!(ids, vec![egui::ViewportId::ROOT, app.windows[0].id]);
    }

    #[test]
    fn the_main_window_closes_the_others_unless_one_has_unsaved_changes() {
        let mut app = app();
        app.open_in_new_window(PathBuf::from("other.oxyprt"));
        app.windows[0].state.doc.modified = true;
        let ctx = egui::Context::default();
        let mut closes = true;
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            closes = app.close_other_windows(ctx);
        });
        assert!(!closes);
        assert_eq!(app.windows.len(), 1);
        assert!(app.status.contains("unsaved changes"), "{}", app.status);
        app.windows[0].state.doc.modified = false;
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            closes = app.close_other_windows(ctx);
        });
        assert!(closes);
        assert!(app.windows.is_empty());
    }

    #[test]
    fn a_window_whose_close_went_through_is_dropped() {
        let mut app = app();
        app.open_in_new_window(PathBuf::from("first.oxyprt"));
        app.open_in_new_window(PathBuf::from("second.oxyprt"));
        let second = app.windows[1].id;
        for w in &mut app.windows {
            w.pending_open = None;
        }
        app.windows[0].state.allow_close = true;
        let ctx = egui::Context::default();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            app.draw_extra_windows(ctx);
        });
        assert_eq!(app.windows.len(), 1);
        assert_eq!(app.windows[0].id, second);
    }
}
