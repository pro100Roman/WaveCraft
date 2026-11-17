//! Separately built `probe-rs` bridge used by the Rust 1.79 WaveCraft process.

use std::error::Error;
use std::io;
use std::thread;
use std::time::Duration;

use wavecraft_debug_probe::{DebugProbeCatalog, DebugProbeDriver};
use wavecraft_probe_rs::ProbeRsDriver;

const GUI_PROCESS_ID_ENVIRONMENT_VARIABLE: &str = "WAVECRAFT_GUI_PROCESS_ID";
const PARENT_WATCH_INTERVAL: Duration = Duration::from_millis(250);

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    match std::env::args().nth(1).as_deref() {
        None | Some("list") => list_probes(),
        Some("catalog") => write_catalog(),
        Some("serve") => {
            start_gui_process_watchdog()?;
            wavecraft_probe_rs::helper::serve(
                ProbeRsDriver,
                io::stdin().lock(),
                io::stdout().lock(),
            )?;
            Ok(())
        }
        Some(command) => {
            Err(format!("unknown command {command:?}; expected list, catalog, or serve").into())
        }
    }
}

fn start_gui_process_watchdog() -> Result<(), Box<dyn Error>> {
    let Some(process_id) = std::env::var_os(GUI_PROCESS_ID_ENVIRONMENT_VARIABLE) else {
        return Ok(());
    };
    let process_id = process_id
        .to_str()
        .ok_or("GUI process ID is not valid UTF-8")?
        .parse::<u32>()?;
    spawn_gui_process_watchdog(process_id)?;
    Ok(())
}

#[cfg(unix)]
fn spawn_gui_process_watchdog(process_id: u32) -> io::Result<()> {
    thread::Builder::new()
        .name("wavecraft-gui-process-watchdog".to_owned())
        .spawn(move || {
            while unix_process_is_alive(process_id) {
                thread::sleep(PARENT_WATCH_INTERVAL);
            }
            std::process::exit(0);
        })?;
    Ok(())
}

#[cfg(unix)]
fn unix_process_is_alive(process_id: u32) -> bool {
    let Ok(process_id) = libc::pid_t::try_from(process_id) else {
        return false;
    };
    if unsafe { libc::kill(process_id, 0) } == 0 {
        return true;
    }
    io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(windows)]
fn spawn_gui_process_watchdog(process_id: u32) -> io::Result<()> {
    use windows_sys::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Threading::{
        INFINITE, OpenProcess, SYNCHRONIZE, WaitForSingleObject,
    };

    let process = unsafe { OpenProcess(SYNCHRONIZE, 0, process_id) };
    if process.is_null() {
        return Err(io::Error::last_os_error());
    }
    let process_address = process as usize;
    thread::Builder::new()
        .name("wavecraft-gui-process-watchdog".to_owned())
        .spawn(move || {
            let process = process_address as windows_sys::Win32::Foundation::HANDLE;
            let wait_result = unsafe { WaitForSingleObject(process, INFINITE) };
            unsafe { CloseHandle(process) };
            if wait_result == WAIT_OBJECT_0 {
                std::process::exit(0);
            }
        })?;
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn spawn_gui_process_watchdog(_process_id: u32) -> io::Result<()> {
    Ok(())
}

fn write_catalog() -> Result<(), Box<dyn Error>> {
    let probes = ProbeRsDriver.list_probes()?;
    let catalog = DebugProbeCatalog {
        probes,
        // Network probes are configured manually and therefore are not part of USB discovery.
        target_groups: ProbeRsDriver::supported_target_groups(),
    };
    serde_json::to_writer(io::stdout().lock(), &catalog)?;
    Ok(())
}

fn list_probes() -> Result<(), Box<dyn Error>> {
    let probes = ProbeRsDriver.list_probes()?;
    serde_json::to_writer(io::stdout().lock(), &probes)?;
    Ok(())
}
