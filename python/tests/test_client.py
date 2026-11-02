import io
import json
import subprocess
import unittest
from pathlib import Path
from unittest.mock import patch

from wavecraft import (
    WaveCraftClient,
    WaveCraftError,
    WaveCraftProcessError,
    WaveCraftProtocolError,
)


class FakeProcess:
    def __init__(self, messages, return_code=0):
        self.stdout = io.StringIO("".join(json.dumps(message) + "\n" for message in messages))
        self.return_code = return_code
        self.terminated = False
        self.killed = False

    def wait(self, timeout=None):
        return self.return_code

    def terminate(self):
        self.terminated = True

    def kill(self):
        self.killed = True


class WaveCraftClientTests(unittest.TestCase):
    def setUp(self):
        self.client = WaveCraftClient(
            "/opt/wavecraft-cli",
            resource_directory="/opt/fpga",
            cwd="/workspace",
        )

    @patch("wavecraft.client.subprocess.run")
    def test_list_devices_filters_structured_listing(self, run):
        listing = {
            "schema": "wavecraft.devices/v1",
            "devices": [
                {
                    "connection_id": "usb-logic",
                    "kind": "logic_analyzer",
                    "backend": "usb",
                    "model": "dslogic_u3pro16",
                },
                {
                    "connection_id": "sim:logic-0",
                    "kind": "logic_analyzer",
                    "backend": "simulator",
                    "model": "simulated_logic_analyzer",
                },
            ],
        }
        run.return_value = subprocess.CompletedProcess(
            [], 0, stdout=json.dumps(listing), stderr=""
        )

        devices = self.client.list_devices(kind="logic_analyzer", backend="usb")

        self.assertEqual([device["connection_id"] for device in devices], ["usb-logic"])
        run.assert_called_once_with(
            ["/opt/wavecraft-cli", "devices", "list", "--format", "json"],
            cwd="/workspace",
            text=True,
            encoding="utf-8",
            capture_output=True,
            check=False,
        )

    @patch("wavecraft.client.subprocess.run")
    def test_probe_returns_versioned_result(self, run):
        probe = {
            "schema": "wavecraft.device-probe/v1",
            "device": {"connection_id": "usb-scope"},
        }
        run.return_value = subprocess.CompletedProcess(
            [], 0, stdout=json.dumps(probe), stderr=""
        )

        self.assertEqual(
            self.client.probe("usb-scope")["device"]["connection_id"],
            "usb-scope",
        )

    @patch("wavecraft.client.subprocess.Popen")
    def test_capture_streams_events_and_returns_result(self, popen):
        event = {
            "schema": "wavecraft.capture-event/v1",
            "type": "status",
            "status": "capturing",
        }
        result = {
            "schema": "wavecraft.capture-result/v1",
            "summary": {"captured_samples": 65_536},
        }
        popen.return_value = FakeProcess([event, result])
        events = []

        completed = self.client.capture(
            device_id="usb-logic",
            sample_rate_hz=1_000_000,
            sample_count=65_536,
            channels=[0, 2],
            output=Path("capture.dsv"),
            timeout_ms=20_000,
            on_event=events.append,
        )

        self.assertEqual(events, [event])
        self.assertEqual(completed, result)
        command = popen.call_args.args[0]
        self.assertEqual(command[0], "/opt/wavecraft-cli")
        self.assertIn("usb-logic", command)
        self.assertIn("0,2", command)
        self.assertIn("/opt/fpga", command)

    @patch("wavecraft.client.subprocess.Popen")
    def test_capture_raises_structured_error(self, popen):
        error = {
            "schema": "wavecraft.error/v1",
            "code": "device_busy",
            "message": "device is already in use",
            "operation": "device.open",
            "retryable": True,
        }
        popen.return_value = FakeProcess([error], return_code=1)

        with self.assertRaises(WaveCraftError) as raised:
            self.client.capture(
                device_id="usb-logic",
                sample_rate_hz=1_000_000,
                sample_count=1_024,
                channels=[0],
                output="capture.dsv",
            )

        self.assertEqual(raised.exception.code, "device_busy")
        self.assertTrue(raised.exception.retryable)

    @patch("wavecraft.client.subprocess.Popen")
    def test_capture_raises_process_error_without_structured_error(self, popen):
        def start_process(*args, **kwargs):
            kwargs["stderr"].write("unexpected failure")
            kwargs["stderr"].flush()
            return FakeProcess([], return_code=2)

        popen.side_effect = start_process

        with self.assertRaises(WaveCraftProcessError) as raised:
            self.client.capture(
                device_id="usb-logic",
                sample_rate_hz=1_000_000,
                sample_count=1_024,
                channels=[0],
                output="capture.dsv",
            )

        self.assertEqual(raised.exception.return_code, 2)
        self.assertIn("unexpected failure", str(raised.exception))

    @patch("wavecraft.client.subprocess.run")
    def test_invalid_json_raises_protocol_error(self, run):
        run.return_value = subprocess.CompletedProcess(
            [], 0, stdout="not-json", stderr=""
        )

        with self.assertRaises(WaveCraftProtocolError):
            self.client.list_devices()


if __name__ == "__main__":
    unittest.main()
