# WaveCraft Python client

This dependency-free package controls explicitly selected WaveCraft devices
through the versioned JSON and NDJSON interface of `wavecraft-cli`.

Build the CLI and install the client from the workspace:

```sh
cargo build --release -p wavecraft-cli
python3 -m pip install -e python
```

Discover and capture from a selected DSLogic device:

```python
from wavecraft import WaveCraftClient

client = WaveCraftClient.from_workspace(".")
device = client.list_devices(
    kind="logic_analyzer",
    backend="usb",
)[0]

result = client.capture(
    device_id=device["connection_id"],
    sample_rate_hz=1_000_000,
    sample_count=65_536,
    channels=[0, 1, 2, 3],
    output="python-logic.dsv",
    on_event=lambda event: print(event),
)
print(result["summary"]["recording_path"])
```

DSCope uses the same method with scope channels and sample settings. The CLI
currently supplies its validated default frontend and trigger configuration.
Output paths are never overwritten. `WaveCraftError` exposes the structured
`code`, `operation`, `device_id`, and `retryable` fields returned by the Rust
automation service.

Run the Python tests without installing the package:

```sh
PYTHONPATH=python/src python3 -m unittest discover -s python/tests -v
```
