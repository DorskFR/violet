//! Sign-in sessions, in their own SQLite file next to the index. Not library data: losing the
//! file signs everyone out and nothing else.

use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};

use rusqlite::{Connection, OptionalExtension, params};

/// One session. `hash` is the SHA-256 of its token; the token itself is never stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
	pub hash: String,
	pub user: String,
	/// `password:<sha256 of the PHC string>` or `passkey:<credential id>`.
	pub credential: String,
	pub csrf: String,
	/// Unix seconds.
	pub created: i64,
	/// When its user last proved who they are with a password or a passkey; 0 when unknown.
	pub signed_in: i64,
	pub last_seen: i64,
	/// The latest it can be used, whatever its activity.
	pub expires: i64,
	pub label: String,
}

pub struct Sessions {
	conn: Mutex<Connection>,
}

const COLUMNS: &str = "hash, user, credential, csrf, created, last_seen, expires, label, signed_in";

fn row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Session> {
	Ok(Session {
		hash: row.get(0)?,
		user: row.get(1)?,
		credential: row.get(2)?,
		csrf: row.get(3)?,
		created: row.get(4)?,
		last_seen: row.get(5)?,
		expires: row.get(6)?,
		label: row.get(7)?,
		signed_in: row.get(8)?,
	})
}

impl Sessions {
	pub fn open(path: &Path) -> crate::Result<Self> {
		let conn = Connection::open(path)?;
		conn.busy_timeout(std::time::Duration::from_secs(5))?;
		conn.pragma_update(None, "journal_mode", "WAL")?;
		conn.pragma_update(None, "synchronous", "NORMAL")?;
		conn.execute_batch(
			"CREATE TABLE IF NOT EXISTS sessions (
				hash TEXT PRIMARY KEY,
				user TEXT NOT NULL,
				credential TEXT NOT NULL,
				csrf TEXT NOT NULL,
				created INTEGER NOT NULL,
				last_seen INTEGER NOT NULL,
				expires INTEGER NOT NULL,
				label TEXT NOT NULL,
				signed_in INTEGER NOT NULL DEFAULT 0
			);
			CREATE INDEX IF NOT EXISTS sessions_user ON sessions (user COLLATE NOCASE);",
		)?;
		let has_signed_in: bool = conn.query_row(
			"SELECT count(*) > 0 FROM pragma_table_info('sessions') WHERE name = 'signed_in'",
			[],
			|row| row.get(0),
		)?;
		if !has_signed_in {
			conn.execute_batch(
				"ALTER TABLE sessions ADD COLUMN signed_in INTEGER NOT NULL DEFAULT 0",
			)?;
		}
		Ok(Self {
			conn: Mutex::new(conn),
		})
	}

	fn conn(&self) -> MutexGuard<'_, Connection> {
		self.conn.lock().unwrap_or_else(PoisonError::into_inner)
	}

	pub fn insert(&self, session: &Session) -> crate::Result<()> {
		self.conn().execute(
			&format!(
				"INSERT INTO sessions ({COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)"
			),
			params![
				session.hash,
				session.user,
				session.credential,
				session.csrf,
				session.created,
				session.last_seen,
				session.expires,
				session.label,
				session.signed_in
			],
		)?;
		Ok(())
	}

	pub fn get(&self, hash: &str) -> crate::Result<Option<Session>> {
		Ok(self
			.conn()
			.query_row(
				&format!("SELECT {COLUMNS} FROM sessions WHERE hash = ?1"),
				[hash],
				row,
			)
			.optional()?)
	}

	pub fn touch(&self, hash: &str, at: i64) -> crate::Result<()> {
		self.conn().execute(
			"UPDATE sessions SET last_seen = ?2 WHERE hash = ?1",
			params![hash, at],
		)?;
		Ok(())
	}

	pub fn delete(&self, hash: &str) -> crate::Result<bool> {
		Ok(self
			.conn()
			.execute("DELETE FROM sessions WHERE hash = ?1", [hash])?
			> 0)
	}

	/// The sessions of `user`, newest first.
	pub fn list(&self, user: &str) -> crate::Result<Vec<Session>> {
		let conn = self.conn();
		let mut statement = conn.prepare(&format!(
			"SELECT {COLUMNS} FROM sessions WHERE user = ?1 COLLATE NOCASE
			 ORDER BY created DESC, hash"
		))?;
		let sessions = statement
			.query_map([user], row)?
			.collect::<rusqlite::Result<Vec<_>>>()?;
		Ok(sessions)
	}

	/// Deletes the sessions that were idle since `idle_before` or ran past their end.
	pub fn prune(&self, now: i64, idle_before: i64) -> crate::Result<usize> {
		Ok(self.conn().execute(
			"DELETE FROM sessions WHERE expires <= ?1 OR last_seen <= ?2",
			params![now, idle_before],
		)?)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn session(hash: &str, user: &str, created: i64) -> Session {
		Session {
			hash: hash.into(),
			user: user.into(),
			credential: "password:aa".into(),
			csrf: "c".into(),
			created,
			signed_in: created,
			last_seen: created,
			expires: created + 100,
			label: "Firefox on Linux".into(),
		}
	}

	#[test]
	fn sessions_are_stored_listed_touched_and_pruned() {
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join(".sessions.sqlite");
		let sessions = Sessions::open(&path).unwrap();
		sessions.insert(&session("a", "sam", 10)).unwrap();
		sessions.insert(&session("b", "Sam", 20)).unwrap();
		sessions.insert(&session("c", "alex", 30)).unwrap();
		assert!(sessions.insert(&session("a", "sam", 40)).is_err());
		let listed: Vec<String> = sessions
			.list("SAM")
			.unwrap()
			.into_iter()
			.map(|session| session.hash)
			.collect();
		assert_eq!(listed, ["b", "a"]);
		sessions.touch("a", 50).unwrap();
		assert_eq!(sessions.get("a").unwrap().unwrap().last_seen, 50);
		assert!(sessions.delete("c").unwrap());
		assert!(!sessions.delete("c").unwrap());
		assert_eq!(sessions.get("c").unwrap(), None);
		drop(sessions);

		let sessions = Sessions::open(&path).unwrap();
		assert_eq!(sessions.prune(105, 45).unwrap(), 1, "b idles out");
		assert!(sessions.get("a").unwrap().is_some());
		assert_eq!(sessions.prune(110, 0).unwrap(), 1, "a runs past its end");
		assert!(sessions.list("sam").unwrap().is_empty());
	}

	#[test]
	fn a_file_from_before_sign_in_times_gains_the_column() {
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join(".sessions.sqlite");
		Connection::open(&path)
			.unwrap()
			.execute_batch(
				"CREATE TABLE sessions (hash TEXT PRIMARY KEY, user TEXT NOT NULL,
				credential TEXT NOT NULL, csrf TEXT NOT NULL, created INTEGER NOT NULL,
				last_seen INTEGER NOT NULL, expires INTEGER NOT NULL, label TEXT NOT NULL);
				INSERT INTO sessions VALUES ('old', 'sam', 'password:aa', 'c', 1, 1, 9, 'x');",
			)
			.unwrap();
		let sessions = Sessions::open(&path).unwrap();
		assert_eq!(sessions.get("old").unwrap().unwrap().signed_in, 0);
		sessions.insert(&session("new", "sam", 5)).unwrap();
		assert_eq!(sessions.get("new").unwrap().unwrap().signed_in, 5);
		drop(sessions);
		assert!(Sessions::open(&path).is_ok());
	}
}
