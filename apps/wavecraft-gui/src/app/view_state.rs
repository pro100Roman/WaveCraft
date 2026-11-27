//! Saved capture settings, per-view session settings, and which view and settings tab are active.

use crate::{
    capture_settings_tabs_for_view, channel_names_from_recording_metadata,
    logic_settings_from_recording_metadata, parse_color, parse_logic_acquisition_mode,
    radio_settings_from_recording_metadata, recording_metadata_value,
    saved_logic_settings_metadata, saved_radio_settings_metadata, saved_scope_settings_metadata,
    scope_settings_from_recording_metadata, waveform_tabs, AcquisitionMode,
    CaptureSettingsDocument, CaptureSettingsProfile, CaptureSettingsTab, CombinedSessionSettings,
    DeviceModel, LogicSessionSettings, SavedCaptureSettingsKind, SavedTraceEventView,
    SavedTraceInstrument, SavedTraceInstrumentHeights, SavedWorkspaceView, ScopeSessionSettings,
    VnaMatchingNetwork, WaveformTab, WorkspaceView,
};
use std::collections::BTreeMap;
use std::time::Instant;

use crate::{
    system_trace::{TraceEventView, TraceInstrument},
    WaveCraftApp,
};

impl From<WorkspaceView> for SavedWorkspaceView {
    fn from(view: WorkspaceView) -> Self {
        match view {
            WorkspaceView::Waveform(WaveformTab::LogicAnalyzer) => Self::LogicAnalyzer,
            WorkspaceView::Waveform(WaveformTab::Oscilloscope) => Self::Oscilloscope,
            WorkspaceView::Waveform(WaveformTab::RadioReceiver) => Self::RadioSpectrum,
            WorkspaceView::Waveform(WaveformTab::Combined) => Self::Combined,
            WorkspaceView::Vna => Self::Vna,
            WorkspaceView::SystemTrace => Self::SystemTrace,
            WorkspaceView::Serial => Self::Serial,
            WorkspaceView::Battery => Self::Battery,
            WorkspaceView::Battery2 => Self::Battery2,
            WorkspaceView::Battery3 => Self::Battery3,
            WorkspaceView::Battery4 => Self::Battery4,
            WorkspaceView::Battery5 => Self::Battery5,
            WorkspaceView::Battery6 => Self::Battery6,
            WorkspaceView::Battery7 => Self::Battery7,
            WorkspaceView::Battery8 => Self::Battery8,
            WorkspaceView::Battery9 => Self::Battery9,
            WorkspaceView::Battery10 => Self::Battery10,
            WorkspaceView::PdPower => Self::PdPower,
            WorkspaceView::PdPower2 => Self::PdPower2,
            WorkspaceView::PdPower3 => Self::PdPower3,
            WorkspaceView::PdPower4 => Self::PdPower4,
            WorkspaceView::PdPower5 => Self::PdPower5,
        }
    }
}

impl From<SavedWorkspaceView> for WorkspaceView {
    fn from(view: SavedWorkspaceView) -> Self {
        match view {
            SavedWorkspaceView::LogicAnalyzer => Self::Waveform(WaveformTab::LogicAnalyzer),
            SavedWorkspaceView::Oscilloscope | SavedWorkspaceView::Lissajous => {
                Self::Waveform(WaveformTab::Oscilloscope)
            }
            SavedWorkspaceView::RadioSpectrum => Self::Waveform(WaveformTab::RadioReceiver),
            SavedWorkspaceView::Combined => Self::Waveform(WaveformTab::Combined),
            SavedWorkspaceView::Vna => Self::Vna,
            SavedWorkspaceView::SystemTrace => Self::SystemTrace,
            SavedWorkspaceView::Serial => Self::Serial,
            SavedWorkspaceView::Battery => Self::Battery,
            SavedWorkspaceView::Battery2 => Self::Battery2,
            SavedWorkspaceView::Battery3 => Self::Battery3,
            SavedWorkspaceView::Battery4 => Self::Battery4,
            SavedWorkspaceView::Battery5 => Self::Battery5,
            SavedWorkspaceView::Battery6 => Self::Battery6,
            SavedWorkspaceView::Battery7 => Self::Battery7,
            SavedWorkspaceView::Battery8 => Self::Battery8,
            SavedWorkspaceView::Battery9 => Self::Battery9,
            SavedWorkspaceView::Battery10 => Self::Battery10,
            SavedWorkspaceView::PdPower => Self::PdPower,
            SavedWorkspaceView::PdPower2 => Self::PdPower2,
            SavedWorkspaceView::PdPower3 => Self::PdPower3,
            SavedWorkspaceView::PdPower4 => Self::PdPower4,
            SavedWorkspaceView::PdPower5 => Self::PdPower5,
        }
    }
}

impl From<TraceEventView> for SavedTraceEventView {
    fn from(view: TraceEventView) -> Self {
        match view {
            TraceEventView::Events => Self::Events,
            TraceEventView::Logs => Self::Logs,
            TraceEventView::Terminal => Self::RttTerminal,
            TraceEventView::ProbeLog => Self::ProbeLog,
            TraceEventView::CpuLoad => Self::CpuLoad,
            TraceEventView::ContextStatistics => Self::ContextStatistics,
        }
    }
}

impl From<SavedTraceEventView> for TraceEventView {
    fn from(view: SavedTraceEventView) -> Self {
        match view {
            SavedTraceEventView::Events => Self::Events,
            SavedTraceEventView::Logs => Self::Logs,
            SavedTraceEventView::RttTerminal => Self::Terminal,
            SavedTraceEventView::ProbeLog => Self::ProbeLog,
            SavedTraceEventView::CpuLoad => Self::CpuLoad,
            SavedTraceEventView::ContextStatistics => Self::ContextStatistics,
        }
    }
}

impl From<TraceInstrument> for SavedTraceInstrument {
    fn from(instrument: TraceInstrument) -> Self {
        match instrument {
            TraceInstrument::Timeline => Self::Timeline,
            TraceInstrument::Analysis => Self::Analysis,
        }
    }
}

impl From<SavedTraceInstrument> for TraceInstrument {
    fn from(instrument: SavedTraceInstrument) -> Self {
        match instrument {
            SavedTraceInstrument::Timeline => Self::Timeline,
            SavedTraceInstrument::Analysis => Self::Analysis,
        }
    }
}

impl WaveCraftApp {
    pub(crate) fn track_window_size(&mut self, context: &crate::egui::Context) {
        let now = Instant::now();
        if let Some(size) = context.input(|input| {
            input
                .viewport()
                .inner_rect
                .map(|rect| rect.size())
                .and_then(crate::saved_window_size_from_vec2)
        }) {
            self.observe_window_size(size, now);
        }
        self.save_window_size_if_due(now);
    }

    fn observe_window_size(&mut self, size: crate::SavedWindowSize, now: Instant) {
        if self.observed_window_size == Some(size) {
            return;
        }
        self.observed_window_size = Some(size);
        self.window_size_save_due = Some(now + crate::WINDOW_SIZE_SAVE_DELAY);
    }

    fn save_window_size_if_due(&mut self, now: Instant) {
        if self
            .window_size_save_due
            .is_some_and(|save_due| now >= save_due)
        {
            self.window_size_save_due = None;
            self.save_global_settings();
        }
    }

    pub(crate) fn save_pending_window_size(&mut self) {
        if self.window_size_save_due.take().is_some() {
            self.save_global_settings();
        }
    }

