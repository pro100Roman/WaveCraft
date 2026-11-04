//! Device-independent SDR discovery, capabilities, and configuration contracts.

use std::collections::HashSet;
use std::ops::RangeInclusive;
use wavecraft_core::{CaptureError, CaptureSource, DeviceDescriptor, InstrumentKind};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum RadioProvider {
    Simulator,
    HackRf,
    LimeSuiteNg,
    Libiio,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum RadioModel {
    Simulated,
    HackRfOne,
    LimeSdrUsbTypeA,
    AntSdrE200,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ConnectionBackend {
    Usb,
    Network,
    Simulator,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ValueDomain {
    Discrete(Vec<u64>),
    Stepped {
        range: RangeInclusive<u64>,
        step: u64,
    },
}

impl ValueDomain {
    pub fn contains(&self, value: u64) -> bool {
        match self {
            Self::Discrete(values) => values.contains(&value),
            Self::Stepped { range, step } => {
                range.contains(&value)
                    && *step != 0
                    && value.saturating_sub(*range.start()) % *step == 0
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GainControlDescriptor {
    pub id: String,
    pub label: String,
    pub help: Option<String>,
    pub range_db: RangeInclusive<i32>,
    pub step_db: u32,
    pub default_db: i32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RadioCapabilities {
    pub frequency_hz: ValueDomain,
    pub sample_rates_hz: ValueDomain,
    pub bandwidths_hz: ValueDomain,
    pub receive_channels: u8,
    pub gain_controls: Vec<GainControlDescriptor>,
    pub supports_hardware_timestamps: bool,
    pub supports_external_clock: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RadioCaptureSettings {
    pub center_frequency_hz: u64,
    pub sample_rate_hz: u64,
    pub bandwidth_hz: u64,
    pub receive_channel: u8,
    pub sample_limit: Option<u64>,
}

impl RadioCaptureSettings {
    pub fn validate(&self, capabilities: &RadioCapabilities) -> Result<(), CaptureError> {
        if !capabilities.frequency_hz.contains(self.center_frequency_hz) {
            return Err(CaptureError::new("radio center frequency is unsupported"));
        }
        if !capabilities.sample_rates_hz.contains(self.sample_rate_hz) {
            return Err(CaptureError::new("radio sample rate is unsupported"));
        }
        if !capabilities.bandwidths_hz.contains(self.bandwidth_hz) {
            return Err(CaptureError::new("radio bandwidth is unsupported"));
        }
        if self.receive_channel >= capabilities.receive_channels {
            return Err(CaptureError::new("radio receive channel is unsupported"));
        }
        if self.sample_limit == Some(0) {
            return Err(CaptureError::new(
                "radio sample limit must be greater than zero",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct HackRfSettings {
    pub lna_gain_db: Option<u32>,
    pub vga_gain_db: Option<u32>,
    pub amplifier_enabled: bool,
    pub antenna_power_enabled: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LimeSdrUsbSettings {
    pub antenna: Option<String>,
    pub gain_db: Option<i32>,
    pub external_clock_hz: Option<u64>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AntSdrE200Settings {
    pub antenna: Option<String>,
    pub gain_db: Option<i32>,
    pub iio_uri: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RadioProviderSettings {
    Simulator,
    HackRf(HackRfSettings),
    LimeSdrUsb(LimeSdrUsbSettings),
    AntSdrE200(AntSdrE200Settings),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveredRadio {
    pub descriptor: DeviceDescriptor,
    pub provider: RadioProvider,
    pub model: RadioModel,
    pub connection_backend: ConnectionBackend,
    pub persistent_id: Option<String>,
    pub capabilities: RadioCapabilities,
}

impl DiscoveredRadio {
    pub fn validate(&self) -> Result<(), CaptureError> {
        if self.descriptor.kind != InstrumentKind::RadioReceiver {
            return Err(CaptureError::new(
                "SDR discovery returned a non-radio instrument",
            ));
        }
        if self.capabilities.receive_channels == 0 {
            return Err(CaptureError::new(
                "SDR discovery returned no receive channels",
            ));
        }
        let identity_is_consistent = matches!(
            (self.provider, self.model, self.connection_backend),
            (
                RadioProvider::Simulator,
                RadioModel::Simulated,
                ConnectionBackend::Simulator
            ) | (
                RadioProvider::HackRf,
                RadioModel::HackRfOne,
                ConnectionBackend::Usb
            ) | (
                RadioProvider::LimeSuiteNg,
                RadioModel::LimeSdrUsbTypeA,
                ConnectionBackend::Usb
            ) | (
                RadioProvider::Libiio,
                RadioModel::AntSdrE200,
                ConnectionBackend::Network
            )
        );
        if !identity_is_consistent {
            return Err(CaptureError::new(
                "SDR provider, model, and connection backend are inconsistent",
            ));
        }
        Ok(())
    }
}

pub trait RadioProviderAdapter: Send + Sync {
    fn provider(&self) -> RadioProvider;

    fn discover(&self) -> Result<Vec<DiscoveredRadio>, CaptureError>;

    fn open(
        &self,
        device: &DiscoveredRadio,
        settings: RadioCaptureSettings,
        provider_settings: RadioProviderSettings,
    ) -> Result<Box<dyn CaptureSource>, CaptureError>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RadioProviderFailure {
    pub provider: RadioProvider,
    pub error: CaptureError,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RadioDiscoveryReport {
    pub devices: Vec<DiscoveredRadio>,
    pub failures: Vec<RadioProviderFailure>,
}

impl RadioDiscoveryReport {
    pub fn device(&self, connection_id: &str) -> Option<&DiscoveredRadio> {
        self.devices
            .iter()
            .find(|device| device.descriptor.id.as_str() == connection_id)
    }
}

/// Aggregates independent SDR providers without allowing one provider failure
/// to hide devices returned successfully by another.
#[derive(Default)]
pub struct RadioRegistry {
    adapters: Vec<Box<dyn RadioProviderAdapter>>,
}

impl RadioRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_adapter(mut self, adapter: impl RadioProviderAdapter + 'static) -> Self {
        self.adapters.push(Box::new(adapter));
        self
    }

    pub fn push_adapter(&mut self, adapter: impl RadioProviderAdapter + 'static) {
        self.adapters.push(Box::new(adapter));
    }

    pub fn discover(&self) -> RadioDiscoveryReport {
        let mut report = RadioDiscoveryReport::default();
        let mut connection_ids = HashSet::new();
        for adapter in &self.adapters {
            let provider = adapter.provider();
            let devices = match adapter.discover() {
                Ok(devices) => devices,
                Err(error) => {
                    report
                        .failures
                        .push(RadioProviderFailure { provider, error });
                    continue;
                }
            };
            for device in devices {
                let validation = device.validate().and_then(|()| {
                    if device.provider == provider {
                        Ok(())
                    } else {
                        Err(CaptureError::new(
                            "SDR adapter returned a device for a different provider",
                        ))
                    }
                });
                if let Err(error) = validation {
                    report
                        .failures
                        .push(RadioProviderFailure { provider, error });
                    continue;
                }
                if !connection_ids.insert(device.descriptor.id.clone()) {
                    report.failures.push(RadioProviderFailure {
                        provider,
                        error: CaptureError::new(format!(
                            "duplicate SDR connection ID {}",
                            device.descriptor.id
                        )),
                    });
                    continue;
                }
                report.devices.push(device);
            }
        }
        report
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wavecraft_core::{DeviceId, InstrumentKind};

    fn capabilities() -> RadioCapabilities {
        RadioCapabilities {
            frequency_hz: ValueDomain::Stepped {
                range: 1_000_000..=6_000_000_000,
                step: 1,
            },
            sample_rates_hz: ValueDomain::Discrete(vec![1_000_000, 2_000_000, 10_000_000]),
            bandwidths_hz: ValueDomain::Stepped {
                range: 200_000..=10_000_000,
                step: 100_000,
            },
            receive_channels: 2,
            gain_controls: vec![GainControlDescriptor {
                id: "gain".to_owned(),
                label: "Gain".to_owned(),
                help: None,
                range_db: -12..=60,
                step_db: 1,
                default_db: 0,
            }],
            supports_hardware_timestamps: false,
            supports_external_clock: false,
        }
    }

    struct FixtureAdapter {
        provider: RadioProvider,
        result: Result<Vec<DiscoveredRadio>, CaptureError>,
    }

    impl RadioProviderAdapter for FixtureAdapter {
        fn provider(&self) -> RadioProvider {
            self.provider
        }

        fn discover(&self) -> Result<Vec<DiscoveredRadio>, CaptureError> {
            self.result.clone()
        }

        fn open(
            &self,
            _device: &DiscoveredRadio,
            _settings: RadioCaptureSettings,
            _provider_settings: RadioProviderSettings,
        ) -> Result<Box<dyn CaptureSource>, CaptureError> {
            Err(CaptureError::new("fixture does not capture"))
        }
    }

    fn discovered_radio(id: &str, provider: RadioProvider) -> DiscoveredRadio {
        let (model, connection_backend) = match provider {
            RadioProvider::Simulator => (RadioModel::Simulated, ConnectionBackend::Simulator),
            RadioProvider::HackRf => (RadioModel::HackRfOne, ConnectionBackend::Usb),
            RadioProvider::LimeSuiteNg => (RadioModel::LimeSdrUsbTypeA, ConnectionBackend::Usb),
            RadioProvider::Libiio => (RadioModel::AntSdrE200, ConnectionBackend::Network),
        };
        DiscoveredRadio {
            descriptor: DeviceDescriptor {
                id: DeviceId::new(id),
                name: id.to_owned(),
                kind: InstrumentKind::RadioReceiver,
            },
            provider,
            model,
            connection_backend,
            persistent_id: Some(id.to_owned()),
            capabilities: capabilities(),
        }
    }

    #[test]
    fn portable_settings_validate_against_provider_capabilities() {
        let settings = RadioCaptureSettings {
            center_frequency_hz: 433_920_000,
            sample_rate_hz: 2_000_000,
            bandwidth_hz: 1_000_000,
            receive_channel: 1,
            sample_limit: Some(2_000_000),
        };
        assert!(settings.validate(&capabilities()).is_ok());

        let invalid = RadioCaptureSettings {
            receive_channel: 2,
            ..settings
        };
        assert!(invalid.validate(&capabilities()).is_err());
    }

    #[test]
    fn all_planned_physical_providers_have_distinct_settings() {
        let settings = [
            RadioProviderSettings::HackRf(HackRfSettings::default()),
            RadioProviderSettings::LimeSdrUsb(LimeSdrUsbSettings::default()),
            RadioProviderSettings::AntSdrE200(AntSdrE200Settings::default()),
        ];
        assert_eq!(settings.len(), 3);
        assert_ne!(RadioModel::HackRfOne, RadioModel::LimeSdrUsbTypeA);
        assert_ne!(RadioModel::LimeSdrUsbTypeA, RadioModel::AntSdrE200);
    }

    #[test]
    fn discovered_devices_must_be_radio_receivers() {
        let radio = DiscoveredRadio {
            descriptor: DeviceDescriptor {
                id: DeviceId::new("radio"),
                name: "Radio".to_owned(),
                kind: InstrumentKind::RadioReceiver,
            },
            provider: RadioProvider::Simulator,
            model: RadioModel::Simulated,
            connection_backend: ConnectionBackend::Simulator,
            persistent_id: Some("radio".to_owned()),
            capabilities: capabilities(),
        };
        assert!(radio.validate().is_ok());

        let invalid = DiscoveredRadio {
            descriptor: DeviceDescriptor {
                kind: InstrumentKind::Oscilloscope,
                ..radio.descriptor.clone()
            },
            ..radio
        };
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn selected_physical_models_use_their_required_host_transports() {
        let descriptor = DeviceDescriptor {
            id: DeviceId::new("physical-radio"),
            name: "Physical radio".to_owned(),
            kind: InstrumentKind::RadioReceiver,
        };
        let lime = DiscoveredRadio {
            descriptor: descriptor.clone(),
            provider: RadioProvider::LimeSuiteNg,
            model: RadioModel::LimeSdrUsbTypeA,
            connection_backend: ConnectionBackend::Usb,
            persistent_id: None,
            capabilities: capabilities(),
        };
        let antsdr = DiscoveredRadio {
            descriptor,
            provider: RadioProvider::Libiio,
            model: RadioModel::AntSdrE200,
            connection_backend: ConnectionBackend::Network,
            persistent_id: None,
            capabilities: capabilities(),
        };

        assert!(lime.validate().is_ok());
        assert!(antsdr.validate().is_ok());
        let invalid_antsdr = DiscoveredRadio {
            connection_backend: ConnectionBackend::Usb,
            ..antsdr
        };
        assert!(invalid_antsdr.validate().is_err());
    }

    #[test]
    fn registry_preserves_successes_when_another_provider_fails() {
        let registry = RadioRegistry::new()
            .with_adapter(FixtureAdapter {
                provider: RadioProvider::HackRf,
                result: Err(CaptureError::new("libhackrf unavailable")),
            })
            .with_adapter(FixtureAdapter {
                provider: RadioProvider::Simulator,
                result: Ok(vec![discovered_radio(
                    "sim:radio-0",
                    RadioProvider::Simulator,
                )]),
            });

        let report = registry.discover();
        assert_eq!(report.devices.len(), 1);
        assert_eq!(report.failures.len(), 1);
        assert_eq!(report.failures[0].provider, RadioProvider::HackRf);
        assert_eq!(
            report.device("sim:radio-0").map(|device| device.model),
            Some(RadioModel::Simulated)
        );
    }

    #[test]
    fn registry_selects_identical_models_by_exact_connection_id() {
        let registry = RadioRegistry::new().with_adapter(FixtureAdapter {
            provider: RadioProvider::HackRf,
            result: Ok(vec![
                discovered_radio("hackrf:serial-a", RadioProvider::HackRf),
                discovered_radio("hackrf:serial-b", RadioProvider::HackRf),
            ]),
        });

        let report = registry.discover();
        assert_eq!(report.devices.len(), 2);
        assert_eq!(
            report
                .device("hackrf:serial-b")
                .and_then(|device| device.persistent_id.as_deref()),
            Some("hackrf:serial-b")
        );
    }

    #[test]
    fn registry_reports_invalid_provider_identity_and_duplicate_ids() {
        let registry = RadioRegistry::new()
            .with_adapter(FixtureAdapter {
                provider: RadioProvider::HackRf,
                result: Ok(vec![discovered_radio(
                    "shared-radio",
                    RadioProvider::Simulator,
                )]),
            })
            .with_adapter(FixtureAdapter {
                provider: RadioProvider::Simulator,
                result: Ok(vec![
                    discovered_radio("shared-radio", RadioProvider::Simulator),
                    discovered_radio("shared-radio", RadioProvider::Simulator),
                ]),
            });

        let report = registry.discover();
        assert_eq!(report.devices.len(), 1);
        assert_eq!(report.failures.len(), 2);
        assert!(report.failures[0]
            .error
            .to_string()
            .contains("different provider"));
        assert!(report.failures[1]
            .error
            .to_string()
            .contains("duplicate SDR connection ID"));
    }
}
