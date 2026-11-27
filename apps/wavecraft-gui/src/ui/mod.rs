//! The panels, windows, and dialogs `update()` paints each frame, and the code
//! that applies what the user did in them. Split out of the application module.

pub mod app_windows;
pub mod battery;
pub mod capture_settings_panel;
pub mod capture_setup_panel;
pub mod central_panel;
pub mod decoded_panel;
pub mod dialogs;
pub mod frame_actions;
pub mod frame_state;
pub(crate) mod manual_value;
mod plot_hover;
pub mod protocol_analyzer_dialog;
pub mod radio_settings;
pub mod scope_frontend_changes;
pub mod scope_settings;
pub mod scope_tools_panel;
pub mod serial;
mod serial_graph;
mod serial_line_style;
pub mod session_tabs;
pub(crate) mod setup_panel;
pub mod status_bar;
pub mod system_trace;
pub mod toolbar;
pub(crate) mod view_shell;
pub mod view_tabs;
pub mod vna;
pub mod waveform_tabs;