    pub(crate) fn save_global_settings(&mut self) {
        let Some(active_view) = self.active_workspace_view else {
            return;
        };
        let saved_view = SavedWorkspaceView::from(active_view);
        let window_inner_size = self.observed_window_size;
        let workspace_view_order = self
            .workspace_view_layout
            .order()
            .iter()
            .copied()
            .map(SavedWorkspaceView::from)
            .collect::<Vec<_>>();
        let hidden_workspace_views = WorkspaceView::ALL
            .into_iter()
            .filter(|view| !self.workspace_view_layout.is_visible(*view))
            .map(SavedWorkspaceView::from)
            .collect::<Vec<_>>();
        let collapsed_setup_views = WorkspaceView::ALL
            .into_iter()
            .filter(|view| !self.workspace_view_layout.setup_panel_open(*view))
            .map(SavedWorkspaceView::from)
            .collect::<Vec<_>>();
        let event_view_order = self
            .system_trace
            .event_view_order()
            .iter()
            .copied()
            .map(SavedTraceEventView::from)
            .collect::<Vec<_>>();
        let instrument_order = self
            .system_trace
            .trace_instrument_order()
            .iter()
            .copied()
            .map(SavedTraceInstrument::from)
            .collect::<Vec<_>>();
        let instrument_heights = SavedTraceInstrumentHeights {
            timeline: self.system_trace.timeline_view_height().round() as u32,
            analysis: self.system_trace.event_view_height().round() as u32,
        };
        let timeline_label_width = self.system_trace.timeline_label_width().round() as u32;
        let power_fractions = self
            .pd_power
            .iter()
            .map(|state| state.plot_fraction)
            .collect::<Vec<_>>();
        if self.saved_capture_settings.global.pd_power_plot_fractions == power_fractions
            && self.saved_capture_settings.global.pd_power_plot_fraction
                == Some(self.pd_power[0].plot_fraction)
            && self.saved_capture_settings.global.serial.as_ref() == Some(&self.serial.settings)
            && self.saved_capture_settings.global.collapsed_setup_views == collapsed_setup_views
            && self.saved_capture_settings.global.scope_plot_tab == self.scope_plot_tab
            && self.saved_capture_settings.global.hidden_workspace_views == hidden_workspace_views
            && self.saved_capture_settings.global.last_active_view == Some(saved_view)
            && self.saved_capture_settings.global.window_inner_size == window_inner_size
            && self
                .saved_capture_settings
                .global
                .workspace_view_order
                .as_deref()
                == Some(workspace_view_order.as_slice())
            && self
                .saved_capture_settings
                .global
                .system_trace_event_view_order
                .as_deref()
                == Some(event_view_order.as_slice())
            && self
                .saved_capture_settings
                .global
                .system_trace_instrument_order
                .as_deref()
                == Some(instrument_order.as_slice())
            && self
                .saved_capture_settings
                .global
                .system_trace_instrument_heights
                == Some(instrument_heights)
            && self
                .saved_capture_settings
                .global
                .system_trace_timeline_label_width
                == Some(timeline_label_width)
        {
            return;
        }

        let mut document = self.saved_capture_settings.clone();
        document.global.pd_power_plot_fractions = power_fractions;
        document.global.pd_power_plot_fraction = Some(self.pd_power[0].plot_fraction);
        document.global.collapsed_setup_views = collapsed_setup_views;
        document.global.serial = Some(self.serial.settings.clone());
        document.global.scope_plot_tab = self.scope_plot_tab;
        document.global.hidden_workspace_views = hidden_workspace_views;
        document.global.last_active_view = Some(saved_view);
        document.global.window_inner_size = window_inner_size;
        document.global.workspace_view_order = Some(workspace_view_order);
        document.global.system_trace_event_view_order = Some(event_view_order);
        document.global.system_trace_instrument_order = Some(instrument_order);
        document.global.system_trace_instrument_heights = Some(instrument_heights);
        document.global.system_trace_timeline_label_width = Some(timeline_label_width);
        let result = self
            .capture_settings_repository
            .as_ref()
            .map(|repository| repository.save(&document));
        self.saved_capture_settings = document;
        if let Some(Err(error)) = result {
            self.last_error = Some(format!("Global settings could not be saved: {error}"));
        }
    }

