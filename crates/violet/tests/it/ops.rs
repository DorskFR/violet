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
		[format!("{sha}.pdf"), format!("{sha}.sidecar.json")]
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
	let (mut harness, id, _) = letter().await;
	let index = harness.app.library.index_path();
	for suffix in ["", "-wal", "-shm"] {
		let _ = std::fs::remove_file(format!("{}{suffix}", index.display()));
	}
	let reopened = harness.reopen(Models::default());
	assert!(reopened.index().get(id).unwrap().is_some());
	reopened.index().remove(id).unwrap();
	let folder = harness.app.library.library_dir().join("Unsorted/Undated");
	std::fs::write(folder.join("orphan.json"), b"{}").unwrap();
	let reindexed = reopened.reindex().unwrap();
	assert_eq!(reindexed.documents, 1);
	assert_eq!(
		reindexed.problems,
		[(
			PathBuf::from("Unsorted/Undated/orphan.json"),
			"no document".to_string()
		)]
	);
	assert!(reopened.index().get(id).unwrap().is_some());
}

fn tree(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
	let mut files = Vec::new();
	let mut pending = vec![dir.to_path_buf()];
	while let Some(dir) = pending.pop() {
		for entry in std::fs::read_dir(&dir).unwrap() {
			let path = entry.unwrap().path();
			if path.is_dir() {
				pending.push(path);
			} else {
				files.push((path.clone(), std::fs::read(&path).unwrap()));
			}
		}
	}
	files.sort();
	files
}

#[tokio::test]
async fn reindex_reports_drift_with_paths_and_fails_without_changing_files() {
	let (harness, id, _) = letter().await;
	let library = harness.app.library.library_dir();
	let copy = library.join("Other/Undated");
	std::fs::create_dir_all(&copy).unwrap();
	std::fs::copy(library.join(WHOLE), copy.join("letter.pdf")).unwrap();
	std::fs::copy(
		library.join("Unsorted/Undated/letter.json"),
		copy.join("letter.json"),
	)
	.unwrap();
	std::fs::write(library.join("Unsorted/Undated/letter.txt"), b"x").unwrap();
	std::fs::write(library.join("Unsorted/Undated/loose.pdf"), b"x").unwrap();
	let before = tree(&library);
	let root = harness.root();
	drop(harness.app);

	let output = std::process::Command::new(env!("CARGO_BIN_EXE_violet"))
		.env_clear()
		.arg("--library")
		.arg(&root)
		.arg("reindex")
		.output()
		.unwrap();
	assert!(!output.status.success(), "{output:?}");
	let stderr = String::from_utf8_lossy(&output.stderr);
	let at = |path: &str| library.join(path).display().to_string();
	for expected in [
		format!(
			"warning: {}: same id {id} as Other/Undated/letter.pdf",
			at(WHOLE)
		),
		format!(
			"warning: {}: its sidecar Unsorted/Undated/letter.json belongs to {WHOLE}",
			at("Unsorted/Undated/letter.txt")
		),
		format!("warning: {}: no sidecar", at("Unsorted/Undated/loose.pdf")),
		"3 problem(s) in the library; no file was changed".to_string(),
	] {
		assert!(stderr.contains(&expected), "{expected}\n{stderr}");
	}
	assert_eq!(String::from_utf8_lossy(&output.stdout), "1 documents\n");
	assert_eq!(tree(&library), before);
}

