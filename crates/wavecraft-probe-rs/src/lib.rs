//! `probe-rs` adapter for WaveCraft's provider-neutral debug-probe interface.

pub mod helper;
mod jlink_network;

use probe_rs::probe::blackmagic::BlackMagicProbeFactory;
use probe_rs::probe::cmsisdap::CmsisDapFactory;
use probe_rs::probe::jlink::JLinkFactory;
use probe_rs::probe::list::Lister;
use probe_rs::probe::stlink::StLinkFactory;
use probe_rs::probe::{
    DebugProbeError as ProbeRsDebugProbeError, DebugProbeInfo, DebugProbeSelector,
    Probe as ProbeRsProbe, ProbeCreationError, ProbeTransport, WireProtocol as ProbeRsWireProtocol,
};
use probe_rs::rtt::{Rtt, RttTarget, ScanRegion, try_attach_to_rtt};
use probe_rs::{MemoryInterface, Permissions, Session, config};
use std::error::Error as StdError;
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use wavecraft_debug_probe::{
    AttachConfiguration, DebugProbeDriver, DebugProbeError, DebugProbeErrorKind, DebugProbeHandle,
    DebugSession, DebugTargetDescriptor, CMSIS_DAP_NETWORK_CONNECTION_ID_PREFIX,
    JLINK_NETWORK_CONNECTION_ID_PREFIX, ProbeDescriptor, ProbeProvider, ProbeSelector,
    ProbeTargetGroup, RttChannelDescriptor, RttChannelDirection, RttConfiguration,
    RttControlBlockLocation, TargetSelector, WireProtocol,
};

const BLACK_MAGIC_NETWORK_SELECTOR_PREFIX: &str = "1d50:6018:";
const PROBE_RS_CMSIS_DAP_TCP_SELECTOR_PREFIX: &str = "0000:0001:";
const NETWORK_BMP_ATTEMPT_TIMEOUT: Duration = Duration::from_millis(2_500);
const NETWORK_BMP_RETRY_DELAY: Duration = Duration::from_millis(100);
const NETWORK_BMP_OPEN_ATTEMPTS: usize = 2;
const NETWORK_BMP_TIMEOUT_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Minimum Rust release required by the pinned `probe-rs` adapter dependency.
pub const MINIMUM_RUST_VERSION: &str = "1.95";

/// Discovers all probe families registered by `probe-rs`.
#[derive(Clone, Copy, Debug, Default)]
pub struct ProbeRsDriver;

impl ProbeRsDriver {
    /// Returns probe-rs target names grouped by the transport backend that can
    /// communicate with their core architecture.
    pub fn supported_target_groups() -> Vec<ProbeTargetGroup> {
        let registry = config::Registry::from_builtin_families();
        [
            ProbeProvider::JLink,
            ProbeProvider::StLink,
            ProbeProvider::BlackMagicProbe,
            ProbeProvider::CmsisDap,
        ]
        .into_iter()
        .map(|provider| ProbeTargetGroup {
            targets: target_descriptors(&registry, &provider),
            provider,
        })
        .collect()
    }
}

fn target_descriptors(
    registry: &config::Registry,
    provider: &ProbeProvider,
) -> Vec<DebugTargetDescriptor> {
    let mut targets = registry
        .families()
        .iter()
        .flat_map(|family| {
            family
                .variants
                .iter()
                .filter(|chip| {
                    chip.cores
                        .iter()
                        .all(|core| provider_supports_core(provider, core.core_type))
                })
                .flat_map(|chip| {
                    chip.package_variants().map(|name| DebugTargetDescriptor {
                        name: name.clone(),
                        family: family.name.clone(),
                    })
                })
        })
        .collect::<Vec<_>>();
    targets.sort_by(|left, right| {
        left.name
            .to_ascii_lowercase()
            .cmp(&right.name.to_ascii_lowercase())
            .then_with(|| left.name.cmp(&right.name))
    });
    targets.dedup_by(|left, right| left.name.eq_ignore_ascii_case(&right.name));
    targets
}

