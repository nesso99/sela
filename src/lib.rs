//! Birch is a native desktop database client.
//!
//! Database work lives in [`db`] and does not touch the UI. Saved connections
//! live in [`config`]. The GPUI window is [`ui`].

pub mod config;
pub mod db;
pub mod ui;

pub use config::{ConfigStore, SavedConnection};
pub use db::{DatabaseSession, Engine, PAGE_SIZE};

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
