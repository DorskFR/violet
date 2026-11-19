//! The violet server: ingest pipeline, file operations with undo, HTTP API and CLI plumbing.

pub mod account;
pub mod app;
pub mod auth;
pub mod cache;
pub mod error;
pub mod extract;
pub mod http;
pub mod login;
pub mod mail;
pub mod ops;
pub mod pipeline;
pub mod users;
pub mod watcher;

pub use app::{App, Models, Reindexed};
pub use error::{Error, Result};
