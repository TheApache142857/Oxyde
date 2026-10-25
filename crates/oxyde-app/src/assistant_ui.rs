//! The assistant panel (the *MCP* button beside the command tabs): a conversation with a
//! coding agent that models on the document open in this window through Oxyde's MCP tools.
//! The plumbing is in `assistant.rs`; here are the panel and what the window does for the
//! agent's tools: it lends the document and its kernel to an `oxyde_mcp::Session` for each
//! message, and records what a reply changed as one undo step.

use std::path::PathBuf;

use eframe::egui::{self, RichText};
use oxyde_mcp::window::WindowDocument;
use serde_json::{Value, json};

use crate::app::OxydeApp;
use crate::assistant::{Entry, KNOWN_AGENTS, State, find_agents};

impl OxydeApp {
    /// The *MCP* button, after the command tabs: opens and closes the panel.
    pub(crate) fn assistant_toggle(&mut self, ui: &mut egui::Ui) {
        let text = RichText::new("MCP").size(13.0);
        let text = if self.assistant.open {
            text.strong()
        } else {
            text
        };
        if ui
            .selectable_label(self.assistant.open, text)
            .on_hover_text(
                "Assistant: a coding agent installed on this computer (Claude Code, Codex) \
                 models on this document through Oxyde's MCP tools",
            )
            .clicked()
        {
            self.assistant.open = !self.assistant.open;
        }
    }

    /// The folder the agent works in: the document's, else a scratch folder of Oxyde's.
    fn assistant_folder(&self) -> PathBuf {
        let path = match &self.asm {
            Some(doc) => doc.path.clone(),
            None => self.doc.path.clone(),
        };
        if let Some(folder) = path.and_then(|p| p.parent().map(std::path::Path::to_path_buf)) {
            return folder;
        }
        // An unsaved document has no folder: a scratch one of Oxyde's then, never the
        // user's home folder, which the agent's own file tools would otherwise work in.
        let scratch = std::env::temp_dir().join("oxyde-assistant");
        let _ = std::fs::create_dir_all(&scratch);
        scratch
    }

    /// Each frame: what the agent said, and its tools' messages, answered on the document.
    pub(crate) fn serve_assistant(&mut self) {
        let folder = self.assistant_folder();
        self.assistant.poll(&folder);
        let requests: Vec<_> = match &self.assistant.server {
            Some(server) => server.requests.try_iter().collect(),
            None => return,
        };
        for request in requests {
            if let Some(answer) = self.answer_tool_message(request.message.clone()) {
                request.answer(&answer);
            }
        }
    }

