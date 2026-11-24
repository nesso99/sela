//! Persist query history in `history.sqlite` (FTS5 on statement text).
//!
//! Never stores passwords. The file is created with Unix mode `0600`. SQLite
//! journaling is the durability path; a corrupt file is renamed to
//! `history.sqlite.bad` and replaced with an empty store.

mod store;

pub use store::{HistoryOpen, HistoryStore};

#[cfg(test)]
mod tests;
