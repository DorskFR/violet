use std::path::{Path, PathBuf};

use serde::Serialize;
use uuid::Uuid;

/// The SQLite index at `.index.sqlite`: documents, tags, facts and an FTS5 table over page
/// text, title, issuer, tags and summary. WAL mode. Safe to delete: `rebuild` recreates it.
pub struct Index {
	conn: rusqlite::Connection,
}

#[derive(Debug, Clone, Default)]
pub struct Query {
	/// FTS5 text; plain words are matched as prefixes, quotes kept as phrases.
	pub text: Option<String>,
	pub area: Option<String>,
	pub doc_type: Option<String>,
	pub issuer: Option<String>,
	pub tags: Vec<String>,
	/// Inclusive `YYYY-MM-DD` bounds on the document date.
	pub date_from: Option<String>,
	pub date_to: Option<String>,
	/// Bounds on any fact amount in minor units (any currency unless `currency` is set).
	pub amount_min: Option<i64>,
	pub amount_max: Option<i64>,
	pub currency: Option<String>,
	pub limit: u32,
	pub offset: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct Hit {
	pub id: Uuid,
	/// Relative to `library/`.
	pub path: PathBuf,
	pub title: String,
	pub area: String,
	pub doc_type: String,
	pub issuer: Option<String>,
	pub date: Option<String>,
	pub tags: Vec<String>,
	/// The `total` fact if present, else the largest amount.
	pub amount_minor: Option<i64>,
	pub currency: Option<String>,
	pub pages: u32,
	/// Matching text with the terms wrapped in `<mark>`; HTML-escaped otherwise.
	pub snippet: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchResult {
	pub total: u64,
	pub hits: Vec<Hit>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Facet {
	pub value: String,
	pub count: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Facets {
	pub areas: Vec<Facet>,
	pub doc_types: Vec<Facet>,
	pub issuers: Vec<Facet>,
	pub tags: Vec<Facet>,
	pub years: Vec<Facet>,
}

impl Index {
	/// Opens or creates the index and runs migrations.
	pub fn open(path: &Path) -> crate::Result<Self> {
		todo!()
	}
	/// Replaces the documents with the library's sidecars. Jobs are kept.
	pub fn rebuild(&mut self, library: &crate::Library) -> crate::Result<usize> {
		todo!()
	}
	/// Inserts or replaces one document by `sidecar.id`.
	pub fn upsert(&mut self, path: &Path, sidecar: &crate::Sidecar) -> crate::Result<()> {
		todo!()
	}
	pub fn remove(&mut self, id: Uuid) -> crate::Result<()> {
		todo!()
	}
	/// The document's current path and sidecar.
	pub fn get(&self, id: Uuid) -> crate::Result<Option<(PathBuf, crate::Sidecar)>> {
		todo!()
	}
	/// A document with exactly this file sha256 or source sha256.
	pub fn find_by_sha256(&self, sha256: &str) -> crate::Result<Option<Uuid>> {
		todo!()
	}
	/// The most similar document by text (word-shingle Jaccard), if at or above `threshold`.
	pub fn similar(&self, text: &str, threshold: f32) -> crate::Result<Option<(Uuid, f32)>> {
		todo!()
	}
	/// Newest documents first when `query.text` is empty, else by FTS rank.
	pub fn search(&self, query: &Query) -> crate::Result<SearchResult> {
		todo!()
	}
	pub fn facets(&self) -> crate::Result<Facets> {
		todo!()
	}
	/// Known values, to keep the analysis consistent with the existing library.
	pub fn areas(&self) -> crate::Result<Vec<String>> {
		todo!()
	}
	pub fn issuers(&self) -> crate::Result<Vec<String>> {
		todo!()
	}
	pub fn tags(&self) -> crate::Result<Vec<String>> {
		todo!()
	}
	/// Shared connection for the job queue, which lives in the same file.
	pub fn connection(&self) -> &rusqlite::Connection {
		&self.conn
	}
}
