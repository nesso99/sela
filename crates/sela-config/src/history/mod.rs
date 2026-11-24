//! Persist query history in `history.sqlite` (FTS5 on statement text).
//!
//! Stores statement text, including ordinary SQL literals. Password-setting DDL
//! is skipped by `sela-core` before insert; connection keychain secrets are never
//! written here. The file is created with Unix mode `0600`. SQLite journaling is
//! the durability path; a corrupt file is renamed to `history.sqlite.bad` and
//! replaced with an empty store.

mod store;

pub use store::{HistoryOpen, HistoryStore};

#[cfg(test)]
mod tests;