    pub(crate) fn apply_saved_capture_settings(&mut self) {
        if let Some(fraction) = self
            .saved_capture_settings
            .global
            .pd_power_plot_fraction
            .filter(|v| v.is_finite() && (0.0..=1.0).contains(v))
        {
            self.pd_power[0].plot_fraction = fraction;
        }
        if let Some(settings) = &self.saved_capture_settings.global.pd_power {
            if settings.validate().is_ok() {
                self.pd_power[0].settings = settings.clone();
            }
        }
        for (index, state) in self.pd_power.iter_mut().enumerate() {
            if let Some(settings) = self
                .saved_capture_settings
                .global
                .pd_power_views
                .get(index)
                .filter(|settings| settings.validate().is_ok())
            {
                state.settings = settings.clone();
            }
            if let Some(fraction) = self
                .saved_capture_settings
                .global
                .pd_power_plot_fractions
                .get(index)
                .filter(|v| v.is_finite() && (0.0..=1.0).contains(*v))
            {
                state.plot_fraction = *fraction;
            }
        }
        // Legacy layouts have one tab per instrument. New layouts list every slot and
        // persist visibility in the common hidden-view list.
        for view in WorkspaceView::PD_POWER
            .into_iter()
            .skip(1)
            .chain(WorkspaceView::BATTERY.into_iter().skip(1))
        {
            let saved = SavedWorkspaceView::from(view);
            if self
                .saved_capture_settings
                .global
                .workspace_view_order
                .as_ref()
                .is_some_and(|order| order.contains(&saved))
            {
                self.workspace_view_layout.set_visible(
                    view,
                    !self
                        .saved_capture_settings
                        .global
                        .hidden_workspace_views
                        .contains(&saved),
                );
            }
        }
        if let Some(settings) = &self.saved_capture_settings.global.battery {
            match settings.validate() {
                Ok(()) => self.battery[0].settings = settings.clone(),
                Err(error) => {
                    self.battery[0].error = Some(format!("Saved battery settings ignored: {error}"))
                }
            }
        }
        for (state, settings) in self
            .battery
            .iter_mut()
            .zip(&self.saved_capture_settings.global.battery_views)
        {
            match settings.validate() {
                Ok(()) => {
                    state.settings = settings.clone();
                    state.error = None;
                }
                Err(error) => {
                    state.error = Some(format!("Saved battery settings ignored: {error}"))
                }
            }
        }
        if let Some(settings) = &self.saved_capture_settings.global.serial {
            self.serial.settings = settings.clone();
            self.serial.baud_draft = None;
            self.serial
                .settings
                .fields
                .truncate(wavecraft_serial::MAX_TEXT_FIELDS);
            self.serial.settings.graph_percent = self.serial.settings.graph_percent.clamp(10, 90);
        }
        for view in &self.saved_capture_settings.global.collapsed_setup_views {
            self.workspace_view_layout
                .set_setup_panel_open((*view).into(), false);
        }
        for view in &self.saved_capture_settings.global.hidden_workspace_views {
            // Lissajous visibility used to affect only the separate XY view.
            if *view != SavedWorkspaceView::Lissajous {
                self.workspace_view_layout
                    .set_visible((*view).into(), false);
            }
        }
        if let Some(order) = self
            .saved_capture_settings
            .global
            .workspace_view_order
            .as_ref()
        {
            let mut order = order
                .iter()
                .copied()
                .filter(|view| *view != SavedWorkspaceView::Lissajous)
                .map(WorkspaceView::from)
                .collect::<Vec<_>>();
            // Append newly introduced views while retaining legacy tab ordering.
            for view in WorkspaceView::ALL {
                if !order.contains(&view) {
                    order.push(view);
                }
            }
            self.workspace_view_layout.set_order(&order);
        }
        if let Some(order) = self
            .saved_capture_settings
            .global
            .system_trace_event_view_order
            .as_ref()
        {
            let order = order
                .iter()
                .copied()
                .map(TraceEventView::from)
                .collect::<Vec<_>>();
            self.system_trace.set_event_view_order(&order);
        }
        if let Some(order) = self
            .saved_capture_settings
            .global
            .system_trace_instrument_order
            .as_ref()
        {
            let order = order
                .iter()
                .copied()
                .map(TraceInstrument::from)
                .collect::<Vec<_>>();
            self.system_trace.set_trace_instrument_order(&order);
        }
        if let Some(heights) = self
            .saved_capture_settings
            .global
            .system_trace_instrument_heights
        {
            self.system_trace
                .set_trace_instrument_heights(heights.timeline as f32, heights.analysis as f32);
        }
        if let Some(width) = self
            .saved_capture_settings
            .global
            .system_trace_timeline_label_width
        {
            self.system_trace.set_timeline_label_width(width as f32);
        }
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
        if let Some(profile) = self
            .saved_capture_settings
            .profiles
            .vector_network_analyzer
            .as_ref()
        {
            let mut settings = self.vna_settings;
            settings.start_mhz = recording_metadata_value(&profile.values, "vna.start_mhz")
                .unwrap_or(settings.start_mhz);
            settings.stop_mhz = recording_metadata_value(&profile.values, "vna.stop_mhz")
                .unwrap_or(settings.stop_mhz);
            settings.points =
                recording_metadata_value(&profile.values, "vna.points").unwrap_or(settings.points);
            settings.averaging = recording_metadata_value(&profile.values, "vna.averaging")
                .unwrap_or(settings.averaging);
            if let Some(network) = profile
                .values
                .get("vna.matching_network")
                .and_then(|value| VnaMatchingNetwork::from_persisted_value(value))
            {
                settings.matching_network = network;
            }
            if let Some(matching_q) =
                recording_metadata_value::<f64>(&profile.values, "vna.matching_q")
            {
                let mut candidate = settings;
                candidate.matching_q = matching_q;
                if candidate.matching_q_is_valid() {
                    settings.matching_q = matching_q;
                }
            }
            if settings.sweep_config().is_ok() && settings.matching_q_is_valid() {
                self.vna_settings = settings;
                self.vna_start_frequency_input = settings.start_mhz.to_string();
                self.vna_stop_frequency_input = settings.stop_mhz.to_string();
            }
            if let Some(serialized) = profile.values.get("vna.one_port_calibration") {
                match serde_json::from_str::<wavecraft_vna::OnePortCalibration>(serialized) {
                    Ok(calibration) => match calibration.validate() {
                        Ok(()) => {
                            let averaging = recording_metadata_value(
                                &profile.values,
                                "vna.calibration_averaging",
                            )
                            .unwrap_or(settings.averaging);
                            let mut active = crate::ActiveVnaCalibration::from_completed_sol(
                                calibration,
                                averaging,
                            );
                            if let Some(name) = profile
                                .values
                                .get("vna.calibration_name")
                                .map(|name| name.trim())
                                .filter(|name| !name.is_empty())
                            {
                                active.name = name.to_owned();
                            }
                            self.vna_calibration = Some(active);
                        }
                        Err(error) => {
                            self.last_error =
                                Some(format!("Saved VNA calibration was ignored: {error}"));
                        }
                    },
                    Err(error) => {
                        self.last_error = Some(format!(
                            "Saved VNA calibration could not be parsed and was ignored: {error}"
                        ));
                    }
                }
            }
        }
        if let Some(profile) = self.saved_capture_settings.profiles.system_trace.as_ref() {
            self.system_trace_settings
                .apply_saved_values(&profile.values);
            self.reload_system_trace_assets();
        }
    }

    pub(crate) fn save_setup_settings(&mut self, view: WorkspaceView) {
        match view {
            WorkspaceView::Waveform(WaveformTab::LogicAnalyzer) => {
                self.save_capture_settings(SavedCaptureSettingsKind::LogicAnalyzer)
            }
            WorkspaceView::Waveform(WaveformTab::Oscilloscope) => {
                self.save_capture_settings(SavedCaptureSettingsKind::Oscilloscope)
            }
            WorkspaceView::Waveform(WaveformTab::RadioReceiver) => {
                self.save_capture_settings(SavedCaptureSettingsKind::RadioFrequency)
            }
            WorkspaceView::Waveform(WaveformTab::Combined) => self.save_capture_profiles(
                &[
                    SavedCaptureSettingsKind::LogicAnalyzer,
                    SavedCaptureSettingsKind::Oscilloscope,
                ],
                "Combined",
            ),
            WorkspaceView::Vna => {
                self.save_capture_settings(SavedCaptureSettingsKind::VectorNetworkAnalyzer)
            }
            WorkspaceView::SystemTrace => {
                self.save_capture_settings(SavedCaptureSettingsKind::SystemTrace)
            }
            WorkspaceView::PdPower
            | WorkspaceView::PdPower2
            | WorkspaceView::PdPower3
            | WorkspaceView::PdPower4
            | WorkspaceView::PdPower5 => {
                let index = view.pd_power_index().unwrap_or(0);
                if let Err(error) = self.pd_power[index].settings.validate() {
                    self.pd_power[index].error = Some(error.to_string());
                    return;
                }
                let mut document = self.saved_capture_settings.clone();
                document
                    .global
                    .pd_power_views
                    .resize_with(5, crate::pd_power::PowerSettings::default);
                // Retain an existing legacy primary profile when saving a new slot.
                if let Some(primary) = &document.global.pd_power {
                    document.global.pd_power_views[0] = primary.clone();
                }
                document.global.pd_power_views[index] = self.pd_power[index].settings.clone();
                if index == 0 {
                    document.global.pd_power = Some(self.pd_power[0].settings.clone());
                }
                if self.persist_setup_document(document, view.label()) {
                    self.pd_power[index].status = self.status.clone();
                }
            }
            WorkspaceView::Battery
            | WorkspaceView::Battery2
            | WorkspaceView::Battery3
            | WorkspaceView::Battery4
            | WorkspaceView::Battery5
            | WorkspaceView::Battery6
            | WorkspaceView::Battery7
            | WorkspaceView::Battery8
            | WorkspaceView::Battery9
            | WorkspaceView::Battery10 => {
                let index = view.battery_index().unwrap_or(0);
                if let Err(error) = self.battery[index].settings.validate() {
                    self.battery[index].error = Some(error);
                    return;
                }
                let mut document = self.saved_capture_settings.clone();
                document.global.battery_views.resize_with(
                    WorkspaceView::BATTERY.len(),
                    crate::battery::BatterySettings::default,
                );
                if let Some(primary) = &document.global.battery {
                    document.global.battery_views[0] = primary.clone();
                }
                document.global.battery_views[index] = self.battery[index].settings.clone();
                if index == 0 {
                    document.global.battery = Some(self.battery[0].settings.clone());
                }
                if self.persist_setup_document(document, view.label()) {
                    self.battery[index].status = self.status.clone();
                    self.battery[index].error = None;
                }
            }
            WorkspaceView::Serial => {
                let mut document = self.saved_capture_settings.clone();
                document.global.serial = Some(self.serial.settings.clone());
                if self.persist_setup_document(document, "Serial") {
                    self.serial.status = self.status.clone();
                }
            }
        }
    }

