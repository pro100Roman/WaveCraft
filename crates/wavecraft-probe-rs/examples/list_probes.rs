use std::error::Error;
use wavecraft_debug_probe::DebugProbeDriver;
use wavecraft_probe_rs::ProbeRsDriver;

fn main() -> Result<(), Box<dyn Error>> {
    let probes = ProbeRsDriver.list_probes()?;
    if probes.is_empty() {
        println!("No supported debug probes found.");
        return Ok(());
    }

    for probe in probes {
        println!(
            "{}: {:?} {}{}",
            probe.connection_id,
            probe.provider,
            probe.model,
            probe
                .serial_number
                .as_deref()
                .map(|serial| format!(" (serial {serial})"))
                .unwrap_or_default()
        );
    }
    Ok(())
}
