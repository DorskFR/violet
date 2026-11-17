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
	/// Appends one line and fsyncs.
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
				line.insert(0, b'\n');
			}
		}
		file.write_all(&line)?;
		file.sync_all()?;
		Ok(())
	}
	/// All entries, oldest first. A torn last line is skipped, as is one that a later append
	/// closed off.
	pub fn entries(&self) -> crate::Result<Vec<Entry>> {
		let bytes = match fs::read(&self.path) {
			Ok(bytes) => bytes,
			Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
			Err(error) => return Err(error.into()),
		};
		Ok(bytes
			.split(|byte| *byte == b'\n')
			.filter_map(|line| serde_json::from_slice(line).ok())
			.collect())
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

	#[test]
	fn a_torn_last_line_is_skipped_and_survives_later_appends() {
		let (_dir, journal) = journal();
		let first = duplicate();
		journal.append(&first).unwrap();
		let mut file = fs::OpenOptions::new()
			.append(true)
			.open(&journal.path)
			.unwrap();
		file.write_all(br#"{"id":"0f0e0d0c-0b0a-4908-8706-050403020100","at":"2024"#)
			.unwrap();
		drop(file);
		assert_eq!(journal.entries().unwrap(), vec![first.clone()]);

		let second = duplicate();
		journal.append(&second).unwrap();
		assert_eq!(journal.entries().unwrap(), vec![first, second]);
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