    pub(crate) fn save_capture_settings(&mut self, kind: SavedCaptureSettingsKind) {
        self.save_capture_profiles(&[kind], kind.label());
    }

    fn save_capture_profiles(&mut self, kinds: &[SavedCaptureSettingsKind], label: &str) {
        let mut document = self.saved_capture_settings.clone();
        for &kind in kinds {
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
                SavedCaptureSettingsKind::VectorNetworkAnalyzer => {
                    let mut values = BTreeMap::new();
                    values.insert(
                        "vna.start_mhz".to_owned(),
                        self.vna_settings.start_mhz.to_string(),
                    );
                    values.insert(
                        "vna.stop_mhz".to_owned(),
                        self.vna_settings.stop_mhz.to_string(),
                    );
                    values.insert(
                        "vna.points".to_owned(),
                        self.vna_settings.points.to_string(),
                    );
                    values.insert(
                        "vna.averaging".to_owned(),
                        self.vna_settings.averaging.to_string(),
                    );
                    values.insert(
                        "vna.matching_network".to_owned(),
                        self.vna_settings
                            .matching_network
                            .persisted_value()
                            .to_owned(),
                    );
                    values.insert(
                        "vna.matching_q".to_owned(),
                        self.vna_settings.matching_q.to_string(),
                    );
                    if let Some(calibration) = &self.vna_calibration {
                        let serialized = match serde_json::to_string(&calibration.calibration) {
                            Ok(serialized) => serialized,
                            Err(error) => {
                                self.set_error(format!(
                                    "failed to serialize VNA calibration: {error}"
                                ));
                                return;
                            }
                        };
                        values.insert("vna.one_port_calibration".to_owned(), serialized);
                        values.insert("vna.calibration_name".to_owned(), calibration.name.clone());
                        values.insert(
                            "vna.calibration_averaging".to_owned(),
                            calibration.averaging.to_string(),
                        );
                    }
                    CaptureSettingsProfile::new(values)
                }
                SavedCaptureSettingsKind::SystemTrace => {
                    CaptureSettingsProfile::new(self.system_trace_settings.saved_values())
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
                SavedCaptureSettingsKind::VectorNetworkAnalyzer => {
                    document.profiles.vector_network_analyzer = Some(profile)
                }
                SavedCaptureSettingsKind::SystemTrace => {
                    document.profiles.system_trace = Some(profile)
                }
            }
        }
        self.persist_setup_document(document, label);
    }

