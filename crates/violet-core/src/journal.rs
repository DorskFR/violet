use std::collections::HashSet;
use std::fs;
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

/// Append-only `.journal.jsonl`. An undo appends an `Undo` entry; nothing is rewritten.
pub struct Journal {
	path: PathBuf,
}

/// The change in progress, in `.pending.json` next to the journal: the entry it will append
/// and every step already started, with what is needed to take it back.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pending {
	pub entry: Uuid,
	pub steps: Vec<Step>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Step {
	/// A new document was written at a free path.
	Create { document: (Uuid, PathBuf) },
	/// A document went to `.originals/` as `original`.
	Remove {
		original: String,
		document: (Uuid, PathBuf),
		sidecar: Box<crate::Sidecar>,
	},
	/// A document was rewritten in place; `original` holds its bytes before.
	Replace {
		original: String,
		document: (Uuid, PathBuf),
		before: Box<crate::Sidecar>,
	},
	/// A settings file at the library root was written or set aside; `original` holds what it
	/// was before, none when it did not exist.
	Settings {
		file: String,
		original: Option<String>,
	},
	/// A document was moved and its sidecar rewritten.
	Move {
		document: Uuid,
		from: PathBuf,
		to: PathBuf,
		before: Box<crate::Sidecar>,
	},
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
	pub id: Uuid,
	#[serde(with = "time::serde::rfc3339")]
	pub at: OffsetDateTime,
	/// "agent", "user" (the default user) or "user:<name>".
	pub actor: String,
	/// The user an agent entry worked for.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub on_behalf_of: Option<String>,
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
	Ingest {
		original: String,
		original_name: String,
		documents: Vec<(Uuid, PathBuf)>,
	},
	/// An inbox file was an exact duplicate of an existing document and was stashed.
	Duplicate {
		original: String,
		original_name: String,
		of: Uuid,
	},
	Split {
		original: String,
		from: (Uuid, PathBuf),
		into: Vec<(Uuid, PathBuf)>,
	},
	Merge {
		originals: Vec<(String, Uuid, PathBuf)>,
		into: (Uuid, PathBuf),
	},
	/// Pages rotated or deleted; `original` is the file before the edit.
	EditPages {
		original: String,
		document: (Uuid, PathBuf),
		detail: String,
	},
	/// Metadata changed, which may also move the file.
	Update {
		document: Uuid,
		from: PathBuf,
		to: PathBuf,
		before: Box<crate::Sidecar>,
	},
	Delete {
		original: String,
		document: (Uuid, PathBuf),
		sidecar: Box<crate::Sidecar>,
	},
	/// The file of a document was read again into other bytes or replaced by another file,
	/// keeping its id; `original` is the file before, `before` its sidecar.
	Replace {
		original: String,
		document: Uuid,
		from: PathBuf,
		to: PathBuf,
		before: Box<crate::Sidecar>,
	},
	/// A settings file at the library root (`.users.json`, `.areas.json`) changed: `original`
	/// is what it was before, none when it did not exist, and `sha256` what it is now, none
	/// when it was set aside.
	Settings {
		file: String,
		original: Option<String>,
		sha256: Option<String>,
	},
	Undo {
		entry: Uuid,
	},
}

