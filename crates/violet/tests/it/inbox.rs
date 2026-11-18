use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use violet::pipeline::{self, Outcome};
use violet::watcher::{self, Scanner};
use violet_core::{Action, hash};

use crate::common::{Fake, Harness, analysis, is_ocr, kind, segment, splitting_fake, text_pdf};

/// A file to write over, and the bytes to write.
type Replacement = (PathBuf, Vec<u8>);

fn pages_of(body: &serde_json::Value) -> Vec<u32> {
	crate::common::user_text(body)
		.lines()
		.find_map(|line| line.strip_prefix("Pages: "))
		.unwrap()
		.split(", ")
		.map(|page| page.parse().unwrap())
		.collect()
}

#[tokio::test]
async fn a_file_replaced_while_it_is_read_is_filed_on_its_own() {
	let replace: Arc<Mutex<Option<Replacement>>> = Arc::default();
	let pending = replace.clone();
	let fake = Fake::start(move |body| {
		if is_ocr(body) {
			return (200, "[BLANK]".into());
		}
		if let Some((path, bytes)) = pending.lock().unwrap().take() {
			std::fs::write(path, bytes).unwrap();
		}
		analysis(
			vec![segment(
				&pages_of(body),
				"Memo",
				"Work",
				"Acme",
				"2022-05-06",
			)],
			&[],
		)
	})
	.await;
	let harness = Harness::new(fake.models());
	let first = text_pdf(&["the first scan of the morning"]);
	let second = text_pdf(&["the second scan, written over the first"]);
	let relative = harness.drop_file("scan.pdf", &first);
	*replace.lock().unwrap() = Some((harness.app.library.inbox().join(&relative), second.clone()));

	let Outcome::Filed { documents, .. } = pipeline::ingest(&harness.app, &relative).await.unwrap()
	else {
		panic!("not filed");
	};
	assert_eq!(documents.len(), 1);
	assert_eq!(harness.inbox(), ["scan.pdf"]);
	assert_eq!(
		std::fs::read(harness.app.library.inbox().join("scan.pdf")).unwrap(),
		second
	);
	let first_original = format!("{}.pdf", hash::sha256(&first));
	let Action::Ingest { original, .. } = harness.last_entry().action else {
		panic!("not an ingest");
	};
	assert_eq!(original, first_original);
	assert_eq!(
		std::fs::read(harness.app.library.originals().join(&original)).unwrap(),
		first
	);

	let again = pipeline::ingest(&harness.app, &relative).await.unwrap();
	assert!(matches!(again, Outcome::Filed { .. }), "{again:?}");
	assert!(harness.inbox().is_empty());
	assert_eq!(harness.documents().len(), 2);
	let mut originals = vec![first_original, format!("{}.pdf", hash::sha256(&second))];
	originals.sort();
	assert_eq!(harness.originals(), originals);
}

#[tokio::test]
async fn a_copy_of_a_queued_file_is_queued_once_that_job_is_done() {
	let fake = splitting_fake().await;
	let harness = Harness::new(fake.models());
	let pdf = text_pdf(&["a memo dropped twice under two names"]);
	harness.drop_file("a.pdf", &pdf);
	harness.drop_file("b.pdf", &pdf);
	let mut scanner = Scanner::default();
	assert_eq!(scanner.scan(&harness.app, Duration::ZERO).queued, 1);
	assert_eq!(scanner.scan(&harness.app, Duration::ZERO).queued, 0);

	let done = watcher::drain(&harness.app).await.unwrap();
	assert_eq!(done.len(), 1);
	assert_eq!(harness.inbox(), ["b.pdf"]);

	assert_eq!(scanner.scan(&harness.app, Duration::ZERO).queued, 1);
	let done = watcher::drain(&harness.app).await.unwrap();
	assert!(matches!(done[0].1, Ok(Outcome::Duplicate { .. })));
	assert!(harness.inbox().is_empty());
	assert_eq!(
		harness.entries().iter().map(kind).collect::<Vec<_>>(),
		["ingest", "duplicate"]
	);
	assert_eq!(scanner.scan(&harness.app, Duration::ZERO).queued, 0);
}

#[tokio::test]
async fn a_page_with_a_little_text_is_never_dropped_as_blank() {
	let fake = Fake::start(|body| {
		if is_ocr(body) {
			return (200, "[BLANK]".into());
		}
		analysis(
			vec![segment(&[1], "Contract", "Work", "Acme", "2022-05-06")],
			&[2, 3],
		)
	})
	.await;
	let harness = Harness::new(fake.models());
	let pdf = text_pdf(&["Service contract between Acme and the tenant", "Signed", ""]);
	let id = harness.one("contract.pdf", &pdf).await;
	let (_, sidecar) = harness
		.documents()
		.into_iter()
		.find(|(_, sidecar)| sidecar.id == id)
		.unwrap();
	let texts: Vec<&str> = sidecar.pages.iter().map(|page| page.text.trim()).collect();
	assert_eq!(texts.len(), 2, "{texts:?}");
	assert_eq!(texts[1], "Signed");
	assert!(
		harness
			.last_entry()
			.summary
			.ends_with(", dropped 1 blank page"),
		"{}",
		harness.last_entry().summary
	);
}
