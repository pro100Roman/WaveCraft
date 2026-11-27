# Shared instrument view pattern

All workspaces use the same instrument chrome, based on the LA/OSC layout.
`apps/wavecraft-gui/src/ui/view_shell.rs` owns visual structure; individual
views own their settings editors, plot painters, and domain actions. This
applies equally to LA, OSC, RF, Combined, VNA, System Trace, and Serial.

## Components

| Component | Responsibility |
| --- | --- |
| `view_toolbar` | Common application identity, App menu, Snapshot button, toolbar background and spacing; returns Settings and Snapshot requests |
| `workspace_tab_bar` / `workspace_tab` | Scrollable workspace tabs, shared baseline, active outline and accent; the application owns selection, visibility and drag ordering |
| `SetupPanel` | 300-point left panel, heading, typography, constrained scrolling, mandatory collapse/expand and pinned Save settings footer |
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
3. Register the setup panel identity in `ui/setup_panel.rs` and its save behavior
   in `save_setup_settings`. Retain settings, capture state, and workers with their existing application
   owners. Return typed requests through `FrameActions` where applicable.
4. Adapt any local connection status or errors in `active_view_status` in
   `ui/status_bar.rs`. The application paints that area once for every view.
5. Verify common dimensions, message placement and the new view's actions in
   headless egui frame tests; inspect the native view and capture a screenshot.

The composition is intentionally small (illustrative Rust):

```rust,ignore
let toolbar = view_toolbar(context, "Instrument description", self.pending_snapshot.is_none(), |ui| {
    // Refresh and file actions, followed by right-aligned Run/Stop controls.
});
self.settings_open |= toolbar.settings_clicked;
actions.snapshot_clicked |= toolbar.snapshot_clicked;

self.show_setup_panel(context, WorkspaceView::Instrument, actions, |app, ui, actions| {
    settings_card(ui, |ui| {
        // Instrument settings sections; edit app state and emit domain actions.
    });
    // Return availability after edits, including validation of new values.
    settings_are_valid && !app.is_busy()
});

workspace_panel(context, |ui| {
    view_card(ui, |ui| {
        // Title, tabs and optional plot controls.
        let rect = remaining_plot_rect(ui);
        // Paint the instrument's plot in rect.
    });
});
```

## Snapshots

Snapshot belongs to the toolbar template and appears once in every workspace,
including Serial/CSV and System Trace. Disable it while a screenshot request is
pending. The frame dispatch in `paint_central_panel` queues the screenshot after
painting the active view, using that frame's bounds and display scale. Views with
a dedicated plot keep their plot crop; mixed views and tables use their visible
central workspace. Setup panels, toolbars and status messages are excluded.

`finish_pending_snapshot` receives the screenshot, crops it through the shared
image helper, asks for an output directory, and saves a PNG without overwriting
existing snapshots. Status and errors use the common message area. Serial keeps
the imported file name visible alongside snapshot feedback. Do not implement
additional per-view screenshot handlers or duplicate Snapshot buttons.

All left panels use `show_setup_panel` in `ui/setup_panel.rs`. This adapter
connects the visual template to `WorkspaceViewLayout` and `FrameActions`.
`SetupPanel::show` always requires an open flag and always renders the same
`Save settings` footer below the scroll area. Views supply only their body and
return whether the edited values may be saved; they do not paint save controls.
The footer remains visible even when the settings body overflows or sections
are collapsed.

Collapse state is independent for each workspace and survives restarts through
`GlobalSettings::collapsed_setup_views`. Older settings files default to open
panels. Settings and captured data stay intact across collapse/expand.
Panel IDs remain distinct per workspace to isolate scrolling and interaction.

The adapter emits `FrameActions::save_setup_settings_view`. The frame applies
that request through `save_setup_settings`, preserving the existing per-type
capture profiles. Combined saves LA and OSC profiles together in one write;
Serial saves `global.serial`, retaining compatibility with existing settings
files and its automatic saves. Successful explicit saves and failures use the
common message area.

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
