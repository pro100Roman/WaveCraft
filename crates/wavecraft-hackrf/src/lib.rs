//! Receive-only HackRF One discovery and capture adapter.
//!
//! `libhackrf` is loaded at runtime so the default WaveCraft build and test
//! suite remain usable on machines without HackRF host software installed.

use libloading::Library;
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::path::{Path, PathBuf};
use std::ptr;
use std::slice;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use wavecraft_core::{
    CaptureBatch, CaptureContext, CaptureError, CaptureSource, DeviceDescriptor, DeviceId,
    InstrumentKind, Iq8, IqBatch,
};
use wavecraft_sdr::{
    ConnectionBackend, DiscoveredRadio, GainControlDescriptor, HackRfSettings, RadioCapabilities,
    RadioCaptureSettings, RadioModel, RadioProvider, RadioProviderAdapter, RadioProviderSettings,
    ValueDomain,
};

const HACKRF_ONE_USB_PRODUCT_ID: c_int = 0x6089;
const RECEIVE_QUEUE_CAPACITY: usize = 8;
const RECEIVE_POLL_INTERVAL: Duration = Duration::from_millis(20);
const DEFAULT_LNA_GAIN_DB: u32 = 16;
const DEFAULT_VGA_GAIN_DB: u32 = 16;

const HACKRF_BANDWIDTHS_HZ: [u64; 16] = [
    1_750_000, 2_500_000, 3_500_000, 5_000_000, 5_500_000, 6_000_000, 7_000_000, 8_000_000,
    9_000_000, 10_000_000, 12_000_000, 14_000_000, 15_000_000, 20_000_000, 24_000_000, 28_000_000,
];

/// Capabilities advertised for a HackRF One receive channel.
pub fn hackrf_one_capabilities() -> RadioCapabilities {
    RadioCapabilities {
        frequency_hz: ValueDomain::Stepped {
            range: 1_000_000..=6_000_000_000,
            step: 1,
        },
        sample_rates_hz: ValueDomain::Stepped {
            range: 2_000_000..=20_000_000,
            step: 1,
        },
        bandwidths_hz: ValueDomain::Discrete(HACKRF_BANDWIDTHS_HZ.to_vec()),
        receive_channels: 1,
        gain_controls: vec![
            GainControlDescriptor {
                id: "lna".to_owned(),
                label: "IF (LNA) gain".to_owned(),
                range_db: 0..=40,
                step_db: 8,
                default_db: DEFAULT_LNA_GAIN_DB as i32,
            },
            GainControlDescriptor {
                id: "vga".to_owned(),
                label: "Baseband (VGA) gain".to_owned(),
                range_db: 0..=62,
                step_db: 2,
                default_db: DEFAULT_VGA_GAIN_DB as i32,
            },
        ],
        supports_hardware_timestamps: false,
        supports_external_clock: true,
    }
}

/// HackRF provider adapter. The default instance searches platform library
/// paths; `with_library_path` is useful for non-standard installations.
#[derive(Clone, Debug, Default)]
pub struct HackRfAdapter {
    library_path: Option<PathBuf>,
}

impl HackRfAdapter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_library_path(path: impl Into<PathBuf>) -> Self {
        Self {
            library_path: Some(path.into()),
        }
    }

    fn load_runtime(&self) -> Result<Arc<HackRfRuntime>, CaptureError> {
        HackRfRuntime::load(self.library_path.as_deref()).map(Arc::new)
    }
}

impl RadioProviderAdapter for HackRfAdapter {
    fn provider(&self) -> RadioProvider {
        RadioProvider::HackRf
    }

    fn discover(&self) -> Result<Vec<DiscoveredRadio>, CaptureError> {
        let runtime = self.load_runtime()?;
        runtime.discover()
    }

    fn open(
        &self,
        device: &DiscoveredRadio,
        settings: RadioCaptureSettings,
        provider_settings: RadioProviderSettings,
    ) -> Result<Box<dyn CaptureSource>, CaptureError> {
        validate_device(device)?;
        validate_capture_settings(&settings)?;
        let RadioProviderSettings::HackRf(provider_settings) = provider_settings else {
            return Err(CaptureError::new(
                "HackRF capture requires HackRF provider settings",
            ));
        };
        validate_provider_settings(&provider_settings)?;
        let serial = device.persistent_id.clone().ok_or_else(|| {
            CaptureError::new("HackRF One does not expose a serial number and cannot be reopened")
        })?;
        let runtime = self.load_runtime()?;

        Ok(Box::new(HackRfCaptureSource {
            descriptor: device.descriptor.clone(),
            serial,
            settings,
            provider_settings,
            runtime,
        }))
    }
}

