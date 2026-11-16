//! Optional SEGGER J-Link DLL transport for probes reached over TCP/IP.

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Instant;

use libloading::Library;
use wavecraft_debug_probe::{
    AttachConfiguration, DebugProbeError, DebugProbeErrorKind, DebugProbeHandle, DebugSession,
    ProbeDescriptor, ProbeProvider, RttChannelDescriptor, RttChannelDirection, RttConfiguration,
    RttControlBlockLocation, TargetSelector, WireProtocol,
};

const JLINK_LIBRARY_ENVIRONMENT_VARIABLE: &str = "WAVECRAFT_JLINK_LIBRARY";
const RTT_CONTROL_START: u32 = 0;
const RTT_CONTROL_STOP: u32 = 1;
const RTT_CONTROL_GET_DESCRIPTOR: u32 = 2;
const RTT_CONTROL_GET_BUFFER_COUNT: u32 = 3;
const RTT_DIRECTION_UP: u32 = 0;
const RTT_DIRECTION_DOWN: u32 = 1;
const JLINK_INTERFACE_JTAG: c_int = 0;
const JLINK_INTERFACE_SWD: c_int = 1;

type LogCallback = Option<unsafe extern "system" fn(*const c_char)>;
type SelectIp = unsafe extern "system" fn(*const c_char, c_int) -> i8;
type OpenEx = unsafe extern "system" fn(LogCallback, LogCallback) -> *const c_char;
type Close = unsafe extern "system" fn();
type ExecCommand = unsafe extern "system" fn(*const c_char, *mut c_char, c_int) -> c_int;
type SelectTargetInterface = unsafe extern "system" fn(c_int) -> c_int;
type SetSpeed = unsafe extern "system" fn(u32);
type Connect = unsafe extern "system" fn() -> c_int;
type Reset = unsafe extern "system" fn() -> c_int;
type ReadMemory = unsafe extern "system" fn(u32, u32, *mut c_void) -> c_int;
type WriteMemory = unsafe extern "system" fn(u32, u32, *const c_void) -> c_int;
type RttControl = unsafe extern "system" fn(u32, *mut c_void) -> c_int;
type RttRead = unsafe extern "system" fn(u32, *mut c_char, u32) -> c_int;
type RttWrite = unsafe extern "system" fn(u32, *const c_char, u32) -> c_int;

struct JLinkApi {
    select_ip: SelectIp,
    open_ex: OpenEx,
    close: Close,
    exec_command: ExecCommand,
    select_target_interface: SelectTargetInterface,
    set_speed: SetSpeed,
    connect: Connect,
    reset: Reset,
    read_memory: ReadMemory,
    write_memory: WriteMemory,
    rtt_control: RttControl,
    rtt_read: RttRead,
    rtt_write: RttWrite,
    _library: Library,
}

impl JLinkApi {
    fn load() -> Result<Self, DebugProbeError> {
        let mut failures = Vec::new();
        for candidate in library_candidates() {
            // SAFETY: The library remains owned by `JLinkApi` for longer than every copied
            // function pointer, and every symbol below uses its documented C ABI signature.
            let library = match unsafe { Library::new(&candidate) } {
                Ok(library) => library,
                Err(error) => {
                    failures.push(format!("{}: {error}", candidate.display()));
                    continue;
                }
            };
            // SAFETY: Symbol names and signatures match the SEGGER J-Link SDK API.
            let loaded = unsafe { Self::from_library(library) };
            match loaded {
                Ok(api) => return Ok(api),
                Err(error) => failures.push(format!("{}: {error}", candidate.display())),
            }
        }
        Err(DebugProbeError::new(
            DebugProbeErrorKind::Unsupported,
            format!(
                "SEGGER J-Link software is required for network J-Link connections; install it or set {JLINK_LIBRARY_ENVIRONMENT_VARIABLE}{}",
                failures
                    .last()
                    .map_or_else(String::new, |failure| format!(" ({failure})"))
            ),
        ))
    }

