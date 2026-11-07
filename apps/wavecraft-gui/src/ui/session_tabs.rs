//! The session tab strip under the waveforms, and the actions it triggers.

use crate::{
    egui, CaptureTarget, SelectableHoverText, SessionNameEditor, SessionTabAction, WaveformTab,
    MAX_SESSION_TABS, PANEL, SESSION_NAME_MAX_CHARS,
};

use crate::WaveCraftApp;

/// What the user did to the tab strip in one frame.
#[derive(Default)]
struct SessionTabOutcome {
    action: Option<(CaptureTarget, SessionTabAction)>,
    name_edit: SessionNameEdit,
}

/// How the open session name field ended this frame.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum SessionNameEdit {
    #[default]
    Keep,
    Commit,
    Cancel,
}

/// The sessions of the instrument whose view is shown.
struct SessionTabStrip {
    target: CaptureTarget,
    names: Vec<String>,
    active: usize,
}

/// Whether the close button works and what its tooltip says.
fn session_close_button_state(sessions_enabled: bool, tab_count: usize) -> (bool, &'static str) {
    (
        sessions_enabled && tab_count > 1,
        if !sessions_enabled {
            "Stop the active acquisition before closing a session"
        } else if tab_count <= 1 {
            "At least one session must remain open"
        } else {
            "Close and permanently discard this session"
        },
    )
}

/// Whether the add button works and what its tooltip says.
fn session_add_button_state(sessions_enabled: bool, tab_count: usize) -> (bool, &'static str) {
    (
        sessions_enabled && tab_count < MAX_SESSION_TABS,
        if tab_count >= MAX_SESSION_TABS {
            "The 10-session limit has been reached"
        } else if !sessions_enabled {
            "Stop the active acquisition before adding a session"
        } else {
            "Add a new empty session"
        },
    )
}

/// The text field that renames a session in place.
fn paint_session_name_field(
    ui: &mut egui::Ui,
    editor: &mut SessionNameEditor,
    sessions_enabled: bool,
) -> SessionNameEdit {
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
    let escape_pressed = ui.input(|input| input.key_pressed(egui::Key::Escape));
    let enter_pressed = ui.input(|input| input.key_pressed(egui::Key::Enter));
    if escape_pressed {
        SessionNameEdit::Cancel
    } else if enter_pressed || response.lost_focus() {
        SessionNameEdit::Commit
    } else {
        SessionNameEdit::Keep
    }
}

/// A session's tab: a click selects it, a double click starts renaming it.
fn paint_session_tab_label(
    ui: &mut egui::Ui,
    name: &str,
    index: usize,
    selected: bool,
    sessions_enabled: bool,
) -> Option<SessionTabAction> {
    let response = ui
        .add_enabled(sessions_enabled, egui::SelectableLabel::new(selected, name))
        .on_hover_selectable_text(if sessions_enabled {
            "Show this session · double-click to rename"
        } else {
            "Stop the active acquisition before changing sessions"
        });
    if response.double_clicked() {
        Some(SessionTabAction::BeginRename(index))
    } else if response.clicked() {
        Some(SessionTabAction::Select(index))
    } else {
        None
    }
}

fn paint_session_close_button(
    ui: &mut egui::Ui,
    index: usize,
    sessions_enabled: bool,
    tab_count: usize,
) -> Option<SessionTabAction> {
    let (close_enabled, hover) = session_close_button_state(sessions_enabled, tab_count);
    ui.add_enabled(close_enabled, egui::Button::new("×").small().frame(false))
        .on_hover_selectable_text(hover)
        .clicked()
        .then_some(SessionTabAction::Close(index))
}

fn paint_session_add_button(
    ui: &mut egui::Ui,
    sessions_enabled: bool,
    tab_count: usize,
) -> Option<SessionTabAction> {
    let (add_enabled, hover) = session_add_button_state(sessions_enabled, tab_count);
    ui.add_enabled(add_enabled, egui::Button::new("+").small())
        .on_hover_selectable_text(hover)
        .clicked()
        .then_some(SessionTabAction::Add)
}