fn validate_device(device: &DiscoveredRadio) -> Result<(), CaptureError> {
    device.validate()?;
    if device.provider != RadioProvider::HackRf
        || device.model != RadioModel::HackRfOne
        || device.connection_backend != ConnectionBackend::Usb
    {
        return Err(CaptureError::new(
            "the selected radio is not a USB HackRF One",
        ));
    }
    Ok(())
}

fn validate_capture_settings(settings: &RadioCaptureSettings) -> Result<(), CaptureError> {
    settings.validate(&hackrf_one_capabilities())
}

fn validate_provider_settings(settings: &HackRfSettings) -> Result<(), CaptureError> {
    let lna_gain_db = settings.lna_gain_db.unwrap_or(DEFAULT_LNA_GAIN_DB);
    if lna_gain_db > 40 || lna_gain_db % 8 != 0 {
        return Err(CaptureError::new(
            "HackRF LNA gain must be between 0 and 40 dB in 8 dB steps",
        ));
    }
    let vga_gain_db = settings.vga_gain_db.unwrap_or(DEFAULT_VGA_GAIN_DB);
    if vga_gain_db > 62 || vga_gain_db % 2 != 0 {
        return Err(CaptureError::new(
            "HackRF VGA gain must be between 0 and 62 dB in 2 dB steps",
        ));
    }
    Ok(())
}

struct HackRfCaptureSource {
    descriptor: DeviceDescriptor,
    serial: String,
    settings: RadioCaptureSettings,
    provider_settings: HackRfSettings,
    runtime: Arc<HackRfRuntime>,
}

impl CaptureSource for HackRfCaptureSource {
    fn descriptor(&self) -> DeviceDescriptor {
        self.descriptor.clone()
    }

    fn run(self: Box<Self>, context: CaptureContext) -> Result<(), CaptureError> {
        let source = *self;
        let serial = CString::new(source.serial.as_str())
            .map_err(|_| CaptureError::new("HackRF serial number contains a NUL byte"))?;
        let device = source.runtime.open_by_serial(&serial)?;
        source.configure(device.pointer())?;
        context.synchronize_start()?;

        let (sender, receiver) = mpsc::sync_channel(RECEIVE_QUEUE_CAPACITY);
        let mut callback_state = Box::new(RxCallbackState::new(
            sender,
            context.elapsed(),
            source.settings.sample_rate_hz,
        ));
        let callback_context = callback_state.as_mut() as *mut RxCallbackState as *mut c_void;
        let stream =
            source
                .runtime
                .start_rx(device.pointer(), receive_callback, callback_context)?;

        let capture_result = source.receive(&context, &receiver, &callback_state, device.pointer());
        callback_state.stop_requested.store(true, Ordering::Release);
        let stop_result = stream.stop();
        let close_result = device.close();

        capture_result.and(stop_result).and(close_result)
    }
}

impl HackRfCaptureSource {
    fn configure(&self, device: *mut HackRfDevice) -> Result<(), CaptureError> {
        self.runtime
            .set_sample_rate(device, self.settings.sample_rate_hz as f64)?;
        self.runtime
            .set_bandwidth(device, self.settings.bandwidth_hz as u32)?;
        self.runtime
            .set_frequency(device, self.settings.center_frequency_hz)?;
        self.runtime.set_lna_gain(
            device,
            self.provider_settings
                .lna_gain_db
                .unwrap_or(DEFAULT_LNA_GAIN_DB),
        )?;
        self.runtime.set_vga_gain(
            device,
            self.provider_settings
                .vga_gain_db
                .unwrap_or(DEFAULT_VGA_GAIN_DB),
        )?;
        self.runtime
            .set_amplifier(device, self.provider_settings.amplifier_enabled)
    }