    unsafe fn from_library(library: Library) -> Result<Self, libloading::Error> {
        Ok(Self {
            // SAFETY: Each symbol uses the documented SEGGER J-Link SDK signature.
            select_ip: unsafe { load_symbol(&library, b"JLINKARM_SelectIP\0")? },
            // SAFETY: See above.
            open_ex: unsafe { load_symbol(&library, b"JLINKARM_OpenEx\0")? },
            // SAFETY: See above.
            close: unsafe { load_symbol(&library, b"JLINKARM_Close\0")? },
            // SAFETY: See above.
            exec_command: unsafe { load_symbol(&library, b"JLINKARM_ExecCommand\0")? },
            // SAFETY: See above.
            select_target_interface: unsafe { load_symbol(&library, b"JLINKARM_TIF_Select\0")? },
            // SAFETY: See above.
            set_speed: unsafe { load_symbol(&library, b"JLINKARM_SetSpeed\0")? },
            // SAFETY: See above.
            connect: unsafe { load_symbol(&library, b"JLINKARM_Connect\0")? },
            // SAFETY: See above.
            reset: unsafe { load_symbol(&library, b"JLINKARM_Reset\0")? },
            // SAFETY: See above.
            read_memory: unsafe { load_symbol(&library, b"JLINKARM_ReadMem\0")? },
            // SAFETY: See above.
            write_memory: unsafe { load_symbol(&library, b"JLINKARM_WriteMem\0")? },
            // SAFETY: See above.
            rtt_control: unsafe { load_symbol(&library, b"JLINK_RTTERMINAL_Control\0")? },
            // SAFETY: See above.
            rtt_read: unsafe { load_symbol(&library, b"JLINK_RTTERMINAL_Read\0")? },
            // SAFETY: See above.
            rtt_write: unsafe { load_symbol(&library, b"JLINK_RTTERMINAL_Write\0")? },
            _library: library,
        })
    }
}

unsafe fn load_symbol<T: Copy>(library: &Library, name: &[u8]) -> Result<T, libloading::Error> {
    // SAFETY: The caller supplies the ABI-compatible type for `name`, and retains `library`.
    unsafe { library.get::<T>(name).map(|symbol| *symbol) }
}

fn library_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(configured) = std::env::var_os(JLINK_LIBRARY_ENVIRONMENT_VARIABLE) {
        candidates.push(PathBuf::from(configured));
    }
    #[cfg(target_os = "macos")]
    candidates.extend([
        PathBuf::from("/Applications/SEGGER/JLink/libjlinkarm.dylib"),
        PathBuf::from("/usr/local/lib/libjlinkarm.dylib"),
        PathBuf::from("libjlinkarm.dylib"),
    ]);
    #[cfg(target_os = "linux")]
    candidates.extend([
        PathBuf::from("/opt/SEGGER/JLink/libjlinkarm.so"),
        PathBuf::from("/usr/lib/libjlinkarm.so"),
        PathBuf::from("libjlinkarm.so"),
    ]);
    #[cfg(target_os = "windows")]
    candidates.extend([
        PathBuf::from("JLink_x64.dll"),
        PathBuf::from("JLinkARM.dll"),
    ]);
    candidates
}

pub(crate) fn open(
    endpoint: SocketAddr,
    connection_id: String,
) -> Result<Box<dyn DebugProbeHandle>, DebugProbeError> {
    let api = JLinkApi::load()?;
    let host = CString::new(endpoint.ip().to_string()).map_err(|error| {
        DebugProbeError::new(
            DebugProbeErrorKind::InvalidConfiguration,
            format!("invalid J-Link network address: {error}"),
        )
    })?;
    // SAFETY: `host` is a live NUL-terminated string and the loaded function signature is valid.
    let select_result = unsafe { (api.select_ip)(host.as_ptr(), i32::from(endpoint.port())) };
    if select_result != 0 {
        return Err(transport_error(format!(
            "failed to select network J-Link at {endpoint}"
        )));
    }
    // SAFETY: Null callbacks are supported by the SDK; the API object owns the library.
    let open_error = unsafe { (api.open_ex)(None, None) };
    if !open_error.is_null() {
        // SAFETY: A non-null error from `JLINKARM_OpenEx` is a NUL-terminated SDK string.
        let detail = unsafe { CStr::from_ptr(open_error) }
            .to_string_lossy()
            .into_owned();
        // SAFETY: SEGGER requires closing a partially opened DLL before releasing it.
        unsafe { (api.close)() };
        return Err(transport_error(format!(
            "failed to open network J-Link at {endpoint}: {detail}"
        )));
    }

    Ok(Box::new(JLinkNetworkHandle {
        descriptor: ProbeDescriptor {
            connection_id,
            provider: ProbeProvider::JLink,
            vendor: Some("SEGGER".to_owned()),
            model: "J-Link over network".to_owned(),
            serial_number: Some(endpoint.to_string()),
        },
        connection: Some(JLinkConnection {
            api,
            rtt_started: false,
        }),
    }))
}

struct JLinkNetworkHandle {
    descriptor: ProbeDescriptor,
    connection: Option<JLinkConnection>,
}

