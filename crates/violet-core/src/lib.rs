//! The library on disk: folder layout, sidecars, file naming, the SQLite index, the journal and
//! the job queue. Files and sidecars are the source of truth; the index is rebuilt from them.

pub mod hash;
pub mod index;
pub mod jobs;
pub mod journal;
pub mod library;
pub mod naming;
pub mod sidecar;

pub use index::{
	Facet, Facets, Hit, Index, MailHint, MailRecord, MailStatus, ProcessedMail, Query, SearchResult,
};
pub use jobs::{Job, JobKind, JobQueue, JobState};
pub use journal::{Action, Entry, Journal, Pending, Step};
pub use library::{Library, Scan};
pub use sidecar::{DatePrecision, Fact, MailSource, PageText, Sidecar, Source, TextSource};

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
	#[error("corrupt: {0}")]
	Corrupt(String),
	/// Another process holds the library.
	#[error("locked: {0}")]
	Locked(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[cfg(test)]
pub(crate) mod testutil {
	use time::OffsetDateTime;
	use uuid::Uuid;

	use crate::{Sidecar, sidecar::SIDECAR_VERSION};

	pub fn sidecar(title: &str) -> Sidecar {
		let at = OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap();
		Sidecar {
			version: SIDECAR_VERSION,
			id: Uuid::new_v4(),
			title: title.to_string(),
			area: "Misc".to_string(),
			doc_type: "letter".to_string(),
			issuer: None,
			date: None,
			date_precision: Default::default(),
			language: None,
			summary: None,
			tags: Vec::new(),
			facts: Vec::new(),
			pages: Vec::new(),
			sources: Vec::new(),
			sha256: String::new(),
			confidence: 1.0,
			created_at: at,
			updated_at: at,
			extra: serde_json::Map::new(),
		}
	}
}
