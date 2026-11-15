//! Discovery bridge for the separately built current-`probe-rs` helper.

use std::env;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use wavecraft_debug_probe::{
    DebugProbeCatalog, DebugProbeCommand, DebugProbeError, DebugProbeProtocolClient,
    DebugProbeReply, ProbeDescriptor, ProbeTargetGroup,
};

const HELPER_ENVIRONMENT_VARIABLE: &str = "WAVECRAFT_PROBE_HELPER";
#[cfg(windows)]
const HELPER_EXECUTABLE: &str = "wavecraft-probe-helper.exe";
#[cfg(not(windows))]
const HELPER_EXECUTABLE: &str = "wavecraft-probe-helper";

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct DebugProbeDiscoveryReport {
    pub(crate) probes: Vec<ProbeDescriptor>,
    pub(crate) target_groups: Vec<ProbeTargetGroup>,
    pub(crate) error: Option<String>,
}

impl DebugProbeDiscoveryReport {
    fn failed(error: impl Into<String>) -> Self {
        Self {
            probes: Vec::new(),
            target_groups: Vec::new(),
            error: Some(error.into()),
        }
    }
}

pub(crate) fn discover_debug_probes() -> DebugProbeDiscoveryReport {
    let mut command = match helper_command(HelperInvocation::Catalog) {
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
    match parse_probe_catalog(&output.stdout) {
        Ok(catalog) => DebugProbeDiscoveryReport {
            probes: catalog.probes,
            target_groups: catalog.target_groups,
            error: None,
        },
        Err(error) => DebugProbeDiscoveryReport::failed(error),
    }
}

pub(crate) struct DebugProbeHelperProcess {
    child: Option<Child>,
    client: DebugProbeProtocolClient<BufReader<ChildStdout>, ChildStdin>,
}

impl DebugProbeHelperProcess {
    pub(crate) fn spawn() -> Result<Self, String> {
        let mut command = helper_command(HelperInvocation::Serve)?;
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("failed to start debug-probe helper: {error}"))?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let (Some(stdin), Some(stdout)) = (stdin, stdout) else {
            let _ = child.kill();
            let _ = child.wait();
            return Err("debug-probe helper pipes are unavailable".to_owned());
        };
        Ok(Self {
            child: Some(child),
            client: DebugProbeProtocolClient::new(BufReader::new(stdout), stdin),
        })
    }

    pub(crate) fn send(
        &mut self,
        command: DebugProbeCommand,
    ) -> Result<DebugProbeReply, DebugProbeError> {
        self.client.send(command)
    }

    pub(crate) fn shutdown(mut self) -> Result<(), DebugProbeError> {
        let result = self.client.send(DebugProbeCommand::Shutdown).map(|_| ());
        if let Some(mut child) = self.child.take() {
            if result.is_err() {
                let _ = child.kill();
            }
            let _ = child.wait();
        }
        result
    }
}

impl Drop for DebugProbeHelperProcess {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[derive(Clone, Copy)]
enum HelperInvocation {
    Catalog,
    Serve,
}

fn helper_command(invocation: HelperInvocation) -> Result<Command, String> {
    if let Some(configured) = env::var_os(HELPER_ENVIRONMENT_VARIABLE) {
        let mut command = Command::new(configured);
        add_helper_arguments(&mut command, invocation, false);
        return Ok(command);
    }

    for candidate in helper_candidates() {
        if candidate.is_file() {
            let mut command = Command::new(candidate);
            add_helper_arguments(&mut command, invocation, false);
            return Ok(command);
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
            add_helper_arguments(&mut command, invocation, true);
            return Ok(command);
        }
    }

    Err(format!(
        "debug-probe helper was not found; set {HELPER_ENVIRONMENT_VARIABLE} or install {HELPER_EXECUTABLE} next to WaveCraft"
    ))
}

fn add_helper_arguments(command: &mut Command, invocation: HelperInvocation, through_cargo: bool) {
    if through_cargo {
        command.arg("--");
    }
    match invocation {
        HelperInvocation::Catalog => command.arg("catalog"),
        HelperInvocation::Serve => command.arg("serve"),
    };
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

fn parse_probe_catalog(bytes: &[u8]) -> Result<DebugProbeCatalog, String> {
    serde_json::from_slice(bytes)
        .map_err(|error| format!("debug-probe helper returned invalid data: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use wavecraft_debug_probe::ProbeProvider;

    #[test]
    fn parses_provider_neutral_probe_descriptors() {
        let catalog = parse_probe_catalog(
            br#"{"probes":[{"connection_id":"1366:0105:1234","provider":"JLink","vendor":"SEGGER","model":"J-Link","serial_number":"1234"}],"target_groups":[{"provider":"JLink","targets":[{"name":"nRF52832_xxAA","family":"nRF52 Series"}]}]}"#,
        )
        .expect("valid helper output");

        assert_eq!(
            catalog.probes,
            [ProbeDescriptor {
                connection_id: "1366:0105:1234".to_owned(),
                provider: ProbeProvider::JLink,
                vendor: Some("SEGGER".to_owned()),
                model: "J-Link".to_owned(),
                serial_number: Some("1234".to_owned()),
            }]
        );
        assert_eq!(catalog.target_groups.len(), 1);
        assert_eq!(catalog.target_groups[0].targets[0].name, "nRF52832_xxAA");
    }

    #[test]
    fn rejects_non_protocol_output() {
        let error = parse_probe_catalog(b"J-Link").expect_err("human-readable output is not JSON");

        assert!(error.contains("invalid data"));
    }

    #[test]
    #[ignore = "requires a separately built helper and connected debug probe"]
    fn discovers_a_connected_debug_probe_through_the_helper() {
        let report = discover_debug_probes();

        assert_eq!(report.error, None);
        assert!(!report.probes.is_empty());
        assert!(!report.target_groups.is_empty());
    }
}
