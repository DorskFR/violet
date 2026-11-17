use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

/// Append-only `.journal.jsonl`. An undo appends an `Undo` entry; nothing is rewritten.
pub struct Journal {
	path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
	pub id: Uuid,
	#[serde(with = "time::serde::rfc3339")]
	pub at: OffsetDateTime,
	/// "agent" or "user".
	pub actor: String,
	pub action: Action,
	/// One line for the activity feed.
	pub summary: String,
}

/// Every change to the library, with what is needed to reverse it. Paths are relative to
/// `library/`; `original` is a name in `.originals/`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Action {
	/// An inbox file became one or more documents.
	Ingest { original: String, original_name: String, documents: Vec<(Uuid, PathBuf)> },
	/// An inbox file was an exact duplicate of an existing document and was stashed.
	Duplicate { original: String, original_name: String, of: Uuid },
	Split { original: String, from: (Uuid, PathBuf), into: Vec<(Uuid, PathBuf)> },
	Merge { originals: Vec<(String, Uuid, PathBuf)>, into: (Uuid, PathBuf) },
	/// Pages rotated or deleted; `original` is the file before the edit.
	EditPages { original: String, document: (Uuid, PathBuf), detail: String },
	/// Metadata changed, which may also move the file.
	Update { document: Uuid, from: PathBuf, to: PathBuf, before: Box<crate::Sidecar> },
	Delete { original: String, document: (Uuid, PathBuf), sidecar: Box<crate::Sidecar> },
	Undo { entry: Uuid },
}

impl Journal {
	pub fn open(path: &Path) -> Self {
		Self { path: path.to_path_buf() }
	}
	/// Appends one line and fsyncs.
	pub fn append(&self, entry: &Entry) -> crate::Result<()> {
		todo!()
	}
	/// All entries, oldest first. A torn last line is skipped.
	pub fn entries(&self) -> crate::Result<Vec<Entry>> {
		todo!()
	}
	/// Newest first, at most `limit`, with whether each was undone.
	pub fn recent(&self, limit: usize) -> crate::Result<Vec<(Entry, bool)>> {
		todo!()
	}
}
