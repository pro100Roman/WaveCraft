//! Which view tab is active and which capture settings tab it uses.

use crate::CaptureTarget;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WorkspaceView {
    Waveform(WaveformTab),
    SystemTrace,
}

impl WorkspaceView {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Waveform(tab) => tab.label(),
            Self::SystemTrace => "System Trace",
        }
    }
}

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
    SystemTrace,
}

impl SavedCaptureSettingsKind {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::LogicAnalyzer => "LA",
            Self::Oscilloscope => "OSC",
            Self::RadioFrequency => "RF",
            Self::SystemTrace => "TRACE",
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

pub(crate) fn workspace_views(waveform_tabs: &[WaveformTab]) -> Vec<WorkspaceView> {
    waveform_tabs
        .iter()
        .copied()
        .map(WorkspaceView::Waveform)
        .chain(std::iter::once(WorkspaceView::SystemTrace))
        .collect()
}

pub(crate) fn scope_tools_panel_is_visible(active_tab: Option<WaveformTab>) -> bool {
    matches!(
        active_tab,
        Some(WaveformTab::Oscilloscope) | Some(WaveformTab::Lissajous)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waveform_tabs_require_their_connected_devices() {
        assert!(waveform_tabs(false, false, false, true).is_empty());
        assert_eq!(
            waveform_tabs(true, false, false, true),
            vec![WaveformTab::LogicAnalyzer]
        );
        assert_eq!(
            waveform_tabs(false, true, false, false),
            vec![WaveformTab::Oscilloscope]
        );
        assert_eq!(
            waveform_tabs(false, true, false, true),
            vec![WaveformTab::Oscilloscope, WaveformTab::Lissajous]
        );
        assert_eq!(
            waveform_tabs(true, true, false, false),
            vec![
                WaveformTab::LogicAnalyzer,
                WaveformTab::Oscilloscope,
                WaveformTab::Combined,
            ]
        );
        assert_eq!(
            waveform_tabs(true, true, false, true),
            vec![
                WaveformTab::LogicAnalyzer,
                WaveformTab::Oscilloscope,
                WaveformTab::Lissajous,
                WaveformTab::Combined,
            ]
        );
        assert_eq!(
            waveform_tabs(false, false, true, false),
            vec![WaveformTab::RadioReceiver]
        );
    }

    #[test]
    fn system_trace_is_a_workspace_view_not_a_waveform() {
        assert_eq!(workspace_views(&[]), vec![WorkspaceView::SystemTrace]);
        assert_eq!(
            workspace_views(&[WaveformTab::LogicAnalyzer]),
            vec![
                WorkspaceView::Waveform(WaveformTab::LogicAnalyzer),
                WorkspaceView::SystemTrace,
            ]
        );
    }

    #[test]
    fn scope_tools_panel_is_limited_to_scope_views() {
        assert!(scope_tools_panel_is_visible(Some(
            WaveformTab::Oscilloscope
        )));
        assert!(scope_tools_panel_is_visible(Some(WaveformTab::Lissajous)));
        assert!(!scope_tools_panel_is_visible(Some(
            WaveformTab::LogicAnalyzer
        )));
        assert!(!scope_tools_panel_is_visible(Some(WaveformTab::Combined)));
        assert!(!scope_tools_panel_is_visible(None));
    }

    #[test]
    fn capture_setup_tabs_follow_connected_devices() {
        assert!(capture_settings_tabs(false, false).is_empty());
        assert_eq!(
            capture_settings_tabs(true, false),
            vec![CaptureSettingsTab::DsLogic]
        );
        assert_eq!(
            capture_settings_tabs(false, true),
            vec![CaptureSettingsTab::DsCope]
        );
        let tabs = capture_settings_tabs(true, true);
        assert_eq!(
            tabs,
            vec![CaptureSettingsTab::DsLogic, CaptureSettingsTab::DsCope]
        );
        assert_eq!(
            tabs.iter()
                .copied()
                .map(CaptureSettingsTab::label)
                .collect::<Vec<_>>(),
            ["DSLogic", "DSCope"]
        );
    }

    #[test]
    fn capture_setup_stays_available_for_offline_capture_analysis() {
        assert_eq!(
            capture_settings_tabs_for_view(
                Some(WaveformTab::LogicAnalyzer),
                false,
                false,
                true,
                false,
            ),
            vec![CaptureSettingsTab::DsLogic]
        );
        assert_eq!(
            capture_settings_tabs_for_view(
                Some(WaveformTab::Oscilloscope),
                false,
                false,
                false,
                true,
            ),
            vec![CaptureSettingsTab::DsCope]
        );
        assert_eq!(
            capture_settings_tabs_for_view(Some(WaveformTab::Combined), false, false, true, true,),
            vec![CaptureSettingsTab::DsLogic, CaptureSettingsTab::DsCope]
        );
    }
}
