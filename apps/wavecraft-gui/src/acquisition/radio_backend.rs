//! The radio providers the GUI can use.
//!
//! This is the one place in the GUI that names a provider's driver crate. The
//! rest of the GUI works with `DiscoveredRadio`, `RadioProviderSettings`, and
//! the provider-neutral `RadioRuntimeControl`, so starting, polling, and
//! stopping a capture do not depend on which radio it is. Supporting another
//! provider means adding its arm to the functions below (and its settings to
//! the radio panels), not changing the capture lifecycle.

use crate::{radio_gain_u32, CaptureSource, RadioSettings};
use wavecraft_hackrf::{HackRfAdapter, HackRfRuntimeControl, HackRfRuntimeSettings};
use wavecraft_sdr::{
    DiscoveredRadio, HackRfSettings, RadioCaptureSettings, RadioProvider, RadioProviderSettings,
    RadioRegistry,
};

/// Every radio provider the GUI knows, ready to discover radios.
pub(crate) fn radio_registry() -> RadioRegistry {
    RadioRegistry::new().with_adapter(HackRfAdapter::new())
}

/// The settings `provider` needs besides the common capture settings, taken
/// from what the user has set.
pub(crate) fn radio_provider_settings(
    provider: RadioProvider,
    settings: &RadioSettings,
) -> Result<RadioProviderSettings, String> {
    match provider {
        RadioProvider::HackRf => Ok(RadioProviderSettings::HackRf(HackRfSettings {
            lna_gain_db: radio_gain_u32(settings, "lna")?,
            vga_gain_db: radio_gain_u32(settings, "vga")?,
            amplifier_enabled: settings.amplifier_enabled,
            antenna_power_enabled: settings.antenna_power_enabled,
            frequency_correction_milli_ppm: settings.frequency_correction_milli_ppm,
        })),
        provider => Err(not_implemented(provider)),
    }
}

/// Opens a capture source for `device`, together with the control that changes
/// its tuning while it runs when the provider has one.
pub(crate) fn open_radio_source(
    device: &DiscoveredRadio,
    settings: RadioCaptureSettings,
    provider_settings: RadioProviderSettings,
) -> Result<(Box<dyn CaptureSource>, Option<RadioRuntimeControl>), String> {
    match device.provider {
        RadioProvider::HackRf => {
            let (source, control) = HackRfAdapter::new()
                .open_with_runtime_control(device, settings, provider_settings)
                .map_err(|error| error.to_string())?;
            Ok((source, Some(RadioRuntimeControl::HackRf(control))))
        }
        provider => Err(not_implemented(provider)),
    }
}

fn not_implemented(provider: RadioProvider) -> String {
    format!("capture is not implemented for the {provider:?} radio provider")
}

/// Changes the tuning of a radio capture that is running.
pub(crate) enum RadioRuntimeControl {
    HackRf(HackRfRuntimeControl),
}

impl RadioRuntimeControl {
    /// Applies the tuning, gains, and correction in `settings` to the running capture.
    pub(crate) fn update(&self, settings: &RadioSettings) -> Result<(), String> {
        match self {
            Self::HackRf(control) => hackrf_runtime_settings(settings)
                .and_then(|settings| control.update(settings).map_err(|error| error.to_string())),
        }
    }

    /// The status line to show once an update has been sent.
    pub(crate) fn update_status(&self) -> &'static str {
        match self {
            Self::HackRf(_) => "Updating HackRF tuning during capture…",
        }
    }
}

