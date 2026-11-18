use rusqlite::{OptionalExtension, params};
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

const COLUMNS: &str = "id, kind, key, payload, state, attempts, error, updated_at";
const STATES: [JobState; 4] = [
	JobState::Pending,
	JobState::Running,
	JobState::Done,
	JobState::Failed,
];

impl JobKind {
	fn as_str(self) -> &'static str {
		match self {
			Self::Ingest => "ingest",
		}
	}
	fn parse(value: &str) -> Option<Self> {
		match value {
			"ingest" => Some(Self::Ingest),
			_ => None,
		}
	}
}

impl JobState {
	fn as_str(self) -> &'static str {
		match self {
			Self::Pending => "pending",
			Self::Running => "running",
			Self::Done => "done",
			Self::Failed => "failed",
		}
	}
	fn parse(value: &str) -> Option<Self> {
		STATES.into_iter().find(|state| state.as_str() == value)
	}
}

impl<'a> JobQueue<'a> {
	/// Creates the table if missing.
	pub fn new(conn: &'a rusqlite::Connection) -> crate::Result<Self> {
		conn.execute_batch(
			"CREATE TABLE IF NOT EXISTS jobs (
				id INTEGER PRIMARY KEY AUTOINCREMENT,
				kind TEXT NOT NULL,
				key TEXT NOT NULL,
				payload TEXT NOT NULL,
				state TEXT NOT NULL,
				attempts INTEGER NOT NULL DEFAULT 0,
				error TEXT,
				updated_at INTEGER NOT NULL,
				UNIQUE (kind, key)
			);
			CREATE INDEX IF NOT EXISTS jobs_state ON jobs (state, id);",
		)?;
		Ok(Self { conn })
	}
	/// Returns false when a job with this kind and key exists in any state but `failed`;
	/// a failed one is reset to pending.
	pub fn enqueue(
		&self,
		kind: JobKind,
		key: &str,
		payload: &serde_json::Value,
	) -> crate::Result<bool> {
		let changed = self.conn.execute(
			"INSERT INTO jobs (kind, key, payload, state, updated_at)
			 VALUES (?1, ?2, ?3, 'pending', ?4)
			 ON CONFLICT (kind, key) DO UPDATE
			 SET state = 'pending', payload = excluded.payload, error = NULL,
			     updated_at = excluded.updated_at
			 WHERE jobs.state = 'failed'",
			params![kind.as_str(), key, serde_json::to_string(payload)?, now()],
		)?;
		Ok(changed > 0)
	}
	/// A done job back to pending, for a file dropped again after it was processed. Returns
	/// false when no job with this kind and key is done.
	pub fn reopen(
		&self,
		kind: JobKind,
		key: &str,
		payload: &serde_json::Value,
	) -> crate::Result<bool> {
		let changed = self.conn.execute(
			"UPDATE jobs SET state = 'pending', payload = ?3, error = NULL, updated_at = ?4
			 WHERE kind = ?1 AND key = ?2 AND state = 'done'",
			params![kind.as_str(), key, serde_json::to_string(payload)?, now()],
		)?;
		Ok(changed > 0)
	}
	pub fn state(&self, kind: JobKind, key: &str) -> crate::Result<Option<JobState>> {
		let state: Option<String> = self
			.conn
			.query_row(
				"SELECT state FROM jobs WHERE kind = ?1 AND key = ?2",
				params![kind.as_str(), key],
				|row| row.get(0),
			)
			.optional()?;
		Ok(state.as_deref().and_then(JobState::parse))
	}
	/// Oldest pending job, marked running.
	pub fn claim(&self) -> crate::Result<Option<Job>> {
		let sql = format!(
			"UPDATE jobs SET state = 'running', attempts = attempts + 1, updated_at = ?1
			 WHERE id = (SELECT id FROM jobs WHERE state = 'pending' ORDER BY id LIMIT 1)
			 RETURNING {COLUMNS}"
		);
		Ok(self
			.conn
			.query_row(&sql, params![now()], read_job)
			.optional()?)
	}
	pub fn complete(&self, id: i64) -> crate::Result<()> {
		self.finish(id, JobState::Done, None)
	}
	pub fn fail(&self, id: i64, error: &str) -> crate::Result<()> {
		self.finish(id, JobState::Failed, Some(error))
	}
	/// Running jobs back to pending, at startup.
	pub fn recover(&self) -> crate::Result<usize> {
		Ok(self.conn.execute(
			"UPDATE jobs SET state = 'pending', updated_at = ?1 WHERE state = 'running'",
			params![now()],
		)?)
	}
	/// Counts per state, every state listed.
	pub fn counts(&self) -> crate::Result<Vec<(JobState, u64)>> {
		let mut counts: Vec<(JobState, u64)> = STATES.into_iter().map(|state| (state, 0)).collect();
		let mut statement = self
			.conn
			.prepare("SELECT state, COUNT(*) FROM jobs GROUP BY state")?;
		let rows = statement.query_map([], |row| {
			Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
		})?;
		for row in rows {
			let (state, count) = row?;
			if let Some(slot) = counts.iter_mut().find(|(known, _)| known.as_str() == state) {
				slot.1 = count.unsigned_abs();
			}
		}
		Ok(counts)
	}
	/// Most recent failed jobs.
	pub fn failed(&self, limit: u32) -> crate::Result<Vec<Job>> {
		let sql = format!(
			"SELECT {COLUMNS} FROM jobs WHERE state = 'failed'
			 ORDER BY updated_at DESC, id DESC LIMIT ?1"
		);
		let mut statement = self.conn.prepare(&sql)?;
		let jobs = statement
			.query_map(params![limit], read_job)?
			.collect::<Result<_, _>>()?;
		Ok(jobs)
	}

	fn finish(&self, id: i64, state: JobState, error: Option<&str>) -> crate::Result<()> {
		let changed = self.conn.execute(
			"UPDATE jobs SET state = ?2, error = ?3, updated_at = ?4 WHERE id = ?1",
			params![id, state.as_str(), error, now()],
		)?;
		if changed == 0 {
			return Err(crate::Error::NotFound(format!("job {id}")));
		}
		Ok(())
	}
}

