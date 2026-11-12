//! `probe-rs` adapter for WaveCraft's provider-neutral debug-probe interface.

pub mod helper;

use probe_rs::probe::blackmagic::BlackMagicProbeFactory;
use probe_rs::probe::cmsisdap::CmsisDapFactory;
use probe_rs::probe::jlink::JLinkFactory;
use probe_rs::probe::list::Lister;
use probe_rs::probe::stlink::StLinkFactory;
use probe_rs::probe::{
    DebugProbeError as ProbeRsDebugProbeError, DebugProbeInfo, DebugProbeSelector,
    Probe as ProbeRsProbe, ProbeCreationError, WireProtocol as ProbeRsWireProtocol,
};
use probe_rs::rtt::{Rtt, ScanRegion, try_attach_to_rtt};
use probe_rs::{MemoryInterface, Permissions, Session, config};
use wavecraft_debug_probe::{
    AttachConfiguration, DebugProbeDriver, DebugProbeError, DebugProbeErrorKind, DebugProbeHandle,
    DebugSession, ProbeDescriptor, ProbeProvider, ProbeSelector, RttChannelDescriptor,
    RttChannelDirection, RttConfiguration, RttControlBlockLocation, TargetSelector, WireProtocol,
};

/// Minimum Rust release required by the pinned `probe-rs` adapter dependency.
pub const MINIMUM_RUST_VERSION: &str = "1.95";

/// Discovers all probe families registered by `probe-rs`.
#[derive(Clone, Copy, Debug, Default)]
pub struct ProbeRsDriver;

impl DebugProbeDriver for ProbeRsDriver {
    fn list_probes(&self) -> Result<Vec<ProbeDescriptor>, DebugProbeError> {
        Ok(Lister::new()
            .list_all()
            .iter()
            .map(probe_descriptor)
            .collect())
    }

    fn open_probe(
        &self,
        selector: &ProbeSelector,
    ) -> Result<Box<dyn DebugProbeHandle>, DebugProbeError> {
        let requested: DebugProbeSelector = selector.connection_id.parse().map_err(|error| {
            DebugProbeError::new(
                DebugProbeErrorKind::InvalidConfiguration,
                format!("invalid probe selector {}: {error}", selector.connection_id),
            )
        })?;
        let information = Lister::new()
            .list(Some(&requested))
            .into_iter()
            .find(|information| connection_id(information) == selector.connection_id)
            .ok_or_else(|| {
                DebugProbeError::new(
                    DebugProbeErrorKind::NotFound,
                    format!("debug probe {} was not found", selector.connection_id),
                )
            })?;
        let descriptor = probe_descriptor(&information);
        let probe = information.open().map_err(map_open_error)?;
        Ok(Box::new(ProbeRsHandle { descriptor, probe }))
    }
}

struct ProbeRsHandle {
    descriptor: ProbeDescriptor,
    probe: ProbeRsProbe,
}

impl DebugProbeHandle for ProbeRsHandle {
    fn descriptor(&self) -> &ProbeDescriptor {
        &self.descriptor
    }

    fn attach(
        self: Box<Self>,
        configuration: &AttachConfiguration,
    ) -> Result<Box<dyn DebugSession>, DebugProbeError> {
        let mut probe = self.probe;
        if let Some(protocol) = configuration.wire_protocol {
            probe
                .select_protocol(map_wire_protocol(protocol))
                .map_err(map_probe_error)?;
        }
        if let Some(speed_khz) = configuration.speed_khz {
            probe.set_speed(speed_khz).map_err(map_probe_error)?;
        }
        let target = match &configuration.target {
            TargetSelector::Auto => config::TargetSelector::Auto,
            TargetSelector::Name(name) => config::TargetSelector::Unspecified(name.clone()),
        };
        let mut session = probe
            .attach(target, Permissions::default())
            .map_err(|error| target_error("failed to attach to target", error))?;
        session
            .core(configuration.core_index)
            .map_err(|error| target_error("target core is unavailable", error))?;

        Ok(Box::new(ProbeRsSession {
            session,
            core_index: configuration.core_index,
            rtt: None,
        }))
    }
}

struct ProbeRsSession {
    session: Session,
    core_index: usize,
    rtt: Option<Rtt>,
}

impl DebugSession for ProbeRsSession {
    fn reset_target(&mut self) -> Result<(), DebugProbeError> {
        self.rtt = None;
        let mut core = self
            .session
            .core(self.core_index)
            .map_err(|error| target_error("target core is unavailable", error))?;
        core.reset()
            .map_err(|error| target_error("failed to reset target", error))
    }

    fn read_memory(&mut self, address: u64, destination: &mut [u8]) -> Result<(), DebugProbeError> {
        let mut core = self
            .session
            .core(self.core_index)
            .map_err(|error| target_error("target core is unavailable", error))?;
        core.read(address, destination)
            .map_err(|error| target_error("failed to read target memory", error))
    }

