# Example files

## System Trace

`nrf52832-freertos.SVDat` is a deterministic raw SEGGER SystemView-compatible
event stream for previewing WaveCraft's System Trace interface without target
hardware. It contains three named FreeRTOS-style tasks, ISR and timer nesting,
idle and scheduler intervals, markers, a data sample, and formatted log events.

Open it from the System Trace toolbar with **Open .SVDat…**.

The fixture is reproduced from the workspace root with:

```console
cargo run -p wavecraft-systemview --example generate_demo -- \
  docs/examples/nrf52832-freertos.SVDat
```
