use std::path::{Path, PathBuf};

use uuid::Uuid;
use violet::ops::{self, Patch};
use violet::pipeline::Outcome;
use violet::{Error, Models};
use violet_core::{Action, Fact, hash};

use crate::common::{Harness, kind, splitting_fake, text_pdf, three_pages};

const WHOLE: &str = "Unsorted/Undated/letter.pdf";

async fn letter() -> (Harness, Uuid, Vec<u8>) {
	let harness = Harness::new(Models::default());
	let pdf = text_pdf(&["first page words", "second page words", "third page words"]);
	let id = harness.one("letter.pdf", &pdf).await;
	(harness, id, pdf)
}

fn conflict<T: std::fmt::Debug>(result: Result<T, Error>) -> String {
	match result {
		Err(Error::Conflict(message)) => message,
		other => panic!("not a conflict: {other:?}"),
	}
}

#[tokio::test]
async fn a_metadata_change_moves_the_file_and_undo_moves_it_back() {
	let (harness, id, pdf) = letter().await;
	let before = ops::document(&harness.app, id).unwrap();
	let patch = Patch {
		title: Some("  Lease   renewal ".into()),
		area: Some("Housing".into()),
		doc_type: Some("contract".into()),
		issuer: Some(Some("Landlord Ltd".into())),
		date: Some(Some("2024年3月14日".into())),
		tags: Some(vec!["lease".into(), " ".into(), "Lease".into()]),
		facts: Some(vec![Fact {
			key: "rent".into(),
			value: "¥120,000".into(),
			amount_minor: None,
			currency: None,
		}]),
	};
	let changed = ops::update(&harness.app, id, patch).unwrap();
	let moved = "Housing/2024/2024-03-14 Landlord Ltd Lease renewal.pdf";
	assert_eq!(changed.result.path, PathBuf::from(moved));
	assert_eq!(harness.paths(), [moved]);
	let sidecar = &harness.documents()[0].1;
	assert_eq!(sidecar, &changed.result.sidecar);
	assert_eq!(sidecar.title, "Lease renewal");
	assert_eq!(sidecar.date.as_deref(), Some("2024-03-14"));
	assert_eq!(sidecar.tags, ["lease"]);
	assert_eq!(sidecar.facts[0].amount_minor, Some(120_000));
	assert_eq!(sidecar.facts[0].currency.as_deref(), Some("JPY"));
	assert_eq!(sidecar.confidence, 1.0);
	assert!(sidecar.updated_at > before.sidecar.updated_at);
	assert_eq!(sidecar.sha256, hash::sha256(&pdf));
	assert_eq!(
		harness.app.index().get(id).unwrap().unwrap().0,
		PathBuf::from(moved)
	);

	let entry = harness.last_entry();
	assert_eq!(Some(entry.id), changed.entry);
	assert_eq!(entry.actor, "user");
	match &entry.action {
		Action::Update {
			from,
			to,
			before: recorded,
			..
		} => {
			assert_eq!(from, Path::new(WHOLE));
			assert_eq!(to, Path::new(moved));
			assert_eq!(**recorded, before.sidecar);
		}
		other => panic!("{other:?}"),
	}

	let same = ops::update(&harness.app, id, Patch::default()).unwrap();
	assert_eq!(same.entry, None);
	assert_eq!(harness.entries().len(), 2);

	let cleared = ops::update(
		&harness.app,
		id,
		Patch {
			issuer: Some(None),
			date: Some(Some(String::new())),
			..Patch::default()
		},
	)
	.unwrap();
	assert_eq!(
		cleared.result.path,
		PathBuf::from("Housing/Undated/Lease renewal.pdf")
	);
	ops::undo(&harness.app, cleared.entry.unwrap()).unwrap();
	assert_eq!(harness.paths(), [moved]);

	let undo = ops::undo(&harness.app, entry.id).unwrap();
	assert_eq!(kind(&undo), "undo");
	assert_eq!(harness.paths(), [WHOLE]);
	assert_eq!(harness.documents()[0].1, before.sidecar);
	assert_eq!(
		harness.app.index().get(id).unwrap().unwrap().1,
		before.sidecar
	);
	assert_eq!(harness.file_sha(Path::new(WHOLE)), hash::sha256(&pdf));
	assert_eq!(
		conflict(ops::undo(&harness.app, entry.id)),
		"already undone"
	);
	assert!(conflict(ops::undo(&harness.app, undo.id)).contains("cannot be undone"));
}

