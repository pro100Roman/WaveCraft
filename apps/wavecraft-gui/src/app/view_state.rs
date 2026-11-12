//! Saved capture settings, per-view session settings, and which view and settings tab are active.

use crate::{
    capture_settings_tabs_for_view, channel_names_from_recording_metadata,
    logic_settings_from_recording_metadata, parse_color, parse_logic_acquisition_mode,
    radio_settings_from_recording_metadata, recording_metadata_value,
    saved_logic_settings_metadata, saved_radio_settings_metadata, saved_scope_settings_metadata,
    scope_settings_from_recording_metadata, waveform_tabs, AcquisitionMode, CaptureSettingsProfile,
    CaptureSettingsTab, CombinedSessionSettings, DeviceModel, LogicSessionSettings,
    SavedCaptureSettingsKind, ScopeSessionSettings, WaveformTab, WorkspaceView,
};

use crate::WaveCraftApp;

impl WaveCraftApp {
    pub(crate) fn apply_saved_capture_settings(&mut self) {
        if let Some(profile) = self.saved_capture_settings.profiles.logic_analyzer.as_ref() {
            self.logic_settings = logic_settings_from_recording_metadata(&profile.values);
            self.logic_acquisition_mode = profile
                .values
                .get("logic.acquisition_mode")
                .map(String::as_str)
                .and_then(parse_logic_acquisition_mode)
                .unwrap_or(AcquisitionMode::Single);
            self.repetitive_interval_seconds =
                recording_metadata_value(&profile.values, "logic.repetitive_interval_seconds")
                    .filter(|value: &f32| value.is_finite())
                    .unwrap_or(1.0)
                    .clamp(0.1, 10.0);
            self.logic_smart_timing_enabled =
                recording_metadata_value(&profile.values, "logic.smart_timing_enabled")
                    .unwrap_or(true);
            let (logic_names, _) = channel_names_from_recording_metadata(&profile.values);
            self.logic_channel_names = logic_names;
        }
        if let Some(profile) = self.saved_capture_settings.profiles.oscilloscope.as_ref() {
            self.scope_settings = scope_settings_from_recording_metadata(&profile.values);
            self.scope_auto_sample_rate =
                recording_metadata_value(&profile.values, "scope.auto_sample_rate").unwrap_or(true);
            self.scope_smart_timing_enabled =
                recording_metadata_value(&profile.values, "scope.smart_timing_enabled")
                    .unwrap_or(false);
            let (_, scope_names) = channel_names_from_recording_metadata(&profile.values);
            self.scope_channel_names = scope_names;
            for (index, color) in self.scope_channel_colors.iter_mut().enumerate() {
                if let Some(saved) = profile
                    .values
                    .get(&format!("scope.ch{index}.color"))
                    .and_then(|value| parse_color(value))
                {
                    *color = saved;
                }
            }
        }
        if let Some(profile) = self
            .saved_capture_settings
            .profiles
            .radio_frequency
            .as_ref()
        {
            self.radio_settings = radio_settings_from_recording_metadata(&profile.values);
        }
    }

