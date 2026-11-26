//! Which view tab is active and which capture settings tab it uses.

use crate::CaptureTarget;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WorkspaceView {
    Waveform(WaveformTab),
    Vna,
    SystemTrace,
}

impl WorkspaceView {
    pub(crate) const ALL: [Self; 6] = [
        Self::Waveform(WaveformTab::LogicAnalyzer),
        Self::Waveform(WaveformTab::Oscilloscope),
        Self::Waveform(WaveformTab::RadioReceiver),
        Self::Waveform(WaveformTab::Combined),
        Self::Vna,
        Self::SystemTrace,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Waveform(tab) => tab.label(),
            Self::Vna => "VNA",
            Self::SystemTrace => "System Trace",
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct WorkspaceViewLayout {
    order: [WorkspaceView; WorkspaceView::ALL.len()],
    hidden_views: Vec<WorkspaceView>,
    dragged_view: Option<WorkspaceView>,
    layout_changed: bool,
}

impl Default for WorkspaceViewLayout {
    fn default() -> Self {
        Self {
            order: WorkspaceView::ALL,
            hidden_views: Vec::new(),
            dragged_view: None,
            layout_changed: false,
        }
    }
}

impl WorkspaceViewLayout {
    pub(crate) fn order(&self) -> &[WorkspaceView; WorkspaceView::ALL.len()] {
        &self.order
    }

    pub(crate) fn set_order(&mut self, order: &[WorkspaceView]) -> bool {
        if order.len() != WorkspaceView::ALL.len()
            || !WorkspaceView::ALL
                .iter()
                .all(|view| order.iter().filter(|candidate| *candidate == view).count() == 1)
        {
            return false;
        }
        self.order.copy_from_slice(order);
        true
    }

    pub(crate) fn is_visible(&self, view: WorkspaceView) -> bool {
        !self.hidden_views.contains(&view)
    }

    pub(crate) fn set_visible(&mut self, view: WorkspaceView, visible: bool) {
        self.hidden_views.retain(|hidden| *hidden != view);
        if !visible {
            self.hidden_views.push(view);
        }
        self.finish_drag();
    }

    pub(crate) fn begin_drag(&mut self, view: WorkspaceView) {
        self.dragged_view = Some(view);
    }

    pub(crate) fn dragged_view(&self) -> Option<WorkspaceView> {
        self.dragged_view
    }

    pub(crate) fn drag_to(&mut self, target: WorkspaceView) {
        let Some(dragged) = self.dragged_view else {
            return;
        };
        let Some(from) = self.order.iter().position(|view| *view == dragged) else {
            return;
        };
        let Some(to) = self.order.iter().position(|view| *view == target) else {
            return;
        };
        if from == to {
            return;
        }
        if from < to {
            self.order[from..=to].rotate_left(1);
        } else {
            self.order[to..=from].rotate_right(1);
        }
        self.layout_changed = true;
    }

    pub(crate) fn finish_drag(&mut self) {
        self.dragged_view = None;
    }

    pub(crate) fn take_layout_changed(&mut self) -> bool {
        if self.dragged_view.is_some() {
            return false;
        }
        std::mem::take(&mut self.layout_changed)
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
    VectorNetworkAnalyzer,
    SystemTrace,
}

impl SavedCaptureSettingsKind {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::LogicAnalyzer => "LA",
            Self::Oscilloscope => "OSC",
            Self::RadioFrequency => "RF",
            Self::VectorNetworkAnalyzer => "VNA",
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
        Some(WaveformTab::Oscilloscope) => capture_settings_tabs(false, scope_available),
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
    RadioReceiver,
    Combined,
}

impl WaveformTab {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::LogicAnalyzer => "Logic Analyzer",
            Self::Oscilloscope => "Oscilloscope",
            Self::RadioReceiver => "RF Spectrum",
            Self::Combined => "Combined",
        }
    }

    pub(crate) fn capture_target(self) -> CaptureTarget {
        match self {
            Self::LogicAnalyzer => CaptureTarget::LogicAnalyzer,
            Self::Oscilloscope => CaptureTarget::Oscilloscope,
            Self::RadioReceiver => CaptureTarget::RadioReceiver,
            Self::Combined => CaptureTarget::Combined,
        }
    }

    pub(crate) fn capture_settings_tab(self) -> Option<CaptureSettingsTab> {
        match self {
            Self::LogicAnalyzer => Some(CaptureSettingsTab::DsLogic),
            Self::Oscilloscope => Some(CaptureSettingsTab::DsCope),
            Self::RadioReceiver | Self::Combined => None,
        }
    }
}

pub(crate) fn waveform_tabs() -> Vec<WaveformTab> {
    vec![
        WaveformTab::LogicAnalyzer,
        WaveformTab::Oscilloscope,
        WaveformTab::RadioReceiver,
        WaveformTab::Combined,
    ]
}

pub(crate) fn workspace_views(waveform_tabs: &[WaveformTab]) -> Vec<WorkspaceView> {
    waveform_tabs
        .iter()
        .copied()
        .map(WorkspaceView::Waveform)
        .chain([WorkspaceView::Vna, WorkspaceView::SystemTrace])
        .collect()
}

pub(crate) fn scope_tools_panel_is_visible(active_tab: Option<WaveformTab>) -> bool {
    matches!(active_tab, Some(WaveformTab::Oscilloscope))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waveform_tabs_do_not_require_connected_devices() {
        assert_eq!(
            waveform_tabs(),
            vec![
                WaveformTab::LogicAnalyzer,
                WaveformTab::Oscilloscope,
                WaveformTab::RadioReceiver,
                WaveformTab::Combined,
            ]
        );
    }

    #[test]
    fn system_trace_is_a_workspace_view_not_a_waveform() {
        assert_eq!(
            workspace_views(&[]),
            vec![WorkspaceView::Vna, WorkspaceView::SystemTrace]
        );
        assert_eq!(
            workspace_views(&[WaveformTab::LogicAnalyzer]),
            vec![
                WorkspaceView::Waveform(WaveformTab::LogicAnalyzer),
                WorkspaceView::Vna,
                WorkspaceView::SystemTrace,
            ]
        );
    }

    #[test]
    fn workspace_view_layout_reorders_and_reports_completed_drags() {
        let mut layout = WorkspaceViewLayout::default();
        layout.begin_drag(WorkspaceView::SystemTrace);
        layout.drag_to(WorkspaceView::Waveform(WaveformTab::Oscilloscope));

        assert_eq!(
            layout.order(),
            &[
                WorkspaceView::Waveform(WaveformTab::LogicAnalyzer),
                WorkspaceView::SystemTrace,
                WorkspaceView::Waveform(WaveformTab::Oscilloscope),
                WorkspaceView::Waveform(WaveformTab::RadioReceiver),
                WorkspaceView::Waveform(WaveformTab::Combined),
                WorkspaceView::Vna,
            ]
        );
        assert!(!layout.take_layout_changed());
        layout.finish_drag();
        assert!(layout.take_layout_changed());
        assert!(!layout.take_layout_changed());
    }

    #[test]
    fn workspace_view_layout_rejects_incomplete_or_duplicate_orders() {
        let mut layout = WorkspaceViewLayout::default();
        assert!(!layout.set_order(&WorkspaceView::ALL[..5]));
        assert!(!layout.set_order(&[
            WorkspaceView::SystemTrace,
            WorkspaceView::SystemTrace,
            WorkspaceView::Waveform(WaveformTab::LogicAnalyzer),
            WorkspaceView::Waveform(WaveformTab::Oscilloscope),
            WorkspaceView::Waveform(WaveformTab::RadioReceiver),
        ]));
        assert_eq!(layout.order(), &WorkspaceView::ALL);
    }

    #[test]
    fn scope_tools_panel_is_limited_to_scope_views() {
        assert!(scope_tools_panel_is_visible(Some(
            WaveformTab::Oscilloscope
        )));
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
