//! Discovery bridge for the separately built current-`probe-rs` helper.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

use wavecraft_debug_probe::ProbeDescriptor;

const HELPER_ENVIRONMENT_VARIABLE: &str = "WAVECRAFT_PROBE_HELPER";
#[cfg(windows)]
const HELPER_EXECUTABLE: &str = "wavecraft-probe-helper.exe";
#[cfg(not(windows))]
const HELPER_EXECUTABLE: &str = "wavecraft-probe-helper";

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct DebugProbeDiscoveryReport {
    pub(crate) probes: Vec<ProbeDescriptor>,
    pub(crate) error: Option<String>,
}

impl DebugProbeDiscoveryReport {
    fn failed(error: impl Into<String>) -> Self {
        Self {
            probes: Vec::new(),
            error: Some(error.into()),
        }
    }
}

pub(crate) fn discover_debug_probes() -> DebugProbeDiscoveryReport {
    let mut command = match helper_command() {
        Ok(command) => command,
        Err(error) => return DebugProbeDiscoveryReport::failed(error),
    };
    let output = match command.output() {
        Ok(output) => output,
        Err(error) => {
            return DebugProbeDiscoveryReport::failed(format!(
                "failed to start debug-probe helper: {error}"
            ));
        }
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail = stderr.trim();
        return DebugProbeDiscoveryReport::failed(if detail.is_empty() {
            format!("debug-probe helper exited with {}", output.status)
        } else {
            format!("debug-probe scan failed: {detail}")
        });
    }
    match parse_probe_list(&output.stdout) {
        Ok(probes) => DebugProbeDiscoveryReport {
            probes,
            error: None,
        },
        Err(error) => DebugProbeDiscoveryReport::failed(error),
    }
}

fn helper_command() -> Result<Command, String> {
    if let Some(configured) = env::var_os(HELPER_ENVIRONMENT_VARIABLE) {
        return Ok(Command::new(configured));
    }

    for candidate in helper_candidates() {
        if candidate.is_file() {
            return Ok(Command::new(candidate));
        }
    }

    #[cfg(debug_assertions)]
    {
        let manifest = probe_adapter_manifest();
        if manifest.is_file() {
            let mut command = Command::new(option_env!("CARGO").unwrap_or("cargo"));
            command
                .arg("run")
                .arg("--quiet")
                .arg("--manifest-path")
                .arg(manifest)
                .arg("--bin")
                .arg("wavecraft-probe-helper");
            return Ok(command);
        }
    }

    Err(format!(
        "debug-probe helper was not found; set {HELPER_ENVIRONMENT_VARIABLE} or install {HELPER_EXECUTABLE} next to WaveCraft"
    ))
}

fn helper_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(executable) = env::current_exe() {
        if let Some(directory) = executable.parent() {
            candidates.push(directory.join(HELPER_EXECUTABLE));
        }
    }
    let adapter = probe_adapter_directory();
    candidates.push(adapter.join("target/debug").join(HELPER_EXECUTABLE));
    candidates.push(adapter.join("target/release").join(HELPER_EXECUTABLE));
    candidates
}

fn probe_adapter_directory() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../crates/wavecraft-probe-rs")
}

#[cfg(debug_assertions)]
fn probe_adapter_manifest() -> PathBuf {
    probe_adapter_directory().join("Cargo.toml")
}

fn parse_probe_list(bytes: &[u8]) -> Result<Vec<ProbeDescriptor>, String> {
    serde_json::from_slice(bytes)
        .map_err(|error| format!("debug-probe helper returned invalid data: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use wavecraft_debug_probe::ProbeProvider;

    #[test]
    fn parses_provider_neutral_probe_descriptors() {
        let probes = parse_probe_list(
            br#"[{"connection_id":"1366:0105:1234","provider":"JLink","vendor":"SEGGER","model":"J-Link","serial_number":"1234"}]"#,
        )
        .expect("valid helper output");

        assert_eq!(
            probes,
            [ProbeDescriptor {
                connection_id: "1366:0105:1234".to_owned(),
                provider: ProbeProvider::JLink,
                vendor: Some("SEGGER".to_owned()),
                model: "J-Link".to_owned(),
                serial_number: Some("1234".to_owned()),
            }]
        );
    }

    #[test]
    fn rejects_non_protocol_output() {
        let error = parse_probe_list(b"J-Link").expect_err("human-readable output is not JSON");

        assert!(error.contains("invalid data"));
    }

    #[test]
    #[ignore = "requires a separately built helper and connected debug probe"]
    fn discovers_a_connected_debug_probe_through_the_helper() {
        let report = discover_debug_probes();

        assert_eq!(report.error, None);
        assert!(!report.probes.is_empty());
    }
}
