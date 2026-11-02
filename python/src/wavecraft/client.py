"""Synchronous Python wrapper around the WaveCraft CLI automation contract."""

from __future__ import annotations

import json
import subprocess
import tempfile
from pathlib import Path
from typing import Any, Callable, Mapping, Optional, Sequence, Union

PathArgument = Union[str, Path]
JsonObject = dict[str, Any]
EventHandler = Callable[[Mapping[str, Any]], None]


class WaveCraftError(RuntimeError):
    """A structured error returned by the WaveCraft automation service."""

    def __init__(self, payload: Mapping[str, Any]) -> None:
        self.payload = dict(payload)
        self.code = str(payload.get("code", "unknown"))
        self.operation = str(payload.get("operation", "unknown"))
        self.device_id = payload.get("device_id")
        self.retryable = bool(payload.get("retryable", False))
        super().__init__(str(payload.get("message", "WaveCraft operation failed")))


class WaveCraftProcessError(RuntimeError):
    """The CLI failed without returning a structured automation error."""

    def __init__(self, return_code: int, stderr: str) -> None:
        self.return_code = return_code
        self.stderr = stderr
        detail = stderr.strip() or "no diagnostic output"
        super().__init__(f"WaveCraft exited with status {return_code}: {detail}")


class WaveCraftProtocolError(RuntimeError):
    """The CLI output did not match the documented JSON contract."""


class WaveCraftClient:
    """Control explicitly selected WaveCraft devices through ``wavecraft-cli``.

    ``cli_path`` may be an absolute path or a command available on ``PATH``.
    Physical USB captures require ``resource_directory``. Simulator captures do
    not use FPGA resources.
    """

    def __init__(
        self,
        cli_path: PathArgument = "wavecraft-cli",
        *,
        resource_directory: Optional[PathArgument] = None,
        cwd: Optional[PathArgument] = None,
    ) -> None:
        self.cli_path = str(cli_path)
        self.resource_directory = (
            str(resource_directory) if resource_directory is not None else None
        )
        self.cwd = str(cwd) if cwd is not None else None

    @classmethod
    def from_workspace(
        cls, workspace: PathArgument, *, release: bool = True
    ) -> "WaveCraftClient":
        """Create a client for a CLI built in a WaveCraft workspace."""

        root = Path(workspace).resolve()
        executable = "wavecraft-cli.exe" if _is_windows() else "wavecraft-cli"
        profile = "release" if release else "debug"
        return cls(
            root / "target" / profile / executable,
            resource_directory=root / "resources" / "fpga",
            cwd=root,
        )

    def list_devices(
        self,
        *,
        kind: Optional[str] = None,
        backend: Optional[str] = None,
        model: Optional[str] = None,
    ) -> list[JsonObject]:
        """Return discovered devices, optionally filtered by stable fields."""

        payload = self._run_json("devices", "list", "--format", "json")
        self._expect_schema(payload, "wavecraft.devices/v1")
        devices = payload.get("devices")
        if not isinstance(devices, list):
            raise WaveCraftProtocolError("device listing has no devices array")
        return [
            dict(device)
            for device in devices
            if isinstance(device, dict)
            and (kind is None or device.get("kind") == kind)
            and (backend is None or device.get("backend") == backend)
            and (model is None or device.get("model") == model)
        ]

    def probe(self, device_id: str) -> JsonObject:
        """Probe one exact connection without relying on GUI selection state."""

        payload = self._run_json(
            "devices", "probe", "--id", device_id, "--format", "json"
        )
        self._expect_schema(payload, "wavecraft.device-probe/v1")
        return payload

    def capture(
        self,
        *,
        device_id: str,
        sample_rate_hz: int,
        sample_count: int,
        channels: Sequence[int],
        output: PathArgument,
        timeout_ms: int = 30_000,
        on_event: Optional[EventHandler] = None,
    ) -> JsonObject:
        """Run a finite capture and return its structured completion result.

        ``on_event`` is called synchronously for each status or progress event.
        The output path must not already exist.
        """

        command = [
            self.cli_path,
            "capture",
            "run",
            "--device",
            device_id,
            "--sample-rate",
            str(sample_rate_hz),
            "--samples",
            str(sample_count),
            "--channels",
            ",".join(str(channel) for channel in channels),
            "--output",
            str(output),
            "--timeout-ms",
            str(timeout_ms),
            "--events",
            "ndjson",
        ]
        if self.resource_directory is not None:
            command.extend(["--resource-directory", self.resource_directory])

        result: Optional[JsonObject] = None
        structured_error: Optional[JsonObject] = None
        with tempfile.TemporaryFile(mode="w+", encoding="utf-8") as stderr_file:
            process = subprocess.Popen(
                command,
                cwd=self.cwd,
                stdout=subprocess.PIPE,
                stderr=stderr_file,
                text=True,
                encoding="utf-8",
                bufsize=1,
            )
            if process.stdout is None:
                process.terminate()
                process.wait()
                raise WaveCraftProtocolError("WaveCraft stdout pipe was not created")

            try:
                for line in process.stdout:
                    if not line.strip():
                        continue
                    message = self._decode_json(line)
                    schema = message.get("schema")
                    if schema == "wavecraft.capture-event/v1":
                        if on_event is not None:
                            on_event(message)
                    elif schema == "wavecraft.capture-result/v1":
                        result = message
                    elif schema == "wavecraft.error/v1":
                        structured_error = message
                    else:
                        raise WaveCraftProtocolError(
                            f"unexpected WaveCraft message schema: {schema!r}"
                        )
            except BaseException:
                _terminate_process(process)
                raise

            return_code = process.wait()
            stderr_file.seek(0)
            stderr = stderr_file.read()

        if structured_error is not None:
            raise WaveCraftError(structured_error)
        if return_code != 0:
            raise WaveCraftProcessError(return_code, stderr)
        if result is None:
            raise WaveCraftProtocolError("capture completed without a result message")
        return result

    def _run_json(self, *arguments: str) -> JsonObject:
        completed = subprocess.run(
            [self.cli_path, *arguments],
            cwd=self.cwd,
            text=True,
            encoding="utf-8",
            capture_output=True,
            check=False,
        )
        payload: Optional[JsonObject] = None
        if completed.stdout.strip():
            payload = self._decode_json(completed.stdout)
            if payload.get("schema") == "wavecraft.error/v1":
                raise WaveCraftError(payload)
        if completed.returncode != 0:
            raise WaveCraftProcessError(completed.returncode, completed.stderr)
        if payload is None:
            raise WaveCraftProtocolError("WaveCraft returned no JSON response")
        return payload

    @staticmethod
    def _decode_json(value: str) -> JsonObject:
        try:
            payload = json.loads(value)
        except json.JSONDecodeError as error:
            raise WaveCraftProtocolError(
                f"invalid JSON from WaveCraft at line {error.lineno}: {error.msg}"
            ) from error
        if not isinstance(payload, dict):
            raise WaveCraftProtocolError("WaveCraft JSON response is not an object")
        return payload

    @staticmethod
    def _expect_schema(payload: Mapping[str, Any], expected: str) -> None:
        actual = payload.get("schema")
        if actual != expected:
            raise WaveCraftProtocolError(
                f"expected schema {expected!r}, received {actual!r}"
            )


def _terminate_process(process: subprocess.Popen[str]) -> None:
    process.terminate()
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait()


def _is_windows() -> bool:
    import os

    return os.name == "nt"
