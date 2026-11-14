# Example files

## System Trace

`nrf52832-freertos.SVDat` is a deterministic SEGGER SystemView-compatible
recording for previewing WaveCraft's System Trace interface without target
hardware. Like recordings saved by the SystemView desktop application, it has
a text metadata header followed by a synchronized raw event stream. It contains
more than 1,200 events across three named FreeRTOS-style tasks, repeated ISR and
timer nesting, idle and scheduler intervals, markers, data samples, and
formatted log events. The repeated activity is long enough to exercise the
timeline's 1×–64× zoom and horizontal panning.

Open it from the System Trace toolbar with **Open .SVDat…**.

The fixture is reproduced from the workspace root with:

```console
cargo run -p wavecraft-systemview --example generate_demo -- \
  docs/examples/nrf52832-freertos.SVDat
```