    pub(crate) fn save_capture_settings(&mut self, kind: SavedCaptureSettingsKind) {
        let mut document = self.saved_capture_settings.clone();
        let profile = match kind {
            SavedCaptureSettingsKind::LogicAnalyzer => {
                CaptureSettingsProfile::new(saved_logic_settings_metadata(
                    self.logic_settings,
                    self.logic_acquisition_mode,
                    self.repetitive_interval_seconds,
                    self.logic_smart_timing_enabled,
                    &self.logic_channel_names,
                ))
            }
            SavedCaptureSettingsKind::Oscilloscope => {
                CaptureSettingsProfile::new(saved_scope_settings_metadata(
                    self.scope_settings,
                    self.scope_auto_sample_rate,
                    self.scope_smart_timing_enabled,
                    &self.scope_channel_names,
                    self.scope_channel_colors,
                ))
            }
            SavedCaptureSettingsKind::RadioFrequency => {
                CaptureSettingsProfile::new(saved_radio_settings_metadata(&self.radio_settings))
            }
        };
        match kind {
            SavedCaptureSettingsKind::LogicAnalyzer => {
                document.profiles.logic_analyzer = Some(profile)
            }
            SavedCaptureSettingsKind::Oscilloscope => {
                document.profiles.oscilloscope = Some(profile)
            }
            SavedCaptureSettingsKind::RadioFrequency => {
                document.profiles.radio_frequency = Some(profile)
            }
        }

        let Some(repository) = self.capture_settings_repository.as_ref() else {
            self.set_error(
                "Capture settings cannot be saved because the configuration directory is unavailable"
                    .to_owned(),
            );
            return;
        };
        match repository.save(&document) {
            Ok(()) => {
                self.saved_capture_settings = document;
                self.status = format!("{} settings saved", kind.label());
                self.last_error = None;
            }
            Err(error) => self.set_error(error),
        }
    }

    pub(crate) fn current_logic_session_settings(&self) -> LogicSessionSettings {
        LogicSessionSettings {
            device_id: self.selected_logic_device_id.clone(),
            capture: self.logic_settings,
            acquisition_mode: self.logic_acquisition_mode,
            repetitive_interval_seconds: self.repetitive_interval_seconds,
            channel_colors: self.logic_channel_colors,
            channel_marker_opacities: self.logic_channel_marker_opacities,
            channel_settings_drafts: self.logic_channel_settings_drafts.clone(),
            smart_timing_enabled: self.logic_smart_timing_enabled,
            marker_settings: self.logic_marker_settings.clone(),
            protocol_analyzer: self.protocol_analyzer.clone(),
            channel_names: self.logic_channel_names.clone(),
            viewport: self.logic_viewport,
        }
    }

    pub(crate) fn apply_logic_session_settings(&mut self, settings: LogicSessionSettings) {
        self.selected_logic_device_id = settings.device_id;
        self.logic_settings = settings.capture;
        self.logic_acquisition_mode = settings.acquisition_mode;
        self.repetitive_interval_seconds = settings.repetitive_interval_seconds;
        self.logic_channel_colors = settings.channel_colors;
        self.logic_channel_marker_opacities = settings.channel_marker_opacities;
        self.logic_channel_settings_drafts = settings.channel_settings_drafts;
        self.logic_smart_timing_enabled = settings.smart_timing_enabled;
        self.logic_marker_settings = settings.marker_settings;
        self.protocol_analyzer = settings.protocol_analyzer;
        self.logic_channel_names = settings.channel_names;
        self.logic_viewport = settings.viewport;
    }

    pub(crate) fn save_active_logic_session_settings(&mut self) {
        let settings = self.current_logic_session_settings();
        self.logic_sessions.replace_active_settings(settings);
    }

    pub(crate) fn current_scope_session_settings(&self) -> ScopeSessionSettings {
        ScopeSessionSettings {
            device_id: self.selected_scope_device_id.clone(),
            capture: self.scope_settings,
            auto_sample_rate: self.scope_auto_sample_rate,
            smart_timing_enabled: self.scope_smart_timing_enabled,
            function_settings: self.scope_function_settings,
            marker_settings: self.scope_marker_settings,
            quick_time_measurement: self.scope_quick_time_measurement,
            channel_colors: self.scope_channel_colors,
            channel_names: self.scope_channel_names.clone(),
            viewport: self.scope_viewport,
            fft_viewport: self.scope_fft_viewport,
            measurement_caches: self.scope_measurement_caches,
            measurement_window_positions: self.scope_measurement_window_positions,
            time_per_division_seconds: self.scope_time_per_division_seconds,
        }
    }