    /// Why the agent's tools must wait, if they must: the user is in the middle of something
    /// that a change underneath would break.
    fn assistant_blocked(&self) -> Option<&'static str> {
        if self.busy() {
            Some("Oxyde is busy with a long job; try again when it ends.")
        } else if self.sketch.is_some() {
            Some("The user is editing a sketch; ask them to finish it, then try again.")
        } else if self.dialog.is_some() || self.mate_dialog.is_some() {
            Some("The user has a command open; ask them to finish it, then try again.")
        } else {
            None
        }
    }

    /// Answers one MCP message of the agent's on the window's document.
    fn answer_tool_message(&mut self, message: Value) -> Option<Value> {
        let is_call = message.get("method").and_then(Value::as_str) == Some("tools/call");
        if is_call && let Some(why) = self.assistant_blocked() {
            let id = message.get("id").cloned()?;
            return Some(json!({ "jsonrpc": "2.0", "id": id, "result": {
                "content": [{ "type": "text", "text": why }], "isError": true } }));
        }
        let session = self
            .assistant
            .mcp
            .get_or_insert_with(oxyde_mcp::Session::for_window);
        if let Some(doc) = self.asm.as_mut() {
            let before = doc.assembly.clone();
            let mut lent = WindowDocument::Assembly {
                assembly: std::mem::take(&mut doc.assembly),
                path: doc.path.clone(),
                document_id: doc.document_id.clone(),
                properties: doc.properties.clone(),
            };
            let answer = session.handle_in_window(&mut self.kernel, &mut lent, message);
            if let WindowDocument::Assembly {
                assembly,
                properties,
                ..
            } = lent
            {
                doc.assembly = assembly;
                doc.properties = properties;
            }
            if doc.assembly != before {
                if self.assistant.turn_changed != Some(true) {
                    doc.checkpoint_from(before);
                    if self.assistant.turn_changed.is_some() {
                        self.assistant.turn_changed = Some(true);
                    }
                }
                doc.modified = true;
                doc.load_parts(&mut self.kernel);
                doc.solve();
            }
            return answer;
        }
        let before = self.doc.part.definition();
        let mut lent = WindowDocument::Part {
            part: Box::new(std::mem::replace(
                &mut self.doc.part,
                oxyde_model::Part::new(),
            )),
            path: self.doc.path.clone(),
            document_id: self.doc.document_id.clone(),
            properties: self.doc.properties.clone(),
        };
        let answer = session.handle_in_window(&mut self.kernel, &mut lent, message);
        if let WindowDocument::Part {
            part, properties, ..
        } = lent
        {
            self.doc.part = *part;
            self.doc.properties = properties;
        }
        if self.doc.part.definition() != before {
            if self.assistant.turn_changed == Some(true) {
                self.doc.modified = true;
            } else {
                self.doc.checkpoint_from(before);
                if self.assistant.turn_changed.is_some() {
                    self.assistant.turn_changed = Some(true);
                }
            }
            self.doc.revision = crate::document::next_revision();
            self.selection.clear();
        }
        answer
    }

    /// The panel, on the right of the window, while it is open.
    pub(crate) fn assistant_panel(&mut self, ctx: &egui::Context) {
        if !self.assistant.open {
            return;
        }
        if self.assistant.found.is_none() {
            self.assistant.found = Some(find_agents());
        }
        egui::SidePanel::right("assistant")
            .resizable(true)
            .default_width(380.0)
            .min_width(280.0)
            .show(ctx, |ui| {
                self.assistant_header(ui);
                ui.separator();
                self.assistant_agent_row(ui);
                egui::TopBottomPanel::bottom("assistant_input")
                    .frame(egui::Frame::none())
                    .show_inside(ui, |ui| self.assistant_composer(ui));
                egui::ScrollArea::vertical()
                    .stick_to_bottom(true)
                    .auto_shrink([false, false])
                    .show(ui, |ui| self.assistant_conversation(ui));
            });
    }

    fn assistant_header(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Assistant").strong().size(15.0));
            let t = crate::theme::tokens();
            let color = match self.assistant.state {
                State::Ready => t.accent,
                State::Working | State::Starting => t.warn,
                State::NotConnected => t.dim,
            };
            ui.label(
                RichText::new(self.assistant.state.label())
                    .small()
                    .color(color),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("Close").clicked() {
                    self.assistant.open = false;
                }
                if ui
                    .small_button("New chat")
                    .on_hover_text("Stops the agent and starts a new conversation")
                    .clicked()
                {
                    if let Some(mut session) = self.assistant.mcp.take() {
                        session.release_in_window(&mut self.kernel);
                    }
                    self.assistant.new_chat();
                }
            });
        });
    }

    fn assistant_agent_row(&mut self, ui: &mut egui::Ui) {
        let found = self.assistant.found.clone().unwrap_or_default();
        let a = &mut self.assistant;
        let other = found.len();
        let chosen = found
            .get(a.choice)
            .map_or_else(|| "Other command".to_owned(), |f| f.name.clone());
        ui.horizontal(|ui| {
            ui.label("Agent");
            egui::ComboBox::from_id_salt("assistant_agent")
                .selected_text(chosen)
                .show_ui(ui, |ui| {
                    for (i, agent) in found.iter().enumerate() {
                        ui.selectable_value(&mut a.choice, i, agent.name.as_str())
                            .on_hover_text(agent.program.display().to_string());
                    }
                    ui.selectable_value(&mut a.choice, other, "Other command")
                        .on_hover_text("Any agent that speaks the Agent Client Protocol");
                });
            match a.state {
                State::NotConnected => {
                    if ui.button("Connect").clicked() {
                        a.connect(Some(ui.ctx().clone()));
                    }
                }
                _ => {
                    if ui
                        .button("Disconnect")
                        .on_hover_text("Stops the agent")
                        .clicked()
                    {
                        a.disconnect();
                    }
                }
            }
            if ui
                .small_button("Refresh")
                .on_hover_text("Looks for installed agents again")
                .clicked()
            {
                a.found = Some(find_agents());
            }
        });
        if a.choice >= found.len() {
            ui.add(
                egui::TextEdit::singleline(&mut a.other_command)
                    .hint_text("Program and arguments, e.g. opencode acp")
                    .desired_width(f32::INFINITY),
            );
        }
        if found.is_empty() {
            ui.add_space(4.0);
            ui.label(
                RichText::new(
                    "No coding agent found. Install one (it needs Node.js), sign in to it, \
                     then Refresh:",
                )
                .small(),
            );
            for agent in &KNOWN_AGENTS {
                ui.horizontal(|ui| {
                    ui.label(agent.name);
                    if ui.small_button("Copy").clicked() {
                        ui.output_mut(|o| o.copied_text = agent.install.to_owned());
                    }
                });
                ui.label(RichText::new(agent.install).monospace().small());
            }
        }
        ui.separator();
    }

    fn assistant_conversation(&mut self, ui: &mut egui::Ui) {
        if self.assistant.entries.is_empty() {
            ui.add_space(12.0);
            ui.label(
                RichText::new(
                    "Ask the agent to model or change this document. It works through \
                     Oxyde's tools, on what is open here: changes show at once, and each \
                     reply is one undo step.",
                )
                .weak(),
            );
        }
        let t = crate::theme::tokens();
        let mut choice = None;
        for (index, entry) in self.assistant.entries.iter().enumerate() {
            match entry {
                Entry::User(text) => {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::TOP), |ui| {
                        egui::Frame::none()
                            .fill(t.accent_bg)
                            .rounding(8.0)
                            .inner_margin(egui::Margin::symmetric(10.0, 6.0))
                            .show(ui, |ui| {
                                ui.set_max_width(ui.available_width() * 0.85);
                                ui.label(text.as_str());
                            });
                    });
                }
                Entry::Agent(text) => {
                    ui.add(egui::Label::new(text.trim()).wrap());
                }
                Entry::Thought(text) => {
                    ui.add(
                        egui::Label::new(RichText::new(text.trim()).italics().weak().small())
                            .wrap(),
                    );
                }
                Entry::Tool { title, status, .. } => {
                    let color = match status.as_str() {
                        "failed" => t.err,
                        "completed" => t.dim,
                        _ => t.warn,
                    };
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(format!("Tool: {title}")).small());
                        ui.label(RichText::new(status).small().color(color));
                    });
                }
                Entry::Permission {
                    title,
                    options,
                    chosen,
                    ..
                } => {
                    egui::Frame::group(ui.style()).show(ui, |ui| {
                        ui.label(format!("The agent asks to {title}"));
                        match chosen {
                            Some(name) => {
                                ui.label(RichText::new(format!("Answered: {name}")).weak());
                            }
                            None => {
                                ui.horizontal_wrapped(|ui| {
                                    for option in options {
                                        // Refusals in the error colour, as the agent marks them.
                                        let name = RichText::new(option.name.as_str());
                                        let name = if option.kind.starts_with("reject") {
                                            name.color(t.err)
                                        } else {
                                            name
                                        };
                                        if ui.button(name).clicked() {
                                            choice = Some((index, option.id.clone()));
                                        }
                                    }
                                });
                            }
                        }
                    });
                }
                Entry::Note(text) => {
                    ui.label(RichText::new(text).weak().small());
                }
                Entry::Error(text) => {
                    ui.add(egui::Label::new(RichText::new(text).color(t.err)).wrap());
                }
            }
            ui.add_space(4.0);
        }
        if let Some((index, option)) = choice {
            self.assistant.choose(index, &option);
        }
    }

    fn assistant_composer(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        let a = &mut self.assistant;
        let response = ui.add(
            egui::TextEdit::multiline(&mut a.input)
                .hint_text("Ask the agent to change the design...")
                .desired_rows(3)
                .desired_width(f32::INFINITY),
        );
        // Enter sends, Shift+Enter starts a new line.
        let enter = response.has_focus()
            && ui.input(|i| i.key_pressed(egui::Key::Enter) && !i.modifiers.shift);
        let mut send = false;
        ui.horizontal(|ui| {
            if a.state == State::Working {
                if ui
                    .button("Stop")
                    .on_hover_text("Stops the reply under way")
                    .clicked()
                {
                    a.stop();
                }
            } else if ui.button("Send").clicked() {
                send = true;
            }
        });
        if (send || enter) && a.state != State::Working {
            // The Enter just pressed went into the text: it is not part of the message.
            let text = std::mem::take(&mut a.input);
            a.send(text, Some(ui.ctx().clone()));
            response.request_focus();
        }
        ui.add_space(4.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(app: &mut OxydeApp, id: u64, name: &str, arguments: Value) -> Value {
        app.answer_tool_message(json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": { "name": name, "arguments": arguments } }))
            .unwrap()
    }

    /// The agent's tools model on the window's own part, and what one reply changes is one
    /// undo step.
    #[test]
    fn a_reply_models_on_the_window_part_as_one_undo_step() {
        let mut app = OxydeApp::blank(crate::prefs::Preferences::default(), None);
        // A reply under way.
        app.assistant.turn_changed = Some(false);
        let answer = call(
            &mut app,
            1,
            "sketch",
            json!({ "plane": "front",
            "items": [{ "type": "rect", "center": [0, 0], "size": [40, 20] }] }),
        );
        assert_eq!(answer["result"]["isError"], false, "{answer}");
        let answer = call(
            &mut app,
            2,
            "extrude",
            json!({ "sketch": "Sketch1", "depth": 10 }),
        );
        assert_eq!(answer["result"]["isError"], false, "{answer}");
        assert_eq!(app.doc.part.features().len(), 2);
        assert!(app.doc.part.body().is_some());
        assert_eq!(app.doc.undo_depth(), 1);
        assert!(app.doc.modified);
        // A tool that only looks changes nothing.
        let answer = call(&mut app, 3, "part_info", json!({}));
        assert_eq!(answer["result"]["isError"], false, "{answer}");
        assert_eq!(app.doc.undo_depth(), 1);
        app.undo();
        assert!(app.doc.part.features().is_empty());
    }

    /// Outside a reply, each change is its own step; new, open and save stay the user's.
    #[test]
    fn changes_outside_a_reply_are_steps_of_their_own() {
        let mut app = OxydeApp::blank(crate::prefs::Preferences::default(), None);
        let sketch = json!({ "plane": "front",
            "items": [{ "type": "circle", "center": [0, 0], "diameter": 10 }] });
        call(&mut app, 1, "sketch", sketch.clone());
        call(&mut app, 2, "sketch", sketch);
        assert_eq!(app.doc.undo_depth(), 2);
        let answer = call(
            &mut app,
            3,
            "save_part",
            json!({ "path": "C:/nowhere/x.oxyprt" }),
        );
        assert_eq!(answer["result"]["isError"], true, "{answer}");
    }
}
