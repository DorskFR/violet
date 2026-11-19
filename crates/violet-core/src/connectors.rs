//! What connectors keep between runs, in their own SQLite file next to the index: the secret
//! state of each (such as a trusted-device cookie) and its last runs. Not library data: losing
//! the file means signing in to each site again, and one run right away, and nothing else.

use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};

use rusqlite::{Connection, OptionalExtension, params};

/// The last runs of one connector. Times are Unix seconds.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConnectorRun {
	pub last_run: Option<i64>,
	pub last_success: Option<i64>,
	pub last_error: Option<String>,
	/// JSON of what the last run did.
	pub last_report: Option<String>,
	/// The site asked for a code a scheduled run cannot give.
	pub needs_login: bool,
}

pub struct ConnectorStore {
	conn: Mutex<Connection>,
}

impl ConnectorStore {
	pub fn open(path: &Path) -> crate::Result<Self> {
		let conn = Connection::open(path)?;
		conn.busy_timeout(std::time::Duration::from_secs(5))?;
		conn.pragma_update(None, "journal_mode", "WAL")?;
		conn.pragma_update(None, "synchronous", "NORMAL")?;
		conn.execute_batch(
			"CREATE TABLE IF NOT EXISTS connectors (
				name TEXT PRIMARY KEY,
				state TEXT,
				last_run INTEGER,
				last_success INTEGER,
				last_error TEXT,
				last_report TEXT,
				needs_login INTEGER NOT NULL DEFAULT 0
			);",
		)?;
		Ok(Self {
			conn: Mutex::new(conn),
		})
	}

	fn conn(&self) -> MutexGuard<'_, Connection> {
		self.conn.lock().unwrap_or_else(PoisonError::into_inner)
	}

	/// The JSON a connector saved, as text.
	pub fn state(&self, name: &str) -> crate::Result<Option<String>> {
		Ok(self
			.conn()
			.query_row(
				"SELECT state FROM connectors WHERE name = ?1",
				[name],
				|row| row.get::<_, Option<String>>(0),
			)
			.optional()?
			.flatten())
	}

	pub fn set_state(&self, name: &str, state: Option<&str>) -> crate::Result<()> {
		self.conn().execute(
			"INSERT INTO connectors (name, state) VALUES (?1, ?2)
			 ON CONFLICT (name) DO UPDATE SET state = ?2",
			params![name, state],
		)?;
		Ok(())
	}

	pub fn set_needs_login(&self, name: &str, needs_login: bool) -> crate::Result<()> {
		self.conn().execute(
			"INSERT INTO connectors (name, needs_login) VALUES (?1, ?2)
			 ON CONFLICT (name) DO UPDATE SET needs_login = ?2",
			params![name, needs_login],
		)?;
		Ok(())
	}

	pub fn run(&self, name: &str) -> crate::Result<ConnectorRun> {
		Ok(self
			.conn()
			.query_row(
				"SELECT last_run, last_success, last_error, last_report, needs_login
				 FROM connectors WHERE name = ?1",
				[name],
				|row| {
					Ok(ConnectorRun {
						last_run: row.get(0)?,
						last_success: row.get(1)?,
						last_error: row.get(2)?,
						last_report: row.get(3)?,
						needs_login: row.get(4)?,
					})
				},
			)
			.optional()?
			.unwrap_or_default())
	}

	/// Records a finished run; `error` is none when it succeeded.
	pub fn finish_run(
		&self,
		name: &str,
		at: i64,
		error: Option<&str>,
		report: Option<&str>,
	) -> crate::Result<()> {
		let success = error.is_none().then_some(at);
		self.conn().execute(
			"INSERT INTO connectors (name, last_run, last_success, last_error, last_report)
			 VALUES (?1, ?2, ?3, ?4, ?5)
			 ON CONFLICT (name) DO UPDATE SET last_run = ?2,
				last_success = COALESCE(?3, last_success), last_error = ?4,
				last_report = COALESCE(?5, last_report)",
			params![name, at, success, error, report],
		)?;
		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn state_and_runs_are_kept_across_a_reopen() {
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join(".connectors.sqlite");
		let store = ConnectorStore::open(&path).unwrap();
		assert_eq!(store.state("pay").unwrap(), None);
		assert_eq!(store.run("pay").unwrap(), ConnectorRun::default());
		store.set_state("pay", Some(r#"{"device":"x"}"#)).unwrap();
		store.finish_run("pay", 100, None, Some("{}")).unwrap();
		store
			.finish_run("pay", 200, Some("timed out"), None)
			.unwrap();
		store.set_needs_login("pay", true).unwrap();
		store.set_needs_login("bank", true).unwrap();
		drop(store);

		let store = ConnectorStore::open(&path).unwrap();
		assert_eq!(
			store.state("pay").unwrap().as_deref(),
			Some(r#"{"device":"x"}"#)
		);
		assert_eq!(
			store.run("pay").unwrap(),
			ConnectorRun {
				last_run: Some(200),
				last_success: Some(100),
				last_error: Some("timed out".into()),
				last_report: Some("{}".into()),
				needs_login: true,
			}
		);
		assert_eq!(store.state("bank").unwrap(), None);
		assert!(store.run("bank").unwrap().needs_login);
		store.set_state("pay", None).unwrap();
		store.set_needs_login("pay", false).unwrap();
		assert_eq!(store.state("pay").unwrap(), None);
		assert!(!store.run("pay").unwrap().needs_login);
	}
}