fn provider_supports_core(provider: &ProbeProvider, core_type: config::CoreType) -> bool {
    match provider {
        ProbeProvider::StLink => matches!(
            core_type,
            config::CoreType::Armv6m
                | config::CoreType::Armv7a
                | config::CoreType::Armv7r
                | config::CoreType::Armv7m
                | config::CoreType::Armv7em
                | config::CoreType::Armv8a
                | config::CoreType::Armv8m
        ),
        ProbeProvider::JLink | ProbeProvider::BlackMagicProbe | ProbeProvider::CmsisDap => true,
        ProbeProvider::Other(_) => false,
    }
}

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
        if let Some(endpoint) = network_jlink_endpoint(selector)? {
            return jlink_network::open(endpoint, selector.connection_id.clone());
        }
        let network_bmp_endpoint = network_bmp_endpoint(selector);
        let network_cmsis_dap_endpoint = network_cmsis_dap_endpoint(selector)?;
        let requested_connection_id = network_cmsis_dap_endpoint.map_or_else(
            || selector.connection_id.clone(),
            |endpoint| format!("{PROBE_RS_CMSIS_DAP_TCP_SELECTOR_PREFIX}{endpoint}"),
        );
        let requested: DebugProbeSelector = requested_connection_id.parse().map_err(|error| {
            DebugProbeError::new(
                DebugProbeErrorKind::InvalidConfiguration,
                format!("invalid probe selector {requested_connection_id}: {error}"),
            )
        })?;
        let information = Lister::new()
            .list(Some(&requested))
            .into_iter()
            .find(|information| connection_id(information) == requested_connection_id)
            .ok_or_else(|| {
                DebugProbeError::new(
                    DebugProbeErrorKind::NotFound,
                    format!("debug probe {} was not found", selector.connection_id),
                )
            })?;
        let mut descriptor = probe_descriptor(&information);
        if network_cmsis_dap_endpoint.is_some() {
            descriptor.connection_id.clone_from(&selector.connection_id);
        }
        let probe = if let Some(endpoint) = network_bmp_endpoint {
            open_network_bmp(endpoint)?
        } else {
            information.open().map_err(map_open_error)?
        };
        Ok(Box::new(ProbeRsHandle { descriptor, probe }))
    }
}

fn network_bmp_endpoint(selector: &ProbeSelector) -> Option<SocketAddr> {
    let endpoint = selector
        .connection_id
        .strip_prefix(BLACK_MAGIC_NETWORK_SELECTOR_PREFIX)?;
    endpoint.parse().ok()
}

fn network_cmsis_dap_endpoint(
    selector: &ProbeSelector,
) -> Result<Option<SocketAddr>, DebugProbeError> {
    let Some(endpoint) = selector
        .connection_id
        .strip_prefix(CMSIS_DAP_NETWORK_CONNECTION_ID_PREFIX)
    else {
        return Ok(None);
    };
    endpoint.parse().map(Some).map_err(|error| {
        DebugProbeError::new(
            DebugProbeErrorKind::InvalidConfiguration,
            format!("invalid network CMSIS-DAP endpoint {endpoint:?}: {error}"),
        )
    })
}

fn open_network_bmp(endpoint: SocketAddr) -> Result<ProbeRsProbe, DebugProbeError> {
    let mut last_error = None;
    for attempt in 0..NETWORK_BMP_OPEN_ATTEMPTS {
        match open_network_bmp_once(endpoint) {
            Ok(probe) => return Ok(probe),
            Err(error) => last_error = Some(error),
        }
        if attempt + 1 < NETWORK_BMP_OPEN_ATTEMPTS {
            thread::sleep(NETWORK_BMP_RETRY_DELAY);
        }
    }
    Err(last_error.unwrap_or_else(|| {
        DebugProbeError::new(
            DebugProbeErrorKind::Transport,
            "network BMP connection failed without an error",
        )
    }))
}

fn open_network_bmp_once(endpoint: SocketAddr) -> Result<ProbeRsProbe, DebugProbeError> {
    let proxy_endpoint = start_network_bmp_proxy(
        endpoint,
        NETWORK_BMP_ATTEMPT_TIMEOUT,
        NETWORK_BMP_ATTEMPT_TIMEOUT,
    )?;
    let proxy_selector: DebugProbeSelector =
        format!("{BLACK_MAGIC_NETWORK_SELECTOR_PREFIX}{proxy_endpoint}")
            .parse()
            .map_err(|error| {
                DebugProbeError::new(
                    DebugProbeErrorKind::Transport,
                    format!("failed to create the network BMP proxy selector: {error}"),
                )
            })?;
    let proxy_information = Lister::new()
        .list(Some(&proxy_selector))
        .into_iter()
        .find(|information| connection_id(information) == proxy_selector.to_string())
        .ok_or_else(|| {
            DebugProbeError::new(
                DebugProbeErrorKind::NotFound,
                "network BMP proxy was not found",
            )
        })?;
    proxy_information.open().map_err(map_open_error)
}