impl WaveCraftApp {
    /// Paints the session tabs for the active logic or scope view and applies
    /// the select, add, close, and rename actions the user triggers.
    pub(crate) fn paint_session_tabs(&mut self, context: &egui::Context) {
        let outcome = match self.session_tab_strip() {
            Some(strip) => self.paint_session_tab_strip(context, strip),
            None => SessionTabOutcome::default(),
        };
        self.apply_session_name_edit(outcome.name_edit);
        if let Some((target, action)) = outcome.action {
            self.apply_session_tab_action(target, action);
        }
    }

    /// The sessions to show tabs for, if the active view has any.
    fn session_tab_strip(&self) -> Option<SessionTabStrip> {
        match self.active_waveform_tab {
            Some(WaveformTab::LogicAnalyzer) => Some(SessionTabStrip {
                target: CaptureTarget::LogicAnalyzer,
                names: self.logic_sessions.names(),
                active: self.logic_sessions.active_index(),
            }),
            Some(WaveformTab::Oscilloscope | WaveformTab::Lissajous) => Some(SessionTabStrip {
                target: CaptureTarget::Oscilloscope,
                names: self.scope_sessions.names(),
                active: self.scope_sessions.active_index(),
            }),
            Some(WaveformTab::RadioReceiver | WaveformTab::Combined) | None => None,
        }
    }

