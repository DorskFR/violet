//! The library on disk: folder layout, sidecars, file naming, the SQLite index, the journal and
//! the job queue. Files and sidecars are the source of truth; the index is rebuilt from them.

pub mod hash;
pub mod index;
pub mod jobs;
pub mod journal;
pub mod library;
pub mod naming;
pub mod sidecar;

pub use index::{Facet, Facets, Hit, Index, Query, SearchResult};
pub use jobs::{Job, JobKind, JobQueue, JobState};
pub use journal::{Action, Entry, Journal};
pub use library::Library;
pub use sidecar::{Fact, PageText, Sidecar, Source, TextSource};

#[derive(Debug, thiserror::Error)]
pub enum Error {
	#[error("io: {0}")]
	Io(#[from] std::io::Error),
	#[error("sqlite: {0}")]
	Sqlite(#[from] rusqlite::Error),
	#[error("json: {0}")]
	Json(#[from] serde_json::Error),
	#[error("not found: {0}")]
	NotFound(String),
	#[error("invalid: {0}")]
	Invalid(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