impl DebugProbeHandle for JLinkNetworkHandle {
    fn descriptor(&self) -> &ProbeDescriptor {
        &self.descriptor
    }

    fn attach(
        mut self: Box<Self>,
        configuration: &AttachConfiguration,
    ) -> Result<Box<dyn DebugSession>, DebugProbeError> {
        if configuration.core_index != 0 {
            return Err(DebugProbeError::new(
                DebugProbeErrorKind::Unsupported,
                "network J-Link currently supports core index 0 only",
            ));
        }
        let target = match &configuration.target {
            TargetSelector::Auto => {
                return Err(DebugProbeError::new(
                    DebugProbeErrorKind::InvalidConfiguration,
                    "select a target MCU when using J-Link over the network",
                ));
            }
            TargetSelector::Name(target) => target,
        };
        let connection = self.connection.as_mut().ok_or_else(|| {
            DebugProbeError::new(
                DebugProbeErrorKind::Transport,
                "J-Link connection is closed",
            )
        })?;
        connection.exec_command(&format!("Device = {target}"))?;
        let interface = match configuration.wire_protocol.unwrap_or(WireProtocol::Swd) {
            WireProtocol::Swd => JLINK_INTERFACE_SWD,
            WireProtocol::Jtag => JLINK_INTERFACE_JTAG,
        };
        // SAFETY: The DLL is open and `interface` is a documented target-interface constant.
        if unsafe { (connection.api.select_target_interface)(interface) } < 0 {
            return Err(transport_error(
                "failed to select the J-Link target interface",
            ));
        }
        if let Some(speed_khz) = configuration.speed_khz {
            // SAFETY: The DLL is open and accepts speed in kHz.
            unsafe { (connection.api.set_speed)(speed_khz) };
        }
        // SAFETY: Device, interface, and speed have been configured on the open DLL.
        if unsafe { (connection.api.connect)() } < 0 {
            return Err(DebugProbeError::new(
                DebugProbeErrorKind::Target,
                format!("failed to attach network J-Link to target {target}"),
            ));
        }
        let connection = self.connection.take().ok_or_else(|| {
            DebugProbeError::new(
                DebugProbeErrorKind::Transport,
                "J-Link connection is closed",
            )
        })?;
        Ok(Box::new(JLinkNetworkSession { connection }))
    }
}

struct JLinkNetworkSession {
    connection: JLinkConnection,
}

impl DebugSession for JLinkNetworkSession {
    fn reset_target(&mut self) -> Result<(), DebugProbeError> {
        self.connection.stop_rtt();
        // SAFETY: The DLL has an attached target.
        if unsafe { (self.connection.api.reset)() } < 0 {
            Err(DebugProbeError::new(
                DebugProbeErrorKind::Target,
                "failed to reset target through network J-Link",
            ))
        } else {
            Ok(())
        }
    }

    fn read_memory(&mut self, address: u64, destination: &mut [u8]) -> Result<(), DebugProbeError> {
        let address = u32::try_from(address).map_err(|_| unsupported_address("read", address))?;
        let length = u32::try_from(destination.len())
            .map_err(|_| transport_error("J-Link memory read is too large"))?;
        // SAFETY: `destination` is valid for `length` writable bytes for the duration of the call.
        let result = unsafe {
            (self.connection.api.read_memory)(address, length, destination.as_mut_ptr().cast())
        };
        if result < 0 {
            Err(transport_error("network J-Link memory read failed"))
        } else {
            Ok(())
        }
    }

    fn write_memory(&mut self, address: u64, source: &[u8]) -> Result<(), DebugProbeError> {
        let address = u32::try_from(address).map_err(|_| unsupported_address("write", address))?;
        let length = u32::try_from(source.len())
            .map_err(|_| transport_error("J-Link memory write is too large"))?;
        // SAFETY: `source` is valid for `length` readable bytes for the duration of the call.
        let result =
            unsafe { (self.connection.api.write_memory)(address, length, source.as_ptr().cast()) };
        if result < 0 {
            Err(transport_error("network J-Link memory write failed"))
        } else {
            Ok(())
        }
    }

