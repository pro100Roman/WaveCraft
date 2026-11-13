//! Separately built `probe-rs` bridge used by the Rust 1.79 WaveCraft process.

use std::error::Error;
use std::io;

use wavecraft_debug_probe::{DebugProbeCatalog, DebugProbeDriver};
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
        Some("catalog") => write_catalog(),
        Some("serve") => {
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

fn write_catalog() -> Result<(), Box<dyn Error>> {
    let probes = ProbeRsDriver.list_probes()?;
    let discovered_providers = probes
        .iter()
        .map(|probe| probe.provider.clone())
        .collect::<std::collections::BTreeSet<_>>();
    let catalog = DebugProbeCatalog {
        probes,
        target_groups: ProbeRsDriver::supported_target_groups()
            .into_iter()
            .filter(|group| discovered_providers.contains(&group.provider))
            .collect(),
    };
    serde_json::to_writer(io::stdout().lock(), &catalog)?;
    Ok(())
}

fn list_probes() -> Result<(), Box<dyn Error>> {
    let probes = ProbeRsDriver.list_probes()?;
    serde_json::to_writer(io::stdout().lock(), &probes)?;
    Ok(())
}