    pub(crate) fn apply_scope_session_settings(&mut self, settings: ScopeSessionSettings) {
        self.selected_scope_device_id = settings.device_id;
        self.scope_settings = settings.capture;
        self.scope_auto_sample_rate = settings.auto_sample_rate;
        self.scope_smart_timing_enabled = settings.smart_timing_enabled;
        self.scope_function_settings = settings.function_settings;
        self.scope_marker_settings = settings.marker_settings;
        self.scope_quick_time_measurement = settings.quick_time_measurement;
        self.scope_marker_editor = None;
        self.scope_channel_colors = settings.channel_colors;
        self.scope_channel_names = settings.channel_names;
        self.scope_viewport = settings.viewport;
        self.scope_fft_viewport = settings.fft_viewport;
        self.scope_measurement_caches = settings.measurement_caches;
        self.scope_measurement_window_positions = settings.measurement_window_positions;
        self.scope_time_per_division_seconds = settings.time_per_division_seconds;
    }

    pub(crate) fn save_active_scope_session_settings(&mut self) {
        let settings = self.current_scope_session_settings();
        self.scope_sessions.replace_active_settings(settings);
    }

    pub(crate) fn current_combined_session_settings(&self) -> CombinedSessionSettings {
        CombinedSessionSettings {
            logic: self.current_logic_session_settings(),
            scope: self.current_scope_session_settings(),
            logic_ratio: self.combined_logic_ratio,
            viewport: self.combined_viewport,
        }
    }

    pub(crate) fn apply_combined_session_settings(&mut self, settings: CombinedSessionSettings) {
        self.apply_logic_session_settings(settings.logic);
        self.apply_scope_session_settings(settings.scope);
        self.combined_logic_ratio = settings.logic_ratio;
        self.combined_viewport = settings.viewport;
    }

    pub(crate) fn save_waveform_view_settings(&mut self, tab: WaveformTab) {
        match tab {
            WaveformTab::LogicAnalyzer => self.save_active_logic_session_settings(),
            WaveformTab::Oscilloscope | WaveformTab::Lissajous => {
                self.save_active_scope_session_settings();
            }
            WaveformTab::RadioReceiver => {}
            WaveformTab::Combined => {
                self.combined_settings = self.current_combined_session_settings();
            }
        }
    }

    pub(crate) fn apply_waveform_view_settings(&mut self, tab: WaveformTab) {
        match tab {
            WaveformTab::LogicAnalyzer => {
                let settings = self.logic_sessions.active_settings().clone();
                self.apply_logic_session_settings(settings);
            }
            WaveformTab::Oscilloscope | WaveformTab::Lissajous => {
                let settings = self.scope_sessions.active_settings().clone();
                self.apply_scope_session_settings(settings);
            }
            WaveformTab::RadioReceiver => {}
            WaveformTab::Combined => {
                self.apply_combined_session_settings(self.combined_settings.clone());
            }
        }
    }

    pub(crate) fn reset_session_captures(&mut self) {
        let logic_settings = self.current_logic_session_settings();
        let scope_settings = self.current_scope_session_settings();
        self.logic_sessions.reset(logic_settings);
        self.scope_sessions.reset(scope_settings);
        self.session_name_editor = None;
        self.combined_capture = None;
        self.radio_capture = None;
    }

    pub(crate) fn available_waveform_tabs(&self) -> Vec<WaveformTab> {
        let displayed_logic = self.captures.iter().any(|capture| capture.has_logic)
            || self.logic_sessions.any(|capture| capture.has_logic)
            || self
                .combined_capture
                .as_ref()
                .is_some_and(|capture| capture.has_logic);
        let displayed_scope = self.captures.iter().any(|capture| capture.has_scope)
            || self.scope_sessions.any(|capture| capture.has_scope)
            || self
                .combined_capture
                .as_ref()
                .is_some_and(|capture| capture.has_scope);
        let displayed_radio = self.captures.iter().any(|capture| capture.has_radio)
            || self
                .radio_capture
                .as_ref()
                .is_some_and(|capture| capture.has_radio);
        let has_logic_analyzer = displayed_logic
            || self
                .devices
                .iter()
                .any(|entry| entry.device.profile.model.is_logic_analyzer());
        let has_oscilloscope = displayed_scope
            || self
                .devices
                .iter()
                .any(|entry| entry.device.profile.model == DeviceModel::DsCopeU3P100);
        let has_radio_receiver = displayed_radio || !self.radios.is_empty();
        waveform_tabs(
            has_logic_analyzer,
            has_oscilloscope,
            has_radio_receiver,
            self.scope_function_settings.lissajous.enabled,
        )
    }

