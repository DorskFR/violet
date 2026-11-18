use std::path::{Path, PathBuf};
use std::time::Duration;

use violet::app::ModelStatus;
use violet::pipeline::{self, Outcome};
use violet::watcher::{self, Scanner, Settings};
use violet::{Models, ops};
use violet_core::{Action, JobQueue, Query, TextSource, hash};

use crate::common::{
	Fake, Harness, analysis, is_ocr, kind, segment, splitting_fake, text_pdf, three_pages,
};

const RECEIPT: &str = "Health/2024/2024-03-14 Sakura Pharmacy Receipt.pdf";
const BILL: &str = "Housing/2023/2023-11-02 Tokyo Power Electricity Bill.pdf";

fn queue_inbox(harness: &Harness) -> usize {
	Scanner::default().scan(&harness.app, Duration::ZERO).queued
}

#[tokio::test]
async fn a_scan_of_two_documents_and_a_blank_page_is_split_and_filed() {
	let fake = splitting_fake().await;
	let harness = Harness::new(fake.models());
	let pdf = three_pages();
	let sha = hash::sha256(&pdf);
	harness.drop_file("three.pdf", &pdf);

	assert_eq!(queue_inbox(&harness), 1);
	let done = watcher::drain(&harness.app).await.unwrap();
	assert_eq!(done.len(), 1);
	assert!(done[0].1.is_ok(), "{:?}", done[0].1);

	assert_eq!(harness.paths(), [RECEIPT, BILL]);
	let documents = harness.documents();
	for ((path, sidecar), (page, word)) in
		documents.iter().zip([(1, "Pharmacy"), (3, "electricity")])
	{
		let bytes = std::fs::read(harness.app.library.library_dir().join(path)).unwrap();
		assert_eq!(violet_pdf::page_count(&bytes).unwrap(), 1);
		assert_eq!(sidecar.sha256, hash::sha256(&bytes));
		assert_eq!(sidecar.sources.len(), 1);
		assert_eq!(sidecar.sources[0].sha256, sha);
		assert_eq!(sidecar.sources[0].original_name, "three.pdf");
		assert_eq!(sidecar.sources[0].pages, [page]);
		assert_eq!(sidecar.pages.len(), 1);
		assert_eq!(sidecar.pages[0].number, 1);
		assert_eq!(sidecar.pages[0].source, TextSource::Layer);
		assert!(sidecar.pages[0].text.contains(word));
		assert_eq!(sidecar.facts[0].amount_minor, Some(1200));
		assert_eq!(sidecar.tags, ["payment"]);
		let indexed = harness.app.index().get(sidecar.id).unwrap().unwrap();
		assert_eq!(indexed, (path.clone(), sidecar.clone()));
	}
	assert_eq!(fake.ocr_requests(), 1);
	assert_eq!(fake.analysis_requests(), 1);

	let found = harness
		.app
		.index()
		.search(&Query {
			text: Some("electricity".into()),
			..Query::default()
		})
		.unwrap();
	assert_eq!(found.total, 1);
	assert_eq!(found.hits[0].path, PathBuf::from(BILL));

	let entries = harness.entries();
	assert_eq!(entries.len(), 1);
	assert_eq!(entries[0].actor, "agent");
	assert_eq!(
		entries[0].summary,
		"Filed \"three.pdf\" as 2 documents, dropped 1 blank page"
	);
	match &entries[0].action {
		Action::Ingest {
			original,
			original_name,
			documents: filed,
		} => {
			assert_eq!(original, &format!("{sha}.pdf"));
			assert_eq!(original_name, "three.pdf");
			let expected: Vec<_> = documents
				.iter()
				.map(|(path, sidecar)| (sidecar.id, path.clone()))
				.collect();
			assert_eq!(filed, &expected);
		}
		other => panic!("{other:?}"),
	}
	assert!(harness.inbox().is_empty());
	assert_eq!(harness.originals(), [format!("{sha}.pdf")]);

	let status = ops::status(&harness.app, harness.app.model_status().await).unwrap();
	assert_eq!(
		(status.jobs.done, status.jobs.failed, status.inbox),
		(1, 0, 0)
	);
	assert_eq!(
		status.model,
		ModelStatus {
			vision: true,
			text: true
		}
	);
}

