//! Export and CSV import UI (M3-3 / US-12).
//!
//! Dialogs, background progress, cancel, and safe-mode gates. Core writers /
//! readers live in `sela-core`. Browse uses native `prompt_for_new_path` /
//! `prompt_for_paths`; headless tests still inject typed paths.

pub(crate) mod export_dialog;
pub(crate) mod import_dialog;
pub(crate) mod jobs;
pub(crate) mod logic;
pub(crate) mod messages;

pub(super) use export_dialog::ExportDialog;
pub(super) use import_dialog::ImportDialog;
pub(super) use jobs::TransferJob;
