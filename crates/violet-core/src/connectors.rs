//! The connectors and what they keep between runs, in their own SQLite file next to the index:
//! what each one is, what it signs in with, its secret state (such as a trusted-device cookie)
//! and its last runs. A removed connector leaves its name and owner behind, so that only its
//! owner picks up where it stopped. Not library data: losing the file means adding the
//! connectors again and nothing else.

use std::fmt;
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

/// A connector as it was added, unchecked.
#[derive(Clone, PartialEq, Eq)]
pub struct ConnectorDefinition {
	pub name: String,
	pub kind: String,
	/// The user who added it; none for the implicit user of a library without users.
	pub owner: Option<String>,
	pub username: String,
	pub password: String,
	pub schedule: String,
	pub utc_offset: String,
	pub area: Option<String>,
	pub tags: Vec<String>,
	/// JSON of the settings of the kind.
	pub options: String,
}

impl fmt::Debug for ConnectorDefinition {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("ConnectorDefinition")
			.field("name", &self.name)
			.field("kind", &self.kind)
			.field("owner", &self.owner)
			.finish_non_exhaustive()
	}
}

/// What adding a connector met under its name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectorAdded {
	/// A connector has the name; nothing was added.
	Taken,
	/// Nobody removed a connector of that name.
	New,
	/// Its owner had removed one of that name.
	Back,
	/// Someone else had removed one of that name.
	Other,
}

const DEFINITION_COLUMNS: &[(&str, &str)] = &[
	("removed", "INTEGER NOT NULL DEFAULT 0"),
	("kind", "TEXT"),
	("owner", "TEXT"),
	("username", "TEXT"),
	("password", "TEXT"),
	("schedule", "TEXT"),
	("utc_offset", "TEXT"),
	("area", "TEXT"),
	("tags", "TEXT"),
	("options", "TEXT"),
];
const DEFINITION_SELECT: &str = "SELECT name, kind, owner, username, password, schedule, utc_offset, area, tags, options \
	 FROM connectors";

fn definition_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ConnectorDefinition> {
	let tags: Option<String> = row.get(8)?;
	Ok(ConnectorDefinition {
		name: row.get(0)?,
		kind: row.get(1)?,
		owner: row.get(2)?,
		username: row.get(3)?,
		password: row.get(4)?,
		schedule: row.get(5)?,
		utc_offset: row.get(6)?,
		area: row.get(7)?,
		tags: tags
			.and_then(|tags| serde_json::from_str(&tags).ok())
			.unwrap_or_default(),
		options: row.get(9)?,
	})
}

pub struct ConnectorStore {
	conn: Mutex<Connection>,
}

impl ConnectorStore {
	/// Opens the file, bringing one written before connectors were kept in it up to date: its
	/// rows held the state of connectors a config file named, which nothing describes any more.
	pub fn open(path: &Path) -> crate::Result<Self> {
		let mut conn = Connection::open(path)?;
		conn.busy_timeout(std::time::Duration::from_secs(5))?;
		conn.pragma_update(None, "journal_mode", "WAL")?;
		conn.pragma_update(None, "synchronous", "NORMAL")?;
		let tx = conn.transaction()?;
		tx.execute_batch(
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
		let known: Vec<String> = tx
			.prepare("SELECT name FROM pragma_table_info('connectors')")?
			.query_map([], |row| row.get(0))?
			.collect::<Result<_, _>>()?;
		for (column, kind) in DEFINITION_COLUMNS {
			if !known.iter().any(|name| name == column) {
				tx.execute_batch(&format!(
					"ALTER TABLE connectors ADD COLUMN {column} {kind};"
				))?;
			}
		}
		tx.execute(
			"DELETE FROM connectors WHERE kind IS NULL AND removed = 0",
			[],
		)?;
		tx.commit()?;
		Ok(Self {
			conn: Mutex::new(conn),
		})
	}

	fn conn(&self) -> MutexGuard<'_, Connection> {
		self.conn.lock().unwrap_or_else(PoisonError::into_inner)
	}

