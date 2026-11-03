use crate::CaptureTarget;
use wavecraft_usb::{DeviceModel, DiscoveredDevice, LinkStatus};

pub(crate) struct DeviceEntry {
    pub(crate) device: DiscoveredDevice,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DeviceRole {
    LogicAnalyzer,
    Oscilloscope,
}

impl DeviceRole {
    pub(crate) fn matches(self, device: &DiscoveredDevice) -> bool {
        if device.link_status() == LinkStatus::Unsupported
            || !device.profile.model.supports_capture()
        {
            return false;
        }
        match self {
            Self::LogicAnalyzer => device.profile.model.is_logic_analyzer(),
            Self::Oscilloscope => device.profile.model == DeviceModel::DsCopeU3P100,
        }
    }

    pub(crate) fn empty_label(self) -> &'static str {
        match self {
            Self::LogicAnalyzer => "No DSLogic device available",
            Self::Oscilloscope => "No DSCope device available",
        }
    }
}

pub(crate) fn selected_device_for_role<'a>(
    devices: &'a [DeviceEntry],
    selected_id: Option<&str>,
    role: DeviceRole,
) -> Option<&'a DiscoveredDevice> {
    let selected_id = selected_id?;
    devices
        .iter()
        .map(|entry| &entry.device)
        .find(|device| role.matches(device) && device.location.stable_id() == selected_id)
}

pub(crate) fn first_device_id_for_role(
    devices: &[DeviceEntry],
    role: DeviceRole,
) -> Option<String> {
    devices
        .iter()
        .map(|entry| &entry.device)
        .find(|device| role.matches(device))
        .map(|device| device.location.stable_id())
}

pub(crate) fn capture_devices_for_target(
    devices: &[DeviceEntry],
    selected_logic_device_id: Option<&str>,
    selected_scope_device_id: Option<&str>,
    target: CaptureTarget,
) -> Result<Vec<DiscoveredDevice>, String> {
    let logic = || {
        selected_device_for_role(devices, selected_logic_device_id, DeviceRole::LogicAnalyzer)
            .cloned()
            .ok_or_else(|| "Select a connected DSLogic instrument in Capture Setup".to_owned())
    };
    let scope = || {
        selected_device_for_role(devices, selected_scope_device_id, DeviceRole::Oscilloscope)
            .cloned()
            .ok_or_else(|| "Select a connected DSCope U3P100 in Capture Setup".to_owned())
    };
    match target {
        CaptureTarget::LogicAnalyzer => Ok(vec![logic()?]),
        CaptureTarget::Oscilloscope => Ok(vec![scope()?]),
        CaptureTarget::Combined => Ok(vec![logic()?, scope()?]),
    }
}

pub(crate) struct UsbSpeedWarning {
    pub(crate) devices: Vec<DiscoveredDevice>,
}

pub(crate) fn usb_speed_warning(devices: &[DiscoveredDevice]) -> Option<UsbSpeedWarning> {
    let devices = devices
        .iter()
        .filter(|device| device.link_status() == LinkStatus::ReducedPerformance)
        .cloned()
        .collect::<Vec<_>>();
    (!devices.is_empty()).then_some(UsbSpeedWarning { devices })
}

#[cfg(test)]
mod tests {
    use super::*;
    use wavecraft_usb::UsbSpeedClass;

    #[test]
    fn identical_devices_are_selected_by_connection_id() {
        let entry = |profile, platform_path: &str| DeviceEntry {
            device: DiscoveredDevice {
                profile,
                location: wavecraft_usb::DeviceLocation {
                    platform_path: platform_path.to_owned(),
                },
                speed: UsbSpeedClass::HighSpeed,
            },
        };
        let devices = vec![
            entry(&wavecraft_usb::DSCOPE_U3P100, "usb:1-2.1"),
            entry(&wavecraft_usb::DSCOPE_U3P100, "usb:1-2.2"),
            entry(&wavecraft_usb::DSLOGIC_U3PRO16, "usb:1-3"),
        ];
        let selected_scope_id = devices[1].device.location.stable_id();
        let selected_logic_id = devices[2].device.location.stable_id();
        let selected_scope =
            selected_device_for_role(&devices, Some(&selected_scope_id), DeviceRole::Oscilloscope)
                .expect("the exact scope connection should resolve");

        assert_eq!(
            selected_scope.location.platform_path,
            devices[1].device.location.platform_path
        );
        assert_eq!(
            first_device_id_for_role(&devices, DeviceRole::Oscilloscope),
            Some(devices[0].device.location.stable_id())
        );
        assert!(selected_device_for_role(
            &devices,
            Some(&selected_scope_id),
            DeviceRole::LogicAnalyzer
        )
        .is_none());

        let scope_capture = capture_devices_for_target(
            &devices,
            Some(&selected_logic_id),
            Some(&selected_scope_id),
            CaptureTarget::Oscilloscope,
        )
        .unwrap();
        assert_eq!(scope_capture.len(), 1);
        assert_eq!(scope_capture[0].location.stable_id(), selected_scope_id);

        let combined_capture = capture_devices_for_target(
            &devices,
            Some(&selected_logic_id),
            Some(&selected_scope_id),
            CaptureTarget::Combined,
        )
        .unwrap();
        assert_eq!(combined_capture.len(), 2);
        assert_eq!(combined_capture[0].location.stable_id(), selected_logic_id);
        assert_eq!(combined_capture[1].location.stable_id(), selected_scope_id);
    }

    #[test]
    fn usb_speed_warning_only_lists_reduced_performance_links() {
        let device = |platform_path: &str, speed| DiscoveredDevice {
            profile: &wavecraft_usb::DSLOGIC_U3PRO16,
            location: wavecraft_usb::DeviceLocation {
                platform_path: platform_path.to_owned(),
            },
            speed,
        };
        let devices = [
            device("usb-2", UsbSpeedClass::HighSpeed),
            device("usb-3", UsbSpeedClass::SuperSpeed),
            device("unknown", UsbSpeedClass::Unknown),
        ];

        let warning = usb_speed_warning(&devices).unwrap();

        assert_eq!(warning.devices.len(), 1);
        assert_eq!(warning.devices[0].speed, UsbSpeedClass::HighSpeed);
        assert!(usb_speed_warning(&devices[1..2]).is_none());
    }
}