#[tokio::test]
async fn a_json_input_keeps_its_name_through_edits_and_nothing_stashed_is_overwritten() {
	let harness = Harness::new(Models::default());
	let json = br#"{"invoice": 42, "total": "133.90 EUR"}"#;
	let sha = hash::sha256(json);
	let id = harness.one("data.json", json).await;
	let filed = ops::document(&harness.app, id).unwrap().path;
	assert!(filed.to_string_lossy().ends_with(".json.txt"), "{filed:?}");
	let patch = Patch {
		title: Some("Invoice 42".into()),
		..Patch::default()
	};
	let edited = ops::update(&harness.app, id, patch).unwrap().result.path;
	assert!(
		edited.to_string_lossy().ends_with("Invoice 42.json.txt"),
		"{edited:?}"
	);

	let sidecar = ops::document(&harness.app, id).unwrap().sidecar;
	let entry = ops::delete(&harness.app, id).unwrap();
	assert_eq!(
		harness.originals(),
		[
			format!("{sha}.json"),
			format!("{sha}.json.txt"),
			format!("{sha}.sidecar.json")
		]
	);
	let originals = harness.app.library.originals();
	assert_eq!(
		std::fs::read(originals.join(format!("{sha}.json"))).unwrap(),
		json
	);
	assert_eq!(
		std::fs::read(originals.join(format!("{sha}.json.txt"))).unwrap(),
		json
	);
	assert_eq!(
		harness.app.library.stashed_sidecars(&sha).unwrap(),
		std::slice::from_ref(&sidecar)
	);

	ops::undo(&harness.app, entry).unwrap();
	let restored = ops::document(&harness.app, id).unwrap();
	assert_eq!(restored.path, edited);
	assert_eq!(restored.sidecar, sidecar);
}

fn password_protected(pdf: &[u8]) -> Vec<u8> {
	let mut doc = lopdf::Document::load_mem(pdf).unwrap();
	let id = lopdf::Object::string_literal("0123456789abcdef");
	doc.trailer.set("ID", vec![id.clone(), id]);
	let state = lopdf::EncryptionState::try_from(lopdf::EncryptionVersion::V2 {
		document: &doc,
		owner_password: "owner",
		user_password: "secret",
		key_length: 128,
		permissions: lopdf::Permissions::all(),
	})
	.unwrap();
	doc.encrypt(&state).unwrap();
	let mut out = Vec::new();
	doc.save_to(&mut out).unwrap();
	out
}

#[tokio::test]
async fn page_edits_of_a_password_protected_pdf_are_refused_as_invalid() {
	let (harness, id, pdf) = letter().await;
	let locked = password_protected(&pdf);
	let library = &harness.app.library;
	let mut sidecar = ops::document(&harness.app, id).unwrap().sidecar;
	sidecar.sha256 = hash::sha256(&locked);
	library
		.write_document(Path::new(WHOLE), &locked, &sidecar)
		.unwrap();
	harness.app.reindex().unwrap();
	let entries = harness.entries().len();

	for result in [
		ops::rotate(&harness.app, id, &[1], 90).map(|_| ()),
		ops::delete_pages(&harness.app, id, &[1]).map(|_| ()),
		ops::split(&harness.app, id, &[vec![1], vec![2, 3]]).map(|_| ()),
	] {
		match result {
			Err(Error::Invalid(message)) => assert!(message.contains("password"), "{message}"),
			other => panic!("{other:?}"),
		}
	}
	assert_eq!(harness.entries().len(), entries);
	assert_eq!(
		std::fs::read(library.library_dir().join(WHOLE)).unwrap(),
		locked
	);
}

#[tokio::test]
async fn merging_the_parts_of_one_file_keeps_one_source() {
	let (harness, id, _) = letter().await;
	let parts = ops::split(&harness.app, id, &[vec![1], vec![2, 3]])
		.unwrap()
		.result
		.documents;
	let merged = ops::merge(&harness.app, &parts).unwrap().result.sidecar;
	assert_eq!(merged.sources.len(), 1);
	assert_eq!(merged.sources[0].original_name, "letter.pdf");
	assert_eq!(merged.sources[0].pages, [1, 2, 3]);

	let other = harness.one("other.pdf", &text_pdf(&["other words"])).await;
	let mixed = ops::merge(&harness.app, &[merged.id, other])
		.unwrap()
		.result
		.sidecar;
	assert_eq!(mixed.sources.len(), 2);
}
