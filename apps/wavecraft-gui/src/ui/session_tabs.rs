//! The session tab strip under the waveforms, and the actions it triggers.

use crate::{
    egui, CaptureTarget, SelectableHoverText, SessionNameEditor, SessionTabAction, WaveformTab,
    MAX_SESSION_TABS, PANEL, SESSION_NAME_MAX_CHARS,
};

use crate::WaveCraftApp;

impl WaveCraftApp {
    /// Paints the session tabs for the active logic or scope view and applies
    /// the select, add, close, and rename actions the user triggers.
    pub(crate) fn paint_session_tabs(&mut self, context: &egui::Context) {
        let session_tab_state = match self.active_waveform_tab {
            Some(WaveformTab::LogicAnalyzer) => Some((
                CaptureTarget::LogicAnalyzer,
                self.logic_sessions.names(),
                self.logic_sessions.active_index(),
            )),
            Some(WaveformTab::Oscilloscope | WaveformTab::Lissajous) => Some((
                CaptureTarget::Oscilloscope,
                self.scope_sessions.names(),
                self.scope_sessions.active_index(),
            )),
            Some(WaveformTab::RadioReceiver | WaveformTab::Combined) | None => None,
        };
        let mut session_tab_action = None;
        let mut session_name_commit = false;
        let mut session_name_cancel = false;
        if let Some((target, tab_names, active_tab)) = session_tab_state {
            let tab_count = tab_names.len();
            let sessions_enabled = self.pending.is_none() && !self.capture_target_is_active(target);
            egui::TopBottomPanel::bottom("session_tabs")
                .frame(
                    egui::Frame::none()
                        .fill(PANEL)
                        .inner_margin(egui::Margin::symmetric(14.0, 6.0)),
                )
                .show(context, |ui| {
                    egui::ScrollArea::horizontal()
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                for (index, name) in tab_names.iter().enumerate() {
                                    ui.horizontal(|ui| {
                                        let editing = self
                                            .session_name_editor
                                            .as_ref()
                                            .is_some_and(|editor| {
                                                editor.target == target && editor.index == index
                                            });
                                        if editing {
                                            if let Some(editor) = self.session_name_editor.as_mut() {
                                                let response = ui.add_enabled(
                                                    sessions_enabled,
                                                    egui::TextEdit::singleline(&mut editor.draft)
                                                        .char_limit(SESSION_NAME_MAX_CHARS)
                                                        .desired_width(140.0),
                                                );
                                                if editor.request_focus {
                                                    response.request_focus();
                                                    editor.request_focus = false;
                                                }
                                                let escape_pressed = ui.input(|input| {
                                                    input.key_pressed(egui::Key::Escape)
                                                });
                                                let enter_pressed = ui.input(|input| {
                                                    input.key_pressed(egui::Key::Enter)
                                                });
                                                if escape_pressed {
                                                    session_name_cancel = true;
                                                } else if enter_pressed || response.lost_focus() {
                                                    session_name_commit = true;
                                                }
                                            }
                                        } else {
                                            let response = ui
                                                .add_enabled(
                                                    sessions_enabled,
                                                    egui::SelectableLabel::new(
                                                        active_tab == index,
                                                        name,
                                                    ),
                                                )
                                                .on_hover_selectable_text(if sessions_enabled {
                                                    "Show this session · double-click to rename"
                                                } else {
                                                    "Stop the active acquisition before changing sessions"
                                                });
                                            if response.double_clicked() {
                                                session_tab_action = Some((
                                                    target,
                                                    SessionTabAction::BeginRename(index),
                                                ));
                                            } else if response.clicked() {
                                                session_tab_action = Some((
                                                    target,
                                                    SessionTabAction::Select(index),
                                                ));
                                            }
                                        }
                                        let close_enabled = sessions_enabled && tab_count > 1;
                                        if ui
                                            .add_enabled(
                                                close_enabled,
                                                egui::Button::new("×").small().frame(false),
                                            )
                                            .on_hover_selectable_text(if !sessions_enabled {
                                                "Stop the active acquisition before closing a session"
                                            } else if tab_count <= 1 {
                                                "At least one session must remain open"
                                            } else {
                                                "Close and permanently discard this session"
                                            })
                                            .clicked()
                                        {
                                            session_tab_action =
                                                Some((target, SessionTabAction::Close(index)));
                                        }
                                    });
                                }
                                let add_enabled = sessions_enabled && tab_count < MAX_SESSION_TABS;
                                if ui
                                    .add_enabled(add_enabled, egui::Button::new("+").small())
                                    .on_hover_selectable_text(if tab_count >= MAX_SESSION_TABS {
                                        "The 10-session limit has been reached"
                                    } else if !sessions_enabled {
                                        "Stop the active acquisition before adding a session"
                                    } else {
                                        "Add a new empty session"
                                    })
                                    .clicked()
                                {
                                    session_tab_action = Some((target, SessionTabAction::Add));
                                }
                            });
                        });
                });
        }
        if session_name_cancel {
            self.session_name_editor = None;
        } else if session_name_commit {
            if let Some(editor) = self.session_name_editor.take() {
                let renamed = match editor.target {
                    CaptureTarget::LogicAnalyzer => {
                        self.logic_sessions.rename(editor.index, &editor.draft)
                    }
                    CaptureTarget::Oscilloscope => {
                        self.scope_sessions.rename(editor.index, &editor.draft)
                    }
                    CaptureTarget::RadioReceiver | CaptureTarget::Combined => false,
                };
                if renamed {
                    self.status = "Session renamed".to_owned();
                    self.last_error = None;
                }
            }
        }
        if let Some((target, action)) = session_tab_action {
            let action_session_name = match action {
                SessionTabAction::Close(index) => match target {
                    CaptureTarget::LogicAnalyzer => self.logic_sessions.name(index),
                    CaptureTarget::Oscilloscope => self.scope_sessions.name(index),
                    CaptureTarget::RadioReceiver | CaptureTarget::Combined => None,
                }
                .map(str::to_owned),
                _ => None,
            };
            if !matches!(action, SessionTabAction::BeginRename(_)) {
                self.session_name_editor = None;
            }
            let changed = match target {
                CaptureTarget::LogicAnalyzer => {
                    self.save_active_logic_session_settings();
                    match action {
                        SessionTabAction::Select(index) => {
                            if !self.logic_sessions.select(index) {
                                false
                            } else {
                                let settings = self.logic_sessions.active_settings().clone();
                                self.apply_logic_session_settings(settings);
                                true
                            }
                        }
                        SessionTabAction::Add => {
                            let settings = self.current_logic_session_settings();
                            self.logic_sessions.add(settings)
                        }
                        SessionTabAction::Close(index) => {
                            if !self.logic_sessions.close(index) {
                                false
                            } else {
                                let settings = self.logic_sessions.active_settings().clone();
                                self.apply_logic_session_settings(settings);
                                true
                            }
                        }
                        SessionTabAction::BeginRename(index) => {
                            if let Some(name) = self.logic_sessions.name(index).map(str::to_owned) {
                                self.session_name_editor = Some(SessionNameEditor {
                                    target,
                                    index,
                                    draft: name,
                                    request_focus: true,
                                });
                                true
                            } else {
                                false
                            }
                        }
                    }
                }
                CaptureTarget::Oscilloscope => {
                    self.save_active_scope_session_settings();
                    match action {
                        SessionTabAction::Select(index) => {
                            if !self.scope_sessions.select(index) {
                                false
                            } else {
                                let settings = self.scope_sessions.active_settings().clone();
                                self.apply_scope_session_settings(settings);
                                true
                            }
                        }
                        SessionTabAction::Add => {
                            let settings = self.current_scope_session_settings();
                            self.scope_sessions.add(settings)
                        }
                        SessionTabAction::Close(index) => {
                            if !self.scope_sessions.close(index) {
                                false
                            } else {
                                let settings = self.scope_sessions.active_settings().clone();
                                self.apply_scope_session_settings(settings);
                                true
                            }
                        }
                        SessionTabAction::BeginRename(index) => {
                            if let Some(name) = self.scope_sessions.name(index).map(str::to_owned) {
                                self.session_name_editor = Some(SessionNameEditor {
                                    target,
                                    index,
                                    draft: name,
                                    request_focus: true,
                                });
                                true
                            } else {
                                false
                            }
                        }
                    }
                }
                // Radio and combined views have no session tabs.
                CaptureTarget::RadioReceiver | CaptureTarget::Combined => false,
            };
            if changed {
                match action {
                    SessionTabAction::Add => {
                        let active_name = match target {
                            CaptureTarget::LogicAnalyzer => {
                                self.logic_sessions.name(self.logic_sessions.active_index())
                            }
                            CaptureTarget::Oscilloscope => {
                                self.scope_sessions.name(self.scope_sessions.active_index())
                            }
                            CaptureTarget::RadioReceiver | CaptureTarget::Combined => None,
                        }
                        .unwrap_or("session");
                        self.status = format!("Added {} {active_name}", target.label());
                        self.last_error = None;
                    }
                    SessionTabAction::Close(_) => {
                        self.status = format!(
                            "Closed {} {}; its data was discarded",
                            target.label(),
                            action_session_name.as_deref().unwrap_or("session")
                        );
                        self.last_error = None;
                    }
                    SessionTabAction::Select(_) | SessionTabAction::BeginRename(_) => {}
                }
            }
        }
    }
}
