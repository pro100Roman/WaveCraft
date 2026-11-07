//! Helpers shared by the tests of the `app` modules.
//!
//! They build a `WaveCraftApp` without hardware or a settings file, and real
//! `CaptureSession`s from scripted fake `CaptureSource`s, so the code that
//! starts, polls, and finishes captures can be driven end to end.

use crate::*;

use std::time::{Duration, Instant};
use wavecraft_core::{CaptureContext, CaptureError, DeviceDescriptor, DeviceId};

pub(crate) fn app() -> WaveCraftApp {
    WaveCraftApp::with_settings(None, CaptureSettingsDocument::default())
}

/// How a scripted source ends after it has sent its batches.
pub(crate) enum SourceEnding {
    /// Keep running until the session stops it.
    Idle,
    /// Return normally, which makes the session report the source as stopped.
    Finish,
    /// Return this error, which makes the session report the source as failed.
    Fail(String),
    /// Crash after the batches are sent, so the source never reports that it
    /// stopped and the session's event stream just closes.
    Panic,
}

/// What a fake instrument does once the capture starts.
pub(crate) struct Script {
    pub(crate) batches: Vec<CaptureBatch>,
    pub(crate) ending: SourceEnding,
    /// Makes stopping the session fail, as a crashed capture worker would.
    pub(crate) panic_when_stopped: bool,
}

impl Script {
    /// Sends nothing and runs until stopped.
    pub(crate) fn idle() -> Self {
        Self {
            batches: Vec::new(),
            ending: SourceEnding::Idle,
            panic_when_stopped: false,
        }
    }

    pub(crate) fn sending(batches: Vec<CaptureBatch>, ending: SourceEnding) -> Self {
        Self {
            batches,
            ending,
            panic_when_stopped: false,
        }
    }
}

struct ScriptedSource {
    descriptor: DeviceDescriptor,
    script: Script,
}

impl CaptureSource for ScriptedSource {
    fn descriptor(&self) -> DeviceDescriptor {
        self.descriptor.clone()
    }

    fn run(self: Box<Self>, context: CaptureContext) -> Result<(), CaptureError> {
        context.synchronize_start()?;
        for (sequence, batch) in self.script.batches.into_iter().enumerate() {
            context.emit(sequence as u64, context.elapsed(), batch)?;
        }
        match self.script.ending {
            SourceEnding::Idle => {
                while !context.is_cancelled() {
                    std::thread::sleep(Duration::from_millis(1));
                }
                if self.script.panic_when_stopped {
                    panic!("the scripted source was told to fail when stopped");
                }
                Ok(())
            }
            SourceEnding::Finish => Ok(()),
            SourceEnding::Fail(message) => Err(CaptureError::new(message)),
            SourceEnding::Panic => panic!("the scripted source crashed"),
        }
    }
}

/// A started capture of one idle instrument of the given kind, as the
/// background thread would hand it to the UI.
pub(crate) fn ready(
    app: &WaveCraftApp,
    kind: InstrumentKind,
    mode: AcquisitionMode,
    target: CaptureTarget,
) -> CaptureReady {
    ready_with(app, &[kind], false, mode, target)
}

/// Like `ready`, with one idle instrument per kind; `panic_when_stopped` makes
/// stopping the session fail.
pub(crate) fn ready_with(
    app: &WaveCraftApp,
    kinds: &[InstrumentKind],
    panic_when_stopped: bool,
    mode: AcquisitionMode,
    target: CaptureTarget,
) -> CaptureReady {
    let sources = kinds
        .iter()
        .enumerate()
        .map(|(index, kind)| {
            scripted(
                index,
                *kind,
                Script {
                    panic_when_stopped,
                    ..Script::idle()
                },
            )
        })
        .collect();
    ready_from_sources(app, sources, mode, target)
}

/// A started capture whose instruments follow their scripts, one per entry.
pub(crate) fn ready_scripted(
    app: &WaveCraftApp,
    instruments: Vec<(InstrumentKind, Script)>,
    mode: AcquisitionMode,
    target: CaptureTarget,
) -> CaptureReady {
    let sources = instruments
        .into_iter()
        .enumerate()
        .map(|(index, (kind, script))| scripted(index, kind, script))
        .collect();
    ready_from_sources(app, sources, mode, target)
}

fn scripted(index: usize, kind: InstrumentKind, script: Script) -> Box<dyn CaptureSource> {
    Box::new(ScriptedSource {
        descriptor: DeviceDescriptor {
            id: DeviceId::new(format!("fake:{index}")),
            name: format!("Fake instrument {index}"),
            kind,
        },
        script,
    })
}

fn ready_from_sources(
    app: &WaveCraftApp,
    sources: Vec<Box<dyn CaptureSource>>,
    mode: AcquisitionMode,
    target: CaptureTarget,
) -> CaptureReady {
    let spectrum = || {
        SpectrumProcessor::new(RADIO_SPECTRUM_FFT_LENGTH, SpectrumWindow::Hann)
            .expect("the spectrum processor accepts the GUI FFT length")
    };
    CaptureReady {
        session: CaptureCoordinator::start(sources).expect("the fake sources start"),
        mode,
        target,
        retain_protocol_history: false,
        logic_config: app
            .logic_settings
            .capture_config()
            .expect("the default logic settings are valid"),
        scope_config: app
            .scope_settings
            .capture_config()
            .expect("the default scope settings are valid"),
        scope_trigger_controls: Vec::new(),
        scope_calibration_controls: Vec::new(),
        radio_runtime_control: None,
        radio_spectrum: spectrum(),
        radio_dsp: RadioDspWorker::start(spectrum(), Vec::new()),
    }
}

/// Marks `target` as requested the way pressing Run/Single does.
pub(crate) fn request(app: &mut WaveCraftApp, target: CaptureTarget) {
    app.capture_requests.insert(target);
    app.requested_capture_target = Some(target);
}

/// Polls `target` until `done` holds for the app, failing the test after a few
/// seconds. Fake sources run on their own threads, so their events arrive a
/// little after the capture starts.
pub(crate) fn poll_until(
    app: &mut WaveCraftApp,
    target: CaptureTarget,
    done: impl Fn(&WaveCraftApp) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done(app) {
        assert!(
            Instant::now() < deadline,
            "the capture did not reach the expected state; status: {:?}, error: {:?}",
            app.status,
            app.last_error
        );
        app.poll_capture_target(target);
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Polls `target` until its capture has finished and left the running captures.
pub(crate) fn poll_until_finished(app: &mut WaveCraftApp, target: CaptureTarget) {
    poll_until(app, target, |app| app.capture_for_target(target).is_none());
}

pub(crate) fn scratch_directory(name: &str) -> PathBuf {
    let directory =
        std::env::temp_dir().join(format!("wavecraft-gui-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    directory
}

pub(crate) fn completed_capture(
    app: &WaveCraftApp,
    has_logic: bool,
    has_scope: bool,
    has_radio: bool,
) -> CompletedCapture {
    CompletedCapture {
        waveforms: WaveformStore::default(),
        protocol_decode: ProtocolCaptureDecoder::new(&app.protocol_analyzer.analyzers),
        has_logic,
        has_scope,
        has_radio,
    }
}