#[tokio::test]
async fn invalid_metadata_is_refused() {
	let (harness, id, _) = letter().await;
	for patch in [
		Patch {
			title: Some("   ".into()),
			..Patch::default()
		},
		Patch {
			date: Some(Some("31 February".into())),
			..Patch::default()
		},
		Patch {
			area: Some(String::new()),
			..Patch::default()
		},
	] {
		assert!(matches!(
			ops::update(&harness.app, id, patch),
			Err(Error::Invalid(_))
		));
	}
	assert!(matches!(
		ops::update(&harness.app, Uuid::new_v4(), Patch::default()),
		Err(Error::NotFound(_))
	));
	assert_eq!(harness.entries().len(), 1);
	assert_eq!(harness.paths(), [WHOLE]);
}

#[tokio::test]
async fn a_new_name_never_overwrites_another_document() {
	let harness = Harness::new(Models::default());
	let first = harness.one("a.pdf", &text_pdf(&["alpha words"])).await;
	let second = harness
		.one("b.pdf", &text_pdf(&["beta words entirely different"]))
		.await;
	let rename = |id| {
		ops::update(
			&harness.app,
			id,
			Patch {
				title: Some("Same".into()),
				..Patch::default()
			},
		)
		.unwrap()
	};
	rename(first);
	let renamed = rename(second);
	assert_eq!(
		harness.paths(),
		["Unsorted/Undated/Same (2).pdf", "Unsorted/Undated/Same.pdf"]
	);
	assert_eq!(
		renamed.result.path,
		PathBuf::from("Unsorted/Undated/Same (2).pdf")
	);
	let again = ops::update(
		&harness.app,
		second,
		Patch {
			tags: Some(vec!["x".into()]),
			..Patch::default()
		},
	)
	.unwrap();
	assert_eq!(again.result.path, renamed.result.path);
}

#[tokio::test]
async fn split_and_its_undo() {
	let (harness, id, pdf) = letter().await;
	let split = ops::split(&harness.app, id, &[vec![1, 2], vec![3]]).unwrap();
	assert_eq!(split.result.documents.len(), 2);
	assert_eq!(
		harness.paths(),
		[
			"Unsorted/Undated/letter (1 2).pdf",
			"Unsorted/Undated/letter (2 2).pdf"
		]
	);
	assert!(harness.app.index().get(id).unwrap().is_none());
	let documents = harness.documents();
	let (first_path, first) = &documents[0];
	assert_eq!(first.title, "letter (1/2)");
	assert_eq!(first.id, split.result.documents[0]);
	assert_eq!(first.sources[0].pages, [1, 2]);
	assert_eq!(first.pages.len(), 2);
	assert_eq!(first.sha256, harness.file_sha(first_path));
	let second = &documents[1].1;
	assert_eq!(second.sources[0].pages, [3]);
	assert_eq!(
		(second.pages[0].number, second.pages[0].text.as_str()),
		(1, "third page words")
	);
	let bytes = std::fs::read(harness.app.library.library_dir().join(first_path)).unwrap();
	assert_eq!(violet_pdf::page_count(&bytes).unwrap(), 2);

	let entry = harness.last_entry();
	assert_eq!((kind(&entry), Some(entry.id)), ("split", split.entry));
	ops::undo(&harness.app, entry.id).unwrap();
	assert_eq!(harness.paths(), [WHOLE]);
	let (path, sidecar) = harness.app.index().get(id).unwrap().unwrap();
	assert_eq!(harness.file_sha(&path), hash::sha256(&pdf));
	assert_eq!(sidecar.sha256, hash::sha256(&pdf));
	assert_eq!(sidecar.title, "letter");
	for part in &split.result.documents {
		assert!(harness.app.index().get(*part).unwrap().is_none());
	}
}

#[tokio::test]
async fn split_refuses_bad_groups_and_other_files() {
	let (harness, id, _) = letter().await;
	assert!(matches!(
		ops::split(&harness.app, id, &[]),
		Err(Error::Invalid(_))
	));
	assert!(matches!(
		ops::split(&harness.app, id, &[vec![4]]),
		Err(Error::Invalid(_))
	));
	let text = harness.one("note.txt", b"just a note").await;
	assert!(matches!(
		ops::split(&harness.app, text, &[vec![1]]),
		Err(Error::Invalid(_))
	));
	assert!(matches!(
		ops::rotate(&harness.app, text, &[1], 90),
		Err(Error::Invalid(_))
	));
	assert_eq!(harness.entries().len(), 2);
	assert_eq!(harness.documents().len(), 2);
}

