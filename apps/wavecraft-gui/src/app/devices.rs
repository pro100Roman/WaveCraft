//! Device refresh and the selection of the logic analyzer, oscilloscope, and radio.

use crate::{
    discover_debug_probes, discover_supported_devices, first_device_id_for_role, first_radio_id,
    mpsc, normalize_radio_settings, radio_registry, selected_device_for_role, selected_radio,
    thread, DeviceRole, DiscoveredDevice, WorkerSuccess,
};

use crate::WaveCraftApp;

impl WaveCraftApp {
    pub(crate) fn begin_refresh(&mut self) {
        if self.pending.is_some() || !self.captures.is_empty() {
            return;
        }
        let (sender, receiver) = mpsc::channel();
        self.requested_capture_target = None;
        self.capture_requests.clear();
        self.queued_capture_rearms.clear();
        self.scheduled_repetitive_capture = None;
        self.scope_capture_restart_requested = false;
        self.scope_configuration_due = None;
        self.pending = Some(receiver);
        self.reset_session_captures();
        self.logic_marker_settings.clear();
        self.status = "Discovering instruments…".to_owned();
        self.last_error = None;
        thread::spawn(move || {
            let radio = radio_registry().discover();
            let debug_probes = discover_debug_probes();
            let result = discover_supported_devices()
                .map(|report| WorkerSuccess::Devices {
                    usb: report.devices,
                    radio,
                    debug_probes,
                })
                .map_err(|error| error.to_string());
            let _ = sender.send(result);
        });
    }

    pub(crate) fn reconcile_device_selections(&mut self) {
        if self.selected_logic_device_id.is_none() {
            self.selected_logic_device_id =
                first_device_id_for_role(&self.devices, DeviceRole::LogicAnalyzer);
        }
        if self.selected_scope_device_id.is_none() {
            self.selected_scope_device_id =
                first_device_id_for_role(&self.devices, DeviceRole::Oscilloscope);
        }
        if self.selected_radio_id.is_none() {
            self.selected_radio_id = first_radio_id(&self.radios);
        }
    }

    pub(crate) fn selected_logic_device(&self) -> Option<&DiscoveredDevice> {
        selected_device_for_role(
            &self.devices,
            self.selected_logic_device_id.as_deref(),
            DeviceRole::LogicAnalyzer,
        )
    }

    pub(crate) fn selected_scope_device_ref(&self) -> Option<&DiscoveredDevice> {
        selected_device_for_role(
            &self.devices,
            self.selected_scope_device_id.as_deref(),
            DeviceRole::Oscilloscope,
        )
    }

    pub(crate) fn reconcile_radio_settings(&mut self) {
        let Some(radio) = selected_radio(&self.radios, self.selected_radio_id.as_deref()) else {
            return;
        };
        normalize_radio_settings(&mut self.radio_settings, &radio.capabilities);
    }

    pub(crate) fn selected_scope_device_for_operation(&mut self) -> Option<DiscoveredDevice> {
        let device = self.selected_scope_device_ref().cloned();
        if device.is_none() {
            self.last_error = Some("Select a connected DSCope U3P100 in Capture Setup".to_owned());
        }
        device
    }
}