#[tokio::test]
async fn the_same_bytes_again_are_a_duplicate() {
	let fake = splitting_fake().await;
	let harness = Harness::new(fake.models());
	let pdf = three_pages();
	harness.drop_file("three.pdf", &pdf);
	assert_eq!(queue_inbox(&harness), 1);
	watcher::drain(&harness.app).await.unwrap();
	let filed: Vec<_> = harness
		.documents()
		.iter()
		.map(|(_, sidecar)| sidecar.id)
		.collect();

	harness.drop_file("sub/again.pdf", &pdf);
	assert_eq!(queue_inbox(&harness), 1);
	let done = watcher::drain(&harness.app).await.unwrap();
	let entry = harness.last_entry();
	match (&entry.action, done[0].1.as_ref().unwrap()) {
		(
			Action::Duplicate {
				original_name, of, ..
			},
			Outcome::Duplicate {
				entry: recorded,
				of: reported,
			},
		) => {
			assert_eq!(original_name, "again.pdf");
			assert!(filed.contains(of));
			assert_eq!(of, reported);
			assert_eq!(*recorded, entry.id);
		}
		other => panic!("{other:?}"),
	}
	assert_eq!(harness.paths(), [RECEIPT, BILL]);
	assert_eq!(fake.analysis_requests(), 1);
	assert!(!harness.app.library.inbox().join("sub/again.pdf").exists());
	assert_eq!(harness.originals().len(), 1);

	violet::ops::undo(&harness.app, entry.id).unwrap();
	assert_eq!(harness.paths(), [RECEIPT, BILL]);
}

#[tokio::test]
async fn an_image_is_wrapped_and_read_by_ocr() {
	let fake = Fake::start(|body| {
		if crate::common::is_ocr(body) {
			(200, "Parking ticket 2024-01-05\nTotal 500 yen".into())
		} else {
			assert!(crate::common::user_text(body).contains("Parking ticket"));
			analysis(
				vec![segment(
					&[1],
					"Parking Ticket",
					"Car",
					"City Parking",
					"2024-01-05",
				)],
				&[],
			)
		}
	})
	.await;
	let harness = Harness::new(fake.models());
	let png = violet_pdf::render_png(&text_pdf(&["hello"]), 1, 200).unwrap();
	harness.one("IMG_0001.PNG", &png).await;

	assert_eq!(
		harness.paths(),
		["Car/2024/2024-01-05 City Parking Parking Ticket.pdf"]
	);
	let (path, sidecar) = harness.documents().remove(0);
	let bytes = std::fs::read(harness.app.library.library_dir().join(path)).unwrap();
	assert_eq!(violet_pdf::detect(&bytes), violet_pdf::Kind::Pdf);
	assert_eq!(sidecar.pages[0].source, TextSource::Ocr);
	assert_eq!(
		sidecar.pages[0].text,
		"Parking ticket 2024-01-05\nTotal 500 yen"
	);
	assert_eq!(sidecar.sources[0].sha256, hash::sha256(&png));
	assert_eq!(fake.ocr_requests(), 1);
	assert_eq!(harness.originals(), [format!("{}.png", hash::sha256(&png))]);
}

#[tokio::test]
async fn a_tall_page_is_read_in_tiles() {
	let fake = Fake::start(|body| {
		if crate::common::is_ocr(body) {
			(200, "City Parking ticket 2024-01-05".into())
		} else {
			analysis(
				vec![segment(
					&[1],
					"Parking Ticket",
					"Car",
					"City Parking",
					"2024-01-05",
				)],
				&[],
			)
		}
	})
	.await;
	let harness = Harness::new(fake.models());
	let upright = violet_pdf::rotate(&text_pdf(&["hello"]), &[1], 90).unwrap();
	let png = violet_pdf::render_png(&upright, 1, 300).unwrap();
	harness.one("IMG_0003.PNG", &png).await;

	let (_, sidecar) = harness.documents().remove(0);
	assert_eq!(
		sidecar.pages[0].text,
		"City Parking ticket 2024-01-05\nCity Parking ticket 2024-01-05"
	);
	assert_eq!(fake.ocr_requests(), 2);
	assert_eq!(fake.analysis_requests(), 1);
}