    pub(crate) fn select_waveform_tab(&mut self, tab: WaveformTab) {
        if self.active_waveform_tab != Some(tab) {
            if let Some(active) = self.active_waveform_tab {
                self.save_waveform_view_settings(active);
            }
            self.apply_waveform_view_settings(tab);
        }
        self.active_waveform_tab = Some(tab);
        self.active_workspace_view = Some(WorkspaceView::Waveform(tab));
        if let Some(settings_tab) = tab.capture_settings_tab() {
            self.active_capture_settings_tab = settings_tab;
        }
    }

    pub(crate) fn select_system_trace_view(&mut self) {
        if let Some(active) = self.active_waveform_tab {
            self.save_waveform_view_settings(active);
        }
        self.active_workspace_view = Some(WorkspaceView::SystemTrace);
        self.snapshot_plot_rect = None;
    }

    pub(crate) fn system_trace_view_is_active(&self) -> bool {
        self.active_workspace_view == Some(WorkspaceView::SystemTrace)
    }

    pub(crate) fn active_waveform_view(&self) -> Option<WaveformTab> {
        match self.active_workspace_view {
            Some(WorkspaceView::Waveform(tab)) => Some(tab),
            Some(WorkspaceView::SystemTrace) | None => None,
        }
    }

    pub(crate) fn available_capture_settings_tabs(&self) -> Vec<CaptureSettingsTab> {
        let has_logic_device = self
            .devices
            .iter()
            .any(|entry| entry.device.profile.model.is_logic_analyzer());
        let has_scope_device = self
            .devices
            .iter()
            .any(|entry| entry.device.profile.model == DeviceModel::DsCopeU3P100);
        let has_logic_capture = self.captures.iter().any(|capture| capture.has_logic)
            || self.logic_sessions.any(|capture| capture.has_logic)
            || self
                .combined_capture
                .as_ref()
                .is_some_and(|capture| capture.has_logic);
        let has_scope_capture = self.captures.iter().any(|capture| capture.has_scope)
            || self.scope_sessions.any(|capture| capture.has_scope)
            || self
                .combined_capture
                .as_ref()
                .is_some_and(|capture| capture.has_scope);
        capture_settings_tabs_for_view(
            self.active_waveform_tab,
            has_logic_device,
            has_scope_device,
            has_logic_capture,
            has_scope_capture,
        )
    }

    pub(crate) fn reconcile_capture_settings_tab(&mut self) {
        let available = self.available_capture_settings_tabs();
        if available.contains(&self.active_capture_settings_tab) {
            return;
        }
        if let Some(tab) = available.first() {
            self.active_capture_settings_tab = *tab;
        }
    }

    pub(crate) fn reconcile_waveform_tab(&mut self) {
        let available = self.available_waveform_tabs();
        if self
            .active_waveform_tab
            .is_some_and(|active| available.contains(&active))
        {
            if self.active_workspace_view.is_none() {
                self.active_workspace_view = self.active_waveform_tab.map(WorkspaceView::Waveform);
            }
            return;
        }
        let selected = if available.contains(&WaveformTab::Combined) {
            Some(WaveformTab::Combined)
        } else {
            available.first().copied()
        };
        if let Some(tab) = selected {
            if self.system_trace_view_is_active() {
                self.active_waveform_tab = Some(tab);
            } else {
                self.select_waveform_tab(tab);
            }
        } else {
            self.active_waveform_tab = None;
            if !self.system_trace_view_is_active() {
                self.active_workspace_view = Some(WorkspaceView::SystemTrace);
            }
        }
    }
}