#[tokio::test]
async fn rotate_and_its_undo() {
	let (harness, id, pdf) = letter().await;
	let rotated = ops::rotate(&harness.app, id, &[1, 3], 90).unwrap();
	let sha = rotated.result.sidecar.sha256.clone();
	assert_ne!(sha, hash::sha256(&pdf));
	assert_eq!(harness.file_sha(Path::new(WHOLE)), sha);
	assert_eq!(harness.app.index().get(id).unwrap().unwrap().1.sha256, sha);
	let entry = harness.last_entry();
	match &entry.action {
		Action::EditPages {
			original, detail, ..
		} => {
			assert_eq!(original, &format!("{}.pdf", hash::sha256(&pdf)));
			assert_eq!(detail, "Rotated pages 1, 3 by 90°");
		}
		other => panic!("{other:?}"),
	}
	assert!(matches!(
		ops::rotate(&harness.app, id, &[1], 45),
		Err(Error::Invalid(_))
	));
	assert!(matches!(
		ops::rotate(&harness.app, id, &[9], 90),
		Err(Error::Invalid(_))
	));

	ops::undo(&harness.app, rotated.entry.unwrap()).unwrap();
	assert_eq!(harness.file_sha(Path::new(WHOLE)), hash::sha256(&pdf));
	let restored = harness.app.index().get(id).unwrap().unwrap().1;
	assert_eq!(restored.sha256, hash::sha256(&pdf));
	assert_eq!(restored, harness.documents()[0].1);
	assert!(harness.originals().contains(&format!("{sha}.pdf")));
}

#[tokio::test]
async fn delete_pages_and_its_undo() {
	let (harness, id, pdf) = letter().await;
	let edited = ops::delete_pages(&harness.app, id, &[2]).unwrap();
	let sidecar = &edited.result.sidecar;
	assert_eq!(sidecar.sources[0].pages, [1, 3]);
	let texts: Vec<_> = sidecar
		.pages
		.iter()
		.map(|page| (page.number, page.text.as_str()))
		.collect();
	assert_eq!(texts, [(1, "first page words"), (2, "third page words")]);
	let bytes = std::fs::read(harness.app.library.library_dir().join(WHOLE)).unwrap();
	assert_eq!(violet_pdf::page_count(&bytes).unwrap(), 2);
	assert_eq!(sidecar.sha256, hash::sha256(&bytes));
	assert!(matches!(
		ops::delete_pages(&harness.app, id, &[1, 2]),
		Err(Error::Invalid(_))
	));

	ops::undo(&harness.app, edited.entry.unwrap()).unwrap();
	assert_eq!(harness.file_sha(Path::new(WHOLE)), hash::sha256(&pdf));
	let restored = harness.app.index().get(id).unwrap().unwrap().1;
	assert_eq!(restored.pages.len(), 3);
	assert_eq!(restored.sources[0].pages, [1, 2, 3]);
}

#[tokio::test]
async fn delete_and_its_undo() {
	let (harness, id, pdf) = letter().await;
	let sidecar = ops::document(&harness.app, id).unwrap().sidecar;
	let entry = ops::delete(&harness.app, id).unwrap();
	assert!(harness.documents().is_empty());
	assert!(matches!(
		ops::document(&harness.app, id),
		Err(Error::NotFound(_))
	));
	let sha = hash::sha256(&pdf);
	assert_eq!(
		harness.originals(),
		[format!("{sha}.json"), format!("{sha}.pdf")]
	);
	assert!(matches!(
		ops::delete(&harness.app, id),
		Err(Error::NotFound(_))
	));

	ops::undo(&harness.app, entry).unwrap();
	assert_eq!(harness.paths(), [WHOLE]);
	assert_eq!(harness.file_sha(Path::new(WHOLE)), sha);
	assert_eq!(ops::document(&harness.app, id).unwrap().sidecar, sidecar);
}

