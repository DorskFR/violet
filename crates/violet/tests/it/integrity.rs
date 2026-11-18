use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use uuid::Uuid;
use violet::app::ModelStatus;
use violet::ops::{self, Patch};
use violet::{App, Error, Models};
use violet_core::{Pending, Query, Sidecar, Step};

use crate::common::{Harness, text_pdf, three_pages};

/// Every document with its sidecar and file hash, and what the index lists.
type Snapshot = (Vec<(PathBuf, Sidecar, String)>, Vec<(Uuid, PathBuf)>);

fn snapshot(harness: &Harness) -> Snapshot {
	let files = harness
		.documents()
		.into_iter()
		.map(|(path, sidecar)| {
			let sha = harness.file_sha(&path);
			(path, sidecar, sha)
		})
		.collect();
	let mut indexed: Vec<(Uuid, PathBuf)> = harness
		.app
		.index()
		.search(&Query::default())
		.unwrap()
		.hits
		.into_iter()
		.map(|hit| (hit.id, hit.path))
		.collect();
	indexed.sort();
	(files, indexed)
}

fn pending(harness: &Harness) -> Option<Pending> {
	harness.app.journal().pending().unwrap()
}

/// Makes `dir` read-only until the guard is dropped.
struct ReadOnly(PathBuf);

impl ReadOnly {
	fn new(dir: &Path) -> Self {
		std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o555)).unwrap();
		Self(dir.to_path_buf())
	}
}

impl Drop for ReadOnly {
	fn drop(&mut self) {
		std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755)).unwrap();
	}
}

/// Two documents in different folders.
async fn two_documents() -> (Harness, Uuid, Uuid) {
	let harness = Harness::new(Models::default());
	let first = harness
		.one("one.pdf", &text_pdf(&["alpha page of the first file"]))
		.await;
	let second = harness
		.one("two.pdf", &text_pdf(&["beta page", "gamma page"]))
		.await;
	ops::update(
		&harness.app,
		second,
		Patch {
			area: Some("Housing".into()),
			date: Some(Some("2024-03-14".into())),
			..Patch::default()
		},
	)
	.unwrap();
	(harness, first, second)
}

fn folder_of(harness: &Harness, id: Uuid) -> PathBuf {
	let path = ops::document(&harness.app, id).unwrap().path;
	harness
		.app
		.library
		.library_dir()
		.join(path.parent().unwrap())
}

#[tokio::test]
async fn a_merge_that_fails_after_stashing_a_parent_leaves_the_library_as_it_was() {
	let (harness, first, second) = two_documents().await;
	let before = snapshot(&harness);
	let entries = harness.entries();

	let first_sha = ops::document(&harness.app, first).unwrap().sidecar.sha256;
	let locked = ReadOnly::new(&folder_of(&harness, second));
	assert!(ops::merge(&harness.app, &[first, second]).is_err());
	drop(locked);
	assert!(
		harness
			.originals()
			.contains(&format!("{first_sha}.sidecar.json"))
	);

	assert_eq!(snapshot(&harness), before);
	assert_eq!(harness.entries(), entries);
	assert_eq!(pending(&harness), None);

	let merged = ops::merge(&harness.app, &[first, second]).unwrap();
	assert_eq!(harness.paths(), ["Unsorted/Undated/one.pdf"]);
	ops::undo(&harness.app, merged.entry.unwrap()).unwrap();
	assert_eq!(snapshot(&harness).0.len(), 2);
	assert_eq!(
		harness.paths(),
		[
			"Housing/2024/2024-03-14 two.pdf",
			"Unsorted/Undated/one.pdf"
		]
	);
}

#[tokio::test]
async fn a_split_that_cannot_stash_its_parent_takes_the_parts_back() {
	let harness = Harness::new(Models::default());
	let id = harness.one("letter.pdf", &three_pages()).await;
	let before = snapshot(&harness);
	let entries = harness.entries();

	let locked = ReadOnly::new(&harness.app.library.originals());
	assert!(ops::split(&harness.app, id, &[vec![1], vec![2, 3]]).is_err());
	drop(locked);

	assert_eq!(snapshot(&harness), before);
	assert_eq!(harness.entries(), entries);
	assert_eq!(pending(&harness), None);
	assert_eq!(
		ops::split(&harness.app, id, &[vec![1], vec![2, 3]])
			.unwrap()
			.result
			.documents
			.len(),
		2
	);
}

#[tokio::test]
async fn an_undo_that_fails_midway_keeps_the_change_and_its_undo() {
	let (harness, first, second) = two_documents().await;
	let merged = ops::merge(&harness.app, &[first, second]).unwrap();
	let entry = merged.entry.unwrap();
	let after = snapshot(&harness);
	let entries = harness.entries();

	let locked = ReadOnly::new(&harness.app.library.originals());
	assert!(ops::undo(&harness.app, entry).is_err());
	drop(locked);

	assert_eq!(snapshot(&harness), after);
	assert_eq!(harness.entries(), entries);
	assert_eq!(pending(&harness), None);
	let activity = ops::activity(&harness.app, 1).unwrap();
	assert_eq!(activity[0].entry.id, entry);
	assert!(activity[0].undoable);

	ops::undo(&harness.app, entry).unwrap();
	assert_eq!(
		harness.paths(),
		[
			"Housing/2024/2024-03-14 two.pdf",
			"Unsorted/Undated/one.pdf"
		]
	);
}