    fn receive(
        &self,
        context: &CaptureContext,
        receiver: &Receiver<ReceivedBlock>,
        callback_state: &RxCallbackState,
        device: *mut HackRfDevice,
    ) -> Result<(), CaptureError> {
        let mut first_sample_index = 0_u64;
        let mut sequence = 0_u64;

        while !context.is_cancelled() {
            if let Some(error) = callback_state.failure() {
                return Err(CaptureError::new(error));
            }

            match receiver.recv_timeout(RECEIVE_POLL_INTERVAL) {
                Ok(block) => {
                    let remaining = self
                        .settings
                        .sample_limit
                        .map_or(u64::MAX, |limit| limit.saturating_sub(first_sample_index));
                    if remaining == 0 {
                        break;
                    }
                    let sample_count = remaining.min((block.bytes.len() / 2) as u64) as usize;
                    let samples = block.bytes[..sample_count * 2]
                        .chunks_exact(2)
                        .map(|sample| Iq8 {
                            i: sample[0] as i8,
                            q: sample[1] as i8,
                        })
                        .collect::<Vec<_>>();
                    let emit_result = context.emit(
                        sequence,
                        block.captured_at,
                        CaptureBatch::Iq(IqBatch {
                            sample_rate_hz: self.settings.sample_rate_hz,
                            center_frequency_hz: self.settings.center_frequency_hz,
                            first_sample_index,
                            discontinuity: false,
                            samples,
                        }),
                    );
                    callback_state.recycle_buffer(block.bytes);
                    emit_result?;
                    first_sample_index += sample_count as u64;
                    sequence += 1;
                    if self
                        .settings
                        .sample_limit
                        .is_some_and(|limit| first_sample_index >= limit)
                    {
                        break;
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    if let Some(error) = callback_state.failure() {
                        return Err(CaptureError::new(error));
                    }
                    if !self.runtime.is_streaming(device)? {
                        return Err(CaptureError::new(
                            "HackRF receive stream stopped unexpectedly",
                        ));
                    }
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(CaptureError::new("HackRF receive callback disconnected"));
                }
            }
        }
        Ok(())
    }
}

struct ReceivedBlock {
    captured_at: Duration,
    bytes: Vec<u8>,
}

struct RxCallbackState {
    sender: SyncSender<ReceivedBlock>,
    buffer_pool: Mutex<Vec<Vec<u8>>>,
    failure: Mutex<Option<String>>,
    stop_requested: AtomicBool,
    monotonic_origin: Instant,
    timeline_offset: Duration,
    sample_rate_hz: u64,
}

impl RxCallbackState {
    fn new(
        sender: SyncSender<ReceivedBlock>,
        timeline_offset: Duration,
        sample_rate_hz: u64,
    ) -> Self {
        Self {
            sender,
            buffer_pool: Mutex::new((0..=RECEIVE_QUEUE_CAPACITY).map(|_| Vec::new()).collect()),
            failure: Mutex::new(None),
            stop_requested: AtomicBool::new(false),
            monotonic_origin: Instant::now(),
            timeline_offset,
            sample_rate_hz,
        }
    }

    fn fail(&self, message: impl Into<String>) {
        let mut failure = self
            .failure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if failure.is_none() {
            *failure = Some(message.into());
        }
        self.stop_requested.store(true, Ordering::Release);
    }