#[tokio::test]
async fn a_cut_off_transcription_lowers_the_confidence() {
	let fake = Fake::start(|body| {
		if crate::common::is_ocr(body) {
			(
				crate::common::CUT_OFF,
				"City Parking\nParking ticket 2024-01-05\nTotal 500 yen\nTerms and".into(),
			)
		} else {
			analysis(
				vec![segment(
					&[1],
					"Parking Ticket",
					"Car",
					"City Parking",
					"2024-01-05",
				)],
				&[],
			)
		}
	})
	.await;
	let harness = Harness::new(fake.models());
	let png = violet_pdf::render_png(&text_pdf(&["hello"]), 1, 200).unwrap();
	harness.one("IMG_0002.PNG", &png).await;

	let (_, sidecar) = harness.documents().remove(0);
	assert_eq!(
		sidecar.pages[0].text,
		"City Parking\nParking ticket 2024-01-05\nTotal 500 yen\nTerms and"
	);
	assert!((sidecar.confidence - 0.9 * 0.6).abs() < 1e-6);
	assert_eq!(fake.ocr_requests(), 1);
	assert_eq!(fake.analysis_requests(), 1);
}

#[tokio::test]
async fn without_a_model_files_are_filed_under_unsorted() {
	let harness = Harness::new(Models::default());
	let pdf = text_pdf(&["meeting notes about the roof repair", ""]);
	harness.one("scan 2024-03-14 notes.pdf", &pdf).await;
	harness.one("readme.txt", b"plain words here\n").await;
	let png = violet_pdf::render_png(&text_pdf(&["hello"]), 1, 100).unwrap();
	harness.one("photo.png", &png).await;
	let mail =
		"Subject: Your invoice\nDate: Thu, 14 Mar 2024 10:00:00 +0900\n\nPlease pay 100 yen.\n";
	harness.one("mail.eml", mail.as_bytes()).await;

	assert_eq!(
		harness.paths(),
		[
			"Unsorted/2024/2024-03-14 Your invoice.eml",
			"Unsorted/2024/2024-03-14 scan 2024-03-14 notes.pdf",
			"Unsorted/Undated/photo.pdf",
			"Unsorted/Undated/readme.txt",
		]
	);
	let documents = harness.documents();
	let (mail_path, mail) = &documents[0];
	assert_eq!(mail.pages[0].source, TextSource::Extracted);
	assert!(mail.pages[0].text.contains("Please pay 100 yen."));
	assert!(mail.sources[0].pages.is_empty());
	assert_eq!(harness.file_sha(mail_path), mail.sha256);

	let (notes_path, notes) = &documents[1];
	assert_eq!(harness.file_sha(notes_path), hash::sha256(&pdf));
	assert_eq!(
		(notes.area.as_str(), notes.doc_type.as_str()),
		("Unsorted", "document")
	);
	assert_eq!(notes.date.as_deref(), Some("2024-03-14"));
	assert_eq!(notes.confidence, 0.0);
	assert_eq!(notes.sources[0].pages, [1, 2]);
	assert_eq!(notes.pages[0].source, TextSource::Layer);
	assert_eq!(
		(notes.pages[1].source, notes.pages[1].text.as_str()),
		(TextSource::None, "")
	);

	assert_eq!(documents[2].1.pages[0].source, TextSource::None);
	assert_eq!(documents[3].1.pages[0].text, "plain words here");

	assert_eq!(
		harness.app.model_status().await,
		ModelStatus {
			vision: false,
			text: false
		}
	);
	assert!(harness.inbox().is_empty());
}

#[tokio::test]
async fn the_same_text_in_other_bytes_is_tagged_near_duplicate() {
	let harness = Harness::new(Models::default());
	let words = "the quick brown fox jumps over the lazy dog near the river bank today";
	let first = harness.one("one.pdf", &text_pdf(&[words])).await;
	let second = harness.one("two.pdf", &text_pdf(&[words, ""])).await;
	let (_, sidecar) = harness.app.index().get(second).unwrap().unwrap();
	assert_eq!(sidecar.tags, ["near-duplicate"]);
	assert_eq!(sidecar.extra["duplicate_of"], first.to_string());
	let (_, original) = harness.app.index().get(first).unwrap().unwrap();
	assert!(original.tags.is_empty());
	assert_eq!(harness.documents().len(), 2);
}

