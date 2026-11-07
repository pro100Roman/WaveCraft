//! The panels, windows, and dialogs `update()` paints each frame, and the code
//! that applies what the user did in them. Split out of the application module.

pub mod app_windows;
pub mod capture_settings_panel;
pub mod capture_setup_panel;
pub mod central_panel;
pub mod decoded_panel;
pub mod dialogs;
pub mod frame_actions;
pub mod frame_state;
pub mod protocol_analyzer_dialog;
pub mod radio_settings;
pub mod scope_frontend_changes;
pub mod scope_settings;
pub mod scope_tools_panel;
pub mod session_tabs;
pub mod status_bar;
pub mod toolbar;
pub mod view_tabs;
pub mod waveform_tabs;