    fn failure(&self) -> Option<String> {
        self.failure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn take_buffer(&self) -> Option<Vec<u8>> {
        self.buffer_pool
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pop()
    }

    fn recycle_buffer(&self, mut buffer: Vec<u8>) {
        buffer.clear();
        self.buffer_pool
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(buffer);
    }
}

unsafe extern "C" fn receive_callback(transfer: *mut HackRfTransfer) -> c_int {
    std::panic::catch_unwind(|| receive_callback_inner(transfer)).unwrap_or(1)
}

fn receive_callback_inner(transfer: *mut HackRfTransfer) -> c_int {
    if transfer.is_null() {
        return 1;
    }
    // SAFETY: libhackrf owns `transfer` for the duration of this callback.
    let transfer = unsafe { &*transfer };
    if transfer.rx_context.is_null() {
        return 1;
    }
    // SAFETY: `rx_context` points to the pinned callback state kept alive until
    // `hackrf_stop_rx` has joined the transfer thread.
    let state = unsafe { &*(transfer.rx_context as *const RxCallbackState) };
    if state.stop_requested.load(Ordering::Acquire) {
        return 1;
    }
    if transfer.valid_length < 0 || transfer.valid_length % 2 != 0 {
        state.fail("HackRF returned an invalid odd-length IQ transfer");
        return 1;
    }
    let byte_count = transfer.valid_length as usize;
    if byte_count == 0 {
        return 0;
    }
    if transfer.buffer.is_null() {
        state.fail("HackRF returned a null receive buffer");
        return 1;
    }

    let Some(mut bytes) = state.take_buffer() else {
        state.fail("HackRF receive buffer pool was exhausted");
        return 1;
    };
    // SAFETY: libhackrf guarantees that `valid_length` bytes are readable from
    // the transfer buffer while the callback is active.
    bytes.extend_from_slice(unsafe { slice::from_raw_parts(transfer.buffer, byte_count) });
    let sample_count = byte_count / 2;
    let block_duration = Duration::from_secs_f64(sample_count as f64 / state.sample_rate_hz as f64);
    let captured_at = state
        .timeline_offset
        .saturating_add(state.monotonic_origin.elapsed())
        .saturating_sub(block_duration);
    match state.sender.try_send(ReceivedBlock { captured_at, bytes }) {
        Ok(()) => 0,
        Err(TrySendError::Full(block)) => {
            state.recycle_buffer(block.bytes);
            state.fail("HackRF receive queue overflowed; IQ capture cannot remain lossless");
            1
        }
        Err(TrySendError::Disconnected(block)) => {
            state.recycle_buffer(block.bytes);
            state.stop_requested.store(true, Ordering::Release);
            1
        }
    }
}

#[repr(C)]
struct HackRfDevice {
    _private: [u8; 0],
}

#[repr(C)]
struct HackRfTransfer {
    device: *mut HackRfDevice,
    buffer: *mut u8,
    buffer_length: c_int,
    valid_length: c_int,
    rx_context: *mut c_void,
    tx_context: *mut c_void,
}

#[repr(C)]
struct HackRfDeviceList {
    serial_numbers: *mut *mut c_char,
    usb_board_ids: *mut c_int,
    usb_device_index: *mut c_int,
    device_count: c_int,
    usb_devices: *mut *mut c_void,
    usb_device_count: c_int,
}

type InitFn = unsafe extern "C" fn() -> c_int;
type ExitFn = unsafe extern "C" fn() -> c_int;
type DeviceListFn = unsafe extern "C" fn() -> *mut HackRfDeviceList;
type DeviceListFreeFn = unsafe extern "C" fn(*mut HackRfDeviceList);
type OpenBySerialFn = unsafe extern "C" fn(*const c_char, *mut *mut HackRfDevice) -> c_int;
type CloseFn = unsafe extern "C" fn(*mut HackRfDevice) -> c_int;
type StartRxFn = unsafe extern "C" fn(
    *mut HackRfDevice,
    unsafe extern "C" fn(*mut HackRfTransfer) -> c_int,
    *mut c_void,
) -> c_int;
type DeviceFn = unsafe extern "C" fn(*mut HackRfDevice) -> c_int;
type SetFrequencyFn = unsafe extern "C" fn(*mut HackRfDevice, u64) -> c_int;
type SetSampleRateFn = unsafe extern "C" fn(*mut HackRfDevice, f64) -> c_int;
type SetU32Fn = unsafe extern "C" fn(*mut HackRfDevice, u32) -> c_int;
type SetU8Fn = unsafe extern "C" fn(*mut HackRfDevice, u8) -> c_int;
type ErrorNameFn = unsafe extern "C" fn(c_int) -> *const c_char;

struct HackRfApi {
    _library: Library,
    init: InitFn,
    exit: ExitFn,
    device_list: DeviceListFn,
    device_list_free: DeviceListFreeFn,
    open_by_serial: OpenBySerialFn,
    close: CloseFn,
    start_rx: StartRxFn,
    stop_rx: DeviceFn,
    is_streaming: DeviceFn,
    set_frequency: SetFrequencyFn,
    set_sample_rate: SetSampleRateFn,
    set_bandwidth: SetU32Fn,
    set_lna_gain: SetU32Fn,
    set_vga_gain: SetU32Fn,
    set_amplifier: SetU8Fn,
    error_name: ErrorNameFn,
}

impl HackRfApi {
    fn load(path: Option<&Path>) -> Result<Self, CaptureError> {
        let library = load_library(path)?;
        // SAFETY: every symbol type below matches the official libhackrf C ABI.
        unsafe {
            Ok(Self {
                init: load_symbol(&library, b"hackrf_init\0")?,
                exit: load_symbol(&library, b"hackrf_exit\0")?,
                device_list: load_symbol(&library, b"hackrf_device_list\0")?,
                device_list_free: load_symbol(&library, b"hackrf_device_list_free\0")?,
                open_by_serial: load_symbol(&library, b"hackrf_open_by_serial\0")?,
                close: load_symbol(&library, b"hackrf_close\0")?,
                start_rx: load_symbol(&library, b"hackrf_start_rx\0")?,
                stop_rx: load_symbol(&library, b"hackrf_stop_rx\0")?,
                is_streaming: load_symbol(&library, b"hackrf_is_streaming\0")?,
                set_frequency: load_symbol(&library, b"hackrf_set_freq\0")?,
                set_sample_rate: load_symbol(&library, b"hackrf_set_sample_rate\0")?,
                set_bandwidth: load_symbol(&library, b"hackrf_set_baseband_filter_bandwidth\0")?,
                set_lna_gain: load_symbol(&library, b"hackrf_set_lna_gain\0")?,
                set_vga_gain: load_symbol(&library, b"hackrf_set_vga_gain\0")?,
                set_amplifier: load_symbol(&library, b"hackrf_set_amp_enable\0")?,
                error_name: load_symbol(&library, b"hackrf_error_name\0")?,
                _library: library,
            })
        }
    }