#[tokio::test]
async fn a_job_claimed_by_a_dead_process_completes_once() {
	let fake = splitting_fake().await;
	let mut harness = Harness::new(fake.models());
	harness.drop_file("three.pdf", &three_pages());
	assert_eq!(queue_inbox(&harness), 1);
	{
		let index = harness.app.index();
		let claimed = JobQueue::new(index.connection()).unwrap().claim().unwrap();
		assert!(claimed.is_some());
	}
	assert!(watcher::drain(&harness.app).await.unwrap().is_empty());

	let restarted = harness.reopen(fake.models());
	assert_eq!(watcher::recover(&restarted).unwrap(), 1);
	assert_eq!(
		Scanner::default().scan(&restarted, Duration::ZERO).queued,
		0
	);
	let done = watcher::drain(&restarted).await.unwrap();
	assert_eq!(done.len(), 1);
	assert_eq!(done[0].0.attempts, 2);
	assert_eq!(harness.paths(), [RECEIPT, BILL]);
	assert_eq!(harness.entries().len(), 1);
	assert!(watcher::drain(&restarted).await.unwrap().is_empty());
}

#[tokio::test]
async fn a_run_that_died_while_filing_is_finished_without_duplicates() {
	let fake = splitting_fake().await;
	let mut harness = Harness::new(fake.models());
	let pdf = three_pages();
	harness.ingest("three.pdf", &pdf).await;
	let before = harness.documents();

	let (bill_path, bill) = &before[1];
	harness.app.library.stash_original(bill_path).unwrap();
	harness.app.index().remove(bill.id).unwrap();
	std::fs::remove_file(harness.app.library.journal_path()).unwrap();

	let restarted = harness.reopen(fake.models());
	let relative = harness.drop_file("three.pdf", &pdf);
	let outcome = pipeline::ingest(&restarted, &relative).await.unwrap();
	let after = harness.documents();
	assert_eq!(harness.paths(), [RECEIPT, BILL]);
	assert_eq!(after[0], before[0]);
	assert_ne!(after[1].1.id, bill.id);
	assert_eq!(after[1].1.sources, bill.sources);
	let entries = harness.entries();
	assert_eq!(entries.len(), 1);
	match (&entries[0].action, outcome) {
		(
			Action::Ingest { documents, .. },
			Outcome::Filed {
				documents: filed, ..
			},
		) => {
			assert_eq!(documents, &filed);
			assert_eq!(filed.len(), 2);
			assert_eq!(filed[0].0, before[0].1.id);
		}
		other => panic!("{other:?}"),
	}
	assert!(harness.inbox().is_empty());
}

#[tokio::test]
async fn a_failing_model_fails_the_job_and_leaves_the_file() {
	let fake = Fake::start(|_| (500, "out of memory".into())).await;
	let harness = Harness::new(fake.models());
	harness.drop_file("three.pdf", &text_pdf(&["some words on a page"]));
	let mut scanner = Scanner::default();
	assert_eq!(scanner.scan(&harness.app, Duration::ZERO).queued, 1);
	let done = watcher::drain(&harness.app).await.unwrap();
	assert!(done[0].1.is_err());

	let status = ops::status(&harness.app, ModelStatus::default()).unwrap();
	assert_eq!(
		(status.jobs.failed, status.jobs.done, status.inbox),
		(1, 0, 1)
	);
	assert_eq!(status.failed.len(), 1);
	assert!(status.failed[0].error.as_deref().unwrap().contains("500"));
	assert_eq!(harness.inbox(), ["three.pdf"]);
	assert!(harness.documents().is_empty());
	assert!(harness.entries().is_empty());

	assert_eq!(scanner.scan(&harness.app, Duration::ZERO).queued, 0);
	assert!(watcher::drain(&harness.app).await.unwrap().is_empty());
}