    fn attach_rtt(
        &mut self,
        configuration: &RttConfiguration,
    ) -> Result<Vec<RttChannelDescriptor>, DebugProbeError> {
        self.connection
            .configure_rtt_location(&configuration.control_block)?;
        // SAFETY: RTT is started on an open DLL with an attached target; null selects auto/configured search.
        if unsafe { (self.connection.api.rtt_control)(RTT_CONTROL_START, std::ptr::null_mut()) } < 0
        {
            return Err(transport_error(
                "failed to start RTT through network J-Link",
            ));
        }
        self.connection.rtt_started = true;

        let started = Instant::now();
        let (up_count, down_count) = loop {
            let up_count = self.connection.rtt_buffer_count(RTT_DIRECTION_UP);
            let down_count = self.connection.rtt_buffer_count(RTT_DIRECTION_DOWN);
            if let (Ok(up_count), Ok(down_count)) = (up_count, down_count) {
                break (up_count, down_count);
            }
            if started.elapsed() >= configuration.timeout {
                return Err(DebugProbeError::new(
                    DebugProbeErrorKind::Target,
                    "timed out waiting for the J-Link DLL to find the RTT control block",
                ));
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };

        let mut channels = Vec::with_capacity(up_count + down_count);
        for index in 0..up_count {
            channels.push(self.connection.rtt_descriptor(index, RTT_DIRECTION_UP)?);
        }
        for index in 0..down_count {
            channels.push(self.connection.rtt_descriptor(index, RTT_DIRECTION_DOWN)?);
        }
        Ok(channels)
    }

    fn read_rtt_up(
        &mut self,
        channel_number: usize,
        destination: &mut [u8],
    ) -> Result<usize, DebugProbeError> {
        let channel = u32::try_from(channel_number)
            .map_err(|_| transport_error("RTT channel number is too large"))?;
        let length = u32::try_from(destination.len())
            .map_err(|_| transport_error("RTT read is too large"))?;
        // SAFETY: `destination` is valid for `length` writable bytes for the duration of the call.
        let result = unsafe {
            (self.connection.api.rtt_read)(
                channel,
                destination.as_mut_ptr().cast::<c_char>(),
                length,
            )
        };
        rtt_transfer_result(result, destination.len(), "read")
    }

    fn write_rtt_down(
        &mut self,
        channel_number: usize,
        source: &[u8],
    ) -> Result<usize, DebugProbeError> {
        let channel = u32::try_from(channel_number)
            .map_err(|_| transport_error("RTT channel number is too large"))?;
        let length =
            u32::try_from(source.len()).map_err(|_| transport_error("RTT write is too large"))?;
        // SAFETY: `source` is valid for `length` readable bytes for the duration of the call.
        let result = unsafe {
            (self.connection.api.rtt_write)(channel, source.as_ptr().cast::<c_char>(), length)
        };
        rtt_transfer_result(result, source.len(), "write")
    }
}

struct JLinkConnection {
    api: JLinkApi,
    rtt_started: bool,
}

impl JLinkConnection {
    fn exec_command(&self, command: &str) -> Result<(), DebugProbeError> {
        let command = CString::new(command).map_err(|error| {
            DebugProbeError::new(
                DebugProbeErrorKind::InvalidConfiguration,
                format!("invalid J-Link command: {error}"),
            )
        })?;
        let mut output = [0 as c_char; 256];
        // SAFETY: Both buffers are live and NUL-terminated/output-sized as required by the SDK.
        let result = unsafe {
            (self.api.exec_command)(command.as_ptr(), output.as_mut_ptr(), output.len() as c_int)
        };
        if result < 0 {
            let detail = c_char_buffer(&output);
            Err(transport_error(if detail.is_empty() {
                "J-Link command failed".to_owned()
            } else {
                format!("J-Link command failed: {detail}")
            }))
        } else {
            Ok(())
        }
    }

