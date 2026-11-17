use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;
use violet::ops::{self, Patch};
use violet::{App, Error, Models, pipeline};
use violet_core::{Action, hash};

use crate::common::{Fake, Harness, analysis, is_ocr, kind, text_pdf};

const WHOLE: &str = "Unsorted/Undated/leaflet.pdf";
const REFILED: &str = "Personal/2024/2024-03-02 Ville de Port-Celeste Journee de reboisement.pdf";
const LEAFLET: &str = "Ville de Port-Celeste journee de reboisement 2024-03-02 venez nombreux";

fn leaflet_pdf() -> Vec<u8> {
	text_pdf(&[
		LEAFLET,
		"plan du parc et horaires des navettes",
		"annexe",
		"",
	])
}

fn leaflet(pages: &[u32]) -> Value {
	json!({
		"pages": pages,
		"rotations": [],
		"title": "Journee de reboisement",
		"area": "Personal",
		"doc_type": "leaflet",
		"issuer": "Ville de Port-Celeste",
		"date_printed": "2024-03-02",
		"language": "fr",
		"summary": "Leaflet for a tree planting day.",
		"tags": ["event", "france", "paris", "reboisement"],
		"facts": [],
		"confidence": 0.8,
	})
}

fn annex(pages: &[u32]) -> Value {
	json!({
		"pages": pages,
		"title": "Annexe",
		"area": "Travel",
		"doc_type": "form",
		"confidence": 0.9,
	})
}

/// Answers every analysis with the annex first and the leaflet on the two pages before it.
async fn leaflet_fake() -> Fake {
	Fake::start(|body| {
		if is_ocr(body) {
			return (200, "never asked".into());
		}
		analysis(vec![annex(&[3]), leaflet(&[1, 2])], &[4])
	})
	.await
}

/// A leaflet filed whole without a model, in a library that now has one.
async fn filed(fake: &Fake) -> (Harness, Uuid, Vec<u8>) {
	let mut harness = Harness::new(Models::default());
	let pdf = leaflet_pdf();
	let id = harness.one("leaflet.pdf", &pdf).await;
	assert_eq!(harness.paths(), [WHOLE]);
	harness.app = harness.reopen(fake.models());
	(harness, id, pdf)
}

#[tokio::test]
async fn reading_again_refiles_the_document_whole_and_undo_puts_it_back() {
	let fake = leaflet_fake().await;
	let (harness, id, pdf) = filed(&fake).await;
	let before = ops::document(&harness.app, id).unwrap();

	let changed = pipeline::reanalyze(&harness.app, id).await.unwrap();
	assert_eq!(changed.result.path, PathBuf::from(REFILED));
	assert_eq!(harness.paths(), [REFILED]);
	let sidecar = harness.documents().remove(0).1;
	assert_eq!(sidecar, changed.result.sidecar);
	assert_eq!(sidecar.id, id);
	assert_eq!(sidecar.title, "Journee de reboisement");
	assert_eq!(sidecar.area, "Personal");
	assert_eq!(sidecar.doc_type, "leaflet");
	assert_eq!(sidecar.issuer.as_deref(), Some("Ville de Port-Celeste"));
	assert_eq!(sidecar.date.as_deref(), Some("2024-03-02"));
	assert_eq!(sidecar.language.as_deref(), Some("fr"));
	assert_eq!(
		sidecar.summary.as_deref(),
		Some("Leaflet for a tree planting day.")
	);
	assert_eq!(sidecar.tags, ["event", "reboisement"]);
	assert_eq!(sidecar.confidence, 0.8);
	assert!(sidecar.updated_at > before.sidecar.updated_at);
	assert_eq!(sidecar.created_at, before.sidecar.created_at);
	assert_eq!(sidecar.pages, before.sidecar.pages);
	assert_eq!(sidecar.sources, before.sidecar.sources);
	assert_eq!(sidecar.sha256, hash::sha256(&pdf));
	assert_eq!(harness.file_sha(Path::new(REFILED)), hash::sha256(&pdf));
	assert_eq!(
		harness.app.index().get(id).unwrap().unwrap(),
		(PathBuf::from(REFILED), sidecar.clone())
	);

	assert_eq!(fake.ocr_requests(), 0);
	assert_eq!(fake.analysis_images(), [4]);
	let prompts = fake.analysis_prompts();
	assert!(prompts[0].contains("File: leaflet.pdf"), "{}", prompts[0]);
	assert!(prompts[0].contains("exactly one segment"), "{}", prompts[0]);
	assert!(prompts[0].contains("Pages: 1, 2, 3, 4"), "{}", prompts[0]);

	let entry = harness.last_entry();
	assert_eq!(Some(entry.id), changed.entry);
	assert_eq!(entry.actor, "agent");
	assert_eq!(entry.summary, format!("Read {WHOLE} again, now {REFILED}"));
	match &entry.action {
		Action::Update {
			document,
			from,
			to,
			before: recorded,
		} => {
			assert_eq!(*document, id);
			assert_eq!(from, Path::new(WHOLE));
			assert_eq!(to, Path::new(REFILED));
			assert_eq!(**recorded, before.sidecar);
		}
		other => panic!("{other:?}"),
	}

	let again = pipeline::reanalyze(&harness.app, id).await.unwrap();
	assert_eq!(again.entry, None);
	assert_eq!(harness.entries().len(), 2);

	let undo = ops::undo(&harness.app, entry.id).unwrap();
	assert_eq!(kind(&undo), "undo");
	assert_eq!(harness.paths(), [WHOLE]);
	assert_eq!(harness.documents()[0].1, before.sidecar);
	assert_eq!(harness.file_sha(Path::new(WHOLE)), hash::sha256(&pdf));
	assert_eq!(fake.ocr_requests(), 0);
}