fn now() -> i64 {
	OffsetDateTime::now_utc().unix_timestamp_nanos() as i64
}

fn read_job(row: &rusqlite::Row<'_>) -> rusqlite::Result<Job> {
	let invalid = |column: usize, what: String| {
		rusqlite::Error::FromSqlConversionFailure(column, rusqlite::types::Type::Text, what.into())
	};
	let kind: String = row.get(1)?;
	let payload: String = row.get(3)?;
	let state: String = row.get(4)?;
	let updated_at: i64 = row.get(7)?;
	Ok(Job {
		id: row.get(0)?,
		kind: JobKind::parse(&kind).ok_or_else(|| invalid(1, format!("unknown kind {kind}")))?,
		key: row.get(2)?,
		payload: serde_json::from_str(&payload).map_err(|error| invalid(3, error.to_string()))?,
		state: JobState::parse(&state)
			.ok_or_else(|| invalid(4, format!("unknown state {state}")))?,
		attempts: row.get(5)?,
		error: row.get(6)?,
		updated_at: OffsetDateTime::from_unix_timestamp_nanos(i128::from(updated_at))
			.map_err(|error| invalid(7, error.to_string()))?,
	})
}

#[cfg(test)]
mod tests {
	use serde_json::json;

	use super::*;

	fn conn() -> rusqlite::Connection {
		rusqlite::Connection::open_in_memory().unwrap()
	}

	fn count(queue: &JobQueue<'_>, state: JobState) -> u64 {
		queue
			.counts()
			.unwrap()
			.into_iter()
			.find(|(known, _)| *known == state)
			.unwrap()
			.1
	}

