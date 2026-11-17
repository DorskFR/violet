use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// Work queue in the index database. A job is keyed by `(kind, key)`, so enqueueing the same
/// file twice is a no-op, and a job claimed by a process that died is reclaimed on `recover`.
pub struct JobQueue<'a> {
	conn: &'a rusqlite::Connection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
	/// `key` is the inbox file's sha256, `payload` `{"path": "<inbox-relative>"}`.
	Ingest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
	Pending,
	Running,
	Done,
	Failed,
}

#[derive(Debug, Clone, Serialize)]
pub struct Job {
	pub id: i64,
	pub kind: JobKind,
	pub key: String,
	pub payload: serde_json::Value,
	pub state: JobState,
	pub attempts: u32,
	pub error: Option<String>,
	#[serde(with = "time::serde::rfc3339")]
	pub updated_at: OffsetDateTime,
}

impl<'a> JobQueue<'a> {
	/// Creates the table if missing.
	pub fn new(conn: &'a rusqlite::Connection) -> crate::Result<Self> {
		todo!()
	}
	/// Returns false when a job with this kind and key exists in any state but `failed`;
	/// a failed one is reset to pending.
	pub fn enqueue(&self, kind: JobKind, key: &str, payload: &serde_json::Value) -> crate::Result<bool> {
		todo!()
	}
	/// Oldest pending job, marked running.
	pub fn claim(&self) -> crate::Result<Option<Job>> {
		todo!()
	}
	pub fn complete(&self, id: i64) -> crate::Result<()> {
		todo!()
	}
	pub fn fail(&self, id: i64, error: &str) -> crate::Result<()> {
		todo!()
	}
	/// Running jobs back to pending, at startup.
	pub fn recover(&self) -> crate::Result<usize> {
		todo!()
	}
	/// Counts per state.
	pub fn counts(&self) -> crate::Result<Vec<(JobState, u64)>> {
		todo!()
	}
	/// Most recent failed jobs.
	pub fn failed(&self, limit: u32) -> crate::Result<Vec<Job>> {
		todo!()
	}
}
