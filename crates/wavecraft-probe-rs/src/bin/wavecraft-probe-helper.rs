//! Separately built `probe-rs` bridge used by the Rust 1.79 WaveCraft process.

use std::error::Error;
use std::io;

use wavecraft_debug_probe::DebugProbeDriver;
use wavecraft_probe_rs::ProbeRsDriver;

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    match std::env::args().nth(1).as_deref() {
        None | Some("list") => list_probes(),
        Some("serve") => {
            wavecraft_probe_rs::helper::serve(
                ProbeRsDriver,
                io::stdin().lock(),
                io::stdout().lock(),
            )?;
            Ok(())
        }
        Some(command) => Err(format!("unknown command {command:?}; expected list or serve").into()),
    }
}

fn list_probes() -> Result<(), Box<dyn Error>> {
    let probes = ProbeRsDriver.list_probes()?;
    serde_json::to_writer(io::stdout().lock(), &probes)?;
    Ok(())
}