	#[test]
	fn new_is_repeatable_and_starts_empty() {
		let conn = conn();
		JobQueue::new(&conn).unwrap();
		let queue = JobQueue::new(&conn).unwrap();
		assert_eq!(
			queue.counts().unwrap(),
			vec![
				(JobState::Pending, 0),
				(JobState::Running, 0),
				(JobState::Done, 0),
				(JobState::Failed, 0)
			]
		);
		assert!(queue.claim().unwrap().is_none());
		assert!(queue.failed(10).unwrap().is_empty());
	}

	#[test]
	fn enqueue_is_keyed_by_kind_and_key() {
		let conn = conn();
		let queue = JobQueue::new(&conn).unwrap();
		assert!(
			queue
				.enqueue(JobKind::Ingest, "aaa", &json!({"path": "a.pdf"}))
				.unwrap()
		);
		assert!(
			!queue
				.enqueue(JobKind::Ingest, "aaa", &json!({"path": "copy.pdf"}))
				.unwrap()
		);
		assert!(
			queue
				.enqueue(JobKind::Ingest, "bbb", &json!({"path": "b.pdf"}))
				.unwrap()
		);
		assert_eq!(count(&queue, JobState::Pending), 2);
	}

	#[test]
	fn state_follows_the_job() {
		let conn = conn();
		let queue = JobQueue::new(&conn).unwrap();
		assert_eq!(queue.state(JobKind::Ingest, "aaa").unwrap(), None);
		queue
			.enqueue(JobKind::Ingest, "aaa", &json!({"path": "a.pdf"}))
			.unwrap();
		assert_eq!(
			queue.state(JobKind::Ingest, "aaa").unwrap(),
			Some(JobState::Pending)
		);
		let job = queue.claim().unwrap().unwrap();
		assert_eq!(
			queue.state(JobKind::Ingest, "aaa").unwrap(),
			Some(JobState::Running)
		);
		queue.fail(job.id, "boom").unwrap();
		assert_eq!(
			queue.state(JobKind::Ingest, "aaa").unwrap(),
			Some(JobState::Failed)
		);
	}

	#[test]
	fn claim_takes_the_oldest_pending_job() {
		let conn = conn();
		let queue = JobQueue::new(&conn).unwrap();
		queue
			.enqueue(JobKind::Ingest, "aaa", &json!({"path": "a.pdf"}))
			.unwrap();
		queue
			.enqueue(JobKind::Ingest, "bbb", &json!({"path": "b.pdf"}))
			.unwrap();

		let first = queue.claim().unwrap().unwrap();
		assert_eq!(first.key, "aaa");
		assert_eq!(first.kind, JobKind::Ingest);
		assert_eq!(first.payload, json!({"path": "a.pdf"}));
		assert_eq!(first.state, JobState::Running);
		assert_eq!(first.attempts, 1);
		assert_eq!(first.error, None);
		assert!(first.updated_at.year() >= 2024);

		assert_eq!(queue.claim().unwrap().unwrap().key, "bbb");
		assert!(queue.claim().unwrap().is_none());
		assert_eq!(count(&queue, JobState::Running), 2);
		assert!(!queue.enqueue(JobKind::Ingest, "aaa", &json!({})).unwrap());
	}

	#[test]
	fn completed_jobs_are_not_enqueued_again() {
		let conn = conn();
		let queue = JobQueue::new(&conn).unwrap();
		queue.enqueue(JobKind::Ingest, "aaa", &json!({})).unwrap();
		let job = queue.claim().unwrap().unwrap();
		queue.complete(job.id).unwrap();
		assert_eq!(count(&queue, JobState::Done), 1);
		assert!(!queue.enqueue(JobKind::Ingest, "aaa", &json!({})).unwrap());
		assert!(queue.claim().unwrap().is_none());
	}