    fn write_memory(&mut self, address: u64, source: &[u8]) -> Result<(), DebugProbeError> {
        let mut core = self
            .session
            .core(self.core_index)
            .map_err(|error| target_error("target core is unavailable", error))?;
        core.write(address, source)
            .map_err(|error| target_error("failed to write target memory", error))
    }

    fn attach_rtt(
        &mut self,
        configuration: &RttConfiguration,
    ) -> Result<Vec<RttChannelDescriptor>, DebugProbeError> {
        let region = map_scan_region(&configuration.control_block);
        let mut core = self
            .session
            .core(self.core_index)
            .map_err(|error| target_error("target core is unavailable", error))?;
        let mut rtt = try_attach_to_rtt(&mut core, configuration.timeout, &region)
            .map_err(|error| target_error("failed to attach to RTT", error))?;
        let mut channels = Vec::with_capacity(rtt.up_channels().len() + rtt.down_channels().len());
        channels.extend(
            rtt.up_channels()
                .iter()
                .map(|channel| RttChannelDescriptor {
                    number: channel.number(),
                    direction: RttChannelDirection::Up,
                    name: channel.name().map(str::to_owned),
                    buffer_size: channel.buffer_size(),
                }),
        );
        channels.extend(
            rtt.down_channels()
                .iter()
                .map(|channel| RttChannelDescriptor {
                    number: channel.number(),
                    direction: RttChannelDirection::Down,
                    name: channel.name().map(str::to_owned),
                    buffer_size: channel.buffer_size(),
                }),
        );
        self.rtt = Some(rtt);
        Ok(channels)
    }

    fn read_rtt_up(
        &mut self,
        channel_number: usize,
        destination: &mut [u8],
    ) -> Result<usize, DebugProbeError> {
        let rtt = self.rtt.as_mut().ok_or_else(rtt_not_attached)?;
        let channel = rtt
            .up_channels()
            .iter_mut()
            .find(|channel| channel.number() == channel_number)
            .ok_or_else(|| missing_channel(RttChannelDirection::Up, channel_number))?;
        let mut core = self
            .session
            .core(self.core_index)
            .map_err(|error| target_error("target core is unavailable", error))?;
        channel
            .read(&mut core, destination)
            .map_err(|error| transport_error("failed to read RTT up channel", error))
    }

    fn write_rtt_down(
        &mut self,
        channel_number: usize,
        source: &[u8],
    ) -> Result<usize, DebugProbeError> {
        let rtt = self.rtt.as_mut().ok_or_else(rtt_not_attached)?;
        let channel = rtt
            .down_channels()
            .iter_mut()
            .find(|channel| channel.number() == channel_number)
            .ok_or_else(|| missing_channel(RttChannelDirection::Down, channel_number))?;
        let mut core = self
            .session
            .core(self.core_index)
            .map_err(|error| target_error("target core is unavailable", error))?;
        channel
            .write(&mut core, source)
            .map_err(|error| transport_error("failed to write RTT down channel", error))
    }
}

fn probe_descriptor(information: &DebugProbeInfo) -> ProbeDescriptor {
    let provider = if information.is_probe_type::<JLinkFactory>() {
        ProbeProvider::JLink
    } else if information.is_probe_type::<StLinkFactory>() {
        ProbeProvider::StLink
    } else if information.is_probe_type::<BlackMagicProbeFactory>() {
        ProbeProvider::BlackMagicProbe
    } else if information.is_probe_type::<CmsisDapFactory>() {
        ProbeProvider::CmsisDap
    } else {
        ProbeProvider::Other(information.probe_type())
    };
    let vendor = match &provider {
        ProbeProvider::JLink => Some("SEGGER"),
        ProbeProvider::StLink => Some("STMicroelectronics"),
        ProbeProvider::BlackMagicProbe => Some("Black Magic Debug"),
        ProbeProvider::CmsisDap => Some("Arm CMSIS-DAP"),
        ProbeProvider::Other(_) => None,
    };

    ProbeDescriptor {
        connection_id: connection_id(information),
        provider,
        vendor: vendor.map(str::to_owned),
        model: information.identifier.clone(),
        serial_number: information.serial_number.clone(),
    }
}

fn connection_id(information: &DebugProbeInfo) -> String {
    DebugProbeSelector::from(information).to_string()
}

fn map_wire_protocol(protocol: WireProtocol) -> ProbeRsWireProtocol {
    match protocol {
        WireProtocol::Swd => ProbeRsWireProtocol::Swd,
        WireProtocol::Jtag => ProbeRsWireProtocol::Jtag,
    }
}

fn map_scan_region(location: &RttControlBlockLocation) -> ScanRegion {
    match location {
        RttControlBlockLocation::Exact(address) => ScanRegion::Exact(*address),
        RttControlBlockLocation::Ranges(ranges) => ScanRegion::Ranges(ranges.clone()),
        RttControlBlockLocation::Ram => ScanRegion::Ram,
    }
}