#[tokio::test]
async fn files_that_are_hidden_temporary_or_still_changing_wait() {
	let harness = Harness::new(Models::default());
	harness.drop_file(".hidden.pdf", b"x");
	harness.drop_file("download.pdf.part", b"x");
	harness.drop_file("draft.pdf~", b"x");
	harness.drop_file(".folder/inside.pdf", b"x");
	harness.drop_file("empty.pdf", b"");
	harness.drop_file("growing.txt", b"one");
	let mut scanner = Scanner::default();
	let settle = Duration::from_millis(150);
	let first = scanner.scan(&harness.app, settle);
	assert_eq!((first.queued, first.unsettled), (0, 2));

	std::thread::sleep(Duration::from_millis(200));
	std::fs::write(harness.app.library.inbox().join("growing.txt"), b"one two").unwrap();
	assert_eq!(scanner.scan(&harness.app, settle).queued, 0);
	std::thread::sleep(Duration::from_millis(200));
	let settled = scanner.scan(&harness.app, settle);
	assert_eq!((settled.queued, settled.unsettled), (1, 1));
	assert_eq!(scanner.scan(&harness.app, settle).queued, 0);

	watcher::drain(&harness.app).await.unwrap();
	assert_eq!(harness.paths(), ["Unsorted/Undated/growing.txt"]);
	assert_eq!(harness.documents()[0].1.pages[0].text, "one two");
}

#[tokio::test]
async fn the_watcher_and_worker_file_what_is_dropped() {
	let fake = splitting_fake().await;
	let harness = Harness::new(fake.models());
	harness.drop_file(
		"before/start.txt",
		b"present before the watcher started on 2022-05-06",
	);
	let settings = Settings {
		settle: Duration::from_millis(100),
		rescan: Duration::from_millis(200),
	};
	let watching = tokio::spawn(watcher::watch(harness.app.clone(), settings));
	let working = tokio::spawn(watcher::work(harness.app.clone()));
	tokio::time::sleep(Duration::from_millis(50)).await;
	harness.drop_file("three.pdf", &three_pages());

	let deadline = std::time::Instant::now() + Duration::from_secs(20);
	while harness.entries().len() < 2 {
		assert!(std::time::Instant::now() < deadline, "nothing was filed");
		tokio::time::sleep(Duration::from_millis(50)).await;
	}
	watching.abort();
	working.abort();
	assert_eq!(
		harness.paths(),
		[RECEIPT, BILL, "Work/2022/2022-05-06 Acme Memo.txt"]
	);
	assert!(violet::watcher::inbox_files(&harness.app.library).is_empty());
}

#[tokio::test]
async fn paths_outside_the_inbox_are_refused() {
	let harness = Harness::new(Models::default());
	std::fs::write(harness.root().join("secret.txt"), b"secret").unwrap();
	for bad in ["../secret.txt", "/etc/hostname", ""] {
		assert!(matches!(
			pipeline::ingest(&harness.app, Path::new(bad)).await,
			Err(violet::Error::Invalid(_))
		));
	}
	assert!(harness.root().join("secret.txt").exists());
	assert_eq!(kind_count(&harness), 0);
}

fn kind_count(harness: &Harness) -> usize {
	harness
		.entries()
		.iter()
		.filter(|entry| kind(entry) == "ingest")
		.count()
}

#[tokio::test]
async fn unsorted_is_never_offered_to_the_model_as_a_known_area() {
	let fake = Fake::start(|body| {
		if is_ocr(body) {
			(200, String::new())
		} else {
			analysis(
				vec![segment(
					&[1],
					"Receipt",
					"Health",
					"Sakura Pharmacy",
					"2024-03-14",
				)],
				&[],
			)
		}
	})
	.await;
	let mut harness = Harness::new(Models::default());
	harness.one("notes.txt", b"some notes").await;
	harness.app = harness.reopen(fake.models());
	let text = "Sakura Pharmacy receipt 2024-03-14 total 1,200円";
	harness.one("first.pdf", &text_pdf(&[text])).await;
	harness
		.one("second.pdf", &text_pdf(&[&format!("{text} again")]))
		.await;

	let areas: Vec<String> = harness
		.documents()
		.iter()
		.map(|(_, s)| s.area.clone())
		.collect();
	assert!(areas.contains(&"Unsorted".to_string()));
	let prompts = fake.analysis_prompts();
	let last = prompts.last().unwrap();
	assert!(last.contains("Health"), "{last}");
	assert!(!last.contains("Unsorted"), "{last}");
}