	#[test]
	fn reopen_requeues_only_done_jobs() {
		let conn = conn();
		let queue = JobQueue::new(&conn).unwrap();
		let again = json!({"path": "again.pdf"});
		assert!(!queue.reopen(JobKind::Ingest, "aaa", &again).unwrap());
		queue
			.enqueue(JobKind::Ingest, "aaa", &json!({"path": "a.pdf"}))
			.unwrap();
		assert!(!queue.reopen(JobKind::Ingest, "aaa", &again).unwrap());
		let job = queue.claim().unwrap().unwrap();
		assert!(!queue.reopen(JobKind::Ingest, "aaa", &again).unwrap());
		queue.complete(job.id).unwrap();
		assert!(queue.reopen(JobKind::Ingest, "aaa", &again).unwrap());
		assert_eq!(count(&queue, JobState::Pending), 1);
		let job = queue.claim().unwrap().unwrap();
		assert_eq!(job.payload, again);
		assert_eq!(job.attempts, 2);
	}

	#[test]
	fn failed_jobs_are_listed_and_can_be_retried() {
		let conn = conn();
		let queue = JobQueue::new(&conn).unwrap();
		for key in ["aaa", "bbb", "ccc"] {
			queue
				.enqueue(JobKind::Ingest, key, &json!({"path": key}))
				.unwrap();
		}
		let a = queue.claim().unwrap().unwrap();
		let b = queue.claim().unwrap().unwrap();
		queue.fail(a.id, "model timed out").unwrap();
		queue.fail(b.id, "bad pdf").unwrap();

		let failed = queue.failed(10).unwrap();
		assert_eq!(
			failed
				.iter()
				.map(|job| job.key.as_str())
				.collect::<Vec<_>>(),
			["bbb", "aaa"]
		);
		assert_eq!(failed[0].state, JobState::Failed);
		assert_eq!(failed[0].error.as_deref(), Some("bad pdf"));
		assert_eq!(queue.failed(1).unwrap().len(), 1);

		assert!(
			queue
				.enqueue(JobKind::Ingest, "aaa", &json!({"path": "again"}))
				.unwrap()
		);
		assert_eq!(count(&queue, JobState::Failed), 1);
		let retried = queue.claim().unwrap().unwrap();
		assert_eq!(retried.id, a.id);
		assert_eq!(retried.payload, json!({"path": "again"}));
		assert_eq!(retried.attempts, 2);
		assert_eq!(retried.error, None);
	}

	#[test]
	fn recover_returns_running_jobs_to_pending() {
		let conn = conn();
		let queue = JobQueue::new(&conn).unwrap();
		for key in ["aaa", "bbb", "ccc"] {
			queue.enqueue(JobKind::Ingest, key, &json!({})).unwrap();
		}
		let done = queue.claim().unwrap().unwrap();
		queue.complete(done.id).unwrap();
		let stuck = queue.claim().unwrap().unwrap();

		assert_eq!(queue.recover().unwrap(), 1);
		assert_eq!(queue.recover().unwrap(), 0);
		assert_eq!(count(&queue, JobState::Running), 0);
		assert_eq!(count(&queue, JobState::Pending), 2);
		assert_eq!(count(&queue, JobState::Done), 1);
		let again = queue.claim().unwrap().unwrap();
		assert_eq!(again.id, stuck.id);
		assert_eq!(again.attempts, 2);
	}

	#[test]
	fn finishing_an_unknown_job_is_not_found() {
		let conn = conn();
		let queue = JobQueue::new(&conn).unwrap();
		assert!(matches!(queue.complete(42), Err(crate::Error::NotFound(_))));
		assert!(matches!(
			queue.fail(42, "x"),
			Err(crate::Error::NotFound(_))
		));
	}

	#[test]
	fn jobs_serialise_with_snake_case_states() {
		let conn = conn();
		let queue = JobQueue::new(&conn).unwrap();
		queue
			.enqueue(JobKind::Ingest, "aaa", &json!({"path": "a.pdf"}))
			.unwrap();
		let job = serde_json::to_value(queue.claim().unwrap().unwrap()).unwrap();
		assert_eq!(job["kind"], "ingest");
		assert_eq!(job["state"], "running");
		assert_eq!(job["payload"]["path"], "a.pdf");
		assert!(job["updated_at"].as_str().unwrap().contains('T'));
	}
}
