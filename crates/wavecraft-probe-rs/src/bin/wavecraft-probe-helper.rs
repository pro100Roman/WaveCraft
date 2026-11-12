//! Separately built `probe-rs` bridge used by the Rust 1.79 WaveCraft process.

use std::error::Error;
use std::io;

use wavecraft_debug_probe::DebugProbeDriver;
use wavecraft_probe_rs::ProbeRsDriver;

fn main() {
    if let Err(error) = list_probes() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn list_probes() -> Result<(), Box<dyn Error>> {
    let probes = ProbeRsDriver.list_probes()?;
    serde_json::to_writer(io::stdout().lock(), &probes)?;
    Ok(())
}
