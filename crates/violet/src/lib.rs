//! The violet server: ingest pipeline, file operations with undo, HTTP API and CLI plumbing.

pub mod app;
pub mod cache;
pub mod error;
pub mod extract;
pub mod http;
pub mod ops;
pub mod pipeline;
pub mod watcher;

pub use app::{App, Models};
pub use error::{Error, Result};