#[tokio::test]
async fn a_change_cut_short_by_a_crash_is_rolled_back_on_the_next_open() {
	let (mut harness, first, second) = two_documents().await;
	let before = snapshot(&harness);
	let entries = harness.entries();
	{
		let app = harness.app.clone();
		let (one, one_sidecar) = app.index().get(first).unwrap().unwrap();
		let mut merged = one_sidecar.clone();
		merged.id = Uuid::new_v4();
		let mut index = app.index();
		let mut change = ops::Change::begin(&app, &mut index).unwrap();
		change
			.create(Path::new("Unsorted/Undated/merged.pdf"), b"%PDF-", &merged)
			.unwrap();
		change.remove(&one, &one_sidecar).unwrap();
		std::mem::forget(change);
	}
	assert!(pending(&harness).is_some());
	let status = ops::status(&harness.app, ModelStatus::default()).unwrap();
	assert!(status.journal.unwrap().contains("did not finish"));

	harness.reopen(Models::default());
	assert_eq!(snapshot(&harness), before);
	assert_eq!(harness.entries(), entries);
	assert_eq!(pending(&harness), None);
	assert_eq!(
		ops::status(&harness.app, ModelStatus::default())
			.unwrap()
			.journal,
		None
	);
	assert!(ops::merge(&harness.app, &[first, second]).is_ok());
}

#[tokio::test]
async fn a_leftover_change_is_rolled_back_before_the_next_one() {
	let (harness, first, second) = two_documents().await;
	let before = snapshot(&harness);
	{
		let app = harness.app.clone();
		let (one, one_sidecar) = app.index().get(first).unwrap().unwrap();
		let mut index = app.index();
		let mut change = ops::Change::begin(&app, &mut index).unwrap();
		change.remove(&one, &one_sidecar).unwrap();
		std::mem::forget(change);
	}
	assert_eq!(harness.paths(), ["Housing/2024/2024-03-14 two.pdf"]);
	ops::delete(&harness.app, second).unwrap();
	assert_eq!(harness.paths(), ["Unsorted/Undated/one.pdf"]);
	assert_eq!(snapshot(&harness).0[0], before.0[1]);
}

#[tokio::test]
async fn a_change_that_reached_the_journal_is_kept_on_the_next_open() {
	let (mut harness, first, second) = two_documents().await;
	let merged = ops::merge(&harness.app, &[first, second]).unwrap();
	let after = snapshot(&harness);
	harness
		.app
		.journal()
		.set_pending(&Pending {
			entry: merged.entry.unwrap(),
			steps: vec![Step::Create {
				document: (merged.result.sidecar.id, merged.result.path.clone()),
			}],
		})
		.unwrap();
	harness.reopen(Models::default());
	assert_eq!(snapshot(&harness), after);
	assert_eq!(pending(&harness), None);
}

fn cli(harness: &Harness, args: &[&str]) -> std::process::Output {
	Command::new(env!("CARGO_BIN_EXE_violet"))
		.env_clear()
		.arg("--library")
		.arg(harness.root())
		.args(args)
		.output()
		.unwrap()
}

#[tokio::test]
async fn only_one_process_changes_a_library_at_a_time() {
	let (harness, first, _) = two_documents().await;
	let message = match App::open(harness.root(), Models::default()) {
		Err(Error::Conflict(message)) => message,
		Err(other) => panic!("{other}"),
		Ok(_) => panic!("opened twice"),
	};
	assert!(
		message.contains("in use by another violet process"),
		"{message}"
	);
	assert!(App::open_read_only(harness.root(), Models::default()).is_ok());

	let entry = harness.last_entry().id.to_string();
	let refused = cli(&harness, &["undo", &entry]);
	assert!(!refused.status.success());
	let stderr = String::from_utf8_lossy(&refused.stderr);
	assert!(
		stderr.contains("in use by another violet process"),
		"{stderr}"
	);
	let moved = cli(&harness, &["move", &first.to_string(), "--title", "x"]);
	assert!(!moved.status.success());
	assert_eq!(harness.last_entry().id.to_string(), entry);

	let status = cli(&harness, &["status"]);
	assert!(status.status.success(), "{status:?}");

	let root = harness.root();
	drop(harness.app);
	let undone = Command::new(env!("CARGO_BIN_EXE_violet"))
		.env_clear()
		.arg("--library")
		.arg(&root)
		.args(["undo", &entry])
		.output()
		.unwrap();
	assert!(undone.status.success(), "{undone:?}");
	assert!(String::from_utf8_lossy(&undone.stdout).starts_with("Undid"));
}

#[tokio::test]
async fn an_unreadable_journal_line_stops_undo_and_shows_in_status() {
	let (harness, _, _) = two_documents().await;
	let before = snapshot(&harness);
	let entry = harness.last_entry().id;
	let journal = harness.app.library.journal_path();
	let text = std::fs::read_to_string(&journal).unwrap();
	let mut lines: Vec<&str> = text.lines().collect();
	lines.insert(1, r#"{"id":"not an entry"}"#);
	std::fs::write(&journal, lines.join("\n") + "\n").unwrap();

	let status = ops::status(&harness.app, ModelStatus::default()).unwrap();
	let problem = status.journal.unwrap();
	assert!(problem.contains("line 2"), "{problem}");
	assert!(ops::activity(&harness.app, 10).is_err());
	assert!(ops::undo(&harness.app, entry).is_err());
	assert_eq!(snapshot(&harness), before);
}