    fn check(&self, operation: &str, code: c_int) -> Result<(), CaptureError> {
        if code == 0 {
            return Ok(());
        }
        // SAFETY: libhackrf returns a static string for every error code.
        let name = unsafe { (self.error_name)(code) };
        let name = if name.is_null() {
            "unknown libhackrf error".to_owned()
        } else {
            // SAFETY: a non-null error name is a NUL-terminated static string.
            unsafe { CStr::from_ptr(name) }
                .to_string_lossy()
                .into_owned()
        };
        Err(CaptureError::new(format!(
            "{operation} failed: {name} ({code})"
        )))
    }
}

unsafe fn load_symbol<T: Copy>(library: &Library, name: &[u8]) -> Result<T, CaptureError> {
    // SAFETY: the caller supplies the exact ABI type for the requested symbol.
    unsafe { library.get::<T>(name) }
        .map(|symbol| *symbol)
        .map_err(|error| {
            CaptureError::new(format!(
                "installed libhackrf is missing {}: {error}",
                String::from_utf8_lossy(&name[..name.len().saturating_sub(1)])
            ))
        })
}

fn load_library(path: Option<&Path>) -> Result<Library, CaptureError> {
    if let Some(path) = path {
        // SAFETY: symbol use is constrained to the declared libhackrf ABI.
        return unsafe { Library::new(path) }.map_err(|error| {
            CaptureError::new(format!(
                "could not load libhackrf from {}: {error}",
                path.display()
            ))
        });
    }

    let mut candidates = Vec::<PathBuf>::new();
    if let Some(path) = std::env::var_os("WAVECRAFT_LIBHACKRF_PATH") {
        candidates.push(path.into());
    }
    candidates.extend(platform_library_candidates().iter().map(PathBuf::from));
    let mut last_error = None;
    for candidate in &candidates {
        // SAFETY: symbol use is constrained to the declared libhackrf ABI.
        match unsafe { Library::new(candidate) } {
            Ok(library) => return Ok(library),
            Err(error) => last_error = Some(error.to_string()),
        }
    }
    Err(CaptureError::new(format!(
        "libhackrf is unavailable; install HackRF host tools or set WAVECRAFT_LIBHACKRF_PATH{}",
        last_error.map_or_else(String::new, |error| format!(" ({error})"))
    )))
}

#[cfg(target_os = "macos")]
fn platform_library_candidates() -> &'static [&'static str] {
    &[
        "libhackrf.0.dylib",
        "libhackrf.dylib",
        "/opt/homebrew/lib/libhackrf.dylib",
        "/usr/local/lib/libhackrf.dylib",
    ]
}

#[cfg(target_os = "linux")]
fn platform_library_candidates() -> &'static [&'static str] {
    &["libhackrf.so.0", "libhackrf.so"]
}