    fn persist_setup_document(&mut self, document: CaptureSettingsDocument, label: &str) -> bool {
        let Some(repository) = self.capture_settings_repository.as_ref() else {
            self.set_error(
                "Capture settings cannot be saved because the configuration directory is unavailable"
                    .to_owned(),
            );
            return false;
        };
        match repository.save(&document) {
            Ok(()) => {
                self.saved_capture_settings = document;
                self.status = format!("{} settings saved", label);
                self.last_error = None;
                true
            }
            Err(error) => {
                self.set_error(error);
                false
            }
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
            WaveformTab::Oscilloscope => {
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
            WaveformTab::Oscilloscope => {
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
        waveform_tabs()
            .into_iter()
            .filter(|tab| {
                self.workspace_view_layout
                    .is_visible(WorkspaceView::Waveform(*tab))
            })
            .collect()
    }

    pub(crate) fn available_workspace_views(&self) -> Vec<WorkspaceView> {
        let available = crate::workspace_views(&self.available_waveform_tabs());
        self.workspace_view_layout
            .order()
            .iter()
            .copied()
            .filter(|view| available.contains(view) && self.workspace_view_layout.is_visible(*view))
            .collect()
    }

    pub(crate) fn open_workspace_view(&mut self, view: WorkspaceView) {
        self.workspace_view_layout.open_view(view);
        self.select_workspace_view(view);
    }

    pub(crate) fn select_workspace_view(&mut self, view: WorkspaceView) {
        match view {
            WorkspaceView::Waveform(tab) => self.select_waveform_tab(tab),
            WorkspaceView::Vna => self.select_vna_view(),
            WorkspaceView::SystemTrace => self.select_system_trace_view(),
            WorkspaceView::Serial => self.select_serial_view(),
            battery @ (WorkspaceView::Battery
            | WorkspaceView::Battery2
            | WorkspaceView::Battery3
            | WorkspaceView::Battery4
            | WorkspaceView::Battery5
            | WorkspaceView::Battery6
            | WorkspaceView::Battery7
            | WorkspaceView::Battery8
            | WorkspaceView::Battery9
            | WorkspaceView::Battery10) => self.select_battery_view(battery),
            WorkspaceView::PdPower
            | WorkspaceView::PdPower2
            | WorkspaceView::PdPower3
            | WorkspaceView::PdPower4
            | WorkspaceView::PdPower5 => self.select_pd_power_view(view),
        }
    }

    pub(crate) fn set_workspace_view_visible(&mut self, view: WorkspaceView, visible: bool) {
        let available = self.available_workspace_views();
        if !visible
            && self.workspace_view_layout.is_visible(view)
            && available.iter().all(|candidate| *candidate == view)
        {
            return;
        }
        let next_active = if !visible && self.active_workspace_view == Some(view) {
            available
                .iter()
                .position(|candidate| *candidate == view)
                .and_then(|index| {
                    available[..index]
                        .last()
                        .or_else(|| available.get(index + 1))
                })
                .copied()
        } else {
            None
        };
        if !visible {
            if let Some(index) = view.pd_power_index() {
                self.pd_power[index].stop();
            }
        }
        self.workspace_view_layout.set_visible(view, visible);
        if let Some(next) = next_active {
            self.select_workspace_view(next);
        }
        self.reconcile_waveform_tab();
        self.save_global_settings();
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
        self.save_global_settings();
    }

    pub(crate) fn select_system_trace_view(&mut self) {
        if let Some(active) = self.active_waveform_tab {
            self.save_waveform_view_settings(active);
        }
        self.active_workspace_view = Some(WorkspaceView::SystemTrace);
        self.snapshot_plot_rect = None;
        self.save_global_settings();
    }

    pub(crate) fn select_vna_view(&mut self) {
        let entering_vna = !self.vna_view_is_active();
        if let Some(active) = self.active_waveform_tab {
            self.save_waveform_view_settings(active);
        }
        self.active_workspace_view = Some(WorkspaceView::Vna);
        if entering_vna {
            self.vna_calibration_notice_open = true;
        }
        self.snapshot_plot_rect = None;
        self.save_global_settings();
    }

    pub(crate) fn vna_view_is_active(&self) -> bool {
        self.active_workspace_view == Some(WorkspaceView::Vna)
    }

    pub(crate) fn system_trace_view_is_active(&self) -> bool {
        self.active_workspace_view == Some(WorkspaceView::SystemTrace)
    }

    pub(crate) fn active_waveform_view(&self) -> Option<WaveformTab> {
        match self.active_workspace_view {
            Some(WorkspaceView::Waveform(tab)) => Some(tab),
            Some(
                WorkspaceView::Vna
                | WorkspaceView::SystemTrace
                | WorkspaceView::Serial
                | WorkspaceView::Battery
                | WorkspaceView::Battery2
                | WorkspaceView::Battery3
                | WorkspaceView::Battery4
                | WorkspaceView::Battery5
                | WorkspaceView::Battery6
                | WorkspaceView::Battery7
                | WorkspaceView::Battery8
                | WorkspaceView::Battery9
                | WorkspaceView::Battery10
                | WorkspaceView::PdPower
                | WorkspaceView::PdPower2
                | WorkspaceView::PdPower3
                | WorkspaceView::PdPower4
                | WorkspaceView::PdPower5,
            )
            | None => None,
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
        let mut views = self.available_workspace_views();
        if views.is_empty() {
            // Recover from settings that hide every usable view.
            let fallback = WorkspaceView::Waveform(WaveformTab::LogicAnalyzer);
            self.workspace_view_layout.set_visible(fallback, true);
            views.push(fallback);
        }
        if self
            .active_workspace_view
            .is_some_and(|view| !views.contains(&view))
        {
            match views[0] {
                WorkspaceView::Waveform(tab) => self.select_waveform_tab(tab),
                WorkspaceView::Vna => self.select_vna_view(),
                WorkspaceView::SystemTrace => self.select_system_trace_view(),
                WorkspaceView::Serial => self.select_serial_view(),
                battery @ (WorkspaceView::Battery
                | WorkspaceView::Battery2
                | WorkspaceView::Battery3
                | WorkspaceView::Battery4
                | WorkspaceView::Battery5
                | WorkspaceView::Battery6
                | WorkspaceView::Battery7
                | WorkspaceView::Battery8
                | WorkspaceView::Battery9
                | WorkspaceView::Battery10) => self.select_battery_view(battery),
                power @ (WorkspaceView::PdPower
                | WorkspaceView::PdPower2
                | WorkspaceView::PdPower3
                | WorkspaceView::PdPower4
                | WorkspaceView::PdPower5) => self.select_pd_power_view(power),
            }
        }
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
            if self.system_trace_view_is_active()
                || self.vna_view_is_active()
                || self.serial_view_is_active()
                || self.battery_view_is_active()
                || self.pd_power_view_is_active()
            {
                self.active_waveform_tab = Some(tab);
            } else {
                self.select_waveform_tab(tab);
            }
        } else {
            self.active_waveform_tab = None;
            if !self.system_trace_view_is_active()
                && !self.vna_view_is_active()
                && !self.serial_view_is_active()
                && !self.battery_view_is_active()
                && !self.pd_power_view_is_active()
            {
                match views[0] {
                    WorkspaceView::Waveform(tab) => self.select_waveform_tab(tab),
                    WorkspaceView::Vna => self.select_vna_view(),
                    WorkspaceView::SystemTrace => self.select_system_trace_view(),
                    WorkspaceView::Serial => self.select_serial_view(),
                    battery @ (WorkspaceView::Battery
                    | WorkspaceView::Battery2
                    | WorkspaceView::Battery3
                    | WorkspaceView::Battery4
                    | WorkspaceView::Battery5
                    | WorkspaceView::Battery6
                    | WorkspaceView::Battery7
                    | WorkspaceView::Battery8
                    | WorkspaceView::Battery9
                    | WorkspaceView::Battery10) => self.select_battery_view(battery),
                    power @ (WorkspaceView::PdPower
                    | WorkspaceView::PdPower2
                    | WorkspaceView::PdPower3
                    | WorkspaceView::PdPower4
                    | WorkspaceView::PdPower5) => self.select_pd_power_view(power),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{egui, CaptureSettingsDocument, CaptureSettingsRepository, SavedWindowSize};
    use std::fs;

    fn test_repository(test_name: &str) -> CaptureSettingsRepository {
        let directory = std::env::temp_dir().join(format!(
            "wavecraft-global-view-settings-{test_name}-{}",
            std::process::id()
        ));
        CaptureSettingsRepository::at(directory.join("capture-settings.json"))
    }

    fn remove_test_repository(repository: &CaptureSettingsRepository) {
        let _ = fs::remove_file(repository.path());
        if let Some(parent) = repository.path().parent() {
            let _ = fs::remove_dir(parent);
        }
    }

    #[test]
    fn setup_panel_visibility_persists_independently_for_every_workspace() {
        let repository = test_repository("setup-panel-visibility");
        remove_test_repository(&repository);
        let mut app = WaveCraftApp::with_settings(
            Some(repository.clone()),
            CaptureSettingsDocument::default(),
        );
        for view in WorkspaceView::ALL {
            app.active_workspace_view = Some(view);
            assert!(app.workspace_view_layout.setup_panel_open(view));
            app.workspace_view_layout.set_setup_panel_open(view, false);
            app.save_global_settings();
            let saved = repository.load().expect("layout is saved");
            let restored = WaveCraftApp::with_settings(None, saved);
            for candidate in WorkspaceView::ALL {
                assert_eq!(
                    restored.workspace_view_layout.setup_panel_open(candidate),
                    app.workspace_view_layout.setup_panel_open(candidate),
                    "after {view:?}: {candidate:?}; saved={:?}",
                    app.saved_capture_settings.global.collapsed_setup_views
                );
            }
        }
        app.workspace_view_layout
            .set_setup_panel_open(WorkspaceView::Serial, true);
        app.save_global_settings();
        let restored =
            WaveCraftApp::with_settings(None, repository.load().expect("layout is saved"));
        assert!(restored
            .workspace_view_layout
            .setup_panel_open(WorkspaceView::Serial));
        assert!(!restored
            .workspace_view_layout
            .setup_panel_open(WorkspaceView::Vna));
        remove_test_repository(&repository);
    }

    #[test]
    fn combined_save_persists_both_capture_profiles_and_preserves_other_views() {
        let repository = test_repository("combined-setup-save");
        remove_test_repository(&repository);
        let mut app = WaveCraftApp::with_settings(
            Some(repository.clone()),
            CaptureSettingsDocument::default(),
        );
        app.radio_settings.capture.center_frequency_hz = 123_000_000;
        app.save_setup_settings(WorkspaceView::Waveform(WaveformTab::RadioReceiver));
        let radio_profile = app.saved_capture_settings.profiles.radio_frequency.clone();
        app.logic_channel_names[0] = "Combined logic".into();
        app.scope_channel_names[0] = "Combined scope".into();
        app.save_setup_settings(WorkspaceView::Waveform(WaveformTab::Combined));
        assert!(app.last_error.is_none());
        assert_eq!(app.status, "Combined settings saved");
        let saved = repository.load().expect("combined settings are saved");
        assert_eq!(saved.profiles.radio_frequency, radio_profile);
        let restored = WaveCraftApp::with_settings(None, saved);
        assert_eq!(restored.logic_channel_names[0], "Combined logic");
        assert_eq!(restored.scope_channel_names[0], "Combined scope");
        remove_test_repository(&repository);
    }

    #[test]
    fn serial_setup_save_round_trips_settings_and_reports_success_or_failure() {
        let repository = test_repository("serial-setup-save");
        remove_test_repository(&repository);
        let mut app = WaveCraftApp::with_settings(
            Some(repository.clone()),
            CaptureSettingsDocument::default(),
        );
        app.serial.settings.baud = 230_400;
        app.save_setup_settings(WorkspaceView::Serial);
        assert!(app.last_error.is_none());
        assert_eq!(app.serial.status, "Serial settings saved");
        let restored = WaveCraftApp::with_settings(
            None,
            repository.load().expect("serial settings are saved"),
        );
        assert_eq!(restored.serial.settings.baud, 230_400);
        app.capture_settings_repository = None;
        app.serial.settings.baud = 115_200;
        app.save_setup_settings(WorkspaceView::Serial);
        assert!(app
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains("configuration directory")));
        assert_eq!(
            app.saved_capture_settings
                .global
                .serial
                .as_ref()
                .map(|settings| settings.baud),
            Some(230_400)
        );
        remove_test_repository(&repository);
    }

    #[test]
    fn saved_workspace_views_round_trip_without_gui_layout_details() {
        for view in [
            WorkspaceView::Waveform(WaveformTab::LogicAnalyzer),
            WorkspaceView::Waveform(WaveformTab::Oscilloscope),
            WorkspaceView::Waveform(WaveformTab::RadioReceiver),
            WorkspaceView::Waveform(WaveformTab::Combined),
            WorkspaceView::Vna,
            WorkspaceView::SystemTrace,
        ] {
            assert_eq!(WorkspaceView::from(SavedWorkspaceView::from(view)), view);
        }
    }

    #[test]
    fn selecting_a_view_persists_and_restores_the_global_setting() {
        let repository = test_repository("active-view");
        remove_test_repository(&repository);
        let mut app = WaveCraftApp::with_settings(
            Some(repository.clone()),
            CaptureSettingsDocument::default(),
        );

        app.select_waveform_tab(WaveformTab::Oscilloscope);
        app.scope_plot_tab = crate::ScopePlotTab::Lissajous;
        app.save_global_settings();

        let saved = repository
            .load()
            .expect("the selected view should be saved");
        assert_eq!(
            saved.global.last_active_view,
            Some(SavedWorkspaceView::Oscilloscope)
        );
        let mut restored = WaveCraftApp::with_settings(None, saved);
        restored.reconcile_waveform_tab();
        assert_eq!(
            restored.active_workspace_view,
            Some(WorkspaceView::Waveform(WaveformTab::Oscilloscope))
        );
        assert_eq!(
            restored.active_waveform_tab,
            Some(WaveformTab::Oscilloscope)
        );
        assert_eq!(restored.scope_plot_tab, crate::ScopePlotTab::Lissajous);
        assert!(restored
            .available_waveform_tabs()
            .contains(&WaveformTab::Oscilloscope));

        app.select_system_trace_view();

        let saved = repository
            .load()
            .expect("the system trace view should be saved");
        assert_eq!(
            saved.global.last_active_view,
            Some(SavedWorkspaceView::SystemTrace)
        );
        let restored = WaveCraftApp::with_settings(None, saved);
        assert_eq!(
            restored.active_workspace_view,
            Some(WorkspaceView::SystemTrace)
        );
        assert_eq!(restored.active_waveform_tab, None);

        app.select_vna_view();
        let saved = repository.load().expect("the VNA view should be saved");
        assert_eq!(saved.global.last_active_view, Some(SavedWorkspaceView::Vna));
        let mut restored = WaveCraftApp::with_settings(None, saved);
        restored.reconcile_waveform_tab();
        assert_eq!(restored.active_workspace_view, Some(WorkspaceView::Vna));
        assert_eq!(restored.active_waveform_tab, Some(WaveformTab::Combined));
        assert!(restored.vna_calibration_notice_open);
        remove_test_repository(&repository);
    }

    #[test]
    fn view_visibility_persists_and_hidden_active_view_is_replaced() {
        let repository = test_repository("view-visibility");
        remove_test_repository(&repository);
        let mut app = WaveCraftApp::with_settings(
            Some(repository.clone()),
            CaptureSettingsDocument::default(),
        );
        app.select_waveform_tab(WaveformTab::LogicAnalyzer);
        let logic = WorkspaceView::Waveform(WaveformTab::LogicAnalyzer);
        app.set_workspace_view_visible(logic, false);
        app.set_workspace_view_visible(WorkspaceView::Vna, false);
        assert_ne!(app.active_workspace_view, Some(logic));
        let saved = repository.load().expect("visibility saved");
        let mut restored = WaveCraftApp::with_settings(None, saved);
        restored.reconcile_waveform_tab();
        assert!(!restored.available_workspace_views().contains(&logic));
        assert!(!restored
            .available_workspace_views()
            .contains(&WorkspaceView::Vna));
        assert_eq!(restored.active_workspace_view, app.active_workspace_view);
        restored.set_workspace_view_visible(logic, true);
        assert!(restored.available_workspace_views().contains(&logic));
        assert_eq!(restored.workspace_view_layout.order(), &WorkspaceView::ALL);
        remove_test_repository(&repository);
    }

    #[test]
    fn closing_active_view_selects_its_visible_left_neighbor_in_tab_order() {
        use WorkspaceView::{Battery, PdPower, PdPower2, PdPower3, SystemTrace};
        let logic = WorkspaceView::Waveform(WaveformTab::LogicAnalyzer);
        for (visible, active, closed, expected) in [
            (
                vec![logic, SystemTrace, Battery, PdPower, PdPower2],
                PdPower2,
                PdPower2,
                PdPower,
            ),
            (
                vec![logic, SystemTrace, PdPower3],
                PdPower3,
                PdPower3,
                SystemTrace,
            ),
            (
                vec![PdPower3, Battery, SystemTrace, logic],
                SystemTrace,
                SystemTrace,
                Battery,
            ),
            (
                vec![SystemTrace, Battery, logic],
                SystemTrace,
                SystemTrace,
                Battery,
            ),
            (vec![logic, Battery, PdPower2], PdPower2, Battery, PdPower2),
            (vec![SystemTrace], SystemTrace, SystemTrace, SystemTrace),
        ] {
            let mut app = WaveCraftApp::with_settings(None, CaptureSettingsDocument::default());
            let mut order = visible.clone();
            order.extend(
                WorkspaceView::ALL
                    .into_iter()
                    .filter(|view| !visible.contains(view)),
            );
            assert!(app.workspace_view_layout.set_order(&order));
            for view in WorkspaceView::ALL {
                app.workspace_view_layout
                    .set_visible(view, visible.contains(&view));
            }
            app.select_workspace_view(active);

            app.set_workspace_view_visible(closed, false);

            assert_eq!(app.active_workspace_view, Some(expected), "{visible:?}");
            assert_eq!(
                app.workspace_view_layout.is_visible(closed),
                visible.len() == 1
            );
        }
    }

    #[test]
    fn opening_a_workspace_tab_appends_selects_and_persists_it() {
        let repository = test_repository("open-workspace-tab");
        remove_test_repository(&repository);
        let mut app = WaveCraftApp::with_settings(
            Some(repository.clone()),
            CaptureSettingsDocument::default(),
        );
        app.select_waveform_tab(WaveformTab::LogicAnalyzer);
        app.set_workspace_view_visible(WorkspaceView::SystemTrace, false);
        app.set_workspace_view_visible(WorkspaceView::Vna, false);
        app.open_workspace_view(WorkspaceView::SystemTrace);

        let saved = repository
            .load()
            .expect("the tab changes are saved immediately");
        let mut restored = WaveCraftApp::with_settings(None, saved);
        restored.reconcile_waveform_tab();

        assert_eq!(
            restored.active_workspace_view,
            Some(WorkspaceView::SystemTrace)
        );
        assert_eq!(
            restored.available_workspace_views(),
            app.available_workspace_views()
        );
        assert_eq!(
            restored.available_workspace_views().last(),
            Some(&WorkspaceView::SystemTrace)
        );
        assert!(!restored
            .available_workspace_views()
            .contains(&WorkspaceView::Vna));
        restored.open_workspace_view(WorkspaceView::SystemTrace);
        assert_eq!(
            restored.available_workspace_views(),
            app.available_workspace_views()
        );
        remove_test_repository(&repository);
    }

    #[test]
    fn last_available_view_cannot_be_hidden() {
        let mut app = WaveCraftApp::with_settings(None, CaptureSettingsDocument::default());
        app.select_system_trace_view();
        for view in WorkspaceView::ALL {
            app.set_workspace_view_visible(view, false);
        }
        assert_eq!(app.available_workspace_views().len(), 1);
        assert!(app
            .available_workspace_views()
            .contains(&app.active_workspace_view.unwrap()));
    }

    #[test]
    fn hidden_saved_active_view_and_all_hidden_settings_recover() {
        let mut saved = CaptureSettingsDocument::default();
        saved.global.last_active_view = Some(SavedWorkspaceView::Vna);
        saved.global.hidden_workspace_views = vec![SavedWorkspaceView::Vna];
        let mut app = WaveCraftApp::with_settings(None, saved.clone());
        app.reconcile_waveform_tab();
        assert!(!app.vna_view_is_active());
        saved.global.hidden_workspace_views = WorkspaceView::ALL
            .into_iter()
            .map(SavedWorkspaceView::from)
            .collect();
        let mut app = WaveCraftApp::with_settings(None, saved);
        app.reconcile_waveform_tab();
        assert_eq!(
            app.available_workspace_views(),
            vec![WorkspaceView::Waveform(WaveformTab::LogicAnalyzer)]
        );
        assert_eq!(app.active_waveform_view(), Some(WaveformTab::LogicAnalyzer));
    }

    #[test]
    fn entering_vna_opens_the_calibration_notice_once_per_entry() {
        let mut app = WaveCraftApp::with_settings(None, CaptureSettingsDocument::default());
        app.select_waveform_tab(WaveformTab::LogicAnalyzer);

        app.select_vna_view();
        assert!(app.vna_calibration_notice_open);

        app.vna_calibration_notice_open = false;
        app.select_vna_view();
        assert!(!app.vna_calibration_notice_open);

        app.select_waveform_tab(WaveformTab::Oscilloscope);
        app.select_vna_view();
        assert!(app.vna_calibration_notice_open);
    }

    #[test]
    fn system_trace_layout_persists_across_application_restart() {
        let repository = test_repository("system-trace-layout");
        remove_test_repository(&repository);
        let mut app = WaveCraftApp::with_settings(
            Some(repository.clone()),
            CaptureSettingsDocument::default(),
        );
        app.select_system_trace_view();
        let event_order = [
            TraceEventView::ProbeLog,
            TraceEventView::CpuLoad,
            TraceEventView::Events,
            TraceEventView::Logs,
            TraceEventView::ContextStatistics,
            TraceEventView::Terminal,
        ];
        let instrument_order = [TraceInstrument::Analysis, TraceInstrument::Timeline];
        assert!(app.system_trace.set_event_view_order(&event_order));
        assert!(app
            .system_trace
            .set_trace_instrument_order(&instrument_order));
        assert!(app.system_trace.set_trace_instrument_heights(420.0, 280.0));
        assert!(app.system_trace.set_timeline_label_width(240.0));

        app.save_global_settings();

        let saved = repository.load().expect("the trace layout should be saved");
        let restored = WaveCraftApp::with_settings(None, saved);
        assert_eq!(restored.system_trace.event_view_order(), &event_order);
        assert_eq!(
            restored.system_trace.trace_instrument_order(),
            &instrument_order
        );
        assert_eq!(restored.system_trace.timeline_view_height(), 420.0);
        assert_eq!(restored.system_trace.event_view_height(), 280.0);
        assert_eq!(restored.system_trace.timeline_label_width(), 240.0);
        remove_test_repository(&repository);
    }

    #[test]
    fn workspace_view_order_persists_across_application_restart() {
        let repository = test_repository("workspace-view-order");
        remove_test_repository(&repository);
        let mut app = WaveCraftApp::with_settings(
            Some(repository.clone()),
            CaptureSettingsDocument::default(),
        );
        app.select_waveform_tab(WaveformTab::LogicAnalyzer);
        app.workspace_view_layout
            .begin_drag(WorkspaceView::SystemTrace);
        app.workspace_view_layout
            .drag_to(WorkspaceView::Waveform(WaveformTab::Oscilloscope));
        app.workspace_view_layout.finish_drag();

        app.save_global_settings();

        let saved = repository
            .load()
            .expect("the workspace view order should be saved");
        let restored = WaveCraftApp::with_settings(None, saved);
        assert_eq!(
            restored.workspace_view_layout.order(),
            &[
                WorkspaceView::Waveform(WaveformTab::LogicAnalyzer),
                WorkspaceView::SystemTrace,
                WorkspaceView::Waveform(WaveformTab::Oscilloscope),
                WorkspaceView::Waveform(WaveformTab::RadioReceiver),
                WorkspaceView::Waveform(WaveformTab::Combined),
                WorkspaceView::Vna,
                WorkspaceView::Serial,
                WorkspaceView::Battery,
                WorkspaceView::Battery2,
                WorkspaceView::Battery3,
                WorkspaceView::Battery4,
                WorkspaceView::Battery5,
                WorkspaceView::Battery6,
                WorkspaceView::Battery7,
                WorkspaceView::Battery8,
                WorkspaceView::Battery9,
                WorkspaceView::Battery10,
                WorkspaceView::PdPower,
                WorkspaceView::PdPower2,
                WorkspaceView::PdPower3,
                WorkspaceView::PdPower4,
                WorkspaceView::PdPower5,
            ]
        );
        remove_test_repository(&repository);
    }

    #[test]
    fn window_size_persists_after_resize_and_restores_at_startup() {
        let repository = test_repository("window-size");
        remove_test_repository(&repository);
        let mut app = WaveCraftApp::with_settings(
            Some(repository.clone()),
            CaptureSettingsDocument::default(),
        );
        app.select_waveform_tab(WaveformTab::LogicAnalyzer);
        let now = Instant::now();
        let resized = SavedWindowSize {
            width: 1512,
            height: 982,
        };

        app.observe_window_size(resized, now);
        app.save_window_size_if_due(now + crate::WINDOW_SIZE_SAVE_DELAY);

        let saved = repository.load().expect("the window size should be saved");
        assert_eq!(saved.global.window_inner_size, Some(resized));
        assert_eq!(crate::initial_window_inner_size(&saved), [1512.0, 982.0]);
        let restored = WaveCraftApp::with_settings(None, saved);
        assert_eq!(restored.observed_window_size, Some(resized));
        remove_test_repository(&repository);
    }

    #[test]
    fn invalid_saved_window_size_uses_the_default() {
        let mut settings = CaptureSettingsDocument::default();
        settings.global.window_inner_size = Some(SavedWindowSize {
            width: 10,
            height: 20,
        });

        assert_eq!(
            crate::initial_window_inner_size(&settings),
            crate::DEFAULT_WINDOW_INNER_SIZE
        );
        let app = WaveCraftApp::with_settings(None, settings);
        assert_eq!(app.observed_window_size, None);
    }

    #[test]
    fn pending_window_size_is_saved_when_the_application_exits() {
        let repository = test_repository("window-size-on-exit");
        remove_test_repository(&repository);
        let mut app = WaveCraftApp::with_settings(
            Some(repository.clone()),
            CaptureSettingsDocument::default(),
        );
        app.select_system_trace_view();
        let resized = SavedWindowSize {
            width: 1366,
            height: 768,
        };
        app.observe_window_size(resized, Instant::now());

        app.save_pending_window_size();

        let saved = repository
            .load()
            .expect("the pending window size should be saved on exit");
        assert_eq!(saved.global.window_inner_size, Some(resized));
        assert_eq!(app.window_size_save_due, None);
        remove_test_repository(&repository);
    }

    #[test]
    fn vna_settings_and_calibration_persist_across_restart() {
        let repository = test_repository("vna-calibration");
        remove_test_repository(&repository);
        let calibration = r#"{
            "frequencies_hz":[10000000,20000000],
            "directivity":[{"real":0.0,"imaginary":0.0},{"real":0.0,"imaginary":0.0}],
            "source_match":[{"real":0.0,"imaginary":0.0},{"real":0.0,"imaginary":0.0}],
            "reflection_tracking":[{"real":1.0,"imaginary":0.0},{"real":1.0,"imaginary":0.0}]
        }"#;
        let mut values = BTreeMap::new();
        values.insert("vna.start_mhz".to_owned(), "10".to_owned());
        values.insert("vna.stop_mhz".to_owned(), "20".to_owned());
        values.insert("vna.points".to_owned(), "2".to_owned());
        values.insert("vna.averaging".to_owned(), "4".to_owned());
        values.insert("vna.matching_network".to_owned(), "pi".to_owned());
        values.insert("vna.matching_q".to_owned(), "1.5".to_owned());
        values.insert(
            "vna.calibration_name".to_owned(),
            "Bench antenna fixture".to_owned(),
        );
        values.insert("vna.calibration_averaging".to_owned(), "8".to_owned());
        values.insert(
            "vna.one_port_calibration".to_owned(),
            calibration.to_owned(),
        );
        let mut saved = CaptureSettingsDocument::default();
        saved.profiles.vector_network_analyzer = Some(CaptureSettingsProfile::new(values));
        let mut app = WaveCraftApp::with_settings(Some(repository.clone()), saved);

        app.save_capture_settings(SavedCaptureSettingsKind::VectorNetworkAnalyzer);

        let restored = WaveCraftApp::with_settings(
            None,
            repository.load().expect("the VNA profile should be saved"),
        );
        assert_eq!(restored.vna_settings.start_mhz, 10.0);
        assert_eq!(restored.vna_settings.stop_mhz, 20.0);
        assert_eq!(restored.vna_settings.points, 2);
        assert_eq!(restored.vna_settings.averaging, 4);
        assert_eq!(
            restored.vna_settings.matching_network,
            VnaMatchingNetwork::Pi
        );
        assert_eq!(restored.vna_settings.matching_q, 1.5);
        let restored_calibration = restored
            .vna_calibration
            .expect("the calibration should be restored");
        assert_eq!(restored_calibration.name, "Bench antenna fixture");
        assert_eq!(restored_calibration.averaging, 8);
        assert!(restored_calibration.calibration.matches_config(
            restored
                .vna_settings
                .sweep_config()
                .expect("the restored sweep should be valid")
        ));
        remove_test_repository(&repository);
    }

    #[test]
    fn window_size_is_read_from_the_native_viewport() {
        let context = egui::Context::default();
        let mut app = WaveCraftApp::with_settings(None, CaptureSettingsDocument::default());
        let mut input = egui::RawInput::default();
        let viewport = input.viewports.entry(input.viewport_id).or_default();
        viewport.inner_rect = Some(egui::Rect::from_min_size(
            egui::pos2(40.0, 30.0),
            egui::vec2(1600.0, 1000.0),
        ));

        let _ = context.run(input, |context| app.track_window_size(context));

        assert_eq!(
            app.observed_window_size,
            Some(SavedWindowSize {
                width: 1600,
                height: 1000,
            })
        );
        assert!(app.window_size_save_due.is_some());
    }

    #[test]
    fn legacy_lissajous_view_migrates_into_scope_without_losing_view_order() {
        let mut saved = CaptureSettingsDocument::default();
        saved.global.last_active_view = Some(SavedWorkspaceView::Lissajous);
        saved.global.hidden_workspace_views =
            vec![SavedWorkspaceView::Lissajous, SavedWorkspaceView::Vna];
        saved.global.workspace_view_order = Some(vec![
            SavedWorkspaceView::SystemTrace,
            SavedWorkspaceView::Lissajous,
            SavedWorkspaceView::Oscilloscope,
            SavedWorkspaceView::LogicAnalyzer,
            SavedWorkspaceView::Combined,
            SavedWorkspaceView::RadioSpectrum,
            SavedWorkspaceView::Vna,
        ]);
        let mut app = WaveCraftApp::with_settings(None, saved);
        app.reconcile_waveform_tab();
        assert_eq!(app.active_waveform_view(), Some(WaveformTab::Oscilloscope));
        assert_eq!(app.scope_plot_tab, crate::ScopePlotTab::Lissajous);
        assert_eq!(
            app.workspace_view_layout.order()[0],
            WorkspaceView::SystemTrace
        );
        assert_eq!(
            app.workspace_view_layout.order()[1],
            WorkspaceView::Waveform(WaveformTab::Oscilloscope)
        );
        assert!(app
            .available_workspace_views()
            .contains(&WorkspaceView::Waveform(WaveformTab::Oscilloscope)));
        assert!(!app
            .available_workspace_views()
            .contains(&WorkspaceView::Vna));
        app.save_global_settings();
        assert_eq!(
            app.saved_capture_settings.global.last_active_view,
            Some(SavedWorkspaceView::Oscilloscope)
        );
        assert_eq!(
            app.saved_capture_settings.global.scope_plot_tab,
            crate::ScopePlotTab::Lissajous
        );
    }

    #[test]
    fn invalid_saved_workspace_view_order_falls_back_to_default() {
        let mut settings = CaptureSettingsDocument::default();
        settings.global.workspace_view_order = Some(vec![
            SavedWorkspaceView::SystemTrace,
            SavedWorkspaceView::SystemTrace,
            SavedWorkspaceView::LogicAnalyzer,
            SavedWorkspaceView::Oscilloscope,
            SavedWorkspaceView::Lissajous,
            SavedWorkspaceView::RadioSpectrum,
        ]);

        let app = WaveCraftApp::with_settings(None, settings);

        assert_eq!(app.workspace_view_layout.order(), &WorkspaceView::ALL);
    }
}