fn start_network_bmp_proxy(
    endpoint: SocketAddr,
    connect_timeout: Duration,
    response_timeout: Duration,
) -> Result<SocketAddr, DebugProbeError> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .map_err(|error| transport_error("failed to create the network BMP proxy", error))?;
    let local_endpoint = listener
        .local_addr()
        .map_err(|error| transport_error("failed to inspect the network BMP proxy", error))?;
    let remote = TcpStream::connect_timeout(&endpoint, connect_timeout)
        .map_err(|error| transport_error("failed to connect to the network BMP", error))?;
    remote
        .set_nodelay(true)
        .map_err(|error| transport_error("failed to configure the network BMP socket", error))?;
    configure_abortive_close(&remote)
        .map_err(|error| transport_error("failed to configure BMP disconnect handling", error))?;
    thread::Builder::new()
        .name("wavecraft-network-bmp-proxy".to_owned())
        .spawn(move || {
            let Ok((local, _)) = listener.accept() else {
                return;
            };
            let _ = relay_network_bmp(local, remote, response_timeout);
        })
        .map_err(|error| transport_error("failed to start the network BMP proxy", error))?;
    Ok(local_endpoint)
}

// The ESP32JTAG BMP server handles one GDB remote client at a time. An
// ordinary FIN can remain queued in lwIP while a stalled GDB command is being
// processed, leaving the next WaveCraft connection accepted but unanswered.
// Abortive close makes helper termination, timeout, and disconnect release the
// server-side session immediately.
#[cfg(unix)]
fn configure_abortive_close(stream: &TcpStream) -> io::Result<()> {
    use std::mem::size_of;
    use std::os::fd::AsRawFd;

    let linger = libc::linger {
        l_onoff: 1,
        l_linger: 0,
    };
    let result = unsafe {
        // SAFETY: the file descriptor belongs to `stream`, `linger` remains
        // valid for the call, and its exact size is supplied to setsockopt.
        libc::setsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_LINGER,
            (&linger as *const libc::linger).cast(),
            size_of::<libc::linger>() as libc::socklen_t,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(windows)]
fn configure_abortive_close(stream: &TcpStream) -> io::Result<()> {
    use std::mem::size_of;
    use std::os::windows::io::AsRawSocket;
    use windows_sys::Win32::Networking::WinSock::{LINGER, SO_LINGER, SOL_SOCKET, setsockopt};

    let linger = LINGER {
        l_onoff: 1,
        l_linger: 0,
    };
    let result = unsafe {
        // SAFETY: the socket belongs to `stream`, `linger` remains valid for
        // the call, and its exact size is supplied to setsockopt.
        setsockopt(
            stream.as_raw_socket() as usize,
            SOL_SOCKET,
            SO_LINGER,
            (&linger as *const LINGER).cast(),
            size_of::<LINGER>() as i32,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn relay_network_bmp(
    local: TcpStream,
    remote: TcpStream,
    response_timeout: Duration,
) -> io::Result<()> {
    local.set_nodelay(true)?;

    let mut local_reader = local.try_clone()?;
    let mut remote_writer = remote.try_clone()?;
    let mut remote_reader = remote.try_clone()?;
    let mut local_writer = local.try_clone()?;
    let local_stop = local.try_clone()?;
    let pending_response = Arc::new(Mutex::new(None));
    let response_pending = Arc::clone(&pending_response);
    let closing = Arc::new(AtomicBool::new(false));
    let response_closing = Arc::clone(&closing);
    let reverse = thread::Builder::new()
        .name("wavecraft-network-bmp-proxy-reply".to_owned())
        .spawn(move || {
            let result = relay_network_bmp_responses(
                &mut remote_reader,
                &mut local_writer,
                &response_pending,
                &response_closing,
                response_timeout,
            );
            let _ = local_stop.shutdown(Shutdown::Both);
            result
        })?;

    let forward =
        relay_network_bmp_requests(&mut local_reader, &mut remote_writer, &pending_response);
    closing.store(true, Ordering::Release);
    let _ = local.shutdown(Shutdown::Both);
    let reverse = reverse
        .join()
        .map_err(|_| io::Error::other("network BMP response relay panicked"))?;
    forward.and(reverse)
}

fn relay_network_bmp_requests(
    local: &mut TcpStream,
    remote: &mut TcpStream,
    pending_response: &Mutex<Option<Instant>>,
) -> io::Result<()> {
    let mut buffer = [0_u8; 1024];
    loop {
        let count = local.read(&mut buffer)?;
        if count == 0 {
            return Ok(());
        }
        let mut pending = pending_response
            .lock()
            .map_err(|_| io::Error::other("network BMP request state is unavailable"))?;
        pending.get_or_insert_with(Instant::now);
        drop(pending);
        remote.write_all(&buffer[..count])?;
    }
}

fn relay_network_bmp_responses(
    remote: &mut TcpStream,
    local: &mut TcpStream,
    pending_response: &Mutex<Option<Instant>>,
    closing: &AtomicBool,
    response_timeout: Duration,
) -> io::Result<()> {
    let poll_interval = response_timeout
        .min(NETWORK_BMP_TIMEOUT_POLL_INTERVAL)
        .max(Duration::from_millis(1));
    remote.set_read_timeout(Some(poll_interval))?;
    let mut buffer = [0_u8; 2048];
    loop {
        match remote.read(&mut buffer) {
            Ok(0) => return Ok(()),
            Ok(count) => {
                local.write_all(&buffer[..count])?;
                if buffer[..count].contains(&b'#') {
                    *pending_response.lock().map_err(|_| {
                        io::Error::other("network BMP response state is unavailable")
                    })? = None;
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                if closing.load(Ordering::Acquire) {
                    return Ok(());
                }
                let timed_out = pending_response
                    .lock()
                    .map_err(|_| io::Error::other("network BMP response state is unavailable"))?
                    .is_some_and(|started| started.elapsed() >= response_timeout);
                if timed_out {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!(
                            "network BMP did not respond within {:.1} s",
                            response_timeout.as_secs_f32()
                        ),
                    ));
                }
            }
            Err(error) => return Err(error),
        }
    }
}

fn network_jlink_endpoint(
    selector: &ProbeSelector,
) -> Result<Option<std::net::SocketAddr>, DebugProbeError> {
    let Some(endpoint) = selector
        .connection_id
        .strip_prefix(JLINK_NETWORK_CONNECTION_ID_PREFIX)
    else {
        return Ok(None);
    };
    endpoint.parse().map(Some).map_err(|error| {
        DebugProbeError::new(
            DebugProbeErrorKind::InvalidConfiguration,
            format!("invalid network J-Link endpoint {endpoint:?}: {error}"),
        )
    })
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
        let rtt_target = RttTarget::from(&core);
        let mut rtt = try_attach_to_rtt(&mut core, &rtt_target, configuration.timeout, &region)
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
    let ProbeTransport::Usb(transport) = &information.transport;

    ProbeDescriptor {
        connection_id: connection_id(information),
        provider,
        vendor: vendor.map(str::to_owned),
        model: information.identifier.clone(),
        serial_number: transport.serial_number.clone(),
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
    DebugProbeError::new(
        kind,
        format!("failed to open debug probe: {}", describe_error_chain(&error)),
    )
}

fn describe_error_chain(error: &(dyn StdError + 'static)) -> String {
    let mut description = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        description.push_str(": ");
        description.push_str(&cause.to_string());
        source = cause.source();
    }
    description
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
    use std::io::{Read, Write};
    use std::time::Duration;

    #[derive(Debug)]
    struct TestProbeError(io::Error);

    impl std::fmt::Display for TestProbeError {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("probe wrapper")
        }
    }

    impl std::error::Error for TestProbeError {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            Some(&self.0)
        }
    }

    #[test]
    fn error_descriptions_include_every_source() {
        let error = TestProbeError(io::Error::new(
            io::ErrorKind::ConnectionReset,
            "TCP peer closed the connection",
        ));

        assert_eq!(
            describe_error_chain(&error),
            "probe wrapper: TCP peer closed the connection"
        );
    }

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
            let ProbeTransport::Usb(transport) = &information.transport;
            assert_eq!(descriptor.serial_number, transport.serial_number);
            assert_eq!(
                descriptor.connection_id,
                DebugProbeSelector::from(&information).to_string()
            );
        }
    }

    #[test]
    fn lists_a_network_black_magic_probe_from_its_selector() {
        let selector: DebugProbeSelector = "1d50:6018:192.0.2.42:2000"
            .parse()
            .expect("network BMP selector is valid");
        let information = Lister::new()
            .list(Some(&selector))
            .into_iter()
            .find(|probe| probe.is_probe_type::<BlackMagicProbeFactory>())
            .expect("network BMP is listed from its selector");

        assert_eq!(
            probe_descriptor(&information),
            ProbeDescriptor {
                connection_id: "1d50:6018:192.0.2.42:2000".to_owned(),
                provider: ProbeProvider::BlackMagicProbe,
                vendor: Some("Black Magic Debug".to_owned()),
                model: "192.0.2.42:2000".to_owned(),
                serial_number: Some("192.0.2.42:2000".to_owned()),
            }
        );
    }

    #[test]
    fn lists_a_network_cmsis_dap_probe_from_its_selector() {
        let selector: DebugProbeSelector = "0000:0001:192.0.2.44:4441"
            .parse()
            .expect("network CMSIS-DAP selector is valid");
        let information = Lister::new()
            .list(Some(&selector))
            .into_iter()
            .find(|probe| probe.is_probe_type::<CmsisDapFactory>())
            .expect("network CMSIS-DAP is listed from its selector");

        assert_eq!(
            probe_descriptor(&information),
            ProbeDescriptor {
                connection_id: "0000:0001:192.0.2.44:4441".to_owned(),
                provider: ProbeProvider::CmsisDap,
                vendor: Some("Arm CMSIS-DAP".to_owned()),
                model: "CMSIS-DAP TCP".to_owned(),
                serial_number: Some("192.0.2.44:4441".to_owned()),
            }
        );
    }

    #[test]
    fn recognizes_network_cmsis_dap_connection_ids() {
        let selector = ProbeSelector::connection_id(format!(
            "{CMSIS_DAP_NETWORK_CONNECTION_ID_PREFIX}[2001:db8::44]:4987"
        ));

        assert_eq!(
            network_cmsis_dap_endpoint(&selector).expect("network selector is valid"),
            Some(
                "[2001:db8::44]:4987"
                    .parse()
                    .expect("socket address is valid")
            )
        );
        assert!(
            network_cmsis_dap_endpoint(&ProbeSelector::connection_id("0d28:0204:C123"))
                .expect("USB selector is valid")
                .is_none()
        );
        assert!(
            network_cmsis_dap_endpoint(&ProbeSelector::connection_id(format!(
                "{CMSIS_DAP_NETWORK_CONNECTION_ID_PREFIX}not-an-endpoint"
            )))
            .is_err()
        );
    }

    #[test]
    fn recognizes_network_black_magic_connection_ids() {
        assert_eq!(
            network_bmp_endpoint(&ProbeSelector::connection_id("1d50:6018:192.0.2.42:2000")),
            Some("192.0.2.42:2000".parse().expect("socket address is valid"))
        );
        assert_eq!(
            network_bmp_endpoint(&ProbeSelector::connection_id("1d50:6018:B123")),
            None
        );
        assert_eq!(
            network_bmp_endpoint(&ProbeSelector::connection_id("1366:0105:J123")),
            None
        );
    }

    #[test]
    fn network_bmp_proxy_keeps_an_idle_session_open_and_relays_data_in_both_directions() {
        let remote_listener =
            TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("test remote endpoint binds");
        let remote_endpoint = remote_listener
            .local_addr()
            .expect("test remote endpoint has an address");
        let remote = thread::spawn(move || {
            let (mut stream, _) = remote_listener.accept().expect("proxy connects to remote");
            let mut request = [0; 4];
            stream
                .read_exact(&mut request)
                .expect("request reaches remote");
            assert_eq!(&request, b"ping");
            stream.write_all(b"pong#").expect("remote writes reply");
        });

        let proxy_endpoint = start_network_bmp_proxy(
            remote_endpoint,
            Duration::from_secs(1),
            Duration::from_millis(50),
        )
        .expect("network BMP proxy starts");
        let mut client = TcpStream::connect(proxy_endpoint).expect("client connects to proxy");
        client
            .set_read_timeout(Some(Duration::from_secs(1)))
            .expect("test read timeout is configured");
        thread::sleep(Duration::from_millis(150));
        client.write_all(b"ping").expect("client writes request");
        let mut reply = [0; 5];
        client.read_exact(&mut reply).expect("reply reaches client");

        assert_eq!(&reply, b"pong#");
        remote.join().expect("test remote exits cleanly");
    }

    #[test]
    fn network_bmp_proxy_closes_an_unanswered_request_after_the_response_timeout() {
        let remote_listener =
            TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("test remote endpoint binds");
        let remote_endpoint = remote_listener
            .local_addr()
            .expect("test remote endpoint has an address");
        let remote = thread::spawn(move || {
            let (mut stream, _) = remote_listener.accept().expect("proxy connects to remote");
            let mut request = [0; 4];
            stream
                .read_exact(&mut request)
                .expect("request reaches remote");
            assert_eq!(&request, b"ping");
            let mut closed = [0; 1];
            let result = stream.read(&mut closed);
            assert!(matches!(result, Ok(0) | Err(_)));
        });

        let proxy_endpoint = start_network_bmp_proxy(
            remote_endpoint,
            Duration::from_secs(1),
            Duration::from_millis(50),
        )
        .expect("network BMP proxy starts");
        let mut client = TcpStream::connect(proxy_endpoint).expect("client connects to proxy");
        client
            .set_read_timeout(Some(Duration::from_secs(1)))
            .expect("test read timeout is configured");
        client.write_all(b"ping").expect("client writes request");
        let mut reply = [0; 1];

        assert!(client.read_exact(&mut reply).is_err());
        remote.join().expect("test remote exits cleanly");
    }

    #[test]
    fn recognizes_network_jlink_connection_ids() {
        let selector = ProbeSelector::connection_id(format!(
            "{JLINK_NETWORK_CONNECTION_ID_PREFIX}[2001:db8::43]:19020"
        ));

        assert_eq!(
            network_jlink_endpoint(&selector).expect("network selector is valid"),
            Some(
                "[2001:db8::43]:19020"
                    .parse()
                    .expect("socket address is valid")
            )
        );
        assert!(
            network_jlink_endpoint(&ProbeSelector::connection_id("1366:0105:1234"))
                .expect("USB selector is valid")
                .is_none()
        );
        assert!(
            network_jlink_endpoint(&ProbeSelector::connection_id(format!(
                "{JLINK_NETWORK_CONNECTION_ID_PREFIX}not-an-endpoint"
            )))
            .is_err()
        );
    }

    #[test]
    fn filters_target_architectures_by_probe_backend() {
        for arm_core in [
            config::CoreType::Armv6m,
            config::CoreType::Armv7m,
            config::CoreType::Armv7em,
            config::CoreType::Armv8m,
        ] {
            assert!(provider_supports_core(&ProbeProvider::JLink, arm_core));
            assert!(provider_supports_core(&ProbeProvider::StLink, arm_core));
            assert!(provider_supports_core(
                &ProbeProvider::BlackMagicProbe,
                arm_core
            ));
            assert!(provider_supports_core(&ProbeProvider::CmsisDap, arm_core));
        }
        for non_arm_core in [
            config::CoreType::Riscv,
            config::CoreType::Riscv64,
            config::CoreType::Xtensa,
        ] {
            assert!(provider_supports_core(&ProbeProvider::JLink, non_arm_core));
            assert!(!provider_supports_core(
                &ProbeProvider::StLink,
                non_arm_core
            ));
            assert!(provider_supports_core(
                &ProbeProvider::BlackMagicProbe,
                non_arm_core
            ));
            assert!(provider_supports_core(
                &ProbeProvider::CmsisDap,
                non_arm_core
            ));
        }
    }

    #[test]
    fn exposes_sorted_provider_specific_target_groups() {
        let groups = ProbeRsDriver::supported_target_groups();

        assert_eq!(groups.len(), 4);
        assert_eq!(groups[0].provider, ProbeProvider::JLink);
        assert_eq!(groups[1].provider, ProbeProvider::StLink);
        assert_eq!(groups[2].provider, ProbeProvider::BlackMagicProbe);
        assert_eq!(groups[3].provider, ProbeProvider::CmsisDap);
        assert!(groups.iter().all(|group| !group.targets.is_empty()));
        assert!(
            groups[3]
                .targets
                .iter()
                .any(|target| target.name == "nRF52832_xxAA")
        );
        for group in groups {
            assert!(group.targets.windows(2).all(|targets| {
                targets[0].name.to_ascii_lowercase() <= targets[1].name.to_ascii_lowercase()
            }));
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