#[cfg(target_os = "windows")]
fn platform_library_candidates() -> &'static [&'static str] {
    &["hackrf.dll", "libhackrf.dll"]
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn platform_library_candidates() -> &'static [&'static str] {
    &["libhackrf.so"]
}

struct HackRfRuntime {
    api: HackRfApi,
}

impl HackRfRuntime {
    fn load(path: Option<&Path>) -> Result<Self, CaptureError> {
        let api = HackRfApi::load(path)?;
        // SAFETY: `hackrf_init` has no arguments and is called before any API use.
        let code = unsafe { (api.init)() };
        api.check("initializing libhackrf", code)?;
        Ok(Self { api })
    }

    fn discover(&self) -> Result<Vec<DiscoveredRadio>, CaptureError> {
        // SAFETY: libhackrf is initialized for this runtime.
        let list = unsafe { (self.api.device_list)() };
        if list.is_null() {
            return Err(CaptureError::new("libhackrf returned a null device list"));
        }
        let list_guard = DeviceListGuard {
            api: &self.api,
            pointer: list,
        };
        // SAFETY: the list remains owned by `list_guard` for this scope.
        let list = unsafe { &*list_guard.pointer };
        if !(0..=1_024).contains(&list.device_count) {
            return Err(CaptureError::new(
                "libhackrf returned an invalid device count",
            ));
        }
        if list.device_count > 0 && (list.serial_numbers.is_null() || list.usb_board_ids.is_null())
        {
            return Err(CaptureError::new(
                "libhackrf returned an incomplete device list",
            ));
        }

        let mut devices = Vec::new();
        for index in 0..list.device_count as usize {
            // SAFETY: both arrays have `device_count` entries per libhackrf's ABI.
            let board_id = unsafe { *list.usb_board_ids.add(index) };
            if board_id != HACKRF_ONE_USB_PRODUCT_ID {
                continue;
            }
            // SAFETY: `serial_numbers` has `device_count` entries; entries may be null.
            let serial_pointer = unsafe { *list.serial_numbers.add(index) };
            let serial = if serial_pointer.is_null() {
                None
            } else {
                // SAFETY: non-null entries are NUL-terminated strings owned by the list.
                let value = unsafe { CStr::from_ptr(serial_pointer) }
                    .to_string_lossy()
                    .into_owned();
                (!value.is_empty()).then_some(value)
            };
            let connection_id = serial.as_ref().map_or_else(
                || format!("hackrf:index-{index}"),
                |value| format!("hackrf:{value}"),
            );
            devices.push(DiscoveredRadio {
                descriptor: DeviceDescriptor {
                    id: DeviceId::new(connection_id),
                    name: "HackRF One".to_owned(),
                    kind: InstrumentKind::RadioReceiver,
                },
                provider: RadioProvider::HackRf,
                model: RadioModel::HackRfOne,
                connection_backend: ConnectionBackend::Usb,
                persistent_id: serial,
                capabilities: hackrf_one_capabilities(),
            });
        }
        Ok(devices)
    }