impl Journal {
	pub fn open(path: &Path) -> Self {
		Self {
			path: path.to_path_buf(),
		}
	}
	/// Appends one line and fsyncs. A torn last line left by a crash is cut off first.
	pub fn append(&self, entry: &Entry) -> crate::Result<()> {
		let mut line = serde_json::to_vec(entry)?;
		line.push(b'\n');
		let mut file = fs::OpenOptions::new()
			.create(true)
			.read(true)
			.append(true)
			.open(&self.path)?;
		if file.seek(SeekFrom::End(0))? > 0 {
			let mut last = [0u8; 1];
			file.seek(SeekFrom::End(-1))?;
			file.read_exact(&mut last)?;
			if last[0] != b'\n' {
				let mut bytes = Vec::new();
				file.seek(SeekFrom::Start(0))?;
				file.read_to_end(&mut bytes)?;
				let start = bytes
					.iter()
					.rposition(|byte| *byte == b'\n')
					.map_or(0, |end| end + 1);
				if serde_json::from_slice::<Entry>(&bytes[start..]).is_ok() {
					line.insert(0, b'\n');
				} else {
					file.set_len(start as u64)?;
				}
			}
		}
		file.write_all(&line)?;
		file.sync_all()?;
		Ok(())
	}
	/// All entries, oldest first. Only a last line without its newline may be unreadable, as
	/// a write cut short; any other line that does not parse is an error.
	pub fn entries(&self) -> crate::Result<Vec<Entry>> {
		let bytes = match fs::read(&self.path) {
			Ok(bytes) => bytes,
			Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
			Err(error) => return Err(error.into()),
		};
		let mut lines: Vec<&[u8]> = bytes.split(|byte| *byte == b'\n').collect();
		let last = lines.pop().unwrap_or_default();
		let mut entries = Vec::with_capacity(lines.len() + 1);
		for (number, line) in lines.into_iter().enumerate() {
			let entry = serde_json::from_slice(line).map_err(|error| {
				crate::Error::Corrupt(format!(
					"{} line {}: {error}",
					self.path.display(),
					number + 1
				))
			})?;
			entries.push(entry);
		}
		if let Ok(entry) = serde_json::from_slice(last) {
			entries.push(entry);
		}
		Ok(entries)
	}
	pub fn pending(&self) -> crate::Result<Option<Pending>> {
		match fs::read(self.pending_path()) {
			Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
			Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
			Err(error) => Err(error.into()),
		}
	}
	pub fn set_pending(&self, pending: &Pending) -> crate::Result<()> {
		crate::library::write_atomic(&self.pending_path(), &serde_json::to_vec(pending)?)
	}
	pub fn clear_pending(&self) -> crate::Result<()> {
		match fs::remove_file(self.pending_path()) {
			Ok(()) => Ok(()),
			Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
			Err(error) => Err(error.into()),
		}
	}
	fn pending_path(&self) -> PathBuf {
		self.path.with_file_name(".pending.json")
	}
	/// Newest first, at most `limit`, with whether each was undone.
	pub fn recent(&self, limit: usize) -> crate::Result<Vec<(Entry, bool)>> {
		let mut undone = HashSet::new();
		let mut recent = Vec::new();
		for entry in self.entries()?.into_iter().rev() {
			let is_undone = undone.contains(&entry.id);
			if let Action::Undo { entry: target } = &entry.action
				&& !is_undone
			{
				undone.insert(*target);
			}
			if recent.len() < limit {
				recent.push((entry, is_undone));
			}
		}
		Ok(recent)
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::testutil::sidecar;

	fn entry(action: Action) -> Entry {
		Entry {
			id: Uuid::new_v4(),
			at: OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap(),
			actor: "agent".into(),
			on_behalf_of: None,
			action,
			summary: "did something".into(),
		}
	}

	fn duplicate() -> Entry {
		entry(Action::Duplicate {
			original: "abc.pdf".into(),
			original_name: "scan.pdf".into(),
			of: Uuid::new_v4(),
		})
	}

	fn journal() -> (tempfile::TempDir, Journal) {
		let dir = tempfile::tempdir().unwrap();
		let journal = Journal::open(&dir.path().join(".journal.jsonl"));
		(dir, journal)
	}

	#[test]
	fn a_missing_journal_is_empty() {
		let (_dir, journal) = journal();
		assert!(journal.entries().unwrap().is_empty());
		assert!(journal.recent(10).unwrap().is_empty());
	}

	#[test]
	fn every_action_round_trips_in_order() {
		let (_dir, journal) = journal();
		let a = (Uuid::new_v4(), PathBuf::from("A/2024/a.pdf"));
		let b = (Uuid::new_v4(), PathBuf::from("健康/2024/領収書.pdf"));
		let actions = vec![
			Action::Ingest {
				original: "abc.pdf".into(),
				original_name: "scan.pdf".into(),
				documents: vec![a.clone(), b.clone()],
			},
			Action::Duplicate {
				original: "abc.pdf".into(),
				original_name: "x".into(),
				of: a.0,
			},
			Action::Split {
				original: "abc.pdf".into(),
				from: a.clone(),
				into: vec![b.clone()],
			},
			Action::Merge {
				originals: vec![("abc.pdf".into(), a.0, a.1.clone())],
				into: b.clone(),
			},
			Action::EditPages {
				original: "abc.pdf".into(),
				document: a.clone(),
				detail: "rotated page 1".into(),
			},
			Action::Update {
				document: a.0,
				from: a.1.clone(),
				to: b.1.clone(),
				before: Box::new(sidecar("before")),
			},
			Action::Delete {
				original: "abc.pdf".into(),
				document: a.clone(),
				sidecar: Box::new(sidecar("gone")),
			},
			Action::Replace {
				original: "abc.doc".into(),
				document: a.0,
				from: a.1.clone(),
				to: b.1.clone(),
				before: Box::new(sidecar("legacy")),
			},
			Action::Settings {
				file: ".users.json".into(),
				original: Some("abc.users.json".into()),
				sha256: Some("def".into()),
			},
			Action::Undo {
				entry: Uuid::new_v4(),
			},
		];
		let written: Vec<Entry> = actions.into_iter().map(entry).collect();
		for entry in &written {
			journal.append(entry).unwrap();
		}
		assert_eq!(journal.entries().unwrap(), written);

		let text = fs::read_to_string(&journal.path).unwrap();
		assert_eq!(text.lines().count(), written.len());
		assert!(text.ends_with('\n'));
		assert!(text.lines().next().unwrap().contains(r#""kind":"ingest""#));
	}

	fn write_raw(journal: &Journal, bytes: &[u8]) {
		let mut file = fs::OpenOptions::new()
			.create(true)
			.append(true)
			.open(&journal.path)
			.unwrap();
		file.write_all(bytes).unwrap();
	}

	#[test]
	fn a_torn_last_line_is_skipped_then_cut_off_by_the_next_append() {
		let (_dir, journal) = journal();
		let first = duplicate();
		journal.append(&first).unwrap();
		write_raw(
			&journal,
			br#"{"id":"0f0e0d0c-0b0a-4908-8706-050403020100","at":"2024"#,
		);
		assert_eq!(journal.entries().unwrap(), vec![first.clone()]);

		let second = duplicate();
		journal.append(&second).unwrap();
		assert_eq!(journal.entries().unwrap(), vec![first, second]);
		let text = fs::read_to_string(&journal.path).unwrap();
		assert_eq!(text.lines().count(), 2);
		assert!(!text.contains("0f0e0d0c"));
	}

	#[test]
	fn a_whole_last_entry_without_its_newline_is_kept() {
		let (_dir, journal) = journal();
		let first = duplicate();
		write_raw(&journal, &serde_json::to_vec(&first).unwrap());
		assert_eq!(journal.entries().unwrap(), vec![first.clone()]);
		let second = duplicate();
		journal.append(&second).unwrap();
		assert_eq!(journal.entries().unwrap(), vec![first, second]);
	}

	#[test]
	fn an_unreadable_line_before_the_last_is_an_error() {
		for bad in [
			&b"not json"[..],
			br#"{"id":"0f0e0d0c-0b0a-4908-8706-050403020100","at":"2024"#,
			br#"{"id":"0f0e0d0c-0b0a-4908-8706-050403020100","at":"2023-11-14T22:13:20Z","actor":"agent","action":{"kind":"from_the_future"},"summary":"x"}"#,
		] {
			let (_dir, journal) = journal();
			journal.append(&duplicate()).unwrap();
			write_raw(&journal, bad);
			write_raw(&journal, b"\n");
			journal.append(&duplicate()).unwrap();
			let error = journal.entries().unwrap_err();
			assert!(matches!(error, crate::Error::Corrupt(_)), "{error}");
			assert!(error.to_string().contains("line 2"), "{error}");
			assert!(journal.recent(10).is_err());
		}
	}

	#[test]
	fn entries_without_a_named_actor_keep_their_old_shape() {
		let old = r#"{"id":"0f0e0d0c-0b0a-4908-8706-050403020100","at":"2023-11-14T22:13:20Z","actor":"user","action":{"kind":"undo","entry":"0f0e0d0c-0b0a-4908-8706-050403020101"},"summary":"x"}"#;
		let entry: Entry = serde_json::from_str(old).unwrap();
		assert_eq!(entry.actor, "user");
		assert_eq!(entry.on_behalf_of, None);
		assert_eq!(serde_json::to_string(&entry).unwrap(), old);

		let mut named = duplicate();
		named.on_behalf_of = Some("sam".into());
		let text = serde_json::to_string(&named).unwrap();
		assert!(text.contains(r#""on_behalf_of":"sam""#), "{text}");
		assert_eq!(serde_json::from_str::<Entry>(&text).unwrap(), named);
	}

	#[test]
	fn pending_round_trips_and_clears() {
		let (_dir, journal) = journal();
		assert_eq!(journal.pending().unwrap(), None);
		let pending = Pending {
			entry: Uuid::new_v4(),
			steps: vec![
				Step::Create {
					document: (Uuid::new_v4(), PathBuf::from("A/2024/a.pdf")),
				},
				Step::Remove {
					original: "abc.pdf".into(),
					document: (Uuid::new_v4(), PathBuf::from("A/2024/b.pdf")),
					sidecar: Box::new(sidecar("b")),
				},
			],
		};
		journal.set_pending(&pending).unwrap();
		assert_eq!(journal.pending().unwrap(), Some(pending));
		journal.clear_pending().unwrap();
		journal.clear_pending().unwrap();
		assert_eq!(journal.pending().unwrap(), None);
	}

	#[test]
	fn recent_is_newest_first_and_limited() {
		let (_dir, journal) = journal();
		let written: Vec<Entry> = (0..5).map(|_| duplicate()).collect();
		for entry in &written {
			journal.append(entry).unwrap();
		}
		let recent = journal.recent(2).unwrap();
		assert_eq!(recent.len(), 2);
		assert_eq!(recent[0].0, written[4]);
		assert_eq!(recent[1].0, written[3]);
		assert!(recent.iter().all(|(_, undone)| !undone));
		assert_eq!(journal.recent(100).unwrap().len(), 5);
		assert!(journal.recent(0).unwrap().is_empty());
	}

	#[test]
	fn recent_marks_undone_entries() {
		let (_dir, journal) = journal();
		let first = duplicate();
		let second = duplicate();
		let undo = entry(Action::Undo { entry: first.id });
		for entry in [&first, &second, &undo] {
			journal.append(entry).unwrap();
		}
		let recent = journal.recent(10).unwrap();
		assert_eq!(
			recent,
			vec![
				(undo.clone(), false),
				(second.clone(), false),
				(first.clone(), true)
			]
		);

		let redo = entry(Action::Undo { entry: undo.id });
		journal.append(&redo).unwrap();
		let recent = journal.recent(10).unwrap();
		assert_eq!(
			recent,
			vec![(redo, false), (undo, true), (second, false), (first, false)]
		);
	}

	#[test]
	fn undone_state_is_known_beyond_the_limit() {
		let (_dir, journal) = journal();
		let first = duplicate();
		journal.append(&first).unwrap();
		journal
			.append(&entry(Action::Undo { entry: first.id }))
			.unwrap();
		let recent = journal.recent(1).unwrap();
		assert_eq!(recent.len(), 1);
		assert!(matches!(recent[0].0.action, Action::Undo { .. }));
	}
}