	/// Every connector, in the order they were added.
	pub fn definitions(&self) -> crate::Result<Vec<ConnectorDefinition>> {
		let conn = self.conn();
		let mut statement = conn.prepare(&format!(
			"{DEFINITION_SELECT} WHERE removed = 0 ORDER BY rowid"
		))?;
		let rows = statement
			.query_map([], definition_row)?
			.collect::<Result<_, _>>()?;
		Ok(rows)
	}

	pub fn definition(&self, name: &str) -> crate::Result<Option<ConnectorDefinition>> {
		Ok(self
			.conn()
			.query_row(
				&format!("{DEFINITION_SELECT} WHERE name = ?1 AND removed = 0"),
				[name],
				definition_row,
			)
			.optional()?)
	}

	/// Who removed a connector of that name, when one was removed and none has the name now.
	pub fn removed_by(&self, name: &str) -> crate::Result<Option<Option<String>>> {
		Ok(self
			.conn()
			.query_row(
				"SELECT owner FROM connectors WHERE name = ?1 AND removed = 1",
				[name],
				|row| row.get(0),
			)
			.optional()?)
	}

	/// Adds a connector that has yet to sign in, unless one has its name.
	pub fn create(&self, definition: &ConnectorDefinition) -> crate::Result<ConnectorAdded> {
		let mut conn = self.conn();
		let tx = conn.transaction()?;
		let before: Option<(bool, Option<String>)> = tx
			.query_row(
				"SELECT removed, owner FROM connectors WHERE name = ?1",
				[&definition.name],
				|row| Ok((row.get(0)?, row.get(1)?)),
			)
			.optional()?;
		let added = match before {
			Some((false, _)) => return Ok(ConnectorAdded::Taken),
			Some((true, owner)) if owner == definition.owner => ConnectorAdded::Back,
			Some((true, _)) => ConnectorAdded::Other,
			None => ConnectorAdded::New,
		};
		tx.execute("DELETE FROM connectors WHERE name = ?1", [&definition.name])?;
		tx.execute(
			"INSERT INTO connectors (name, kind, owner, username, password, schedule, utc_offset,
				area, tags, options, needs_login)
			 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 1)",
			params![
				definition.name,
				definition.kind,
				definition.owner,
				definition.username,
				definition.password,
				definition.schedule,
				definition.utc_offset,
				definition.area,
				serde_json::to_string(&definition.tags)?,
				definition.options,
			],
		)?;
		tx.commit()?;
		Ok(added)
	}

	/// Replaces what a connector is, keeping its owner; `sign_out` also drops its saved state,
	/// so it has to sign in again. False when there is no such connector.
	pub fn update(&self, definition: &ConnectorDefinition, sign_out: bool) -> crate::Result<bool> {
		let changed = self.conn().execute(
			"UPDATE connectors SET kind = ?2, username = ?3, password = ?4, schedule = ?5,
				utc_offset = ?6, area = ?7, tags = ?8, options = ?9,
				state = CASE WHEN ?10 THEN NULL ELSE state END,
				needs_login = CASE WHEN ?10 THEN 1 ELSE needs_login END
			 WHERE name = ?1 AND removed = 0",
			params![
				definition.name,
				definition.kind,
				definition.username,
				definition.password,
				definition.schedule,
				definition.utc_offset,
				definition.area,
				serde_json::to_string(&definition.tags)?,
				definition.options,
				sign_out,
			],
		)?;
		Ok(changed == 1)
	}

	/// Drops a connector with what it signs in with and its saved state, leaving its name and
	/// owner.
	pub fn remove(&self, name: &str) -> crate::Result<bool> {
		let removed = self.conn().execute(
			"UPDATE connectors SET removed = 1, kind = NULL, username = NULL, password = NULL,
				schedule = NULL, utc_offset = NULL, area = NULL, tags = NULL, options = NULL,
				state = NULL, last_run = NULL, last_success = NULL, last_error = NULL,
				last_report = NULL, needs_login = 0
			 WHERE name = ?1 AND removed = 0",
			[name],
		)?;
		Ok(removed == 1)
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
			"UPDATE connectors SET state = ?2 WHERE name = ?1 AND removed = 0",
			params![name, state],
		)?;
		Ok(())
	}

	pub fn set_needs_login(&self, name: &str, needs_login: bool) -> crate::Result<()> {
		self.conn().execute(
			"UPDATE connectors SET needs_login = ?2 WHERE name = ?1 AND removed = 0",
			params![name, needs_login],
		)?;
		Ok(())
	}

	pub fn run(&self, name: &str) -> crate::Result<ConnectorRun> {
		Ok(self
			.conn()
			.query_row(
				"SELECT last_run, last_success, last_error, last_report, needs_login
				 FROM connectors WHERE name = ?1 AND removed = 0",
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
			.unwrap_or(ConnectorRun {
				needs_login: true,
				..ConnectorRun::default()
			}))
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
			"UPDATE connectors SET last_run = ?2, last_success = COALESCE(?3, last_success),
				last_error = ?4, last_report = COALESCE(?5, last_report)
			 WHERE name = ?1 AND removed = 0",
			params![name, at, success, error, report],
		)?;
		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn definition(name: &str) -> ConnectorDefinition {
		ConnectorDefinition {
			name: name.into(),
			kind: "payroll".into(),
			owner: Some("sam".into()),
			username: "sam@example.com".into(),
			password: "hunter2".into(),
			schedule: "every 6h".into(),
			utc_offset: "+09:00".into(),
			area: Some("Work".into()),
			tags: vec!["payroll".into()],
			options: "{}".into(),
		}
	}

	#[test]
	fn connectors_state_and_runs_are_kept_across_a_reopen() {
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join(".connectors.sqlite");
		let store = ConnectorStore::open(&path).unwrap();
		assert_eq!(store.state("pay").unwrap(), None);
		assert!(store.run("pay").unwrap().needs_login);
		assert_eq!(
			store.create(&definition("pay")).unwrap(),
			ConnectorAdded::New
		);
		assert_eq!(
			store.create(&definition("pay")).unwrap(),
			ConnectorAdded::Taken
		);
		assert!(store.run("pay").unwrap().needs_login);
		store.set_state("pay", Some(r#"{"device":"x"}"#)).unwrap();
		store.finish_run("pay", 100, None, Some("{}")).unwrap();
		store
			.finish_run("pay", 200, Some("timed out"), None)
			.unwrap();
		store.set_needs_login("pay", false).unwrap();
		drop(store);

		let store = ConnectorStore::open(&path).unwrap();
		assert_eq!(store.definitions().unwrap(), [definition("pay")]);
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
				needs_login: false,
			}
		);
	}

	#[test]
	fn an_edit_keeps_the_owner_and_signs_out_only_when_asked() {
		let dir = tempfile::tempdir().unwrap();
		let store = ConnectorStore::open(&dir.path().join(".connectors.sqlite")).unwrap();
		store.create(&definition("pay")).unwrap();
		store.set_state("pay", Some("{}")).unwrap();
		store.set_needs_login("pay", false).unwrap();

		let mut edited = definition("pay");
		edited.owner = Some("alex".into());
		edited.schedule = "daily 07:00".into();
		edited.area = None;
		edited.tags.clear();
		assert!(store.update(&edited, false).unwrap());
		let kept = store.definition("pay").unwrap().unwrap();
		assert_eq!(kept.owner.as_deref(), Some("sam"));
		assert_eq!(kept.schedule, "daily 07:00");
		assert!(kept.area.is_none() && kept.tags.is_empty());
		assert_eq!(store.state("pay").unwrap().as_deref(), Some("{}"));
		assert!(!store.run("pay").unwrap().needs_login);

		edited.password = "correct horse".into();
		assert!(store.update(&edited, true).unwrap());
		assert_eq!(store.state("pay").unwrap(), None);
		assert!(store.run("pay").unwrap().needs_login);
		assert!(!store.update(&definition("bank"), true).unwrap());
	}

	#[test]
	fn nothing_is_kept_for_a_connector_that_is_not_there() {
		let dir = tempfile::tempdir().unwrap();
		let store = ConnectorStore::open(&dir.path().join(".connectors.sqlite")).unwrap();
		store.create(&definition("pay")).unwrap();
		store.set_state("pay", Some("{}")).unwrap();
		assert!(store.remove("pay").unwrap());
		assert!(!store.remove("pay").unwrap());
		store.set_state("pay", Some("{}")).unwrap();
		store.set_needs_login("pay", false).unwrap();
		store.finish_run("pay", 100, None, None).unwrap();
		assert_eq!(store.state("pay").unwrap(), None);
		assert!(store.definitions().unwrap().is_empty());
		assert!(store.definition("pay").unwrap().is_none());
		assert!(format!("{:?}", definition("pay")).find("hunter2").is_none());
	}

	#[test]
	fn a_removed_connector_leaves_only_its_name_and_owner_for_who_adds_it_again() {
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join(".connectors.sqlite");
		let store = ConnectorStore::open(&path).unwrap();
		assert_eq!(store.removed_by("pay").unwrap(), None);
		store.create(&definition("pay")).unwrap();
		store.set_state("pay", Some(r#"{"device":"x"}"#)).unwrap();
		store.finish_run("pay", 100, None, Some("{}")).unwrap();
		assert_eq!(store.removed_by("pay").unwrap(), None);
		store.remove("pay").unwrap();
		drop(store);

		let store = ConnectorStore::open(&path).unwrap();
		assert_eq!(store.removed_by("pay").unwrap(), Some(Some("sam".into())));
		assert!(store.definitions().unwrap().is_empty());
		let left: String = store
			.conn()
			.query_row(
				"SELECT quote(kind) || quote(username) || quote(password) || quote(schedule)
					|| quote(options) || quote(state) || quote(last_report) FROM connectors",
				[],
				|row| row.get(0),
			)
			.unwrap();
		assert_eq!(left, "NULL".repeat(7));

		assert_eq!(
			store.create(&definition("pay")).unwrap(),
			ConnectorAdded::Back
		);
		assert_eq!(store.state("pay").unwrap(), None);
		let run = store.run("pay").unwrap();
		assert!(run.needs_login && run.last_run.is_none());
		assert_eq!(store.removed_by("pay").unwrap(), None);

		for owner in [Some("alex".to_string()), None] {
			store.remove("pay").unwrap();
			let mut theirs = definition("pay");
			theirs.owner = owner.clone();
			assert_eq!(store.create(&theirs).unwrap(), ConnectorAdded::Other);
			assert_eq!(store.definition("pay").unwrap().unwrap().owner, owner);
		}
		store.remove("pay").unwrap();
		assert_eq!(store.removed_by("pay").unwrap(), Some(None));
		let mut nobody = definition("pay");
		nobody.owner = None;
		assert_eq!(store.create(&nobody).unwrap(), ConnectorAdded::Back);
	}

	#[test]
	fn a_file_from_before_connectors_were_kept_in_it_opens_without_its_rows() {
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join(".connectors.sqlite");
		let old = Connection::open(&path).unwrap();
		old.execute_batch(
			"CREATE TABLE connectors (
				name TEXT PRIMARY KEY,
				state TEXT,
				last_run INTEGER,
				last_success INTEGER,
				last_error TEXT,
				last_report TEXT,
				needs_login INTEGER NOT NULL DEFAULT 0
			);
			INSERT INTO connectors (name, state, last_run, last_success)
			VALUES ('mf', '{\"cookies\":[]}', 100, 100);",
		)
		.unwrap();
		drop(old);

		let store = ConnectorStore::open(&path).unwrap();
		assert!(store.definitions().unwrap().is_empty());
		assert_eq!(store.state("mf").unwrap(), None);
		assert_eq!(store.removed_by("mf").unwrap(), None);
		assert_eq!(
			store.create(&definition("mf")).unwrap(),
			ConnectorAdded::New
		);
		let run = store.run("mf").unwrap();
		assert!(run.needs_login && run.last_success.is_none());
		drop(store);
		let again = ConnectorStore::open(&path).unwrap();
		assert_eq!(again.definitions().unwrap(), [definition("mf")]);
	}
}