fn hackrf_runtime_settings(settings: &RadioSettings) -> Result<HackRfRuntimeSettings, String> {
    Ok(HackRfRuntimeSettings {
        center_frequency_hz: settings.capture.center_frequency_hz,
        bandwidth_hz: settings.capture.bandwidth_hz,
        lna_gain_db: radio_gain_u32(settings, "lna")?,
        vga_gain_db: radio_gain_u32(settings, "vga")?,
        amplifier_enabled: settings.amplifier_enabled,
        frequency_correction_milli_ppm: settings.frequency_correction_milli_ppm,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use wavecraft_core::{DeviceDescriptor, DeviceId, InstrumentKind};
    use wavecraft_sdr::{ConnectionBackend, RadioModel};

    fn radio(provider: RadioProvider) -> DiscoveredRadio {
        DiscoveredRadio {
            descriptor: DeviceDescriptor {
                id: DeviceId::new("radio:1"),
                name: "Radio".to_owned(),
                kind: InstrumentKind::RadioReceiver,
            },
            provider,
            model: RadioModel::HackRfOne,
            connection_backend: ConnectionBackend::Usb,
            // A radio that cannot be reopened makes the HackRF adapter stop
            // before it loads libhackrf, so these tests need neither the library
            // nor a device.
            persistent_id: None,
            capabilities: wavecraft_hackrf::hackrf_one_capabilities(),
        }
    }

    fn capture_settings() -> RadioCaptureSettings {
        RadioCaptureSettings {
            center_frequency_hz: 868_300_000,
            sample_rate_hz: 8_000_000,
            bandwidth_hz: 1_750_000,
            receive_channel: 0,
            sample_limit: None,
        }
    }

    #[test]
    fn hackrf_provider_settings_follow_the_radio_settings() {
        let mut settings = RadioSettings::default();
        settings.gains_db.insert("lna".to_owned(), 24);
        settings.gains_db.insert("vga".to_owned(), 20);
        settings.amplifier_enabled = true;
        settings.antenna_power_enabled = true;
        settings.frequency_correction_milli_ppm = -8_250;

        assert_eq!(
            radio_provider_settings(RadioProvider::HackRf, &settings),
            Ok(RadioProviderSettings::HackRf(HackRfSettings {
                lna_gain_db: Some(24),
                vga_gain_db: Some(20),
                amplifier_enabled: true,
                antenna_power_enabled: true,
                frequency_correction_milli_ppm: -8_250,
            }))
        );
    }

    #[test]
    fn a_negative_gain_is_rejected_before_a_capture_starts() {
        let mut settings = RadioSettings::default();
        settings.gains_db.insert("lna".to_owned(), -8);

        assert_eq!(
            radio_provider_settings(RadioProvider::HackRf, &settings),
            Err("radio gain lna cannot be negative".to_owned())
        );
    }

    #[test]
    fn a_provider_without_a_capture_driver_is_reported_not_attempted() {
        let message = "capture is not implemented for the LimeSuiteNg radio provider";

        assert_eq!(
            radio_provider_settings(RadioProvider::LimeSuiteNg, &RadioSettings::default()),
            Err(message.to_owned())
        );
        assert_eq!(
            open_radio_source(
                &radio(RadioProvider::LimeSuiteNg),
                capture_settings(),
                RadioProviderSettings::LimeSdrUsb(Default::default()),
            )
            .err(),
            Some(message.to_owned())
        );
    }

    #[test]
    fn a_hackrf_capture_is_opened_by_the_hackrf_adapter() {
        let error = open_radio_source(
            &radio(RadioProvider::HackRf),
            capture_settings(),
            RadioProviderSettings::HackRf(HackRfSettings::default()),
        )
        .err()
        .expect("a radio without a serial number cannot be opened");

        assert!(error.contains("serial number"), "{error}");
    }

    #[test]
    fn the_runtime_settings_carry_only_what_can_change_while_capturing() {
        let mut settings = RadioSettings::default();
        settings.capture.center_frequency_hz = 433_920_000;
        settings.capture.bandwidth_hz = 8_000_000;
        settings.gains_db.insert("lna".to_owned(), 24);
        settings.gains_db.insert("vga".to_owned(), 20);
        settings.amplifier_enabled = true;
        settings.frequency_correction_milli_ppm = 12_500;

        assert_eq!(
            hackrf_runtime_settings(&settings),
            Ok(HackRfRuntimeSettings {
                center_frequency_hz: 433_920_000,
                bandwidth_hz: 8_000_000,
                lna_gain_db: Some(24),
                vga_gain_db: Some(20),
                amplifier_enabled: true,
                frequency_correction_milli_ppm: 12_500,
            })
        );
        settings.gains_db.insert("vga".to_owned(), -2);
        assert_eq!(
            hackrf_runtime_settings(&settings),
            Err("radio gain vga cannot be negative".to_owned())
        );
    }
}