    fn open_by_serial<'a>(&'a self, serial: &CStr) -> Result<OpenedDevice<'a>, CaptureError> {
        let mut pointer = ptr::null_mut();
        // SAFETY: the serial and output pointers are valid for this call.
        let code = unsafe { (self.api.open_by_serial)(serial.as_ptr(), &mut pointer) };
        self.api.check("opening HackRF One", code)?;
        if pointer.is_null() {
            return Err(CaptureError::new(
                "libhackrf opened a device without returning a handle",
            ));
        }
        Ok(OpenedDevice {
            api: &self.api,
            pointer,
        })
    }

    fn start_rx<'a>(
        &'a self,
        device: *mut HackRfDevice,
        callback: unsafe extern "C" fn(*mut HackRfTransfer) -> c_int,
        context: *mut c_void,
    ) -> Result<ActiveRx<'a>, CaptureError> {
        // SAFETY: the device, callback, and callback context remain valid until stop_rx.
        let code = unsafe { (self.api.start_rx)(device, callback, context) };
        self.api.check("starting HackRF receive stream", code)?;
        Ok(ActiveRx {
            runtime: self,
            device,
            active: true,
        })
    }

    fn stop_rx(&self, device: *mut HackRfDevice) -> Result<(), CaptureError> {
        // SAFETY: `device` is an open libhackrf handle.
        let code = unsafe { (self.api.stop_rx)(device) };
        self.api.check("stopping HackRF receive stream", code)
    }

    fn is_streaming(&self, device: *mut HackRfDevice) -> Result<bool, CaptureError> {
        // SAFETY: `device` is an open libhackrf handle.
        let code = unsafe { (self.api.is_streaming)(device) };
        match code {
            1 => Ok(true),
            0 => Ok(false),
            error => self
                .api
                .check("querying HackRF receive stream", error)
                .map(|()| false),
        }
    }

    fn set_frequency(&self, device: *mut HackRfDevice, value: u64) -> Result<(), CaptureError> {
        // SAFETY: `device` is open and the value was validated against capabilities.
        let code = unsafe { (self.api.set_frequency)(device, value) };
        self.api.check("setting HackRF center frequency", code)
    }

    fn set_sample_rate(&self, device: *mut HackRfDevice, value: f64) -> Result<(), CaptureError> {
        // SAFETY: `device` is open and the value was validated against capabilities.
        let code = unsafe { (self.api.set_sample_rate)(device, value) };
        self.api.check("setting HackRF sample rate", code)
    }

    fn set_bandwidth(&self, device: *mut HackRfDevice, value: u32) -> Result<(), CaptureError> {
        // SAFETY: `device` is open and the value was validated against capabilities.
        let code = unsafe { (self.api.set_bandwidth)(device, value) };
        self.api.check("setting HackRF baseband bandwidth", code)
    }

    fn set_lna_gain(&self, device: *mut HackRfDevice, value: u32) -> Result<(), CaptureError> {
        // SAFETY: `device` is open and the value was validated above.
        let code = unsafe { (self.api.set_lna_gain)(device, value) };
        self.api.check("setting HackRF LNA gain", code)
    }

    fn set_vga_gain(&self, device: *mut HackRfDevice, value: u32) -> Result<(), CaptureError> {
        // SAFETY: `device` is open and the value was validated above.
        let code = unsafe { (self.api.set_vga_gain)(device, value) };
        self.api.check("setting HackRF VGA gain", code)
    }

    fn set_amplifier(&self, device: *mut HackRfDevice, enabled: bool) -> Result<(), CaptureError> {
        // SAFETY: `device` is open and libhackrf accepts 0 or 1.
        let code = unsafe { (self.api.set_amplifier)(device, u8::from(enabled)) };
        self.api.check("setting HackRF RF amplifier", code)
    }
}

impl Drop for HackRfRuntime {
    fn drop(&mut self) {
        // SAFETY: all device handles borrowing this runtime have already closed.
        let _ = unsafe { (self.api.exit)() };
    }
}

struct DeviceListGuard<'a> {
    api: &'a HackRfApi,
    pointer: *mut HackRfDeviceList,
}

impl Drop for DeviceListGuard<'_> {
    fn drop(&mut self) {
        // SAFETY: the pointer came from `hackrf_device_list` and is freed once.
        unsafe { (self.api.device_list_free)(self.pointer) };
    }
}

struct OpenedDevice<'a> {
    api: &'a HackRfApi,
    pointer: *mut HackRfDevice,
}

struct ActiveRx<'a> {
    runtime: &'a HackRfRuntime,
    device: *mut HackRfDevice,
    active: bool,
}

impl ActiveRx<'_> {
    fn stop(mut self) -> Result<(), CaptureError> {
        let result = self.runtime.stop_rx(self.device);
        self.active = false;
        result
    }
}

impl Drop for ActiveRx<'_> {
    fn drop(&mut self) {
        if self.active {
            let _ = self.runtime.stop_rx(self.device);
        }
    }
}