fn map_open_error(error: ProbeRsDebugProbeError) -> DebugProbeError {
    let kind = match &error {
        ProbeRsDebugProbeError::ProbeCouldNotBeCreated(ProbeCreationError::NotFound) => {
            DebugProbeErrorKind::NotFound
        }
        ProbeRsDebugProbeError::ProbeCouldNotBeCreated(ProbeCreationError::CouldNotOpen) => {
            DebugProbeErrorKind::Busy
        }
        ProbeRsDebugProbeError::Usb(error)
            if error.kind() == std::io::ErrorKind::PermissionDenied =>
        {
            DebugProbeErrorKind::PermissionDenied
        }
        ProbeRsDebugProbeError::UnsupportedProtocol(_)
        | ProbeRsDebugProbeError::UnsupportedSpeed(_)
        | ProbeRsDebugProbeError::NotImplemented { .. }
        | ProbeRsDebugProbeError::CommandNotSupportedByProbe { .. }
        | ProbeRsDebugProbeError::InterfaceNotAvailable { .. } => DebugProbeErrorKind::Unsupported,
        ProbeRsDebugProbeError::TargetNotFound => DebugProbeErrorKind::Target,
        _ => DebugProbeErrorKind::Transport,
    };
    DebugProbeError::new(kind, format!("failed to open debug probe: {error}"))
}

fn map_probe_error(error: ProbeRsDebugProbeError) -> DebugProbeError {
    let kind = match &error {
        ProbeRsDebugProbeError::UnsupportedProtocol(_)
        | ProbeRsDebugProbeError::UnsupportedSpeed(_)
        | ProbeRsDebugProbeError::NotImplemented { .. }
        | ProbeRsDebugProbeError::CommandNotSupportedByProbe { .. }
        | ProbeRsDebugProbeError::InterfaceNotAvailable { .. } => DebugProbeErrorKind::Unsupported,
        ProbeRsDebugProbeError::TargetNotFound => DebugProbeErrorKind::Target,
        _ => DebugProbeErrorKind::Transport,
    };
    DebugProbeError::new(kind, error.to_string())
}

fn rtt_not_attached() -> DebugProbeError {
    DebugProbeError::new(
        DebugProbeErrorKind::InvalidConfiguration,
        "RTT is not attached",
    )
}

fn missing_channel(direction: RttChannelDirection, channel_number: usize) -> DebugProbeError {
    DebugProbeError::new(
        DebugProbeErrorKind::InvalidConfiguration,
        format!("RTT {direction:?} channel {channel_number} is not available"),
    )
}

fn target_error(context: &str, error: impl std::fmt::Display) -> DebugProbeError {
    DebugProbeError::new(DebugProbeErrorKind::Target, format!("{context}: {error}"))
}

fn transport_error(context: &str, error: impl std::fmt::Display) -> DebugProbeError {
    DebugProbeError::new(
        DebugProbeErrorKind::Transport,
        format!("{context}: {error}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_requested_probe_families_without_exposing_probe_rs_types() {
        let cases = [
            (
                DebugProbeInfo::new(
                    "J-Link",
                    0x1366,
                    0x0101,
                    Some("J123".to_owned()),
                    &JLinkFactory,
                    None,
                    false,
                ),
                ProbeProvider::JLink,
            ),
            (
                DebugProbeInfo::new(
                    "ST-LINK/V2-1",
                    0x0483,
                    0x374b,
                    Some("S123".to_owned()),
                    &StLinkFactory,
                    None,
                    false,
                ),
                ProbeProvider::StLink,
            ),
            (
                DebugProbeInfo::new(
                    "Black Magic Probe",
                    0x1d50,
                    0x6018,
                    Some("B123".to_owned()),
                    &BlackMagicProbeFactory,
                    Some(0),
                    false,
                ),
                ProbeProvider::BlackMagicProbe,
            ),
            (
                DebugProbeInfo::new(
                    "CMSIS-DAP",
                    0x0d28,
                    0x0204,
                    Some("C123".to_owned()),
                    &CmsisDapFactory,
                    None,
                    false,
                ),
                ProbeProvider::CmsisDap,
            ),
        ];

        for (information, expected_provider) in cases {
            let descriptor = probe_descriptor(&information);
            assert_eq!(descriptor.provider, expected_provider);
            assert_eq!(descriptor.serial_number, information.serial_number);
            assert_eq!(
                descriptor.connection_id,
                DebugProbeSelector::from(&information).to_string()
            );
        }
    }

    #[test]
    fn maps_every_rtt_control_block_location() {
        assert!(matches!(
            map_scan_region(&RttControlBlockLocation::Exact(0x2000_0000)),
            ScanRegion::Exact(0x2000_0000)
        ));
        assert!(matches!(
            map_scan_region(&RttControlBlockLocation::Ranges(vec![
                0x2000_0000..0x2001_0000,
                0x3000_0000..0x3000_1000,
            ])),
            ScanRegion::Ranges(ranges)
                if ranges
                    == vec![
                        0x2000_0000..0x2001_0000,
                        0x3000_0000..0x3000_1000,
                    ]
        ));
        assert!(matches!(
            map_scan_region(&RttControlBlockLocation::Ram),
            ScanRegion::Ram
        ));
    }
}