#[tokio::test]
async fn a_near_duplicate_mark_survives_reading_again() {
	let fake = leaflet_fake().await;
	let (harness, id, _) = filed(&fake).await;
	let tags = Patch {
		tags: Some(vec!["near-duplicate".into(), "paris".into()]),
		..Patch::default()
	};
	ops::update(&harness.app, id, tags).unwrap();
	let changed = pipeline::reanalyze(&harness.app, id).await.unwrap();
	assert_eq!(
		changed.result.sidecar.tags,
		["event", "reboisement", "near-duplicate"]
	);
}

#[tokio::test]
async fn a_text_file_is_read_again_without_images() {
	let fake = Fake::start(|_| analysis(vec![leaflet(&[1])], &[])).await;
	let mut harness = Harness::new(Models::default());
	let id = harness.one("leaflet.txt", LEAFLET.as_bytes()).await;
	harness.app = harness.reopen(fake.models());

	let changed = pipeline::reanalyze(&harness.app, id).await.unwrap();
	assert_eq!(
		changed.result.path,
		PathBuf::from(REFILED).with_extension("txt")
	);
	assert_eq!(fake.analysis_images(), [0]);
	assert_eq!(fake.ocr_requests(), 0);
}

#[tokio::test]
async fn reading_again_needs_a_model_a_document_and_text() {
	let fake = leaflet_fake().await;
	let mut harness = Harness::new(Models::default());
	let blob = harness.one("blob.bin", &[0, 1, 2, 3]).await;
	let id = harness.one("leaflet.pdf", &leaflet_pdf()).await;
	assert!(matches!(
		pipeline::reanalyze(&harness.app, id).await,
		Err(Error::Invalid(message)) if message.contains("no model")
	));

	harness.app = harness.reopen(fake.models());
	assert!(matches!(
		pipeline::reanalyze(&harness.app, Uuid::new_v4()).await,
		Err(Error::NotFound(_))
	));
	assert!(matches!(
		pipeline::reanalyze(&harness.app, blob).await,
		Err(Error::Invalid(message)) if message.contains("no text")
	));
	assert_eq!(fake.analysis_requests(), 0);
	assert_eq!(harness.entries().len(), 2);
}

#[tokio::test]
async fn a_change_made_while_the_model_works_wins() {
	let shared: Arc<OnceLock<(Arc<App>, Uuid)>> = Arc::default();
	let seen = shared.clone();
	let fake = Fake::start(move |_| {
		let (app, id) = seen.get().unwrap();
		let patch = Patch {
			title: Some("Mine".into()),
			..Patch::default()
		};
		ops::update(app, *id, patch).unwrap();
		analysis(vec![leaflet(&[1, 2, 3, 4])], &[])
	})
	.await;
	let (harness, id, _) = filed(&fake).await;
	shared.set((harness.app.clone(), id)).ok().unwrap();

	let refused = pipeline::reanalyze(&harness.app, id).await;
	assert!(
		matches!(&refused, Err(Error::Conflict(message)) if message.contains("changed while")),
		"{refused:?}"
	);
	assert_eq!(harness.paths(), ["Unsorted/Undated/Mine.pdf"]);
	assert_eq!(harness.entries().len(), 2);
}

#[tokio::test]
async fn reading_again_over_http() {
	let fake = leaflet_fake().await;
	let (harness, id, _) = filed(&fake).await;
	let router = violet::http::router(harness.app.clone());
	let post = |uri: String| {
		let router = router.clone();
		async move {
			let request = Request::post(uri).body(Body::empty()).unwrap();
			let response = router.oneshot(request).await.unwrap();
			let status = response.status();
			let body = response.into_body().collect().await.unwrap().to_bytes();
			(status, serde_json::from_slice::<Value>(&body).unwrap())
		}
	};

	let (status, answer) = post(format!("/api/documents/{id}/reanalyze")).await;
	assert_eq!(status, StatusCode::OK, "{answer}");
	assert_eq!(answer["path"], REFILED);
	assert_eq!(answer["sidecar"]["tags"], json!(["event", "reboisement"]));
	assert_eq!(
		answer["entry"].as_str().unwrap(),
		harness.last_entry().id.to_string()
	);

	let (status, _) = post(format!("/api/documents/{}/reanalyze", Uuid::new_v4())).await;
	assert_eq!(status, StatusCode::NOT_FOUND);
	let (status, _) = post("/api/documents/nope/reanalyze".into()).await;
	assert_eq!(status, StatusCode::BAD_REQUEST);

	let entry = answer["entry"].as_str().unwrap();
	let (status, _) = post(format!("/api/activity/{entry}/undo")).await;
	assert_eq!(status, StatusCode::OK);
	assert_eq!(harness.paths(), [WHOLE]);
}