    fn configure_rtt_location(
        &self,
        location: &RttControlBlockLocation,
    ) -> Result<(), DebugProbeError> {
        match location {
            RttControlBlockLocation::Exact(address) => {
                self.exec_command(&format!("SetRTTAddr 0x{address:x}"))
            }
            RttControlBlockLocation::Ranges(ranges) => {
                let ranges = ranges
                    .iter()
                    .filter_map(|range| {
                        let size = range.end.checked_sub(range.start)?;
                        Some(format!("0x{:x} 0x{size:x}", range.start))
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                if ranges.is_empty() {
                    Err(DebugProbeError::new(
                        DebugProbeErrorKind::InvalidConfiguration,
                        "RTT search ranges are empty",
                    ))
                } else {
                    self.exec_command(&format!("SetRTTSearchRanges {ranges}"))
                }
            }
            RttControlBlockLocation::Ram => Ok(()),
        }
    }

    fn rtt_buffer_count(&self, direction: u32) -> Result<usize, DebugProbeError> {
        let mut direction = direction;
        // SAFETY: The command expects a pointer to one direction value.
        let count = unsafe {
            (self.api.rtt_control)(
                RTT_CONTROL_GET_BUFFER_COUNT,
                (&mut direction as *mut u32).cast(),
            )
        };
        usize::try_from(count).map_err(|_| transport_error("RTT control block is not ready"))
    }

    fn rtt_descriptor(
        &self,
        channel_number: usize,
        direction: u32,
    ) -> Result<RttChannelDescriptor, DebugProbeError> {
        let buffer_index = c_int::try_from(channel_number)
            .map_err(|_| transport_error("RTT channel number is too large"))?;
        let mut descriptor = JLinkRttBufferDescriptor {
            buffer_index,
            direction,
            name: [0; 32],
            buffer_size: 0,
            flags: 0,
        };
        // SAFETY: `descriptor` has the documented C representation and remains live for the call.
        if unsafe {
            (self.api.rtt_control)(
                RTT_CONTROL_GET_DESCRIPTOR,
                (&mut descriptor as *mut JLinkRttBufferDescriptor).cast(),
            )
        } < 0
        {
            return Err(transport_error(format!(
                "failed to read RTT channel {channel_number} descriptor"
            )));
        }
        Ok(RttChannelDescriptor {
            number: channel_number,
            direction: if direction == RTT_DIRECTION_UP {
                RttChannelDirection::Up
            } else {
                RttChannelDirection::Down
            },
            name: {
                let name = c_char_buffer(&descriptor.name);
                (!name.is_empty()).then_some(name)
            },
            buffer_size: descriptor.buffer_size as usize,
        })
    }

    fn stop_rtt(&mut self) {
        if self.rtt_started {
            // SAFETY: RTT was started on this live DLL connection.
            let _ = unsafe { (self.api.rtt_control)(RTT_CONTROL_STOP, std::ptr::null_mut()) };
            self.rtt_started = false;
        }
    }
}

impl Drop for JLinkConnection {
    fn drop(&mut self) {
        self.stop_rtt();
        // SAFETY: `JLinkConnection` is created only after `OpenEx` succeeds and closes exactly once.
        unsafe { (self.api.close)() };
    }
}

#[repr(C)]
struct JLinkRttBufferDescriptor {
    buffer_index: c_int,
    direction: u32,
    name: [c_char; 32],
    buffer_size: u32,
    flags: u32,
}

fn c_char_buffer(buffer: &[c_char]) -> String {
    let bytes = buffer
        .iter()
        .take_while(|character| **character != 0)
        .map(|character| *character as u8)
        .collect::<Vec<_>>();
    String::from_utf8_lossy(&bytes).into_owned()
}

fn rtt_transfer_result(
    result: c_int,
    buffer_length: usize,
    operation: &str,
) -> Result<usize, DebugProbeError> {
    let count = usize::try_from(result)
        .map_err(|_| transport_error(format!("network J-Link RTT {operation} failed")))?;
    if count > buffer_length {
        Err(transport_error(format!(
            "network J-Link RTT {operation} returned an invalid byte count"
        )))
    } else {
        Ok(count)
    }
}

fn unsupported_address(operation: &str, address: u64) -> DebugProbeError {
    DebugProbeError::new(
        DebugProbeErrorKind::Unsupported,
        format!("network J-Link cannot {operation} address 0x{address:x} above 32-bit space"),
    )
}

fn transport_error(message: impl Into<String>) -> DebugProbeError {
    DebugProbeError::new(DebugProbeErrorKind::Transport, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn c_character_buffers_stop_at_the_first_nul() {
        assert_eq!(
            c_char_buffer(&[b'R' as c_char, b'T' as c_char, 0, b'T' as c_char]),
            "RT"
        );
    }

    #[test]
    fn transfer_results_reject_errors_and_impossible_counts() {
        assert_eq!(rtt_transfer_result(3, 4, "read"), Ok(3));
        assert!(rtt_transfer_result(-1, 4, "read").is_err());
        assert!(rtt_transfer_result(5, 4, "read").is_err());
    }

    #[test]
    fn loads_the_installed_jlink_sdk_symbols_when_available() {
        if library_candidates()
            .iter()
            .any(|candidate| candidate.is_file())
        {
            JLinkApi::load().expect("installed SEGGER J-Link library exposes required symbols");
        }
    }

    #[test]
    fn rtt_descriptor_matches_the_sdk_layout() {
        assert_eq!(std::mem::size_of::<JLinkRttBufferDescriptor>(), 48);
    }
}