#[tokio::test]
async fn merge_and_its_undo() {
	let harness = Harness::new(Models::default());
	let one = text_pdf(&["alpha page of the first file"]);
	let two = text_pdf(&["beta page", "gamma page of the second file"]);
	let first = harness.one("one.pdf", &one).await;
	let second = harness.one("two.pdf", &two).await;
	ops::update(
		&harness.app,
		second,
		Patch {
			tags: Some(vec!["second".into()]),
			..Patch::default()
		},
	)
	.unwrap();
	let second_sidecar = ops::document(&harness.app, second).unwrap().sidecar;

	assert!(matches!(
		ops::merge(&harness.app, &[first]),
		Err(Error::Invalid(_))
	));
	assert!(matches!(
		ops::merge(&harness.app, &[first, first]),
		Err(Error::Invalid(_))
	));
	let merged = ops::merge(&harness.app, &[first, second]).unwrap();
	assert_eq!(harness.paths(), ["Unsorted/Undated/one.pdf"]);
	let sidecar = &merged.result.sidecar;
	assert_ne!(sidecar.id, first);
	assert_eq!(sidecar.tags, ["second"]);
	assert_eq!(sidecar.sources.len(), 2);
	let texts: Vec<_> = sidecar.pages.iter().map(|page| page.number).collect();
	assert_eq!(texts, [1, 2, 3]);
	assert_eq!(sidecar.pages[2].text, "gamma page of the second file");
	let bytes = std::fs::read(harness.app.library.library_dir().join(&merged.result.path)).unwrap();
	assert_eq!(violet_pdf::page_count(&bytes).unwrap(), 3);
	assert_eq!(sidecar.sha256, hash::sha256(&bytes));
	assert!(harness.app.index().get(first).unwrap().is_none());

	ops::undo(&harness.app, merged.entry.unwrap()).unwrap();
	assert_eq!(
		harness.paths(),
		["Unsorted/Undated/one.pdf", "Unsorted/Undated/two.pdf"]
	);
	assert_eq!(
		harness.file_sha(Path::new("Unsorted/Undated/one.pdf")),
		hash::sha256(&one)
	);
	assert_eq!(
		harness.file_sha(Path::new("Unsorted/Undated/two.pdf")),
		hash::sha256(&two)
	);
	assert_eq!(
		ops::document(&harness.app, second).unwrap().sidecar,
		second_sidecar
	);
	assert!(harness.app.index().get(sidecar.id).unwrap().is_none());
}

#[tokio::test]
async fn undoing_an_ingest_takes_the_documents_out_and_keeps_the_original() {
	let fake = splitting_fake().await;
	let harness = Harness::new(fake.models());
	let pdf = three_pages();
	let Outcome::Filed { entry, documents } = harness.ingest("three.pdf", &pdf).await else {
		panic!("not filed");
	};
	ops::undo(&harness.app, entry).unwrap();
	assert!(harness.documents().is_empty());
	assert!(harness.inbox().is_empty());
	for (id, _) in &documents {
		assert!(harness.app.index().get(*id).unwrap().is_none());
	}
	let kept = harness.originals();
	assert!(kept.contains(&format!("{}.pdf", hash::sha256(&pdf))));
	assert_eq!(kept.len(), 5);

	let Outcome::Filed {
		documents: again, ..
	} = harness.ingest("three.pdf", &pdf).await
	else {
		panic!("a file dropped again after the undo is filed again");
	};
	assert_eq!(again.len(), 2);
	assert_eq!(harness.documents().len(), 2);
}

#[tokio::test]
async fn undo_is_refused_when_something_changed_since() {
	let (harness, id, _) = letter().await;
	let ingest = harness.entries()[0].id;
	let rotated = ops::rotate(&harness.app, id, &[1], 180).unwrap();
	assert!(conflict(ops::undo(&harness.app, ingest)).contains("Rotated pages 1 by 180°"));

	let activity = ops::activity(&harness.app, 10).unwrap();
	let flags: Vec<_> = activity
		.iter()
		.map(|item| (kind(&item.entry), item.undone, item.undoable))
		.collect();
	assert_eq!(
		flags,
		[("edit_pages", false, true), ("ingest", false, false)]
	);

	let file = harness.app.library.library_dir().join(WHOLE);
	let edited = std::fs::read(&file).unwrap();
	std::fs::write(&file, b"%PDF-1.4 tampered").unwrap();
	assert!(conflict(ops::undo(&harness.app, rotated.entry.unwrap())).contains("changed since"));
	std::fs::write(&file, &edited).unwrap();

	ops::undo(&harness.app, rotated.entry.unwrap()).unwrap();
	let flags: Vec<_> = ops::activity(&harness.app, 2)
		.unwrap()
		.iter()
		.map(|item| (kind(&item.entry), item.undone, item.undoable))
		.collect();
	assert_eq!(flags, [("undo", false, false), ("edit_pages", true, false)]);
	ops::undo(&harness.app, ingest).unwrap();
	assert!(harness.documents().is_empty());
	assert!(matches!(
		ops::undo(&harness.app, Uuid::new_v4()),
		Err(Error::NotFound(_))
	));
}

#[tokio::test]
async fn reindex_rebuilds_from_the_sidecars() {
	let (harness, id, _) = letter().await;
	let index = harness.app.library.index_path();
	for suffix in ["", "-wal", "-shm"] {
		let _ = std::fs::remove_file(format!("{}{suffix}", index.display()));
	}
	let reopened = harness.reopen(Models::default());
	assert!(reopened.index().get(id).unwrap().is_some());
	reopened.index().remove(id).unwrap();
	let folder = harness.app.library.library_dir().join("Unsorted/Undated");
	std::fs::write(folder.join("orphan.json"), b"{}").unwrap();
	assert_eq!(reopened.reindex().unwrap(), 1);
	assert!(reopened.index().get(id).unwrap().is_some());
}
