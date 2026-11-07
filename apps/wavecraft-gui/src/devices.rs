use crate::{Path, PathBuf, REQUIRED_RESOURCE_FILES};

use crate::CaptureTarget;
use wavecraft_sdr::DiscoveredRadio;
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

pub(crate) fn selected_radio<'a>(
    radios: &'a [DiscoveredRadio],
    selected_id: Option<&str>,
) -> Option<&'a DiscoveredRadio> {
    let selected_id = selected_id?;
    radios
        .iter()
        .find(|radio| radio.descriptor.id.as_str() == selected_id)
}

pub(crate) fn first_radio_id(radios: &[DiscoveredRadio]) -> Option<String> {
    radios
        .first()
        .map(|radio| radio.descriptor.id.as_str().to_owned())
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
        CaptureTarget::RadioReceiver => Ok(Vec::new()),
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

pub(crate) fn default_resource_directory() -> String {
    let mut candidates = Vec::new();
    if let Ok(executable) = std::env::current_exe() {
        if let Some(executable_directory) = executable.parent() {
            #[cfg(target_os = "macos")]
            if executable_directory
                .file_name()
                .and_then(|name| name.to_str())
                == Some("MacOS")
            {
                if let Some(contents_directory) = executable_directory.parent() {
                    candidates.push(contents_directory.join("Resources/fpga"));
                }
            }
            candidates.push(executable_directory.join("resources/fpga"));
        }
    }
    candidates.push(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("resources/fpga"),
    );
    candidates.push(PathBuf::from("resources/fpga"));
    let selected = candidates
        .into_iter()
        .find(|candidate| has_required_fpga_resources(candidate))
        .unwrap_or_else(|| PathBuf::from("resources/fpga"));
    selected
        .canonicalize()
        .unwrap_or(selected)
        .to_string_lossy()
        .into_owned()
}

pub(crate) fn has_required_fpga_resources(directory: &Path) -> bool {
    REQUIRED_RESOURCE_FILES
        .iter()
        .all(|name| directory.join(name).is_file())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::*;

    use wavecraft_core::{DeviceDescriptor, DeviceId, InstrumentKind};

    use wavecraft_sdr::{
        ConnectionBackend, RadioCapabilities, RadioModel, RadioProvider, ValueDomain,
    };
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
    fn identical_radios_are_selected_by_connection_id() {
        let radio = |connection_id: &str, persistent_id: &str| DiscoveredRadio {
            descriptor: DeviceDescriptor {
                id: DeviceId::new(connection_id),
                name: "HackRF One".to_owned(),
                kind: InstrumentKind::RadioReceiver,
            },
            provider: RadioProvider::HackRf,
            model: RadioModel::HackRfOne,
            connection_backend: ConnectionBackend::Usb,
            persistent_id: Some(persistent_id.to_owned()),
            capabilities: RadioCapabilities {
                frequency_hz: ValueDomain::Stepped {
                    range: 1_000_000..=6_000_000_000,
                    step: 1,
                },
                sample_rates_hz: ValueDomain::Stepped {
                    range: 2_000_000..=20_000_000,
                    step: 1,
                },
                bandwidths_hz: ValueDomain::Discrete(vec![1_750_000]),
                receive_channels: 1,
                gain_controls: Vec::new(),
                supports_hardware_timestamps: false,
                supports_external_clock: true,
            },
        };
        let radios = vec![
            radio("hackrf:serial-a", "serial-a"),
            radio("hackrf:serial-b", "serial-b"),
        ];

        assert_eq!(first_radio_id(&radios).as_deref(), Some("hackrf:serial-a"));
        assert_eq!(
            selected_radio(&radios, Some("hackrf:serial-b"))
                .expect("the exact HackRF connection should resolve")
                .persistent_id
                .as_deref(),
            Some("serial-b")
        );
        assert!(selected_radio(&radios, Some("hackrf:missing")).is_none());
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

    #[test]
    fn default_resource_directory_contains_complete_upstream_resource_set() {
        let directory = PathBuf::from(default_resource_directory());
        assert!(has_required_fpga_resources(&directory));
    }
}
