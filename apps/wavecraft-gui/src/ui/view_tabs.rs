//! Which view tab is active and which capture settings tab it uses.

use crate::CaptureTarget;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CaptureSettingsTab {
    DsLogic,
    DsCope,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SavedCaptureSettingsKind {
    LogicAnalyzer,
    Oscilloscope,
    RadioFrequency,
}

impl SavedCaptureSettingsKind {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::LogicAnalyzer => "LA",
            Self::Oscilloscope => "OSC",
            Self::RadioFrequency => "RF",
        }
    }
}

impl CaptureSettingsTab {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::DsLogic => "DSLogic",
            Self::DsCope => "DSCope",
        }
    }
}

pub(crate) fn capture_settings_tabs(
    has_logic_analyzer: bool,
    has_oscilloscope: bool,
) -> Vec<CaptureSettingsTab> {
    let mut tabs = Vec::with_capacity(2);
    if has_logic_analyzer {
        tabs.push(CaptureSettingsTab::DsLogic);
    }
    if has_oscilloscope {
        tabs.push(CaptureSettingsTab::DsCope);
    }
    tabs
}

pub(crate) fn capture_settings_tabs_for_view(
    active_waveform_tab: Option<WaveformTab>,
    has_logic_device: bool,
    has_scope_device: bool,
    has_logic_capture: bool,
    has_scope_capture: bool,
) -> Vec<CaptureSettingsTab> {
    let logic_available = has_logic_device || has_logic_capture;
    let scope_available = has_scope_device || has_scope_capture;
    match active_waveform_tab {
        Some(WaveformTab::LogicAnalyzer) => capture_settings_tabs(logic_available, false),
        Some(WaveformTab::Oscilloscope | WaveformTab::Lissajous) => {
            capture_settings_tabs(false, scope_available)
        }
        Some(WaveformTab::RadioReceiver) => Vec::new(),
        Some(WaveformTab::Combined) | None => {
            capture_settings_tabs(logic_available, scope_available)
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WaveformTab {
    LogicAnalyzer,
    Oscilloscope,
    Lissajous,
    RadioReceiver,
    Combined,
}

impl WaveformTab {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::LogicAnalyzer => "Logic Analyzer",
            Self::Oscilloscope => "Oscilloscope",
            Self::Lissajous => "Lissajous",
            Self::RadioReceiver => "RF Spectrum",
            Self::Combined => "Combined",
        }
    }

    pub(crate) fn capture_target(self) -> CaptureTarget {
        match self {
            Self::LogicAnalyzer => CaptureTarget::LogicAnalyzer,
            Self::Oscilloscope | Self::Lissajous => CaptureTarget::Oscilloscope,
            Self::RadioReceiver => CaptureTarget::RadioReceiver,
            Self::Combined => CaptureTarget::Combined,
        }
    }

    pub(crate) fn capture_settings_tab(self) -> Option<CaptureSettingsTab> {
        match self {
            Self::LogicAnalyzer => Some(CaptureSettingsTab::DsLogic),
            Self::Oscilloscope | Self::Lissajous => Some(CaptureSettingsTab::DsCope),
            Self::RadioReceiver | Self::Combined => None,
        }
    }
}

pub(crate) fn waveform_tabs(
    has_logic_analyzer: bool,
    has_oscilloscope: bool,
    has_radio_receiver: bool,
    show_lissajous: bool,
) -> Vec<WaveformTab> {
    let mut tabs = Vec::with_capacity(5);
    if has_logic_analyzer {
        tabs.push(WaveformTab::LogicAnalyzer);
    }
    if has_oscilloscope {
        tabs.push(WaveformTab::Oscilloscope);
    }
    if has_oscilloscope && show_lissajous {
        tabs.push(WaveformTab::Lissajous);
    }
    if has_radio_receiver {
        tabs.push(WaveformTab::RadioReceiver);
    }
    if has_logic_analyzer && has_oscilloscope {
        tabs.push(WaveformTab::Combined);
    }
    tabs
}

pub(crate) fn scope_tools_panel_is_visible(active_tab: Option<WaveformTab>) -> bool {
    matches!(
        active_tab,
        Some(WaveformTab::Oscilloscope) | Some(WaveformTab::Lissajous)
    )
}
