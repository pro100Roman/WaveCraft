# Shared instrument view pattern

All workspaces use the same instrument chrome, based on the LA/OSC layout.
`apps/wavecraft-gui/src/ui/view_shell.rs` owns visual structure; individual
views own their settings editors, plot painters, and domain actions. This
applies equally to LA, OSC, RF, Combined, VNA, System Trace, and Serial.

## Components

| Component | Responsibility |
| --- | --- |
| `view_toolbar` | Common application identity, App menu, toolbar background and spacing; returns the Settings request |
| `SetupPanel` | Fixed 300-point left panel, heading, typography, constrained vertical scrolling; optional collapse/expand |
| `settings_card` | Dark inset grouping settings sections, with shared width constraints and padding |
| `workspace_panel` | Central background and outer margins |
| `view_card` | Full-height plot/table card with a common outline, corner radius and padding |
| `remaining_plot_rect` | Remaining plot area after a view's header; also usable for snapshot cropping |
| `tools_panel` | Expanded heading/body or collapsed strip beside a plot; returns the body's result |
| `settings_scroll` | Width-constrained scrolling for settings in auxiliary panels |
| `ui/status_bar.rs` | Common bottom status row, connection details, error area and process metrics |

`card_frame` is the lower-level shared outline for content that manages its
own height (for example the reorderable System Trace instruments). Waveform
`waveform_card` composes `view_card` and `remaining_plot_rect`; generic chrome
does not belong in a device-specific or waveform-specific renderer.

Primary Run/Stop/Single controls occupy the right end of the toolbar. Refresh,
file operations, and instrument-specific actions occupy the left area after
the shared App menu. Use `theme::toolbar_action_button` for their appearance.

## Adding a view

1. Add the view identity and persistence mapping alongside `WorkspaceView`.
2. Supply toolbar, setup, and central-content painters in `ui/`; register them
   with the existing application dispatch. Use the shell functions below.
3. Retain settings, capture state, and workers with their existing application
   owners. Return typed requests through `FrameActions` where applicable.
4. Adapt any local connection status or errors in `active_view_status` in
   `ui/status_bar.rs`. The application paints that area once for every view.
5. Verify common dimensions, message placement and the new view's actions in
   headless egui frame tests; inspect the native view and capture a screenshot.

The composition is intentionally small (illustrative Rust):

```rust,ignore
let settings_clicked = view_toolbar(context, "Instrument description", |ui| {
    // Refresh and file actions, followed by right-aligned Run/Stop controls.
});
self.settings_open |= settings_clicked;

let mut open = self.instrument.setup_open;
SetupPanel::new("instrument_setup", "INSTRUMENT SETUP")
    .show_collapsible(context, &mut open, |ui| {
        settings_card(ui, |ui| {
            // Instrument settings sections.
        });
    });
self.instrument.setup_open = open;

workspace_panel(context, |ui| {
    view_card(ui, |ui| {
        // Title, tabs and optional plot controls.
        let rect = remaining_plot_rect(ui);
        // Paint the instrument's plot in rect.
    });
});
```

Use `SetupPanel::show` for a fixed panel without collapse state. Copy an open
flag locally when the content closure also borrows the application's state,
then write the flag back. Settings and data stay intact across collapsing.
Keep panel IDs stable and distinct for independent scroll and interaction state.

## Status and errors

The bottom area has one shared visual hierarchy: view identity and operation
status, optional connection details, and process metrics. Errors appear below
that row in a bounded scrolling area, wrap to the available width, and can be
selected and copied with Cmd+C / Ctrl+C, without a separate copy button. Duplicate errors are shown once. Global operation
errors remain visible alongside an active view's local errors.

Serial contributes its own running/disconnected status and error. System Trace
contributes connection/recording state and connection errors. VNA contributes
discovery errors. Existing capture/file operations continue to supply the
application status and error. Switching views does not erase backend state.

Do not place operational errors or connection messages inside a setup panel,
toolbar, or plot. They must remain visible when settings are collapsed. Local
plot annotations, data quality counters, empty-state guidance and actionable
confirmation dialogs still belong with their content; they are not substitutes
for the bottom operation/error area.

All informational labels and tooltips must support text selection and native
keyboard copying. The global theme enables `selectable_labels`; shared message
labels explicitly remain selectable. Do not disable selection in local styles
or add dedicated message-copy controls.

## Changes to the pattern

Extend the shell for new layout capabilities and migrate affected views to it.
Do not start another view-local `instrument_frame`, toolbar header, or settings
panel frame. Keep the shell free of application state, capture operations,
backend imports and worker ownership. The related repository-wide rule lives
in `AGENTS.md`; broader ownership work is described in `gui-modularization.md`.
