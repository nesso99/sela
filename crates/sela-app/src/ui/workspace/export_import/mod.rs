//! Export and CSV import UI (M3-3 / US-12).
//!
//! Dialogs, background progress, cancel, and safe-mode gates. Core writers /
//! readers live in `sela-core`. Native file dialogs are manual-at-RC (M-EX-01);
//! headless tests inject paths.

pub(crate) mod export_dialog;
pub(crate) mod import_dialog;
mod jobs;
pub(crate) mod messages;

pub(super) use export_dialog::ExportDialog;
pub(super) use import_dialog::ImportDialog;
pub(super) use jobs::TransferJob;
