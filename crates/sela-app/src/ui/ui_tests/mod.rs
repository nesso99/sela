//! Headless GPUI UI tests for high-risk flows (#93).
//!
//! Prefer `gpui_kit::TestAppContext` / `#[gpui_kit::test]` so Linux CI needs no
//! display. Run via `scripts/ui-tests.sh`.

mod command_palette;
mod commit_cancel;
mod connection_dialog;
mod export_import;
mod grid_column_autofit;
mod grid_column_width;
mod grid_editing;
mod harness;
mod long_value_popover;
mod reconnect;
mod sql_autocomplete;
mod sql_result_tab;
mod ssh_dialog;
mod ssh_tunnel_workspace;
mod title_bar;
mod workspace_transfer;