    fn paint_session_tab_strip(
        &mut self,
        context: &egui::Context,
        strip: SessionTabStrip,
    ) -> SessionTabOutcome {
        let SessionTabStrip {
            target,
            names,
            active,
        } = strip;
        let tab_count = names.len();
        let sessions_enabled = self.pending.is_none() && !self.capture_target_is_active(target);
        let mut outcome = SessionTabOutcome::default();
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
                            for (index, name) in names.iter().enumerate() {
                                ui.horizontal(|ui| {
                                    let editing =
                                        self.session_name_editor.as_ref().is_some_and(|editor| {
                                            editor.target == target && editor.index == index
                                        });
                                    if editing {
                                        if let Some(editor) = self.session_name_editor.as_mut() {
                                            match paint_session_name_field(
                                                ui,
                                                editor,
                                                sessions_enabled,
                                            ) {
                                                SessionNameEdit::Keep => {}
                                                edit => outcome.name_edit = edit,
                                            }
                                        }
                                    } else if let Some(action) = paint_session_tab_label(
                                        ui,
                                        name,
                                        index,
                                        active == index,
                                        sessions_enabled,
                                    ) {
                                        outcome.action = Some((target, action));
                                    }
                                    if let Some(action) = paint_session_close_button(
                                        ui,
                                        index,
                                        sessions_enabled,
                                        tab_count,
                                    ) {
                                        outcome.action = Some((target, action));
                                    }
                                });
                            }
                            if let Some(action) =
                                paint_session_add_button(ui, sessions_enabled, tab_count)
                            {
                                outcome.action = Some((target, action));
                            }
                        });
                    });
            });
        outcome
    }

    fn apply_session_name_edit(&mut self, edit: SessionNameEdit) {
        match edit {
            SessionNameEdit::Keep => {}
            SessionNameEdit::Cancel => self.session_name_editor = None,
            SessionNameEdit::Commit => {
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
        }
    }

    /// The name of one session of `target`; the radio and combined views have
    /// no session tabs.
    fn session_name(&self, target: CaptureTarget, index: usize) -> Option<&str> {
        match target {
            CaptureTarget::LogicAnalyzer => self.logic_sessions.name(index),
            CaptureTarget::Oscilloscope => self.scope_sessions.name(index),
            CaptureTarget::RadioReceiver | CaptureTarget::Combined => None,
        }
    }

    fn apply_session_tab_action(&mut self, target: CaptureTarget, action: SessionTabAction) {
        let closed_session_name = match action {
            SessionTabAction::Close(index) => self.session_name(target, index).map(str::to_owned),
            _ => None,
        };
        if !matches!(action, SessionTabAction::BeginRename(_)) {
            self.session_name_editor = None;
        }
        if self.change_sessions(target, action) {
            self.announce_session_change(target, action, closed_session_name.as_deref());
        }
    }

    /// Carries out `action` on the sessions of `target`; false when nothing
    /// changed, for example when the last session is closed.
    fn change_sessions(&mut self, target: CaptureTarget, action: SessionTabAction) -> bool {
        match target {
            CaptureTarget::LogicAnalyzer => self.save_active_logic_session_settings(),
            CaptureTarget::Oscilloscope => self.save_active_scope_session_settings(),
            // Radio and combined views have no session tabs.
            CaptureTarget::RadioReceiver | CaptureTarget::Combined => return false,
        }
        match action {
            SessionTabAction::Select(index) => {
                let selected = match target {
                    CaptureTarget::LogicAnalyzer => self.logic_sessions.select(index),
                    CaptureTarget::Oscilloscope => self.scope_sessions.select(index),
                    CaptureTarget::RadioReceiver | CaptureTarget::Combined => false,
                };
                if selected {
                    self.apply_active_session_settings(target);
                }
                selected
            }
            SessionTabAction::Add => match target {
                CaptureTarget::LogicAnalyzer => {
                    let settings = self.current_logic_session_settings();
                    self.logic_sessions.add(settings)
                }
                CaptureTarget::Oscilloscope => {
                    let settings = self.current_scope_session_settings();
                    self.scope_sessions.add(settings)
                }
                CaptureTarget::RadioReceiver | CaptureTarget::Combined => false,
            },
            SessionTabAction::Close(index) => {
                let closed = match target {
                    CaptureTarget::LogicAnalyzer => self.logic_sessions.close(index),
                    CaptureTarget::Oscilloscope => self.scope_sessions.close(index),
                    CaptureTarget::RadioReceiver | CaptureTarget::Combined => false,
                };
                if closed {
                    self.apply_active_session_settings(target);
                }
                closed
            }
            SessionTabAction::BeginRename(index) => {
                if let Some(name) = self.session_name(target, index).map(str::to_owned) {
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

    /// Loads the settings of the session that is now active into the view.
    fn apply_active_session_settings(&mut self, target: CaptureTarget) {
        match target {
            CaptureTarget::LogicAnalyzer => {
                let settings = self.logic_sessions.active_settings().clone();
                self.apply_logic_session_settings(settings);
            }
            CaptureTarget::Oscilloscope => {
                let settings = self.scope_sessions.active_settings().clone();
                self.apply_scope_session_settings(settings);
            }
            CaptureTarget::RadioReceiver | CaptureTarget::Combined => {}
        }
    }

    /// Reports a session that was added or closed in the status line.
    fn announce_session_change(
        &mut self,
        target: CaptureTarget,
        action: SessionTabAction,
        closed_session_name: Option<&str>,
    ) {
        match action {
            SessionTabAction::Add => {
                let active_index = match target {
                    CaptureTarget::LogicAnalyzer => self.logic_sessions.active_index(),
                    CaptureTarget::Oscilloscope => self.scope_sessions.active_index(),
                    CaptureTarget::RadioReceiver | CaptureTarget::Combined => 0,
                };
                let active_name = self.session_name(target, active_index).unwrap_or("session");
                self.status = format!("Added {} {active_name}", target.label());
                self.last_error = None;
            }
            SessionTabAction::Close(_) => {
                self.status = format!(
                    "Closed {} {}; its data was discarded",
                    target.label(),
                    closed_session_name.unwrap_or("session")
                );
                self.last_error = None;
            }
            SessionTabAction::Select(_) | SessionTabAction::BeginRename(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_support::{app, ready, request};
    use crate::*;
    use wavecraft_core::InstrumentKind;

    const SESSION_TARGETS: [CaptureTarget; 2] =
        [CaptureTarget::LogicAnalyzer, CaptureTarget::Oscilloscope];

    fn session_names(app: &WaveCraftApp, target: CaptureTarget) -> Vec<String> {
        match target {
            CaptureTarget::LogicAnalyzer => app.logic_sessions.names(),
            CaptureTarget::Oscilloscope => app.scope_sessions.names(),
            _ => unreachable!("only logic and scope have sessions"),
        }
    }

    fn active_session(app: &WaveCraftApp, target: CaptureTarget) -> usize {
        match target {
            CaptureTarget::LogicAnalyzer => app.logic_sessions.active_index(),
            CaptureTarget::Oscilloscope => app.scope_sessions.active_index(),
            _ => unreachable!("only logic and scope have sessions"),
        }
    }

    /// The channel name of the instrument, which each session keeps its own
    /// copy of.
    fn channel_name(app: &mut WaveCraftApp, target: CaptureTarget) -> &mut String {
        match target {
            CaptureTarget::LogicAnalyzer => &mut app.logic_channel_names[0],
            CaptureTarget::Oscilloscope => &mut app.scope_channel_names[0],
            _ => unreachable!("only logic and scope have sessions"),
        }
    }

    #[test]
    fn the_close_button_needs_a_stopped_instrument_and_a_second_session() {
        assert_eq!(
            session_close_button_state(true, 3),
            (true, "Close and permanently discard this session")
        );
        assert_eq!(
            session_close_button_state(true, 1),
            (false, "At least one session must remain open")
        );
        for tab_count in [1, 3] {
            assert_eq!(
                session_close_button_state(false, tab_count),
                (
                    false,
                    "Stop the active acquisition before closing a session"
                )
            );
        }
    }

    #[test]
    fn the_add_button_needs_a_stopped_instrument_and_room_for_a_session() {
        assert_eq!(
            session_add_button_state(true, MAX_SESSION_TABS - 1),
            (true, "Add a new empty session")
        );
        assert_eq!(
            session_add_button_state(true, MAX_SESSION_TABS),
            (false, "The 10-session limit has been reached")
        );
        assert_eq!(
            session_add_button_state(false, 2),
            (false, "Stop the active acquisition before adding a session")
        );
        assert_eq!(
            session_add_button_state(false, MAX_SESSION_TABS),
            (false, "The 10-session limit has been reached"),
            "a full strip says so even while capturing"
        );
    }

    #[test]
    fn adding_a_session_activates_it_and_says_so() {
        for target in SESSION_TARGETS {
            let mut app = app();
            app.last_error = Some("an old error".to_owned());

            app.apply_session_tab_action(target, SessionTabAction::Add);

            let names = session_names(&app, target);
            assert_eq!(names.len(), 2, "{target:?}");
            assert_eq!(active_session(&app, target), 1, "{target:?}");
            assert_eq!(app.status, format!("Added {} {}", target.label(), names[1]));
            assert_eq!(app.last_error, None);
        }
    }

    #[test]
    fn switching_sessions_keeps_each_ones_own_settings() {
        for target in SESSION_TARGETS {
            let mut app = app();
            *channel_name(&mut app, target) = "first".to_owned();
            app.apply_session_tab_action(target, SessionTabAction::Add);
            *channel_name(&mut app, target) = "second".to_owned();

            app.apply_session_tab_action(target, SessionTabAction::Select(0));
            assert_eq!(active_session(&app, target), 0, "{target:?}");
            assert_eq!(channel_name(&mut app, target), "first", "{target:?}");

            app.apply_session_tab_action(target, SessionTabAction::Select(1));
            assert_eq!(active_session(&app, target), 1, "{target:?}");
            assert_eq!(channel_name(&mut app, target), "second", "{target:?}");
        }
    }

    #[test]
    fn selecting_a_session_that_does_not_exist_changes_nothing() {
        for target in SESSION_TARGETS {
            let mut app = app();
            *channel_name(&mut app, target) = "kept".to_owned();

            app.apply_session_tab_action(target, SessionTabAction::Select(7));

            assert_eq!(active_session(&app, target), 0);
            assert_eq!(channel_name(&mut app, target), "kept");
        }
    }

    #[test]
    fn closing_a_session_discards_it_loads_the_one_left_and_says_so() {
        for target in SESSION_TARGETS {
            let mut app = app();
            *channel_name(&mut app, target) = "first".to_owned();
            app.apply_session_tab_action(target, SessionTabAction::Add);
            *channel_name(&mut app, target) = "second".to_owned();
            let closed = session_names(&app, target)[1].clone();
            app.last_error = Some("an old error".to_owned());

            app.apply_session_tab_action(target, SessionTabAction::Close(1));

            assert_eq!(session_names(&app, target).len(), 1, "{target:?}");
            assert_eq!(channel_name(&mut app, target), "first", "{target:?}");
            assert_eq!(
                app.status,
                format!("Closed {} {closed}; its data was discarded", target.label())
            );
            assert_eq!(app.last_error, None);
        }
    }

    #[test]
    fn the_last_session_cannot_be_closed() {
        for target in SESSION_TARGETS {
            let mut app = app();
            app.status = "unchanged".to_owned();

            app.apply_session_tab_action(target, SessionTabAction::Close(0));

            assert_eq!(session_names(&app, target).len(), 1);
            assert_eq!(app.status, "unchanged");
        }
    }

    #[test]
    fn the_radio_and_combined_views_have_no_sessions_to_change() {
        for target in [CaptureTarget::RadioReceiver, CaptureTarget::Combined] {
            let mut app = app();
            app.status = "unchanged".to_owned();
            for action in [
                SessionTabAction::Add,
                SessionTabAction::Select(0),
                SessionTabAction::Close(0),
                SessionTabAction::BeginRename(0),
            ] {
                app.apply_session_tab_action(target, action);
            }

            assert_eq!(app.status, "unchanged");
            assert!(app.session_name_editor.is_none());
            assert_eq!(app.logic_sessions.names().len(), 1);
            assert_eq!(app.scope_sessions.names().len(), 1);
        }
    }

    #[test]
    fn renaming_starts_from_the_current_name_and_asks_for_focus() {
        for target in SESSION_TARGETS {
            let mut app = app();
            app.apply_session_tab_action(target, SessionTabAction::Add);
            let name = session_names(&app, target)[1].clone();

            app.apply_session_tab_action(target, SessionTabAction::BeginRename(1));

            let editor = app.session_name_editor.as_ref().unwrap();
            assert_eq!(editor.target, target);
            assert_eq!(editor.index, 1);
            assert_eq!(editor.draft, name);
            assert!(editor.request_focus);
        }
    }

    #[test]
    fn a_rename_of_a_missing_session_leaves_an_open_editor_alone() {
        let mut app = app();
        app.apply_session_tab_action(CaptureTarget::LogicAnalyzer, SessionTabAction::Add);
        app.apply_session_tab_action(
            CaptureTarget::LogicAnalyzer,
            SessionTabAction::BeginRename(1),
        );

        app.apply_session_tab_action(
            CaptureTarget::LogicAnalyzer,
            SessionTabAction::BeginRename(9),
        );

        assert_eq!(app.session_name_editor.as_ref().unwrap().index, 1);
    }

    #[test]
    fn any_other_tab_action_closes_the_rename_editor() {
        for action in [
            SessionTabAction::Select(0),
            SessionTabAction::Add,
            SessionTabAction::Close(0),
        ] {
            let mut app = app();
            app.apply_session_tab_action(CaptureTarget::LogicAnalyzer, SessionTabAction::Add);
            app.apply_session_tab_action(
                CaptureTarget::LogicAnalyzer,
                SessionTabAction::BeginRename(1),
            );

            app.apply_session_tab_action(CaptureTarget::LogicAnalyzer, action);

            assert!(app.session_name_editor.is_none(), "{action:?}");
        }
    }

    fn app_renaming(target: CaptureTarget, draft: &str) -> WaveCraftApp {
        let mut app = app();
        app.apply_session_tab_action(target, SessionTabAction::Add);
        app.apply_session_tab_action(target, SessionTabAction::BeginRename(1));
        app.session_name_editor.as_mut().unwrap().draft = draft.to_owned();
        app.status = "before".to_owned();
        app.last_error = Some("an old error".to_owned());
        app
    }

    #[test]
    fn committing_a_name_renames_the_session_of_the_edited_instrument() {
        for target in SESSION_TARGETS {
            let mut app = app_renaming(target, "  Clock  ");

            app.apply_session_name_edit(SessionNameEdit::Commit);

            assert_eq!(session_names(&app, target)[1], "Clock");
            let other = if target == CaptureTarget::LogicAnalyzer {
                CaptureTarget::Oscilloscope
            } else {
                CaptureTarget::LogicAnalyzer
            };
            assert_eq!(session_names(&app, other).len(), 1);
            assert!(app.session_name_editor.is_none());
            assert_eq!(app.status, "Session renamed");
            assert_eq!(app.last_error, None);
        }
    }

    #[test]
    fn an_empty_name_is_not_a_rename() {
        let mut app = app_renaming(CaptureTarget::LogicAnalyzer, "   ");
        let before = session_names(&app, CaptureTarget::LogicAnalyzer);

        app.apply_session_name_edit(SessionNameEdit::Commit);

        assert_eq!(session_names(&app, CaptureTarget::LogicAnalyzer), before);
        assert!(app.session_name_editor.is_none());
        assert_eq!(app.status, "before");
        assert!(app.last_error.is_some());
    }

    #[test]
    fn cancelling_a_rename_drops_the_draft() {
        let mut app = app_renaming(CaptureTarget::LogicAnalyzer, "Clock");
        let before = session_names(&app, CaptureTarget::LogicAnalyzer);

        app.apply_session_name_edit(SessionNameEdit::Cancel);

        assert!(app.session_name_editor.is_none());
        assert_eq!(session_names(&app, CaptureTarget::LogicAnalyzer), before);
        assert_eq!(app.status, "before");
    }

    #[test]
    fn leaving_the_name_field_alone_keeps_the_editor() {
        let mut app = app_renaming(CaptureTarget::LogicAnalyzer, "Clock");

        app.apply_session_name_edit(SessionNameEdit::Keep);

        assert_eq!(app.session_name_editor.as_ref().unwrap().draft, "Clock");
    }

    /// The tab strip painted frame after frame, with the app behind it.
    struct TabsView {
        context: egui::Context,
        app: WaveCraftApp,
        frames: u32,
    }

    impl TabsView {
        fn new(target: CaptureTarget, extra_sessions: usize) -> Self {
            let mut app = app();
            app.active_waveform_tab = Some(match target {
                CaptureTarget::LogicAnalyzer => WaveformTab::LogicAnalyzer,
                CaptureTarget::Oscilloscope => WaveformTab::Oscilloscope,
                CaptureTarget::RadioReceiver => WaveformTab::RadioReceiver,
                CaptureTarget::Combined => WaveformTab::Combined,
            });
            for _ in 0..extra_sessions {
                app.apply_session_tab_action(CaptureTarget::LogicAnalyzer, SessionTabAction::Add);
                app.apply_session_tab_action(CaptureTarget::Oscilloscope, SessionTabAction::Add);
            }
            app.apply_session_tab_action(CaptureTarget::LogicAnalyzer, SessionTabAction::Select(0));
            app.apply_session_tab_action(CaptureTarget::Oscilloscope, SessionTabAction::Select(0));
            app.status = "start".to_owned();
            let mut view = Self {
                context: egui::Context::default(),
                app,
                frames: 0,
            };
            view.frame(Vec::new());
            view
        }

        fn frame(&mut self, events: Vec<egui::Event>) -> Vec<(String, egui::Pos2)> {
            self.frames += 1;
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1000.0, 400.0),
                )),
                time: Some(f64::from(self.frames) / 60.0),
                events,
                ..egui::RawInput::default()
            };
            let output = self
                .context
                .run(input, |context| self.app.paint_session_tabs(context));
            let mut texts = Vec::new();
            fn collect(shape: &egui::Shape, texts: &mut Vec<(String, egui::Pos2)>) {
                match shape {
                    egui::Shape::Vec(shapes) => {
                        shapes.iter().for_each(|shape| collect(shape, texts))
                    }
                    egui::Shape::Text(text) => texts.push((
                        text.galley.text().to_owned(),
                        text.visual_bounding_rect().center(),
                    )),
                    _ => {}
                }
            }
            for clipped in &output.shapes {
                collect(&clipped.shape, &mut texts);
            }
            texts
        }

        fn texts(&mut self) -> Vec<String> {
            self.frame(Vec::new())
                .into_iter()
                .map(|(text, _)| text)
                .collect()
        }

        /// Everything painted this frame, to tell two states apart.
        fn painted(&mut self) -> String {
            self.frames += 1;
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1000.0, 400.0),
                )),
                time: Some(f64::from(self.frames) / 60.0),
                ..egui::RawInput::default()
            };
            let output = self
                .context
                .run(input, |context| self.app.paint_session_tabs(context));
            format!("{:?}", output.shapes)
        }

        /// Rests the pointer on `position` until its tooltip is shown.
        fn hover(&mut self, position: egui::Pos2) -> Vec<String> {
            self.frame(vec![egui::Event::PointerMoved(position)]);
            for _ in 0..60 {
                self.frame(Vec::new());
            }
            self.texts()
        }

        /// Where the `occurrence`th text equal to `text` is painted, left to right.
        fn position_of(&mut self, text: &str, occurrence: usize) -> egui::Pos2 {
            let mut matches = self
                .frame(Vec::new())
                .into_iter()
                .filter(|(painted, _)| painted == text)
                .map(|(_, position)| position)
                .collect::<Vec<_>>();
            matches.sort_by(|a, b| a.x.total_cmp(&b.x));
            *matches
                .get(occurrence)
                .unwrap_or_else(|| panic!("no {occurrence}th {text:?}"))
        }

        fn press(position: egui::Pos2, pressed: bool) -> egui::Event {
            egui::Event::PointerButton {
                pos: position,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::default(),
            }
        }

        fn click(&mut self, position: egui::Pos2) {
            self.frame(vec![
                egui::Event::PointerMoved(position),
                Self::press(position, true),
            ]);
            self.frame(vec![Self::press(position, false)]);
            self.frame(Vec::new());
        }

        fn double_click(&mut self, position: egui::Pos2) {
            self.frame(vec![
                egui::Event::PointerMoved(position),
                Self::press(position, true),
            ]);
            self.frame(vec![Self::press(position, false)]);
            self.frame(vec![Self::press(position, true)]);
            self.frame(vec![Self::press(position, false)]);
            self.frame(Vec::new());
        }

        fn type_text(&mut self, text: &str) {
            self.frame(vec![egui::Event::Text(text.to_owned())]);
        }

        fn key(&mut self, key: egui::Key) {
            self.frame(vec![egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::default(),
            }]);
            self.frame(Vec::new());
        }
    }

    #[test]
    fn the_strip_shows_the_sessions_of_the_active_view_only() {
        let mut logic = TabsView::new(CaptureTarget::LogicAnalyzer, 1);
        let texts = logic.texts();
        assert!(texts.iter().any(|text| text == "Session 1"));
        assert!(texts.iter().any(|text| text == "Session 2"));
        assert!(texts.iter().any(|text| text == "+"));
        assert_eq!(texts.iter().filter(|text| *text == "×").count(), 2);

        for target in [CaptureTarget::RadioReceiver, CaptureTarget::Combined] {
            let mut view = TabsView::new(target, 1);
            assert!(view.texts().is_empty(), "{target:?}");
        }
        let mut scope = TabsView::new(CaptureTarget::Oscilloscope, 0);
        scope.app.active_waveform_tab = Some(WaveformTab::Lissajous);
        assert!(scope.texts().iter().any(|text| text == "Session 1"));
    }

    #[test]
    fn clicking_tab_buttons_selects_adds_and_closes_sessions() {
        for target in SESSION_TARGETS {
            let mut view = TabsView::new(target, 1);

            let second = view.position_of("Session 2", 0);
            view.click(second);
            assert_eq!(active_session(&view.app, target), 1, "{target:?}");

            let add = view.position_of("+", 0);
            view.click(add);
            assert_eq!(session_names(&view.app, target).len(), 3, "{target:?}");

            let close_first = view.position_of("×", 0);
            view.click(close_first);
            assert_eq!(session_names(&view.app, target).len(), 2, "{target:?}");
            assert_eq!(
                view.app.status,
                format!(
                    "Closed {} Session 1; its data was discarded",
                    target.label()
                )
            );
        }
    }

    #[test]
    fn a_running_instrument_blocks_changes_to_its_own_sessions_only() {
        let mut view = TabsView::new(CaptureTarget::LogicAnalyzer, 1);
        request(&mut view.app, CaptureTarget::LogicAnalyzer);
        let started = ready(
            &view.app,
            InstrumentKind::LogicAnalyzer,
            AcquisitionMode::Run,
            CaptureTarget::LogicAnalyzer,
        );
        view.app.handle_capture_ready(started);

        let add = view.position_of("+", 0);
        view.click(add);
        let second = view.position_of("Session 2", 0);
        view.click(second);
        let close = view.position_of("×", 0);
        view.click(close);

        assert_eq!(view.app.logic_sessions.names().len(), 2);
        assert_eq!(view.app.logic_sessions.active_index(), 0);

        view.app.active_waveform_tab = Some(WaveformTab::Oscilloscope);
        let add = view.position_of("+", 0);
        view.click(add);
        assert_eq!(view.app.scope_sessions.names().len(), 3);
    }

    #[test]
    fn work_in_progress_blocks_changes_to_the_sessions() {
        let mut view = TabsView::new(CaptureTarget::LogicAnalyzer, 1);
        let (_sender, receiver) = std::sync::mpsc::channel();
        view.app.pending = Some(receiver);

        let add = view.position_of("+", 0);
        view.click(add);
        let second = view.position_of("Session 2", 0);
        view.click(second);

        assert_eq!(view.app.logic_sessions.names().len(), 2);
        assert_eq!(view.app.logic_sessions.active_index(), 0);
    }

    #[test]
    fn a_double_click_renames_a_session_in_place() {
        let mut view = TabsView::new(CaptureTarget::LogicAnalyzer, 1);

        let second = view.position_of("Session 2", 0);
        view.double_click(second);
        assert!(view.app.session_name_editor.is_some());
        view.frame(Vec::new());
        // The field starts focused with the old name selected or at its end.
        view.type_text("!");
        view.key(egui::Key::Enter);

        assert!(view.app.session_name_editor.is_none());
        assert!(view.app.logic_sessions.names()[1].contains('!'));
        assert_eq!(view.app.status, "Session renamed");
    }

    #[test]
    fn clicking_elsewhere_commits_a_rename() {
        let mut view = TabsView::new(CaptureTarget::LogicAnalyzer, 1);
        let second = view.position_of("Session 2", 0);
        view.double_click(second);
        view.frame(Vec::new());
        view.type_text("!");

        view.click(egui::pos2(900.0, 300.0));

        assert!(view.app.session_name_editor.is_none());
        assert!(view.app.logic_sessions.names()[1].contains('!'));
    }

    #[test]
    fn an_enabled_tab_explains_what_clicking_it_does() {
        let mut view = TabsView::new(CaptureTarget::LogicAnalyzer, 1);
        let second = view.position_of("Session 2", 0);
        assert!(view
            .hover(second)
            .iter()
            .any(|text| text == "Show this session · double-click to rename"));
    }

    #[test]
    fn the_active_session_is_highlighted() {
        for target in SESSION_TARGETS {
            let mut view = TabsView::new(target, 1);
            let first_active = view.painted();
            view.app
                .apply_session_tab_action(target, SessionTabAction::Select(1));

            assert_ne!(view.painted(), first_active, "{target:?}");
        }
    }

    #[test]
    fn escape_abandons_a_rename() {
        let mut view = TabsView::new(CaptureTarget::LogicAnalyzer, 1);
        let second = view.position_of("Session 2", 0);
        view.double_click(second);
        view.frame(Vec::new());
        view.type_text("!");

        view.key(egui::Key::Escape);

        assert!(view.app.session_name_editor.is_none());
        assert_eq!(view.app.logic_sessions.names()[1], "Session 2");
        assert_eq!(view.app.status, "start");
    }

    #[test]
    fn a_session_name_stops_at_the_length_limit() {
        let mut view = TabsView::new(CaptureTarget::LogicAnalyzer, 1);
        let second = view.position_of("Session 2", 0);
        view.double_click(second);
        view.frame(Vec::new());

        view.type_text(&"x".repeat(SESSION_NAME_MAX_CHARS * 2));

        let draft = &view.app.session_name_editor.as_ref().unwrap().draft;
        assert!(draft.chars().count() <= SESSION_NAME_MAX_CHARS, "{draft:?}");
    }

    #[test]
    fn a_rename_of_the_other_instrument_does_not_replace_a_tab() {
        let mut view = TabsView::new(CaptureTarget::LogicAnalyzer, 1);
        view.app.session_name_editor = Some(SessionNameEditor {
            target: CaptureTarget::Oscilloscope,
            index: 1,
            draft: "Elsewhere".to_owned(),
            request_focus: false,
        });

        assert!(view.texts().iter().any(|text| text == "Session 2"));
        assert!(!view.texts().iter().any(|text| text == "Elsewhere"));
    }

    #[test]
    fn typing_is_ignored_while_the_sessions_cannot_change() {
        let mut view = TabsView::new(CaptureTarget::LogicAnalyzer, 1);
        let second = view.position_of("Session 2", 0);
        view.double_click(second);
        view.frame(Vec::new());
        let (_sender, receiver) = std::sync::mpsc::channel();
        view.app.pending = Some(receiver);

        view.type_text("!");
        view.frame(Vec::new());

        assert_eq!(view.app.logic_sessions.names()[1], "Session 2");
        assert!(view
            .app
            .session_name_editor
            .as_ref()
            .map_or(true, |editor| editor.draft == "Session 2"));
    }
}