impl OpenedDevice<'_> {
    fn pointer(&self) -> *mut HackRfDevice {
        self.pointer
    }

    fn close(mut self) -> Result<(), CaptureError> {
        // SAFETY: the pointer is an open handle and is closed exactly once.
        let code = unsafe { (self.api.close)(self.pointer) };
        self.pointer = ptr::null_mut();
        self.api.check("closing HackRF One", code)
    }
}

impl Drop for OpenedDevice<'_> {
    fn drop(&mut self) {
        if !self.pointer.is_null() {
            // SAFETY: this is the fallback close for an open handle.
            let _ = unsafe { (self.api.close)(self.pointer) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capture_settings() -> RadioCaptureSettings {
        RadioCaptureSettings {
            center_frequency_hz: 433_920_000,
            sample_rate_hz: 2_000_000,
            bandwidth_hz: 1_750_000,
            receive_channel: 0,
            sample_limit: Some(4_096),
        }
    }

    #[test]
    fn capabilities_match_hackrf_one_receive_limits() {
        let capabilities = hackrf_one_capabilities();
        assert!(capabilities.frequency_hz.contains(1_000_000));
        assert!(capabilities.frequency_hz.contains(6_000_000_000));
        assert!(capabilities.sample_rates_hz.contains(2_000_000));
        assert!(capabilities.sample_rates_hz.contains(20_000_000));
        assert!(capabilities.bandwidths_hz.contains(1_750_000));
        assert!(capabilities.bandwidths_hz.contains(24_000_000));
        assert!(capabilities.bandwidths_hz.contains(28_000_000));
        assert_eq!(capabilities.receive_channels, 1);
        assert!(!capabilities.supports_hardware_timestamps);
        assert!(capabilities.supports_external_clock);
    }

    #[test]
    fn validates_filter_and_gain_constraints() {
        assert!(validate_capture_settings(&capture_settings()).is_ok());
        let widest_filter = RadioCaptureSettings {
            bandwidth_hz: 28_000_000,
            ..capture_settings()
        };
        assert!(validate_capture_settings(&widest_filter).is_ok());
        assert!(validate_provider_settings(&HackRfSettings {
            lna_gain_db: Some(24),
            vga_gain_db: Some(30),
            amplifier_enabled: true,
        })
        .is_ok());
        assert!(validate_provider_settings(&HackRfSettings {
            lna_gain_db: Some(10),
            ..HackRfSettings::default()
        })
        .is_err());
    }

    #[test]
    fn receive_callback_copies_interleaved_signed_iq() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let mut state = RxCallbackState::new(sender, Duration::ZERO, 2_000_000);
        let mut bytes = vec![0xff, 0x02, 0x80, 0x7f];
        let mut transfer = HackRfTransfer {
            device: ptr::null_mut(),
            buffer: bytes.as_mut_ptr(),
            buffer_length: bytes.len() as c_int,
            valid_length: bytes.len() as c_int,
            rx_context: &mut state as *mut RxCallbackState as *mut c_void,
            tx_context: ptr::null_mut(),
        };

        // SAFETY: the test transfer and callback state remain valid for the call.
        assert_eq!(unsafe { receive_callback(&mut transfer) }, 0);
        let block = receiver.try_recv().unwrap();
        let samples = block
            .bytes
            .chunks_exact(2)
            .map(|sample| Iq8 {
                i: sample[0] as i8,
                q: sample[1] as i8,
            })
            .collect::<Vec<_>>();
        assert_eq!(samples, vec![Iq8 { i: -1, q: 2 }, Iq8 { i: -128, q: 127 }]);
    }

    #[test]
    fn receive_callback_reports_bounded_queue_overflow() {
        let (sender, _receiver) = mpsc::sync_channel(0);
        let mut state = RxCallbackState::new(sender, Duration::ZERO, 2_000_000);
        let mut bytes = vec![0, 0];
        let mut transfer = HackRfTransfer {
            device: ptr::null_mut(),
            buffer: bytes.as_mut_ptr(),
            buffer_length: 2,
            valid_length: 2,
            rx_context: &mut state as *mut RxCallbackState as *mut c_void,
            tx_context: ptr::null_mut(),
        };

        // SAFETY: the test transfer and callback state remain valid for the call.
        assert_eq!(unsafe { receive_callback(&mut transfer) }, 1);
        assert!(state.failure().unwrap().contains("overflowed"));
    }
}
