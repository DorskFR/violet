use std::path::{Path, PathBuf};

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;
use violet::{Error, Models, ops, pipeline};
use violet_core::{Action, Sidecar, Source, hash, sidecar};

use crate::common::{Fake, Harness, analysis, is_ocr, kind};
use crate::reading::{GIF, blank_png, photo_fake};

const LEGACY_DOC: &[u8] = b"\xd0\xcf\x11\xe0\xa1\xb1\x1a\xe1legacy word file";

/// A document an earlier version filed as it was, without reading it.
fn legacy(harness: &Harness, path: &str, bytes: &[u8]) -> Uuid {
	let app = harness.app.clone();
	let sha = hash::sha256(bytes);
	let name = Path::new(path).file_name().unwrap().to_string_lossy();
	let at = ops::now();
	let sidecar = Sidecar {
		version: sidecar::SIDECAR_VERSION,
		id: Uuid::new_v4(),
		title: Path::new(path)
			.file_stem()
			.unwrap()
			.to_string_lossy()
			.into(),
		area: "Unsorted".into(),
		doc_type: "document".into(),
		issuer: None,
		date: None,
		date_precision: Default::default(),
		language: None,
		summary: None,
		tags: Vec::new(),
		facts: Vec::new(),
		pages: Vec::new(),
		sources: vec![Source {
			sha256: sha.clone(),
			original_name: name.to_string(),
			pages: Vec::new(),
			mail: None,
		}],
		sha256: sha.clone(),
		confidence: 0.0,
		created_at: at,
		updated_at: at,
		not_duplicates: Vec::new(),
		extra: serde_json::Map::new(),
	};
	let mut index = app.index();
	let mut change = ops::Change::begin(&app, &mut index).unwrap();
	change.create(Path::new(path), bytes, &sidecar).unwrap();
	let action = Action::Ingest {
		original: ops::original_name(&sha, Path::new(path)),
		original_name: name.to_string(),
		documents: vec![(sidecar.id, path.into())],
	};
	change
		.commit("agent", action, format!("Filed {path}"))
		.unwrap();
	sidecar.id
}

fn described(title: &str, area: &str, doc_type: &str) -> Value {
	json!({
		"pages": [1],
		"rotations": [],
		"title": title,
		"area": area,
		"doc_type": doc_type,
		"issuer": null,
		"date_printed": null,
		"total_printed": null,
		"currency": null,
		"language": "en",
		"summary": "Read again.",
		"tags": [],
		"facts": [],
		"confidence": 0.8,
	})
}

#[tokio::test]
async fn a_photo_filed_without_text_is_read_again_in_place() {
	let fake = photo_fake().await;
	let mut harness = Harness::new(Models::default());
	let id = harness.one("IMG_0001.png", &blank_png()).await;
	let before = harness.documents();
	assert_eq!(harness.paths(), ["Unsorted/Undated/IMG_0001.pdf"]);
	assert!(matches!(
		pipeline::reread(&harness.app, id).await,
		Err(Error::Invalid(message)) if message.contains("no model")
	));
	assert!(matches!(
		pipeline::reread(&harness.app, Uuid::new_v4()).await,
		Err(Error::NotFound(_))
	));
	harness.app = harness.reopen(fake.models());

	let changed = pipeline::reread(&harness.app, id).await.unwrap();
	assert_eq!(changed.result.sidecar.id, id);
	assert_eq!(
		changed.result.path,
		PathBuf::from("Personal/Undated/Sea view.pdf")
	);
	assert_eq!(changed.result.sidecar.doc_type, "photo");
	assert_eq!(fake.ocr_requests(), 1);
	let entry = harness.last_entry();
	assert_eq!(Some(entry.id), changed.entry);
	assert_eq!((kind(&entry), entry.actor.as_str()), ("update", "agent"));
	assert_eq!(
		entry.summary,
		"Read the file of Unsorted/Undated/IMG_0001.pdf again, now Personal/Undated/Sea view.pdf"
	);

	ops::undo(&harness.app, entry.id).unwrap();
	assert_eq!(harness.documents(), before);
}

#[tokio::test]
async fn a_gif_kept_as_it_was_is_read_again_into_a_pdf_and_back_by_undo() {
	let fake = Fake::start(|body| {
		if is_ocr(body) {
			return (200, "Plan of the garden shed".into());
		}
		analysis(vec![described("Garden shed plan", "Housing", "plan")], &[])
	})
	.await;
	let harness = Harness::new(fake.models());
	let id = legacy(&harness, "Unsorted/Undated/drawing.gif", GIF);
	let before = harness.documents();
	let gif_sha = hash::sha256(GIF);

	let changed = pipeline::reread(&harness.app, id).await.unwrap();
	let path = PathBuf::from("Housing/Undated/Garden shed plan.pdf");
	assert_eq!(changed.result.path, path);
	assert_eq!(harness.paths(), [path.to_string_lossy()]);
	let sidecar = &changed.result.sidecar;
	assert_eq!(sidecar.id, id);
	assert_eq!(sidecar.pages.len(), 1);
	assert_eq!(sidecar.pages[0].text, "Plan of the garden shed");
	assert_eq!(sidecar.sources.len(), 1);
	assert_eq!(harness.file_sha(&path), sidecar.sha256);
	let bytes = std::fs::read(harness.app.library.library_dir().join(&path)).unwrap();
	assert_eq!(violet_pdf::detect(&bytes), violet_pdf::Kind::Pdf);
	assert_eq!(harness.app.index().get(id).unwrap().unwrap().0, path);
	let entry = harness.last_entry();
	assert_eq!((kind(&entry), entry.actor.as_str()), ("replace", "agent"));
	assert!(harness.originals().contains(&format!("{gif_sha}.gif")));

	ops::undo(&harness.app, entry.id).unwrap();
	assert_eq!(harness.documents(), before);
	assert_eq!(
		harness.file_sha(Path::new("Unsorted/Undated/drawing.gif")),
		gif_sha
	);
	assert_eq!(
		harness.app.index().get(id).unwrap().unwrap().0,
		PathBuf::from("Unsorted/Undated/drawing.gif")
	);
}

struct Http {
	router: axum::Router,
}

impl Http {
	async fn send(&self, request: Request<Body>) -> (StatusCode, Value) {
		let response = self.router.clone().oneshot(request).await.unwrap();
		let status = response.status();
		let body = response.into_body().collect().await.unwrap().to_bytes();
		(status, serde_json::from_slice(&body).unwrap())
	}

	async fn post(&self, uri: &str) -> (StatusCode, Value) {
		self.send(Request::post(uri).body(Body::empty()).unwrap())
			.await
	}

	async fn replace(&self, uri: &str, parts: &[(&str, &str, &[u8])]) -> (StatusCode, Value) {
		let boundary = "XBOUNDARYX";
		let mut body = Vec::new();
		for (field, file_name, bytes) in parts {
			body.extend_from_slice(
				format!(
					"--{boundary}\r\nContent-Disposition: form-data; name=\"{field}\"; filename=\"{file_name}\"\r\nContent-Type: application/octet-stream\r\n\r\n"
				)
				.as_bytes(),
			);
			body.extend_from_slice(bytes);
			body.extend_from_slice(b"\r\n");
		}
		body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
		let request = Request::post(uri)
			.header(
				header::CONTENT_TYPE,
				format!("multipart/form-data; boundary={boundary}"),
			)
			.body(Body::from(body))
			.unwrap();
		self.send(request).await
	}
}

#[tokio::test]
async fn a_legacy_office_file_is_replaced_over_http_and_undo_brings_it_back() {
	let fake = Fake::start(|body| {
		if is_ocr(body) {
			return (200, "never asked".into());
		}
		analysis(
			vec![described("Garden party invitation", "Personal", "letter")],
			&[],
		)
	})
	.await;
	let harness = Harness::new(fake.models());
	let id = legacy(&harness, "Unsorted/Undated/letter.doc", LEGACY_DOC);
	let before = harness.documents();
	let entries = harness.entries().len();
	let http = Http {
		router: violet::http::router(harness.app.clone()),
	};
	let uri = format!("/api/documents/{id}/replace");

	let (status, answer) = http.post(&format!("/api/documents/{id}/reread")).await;
	assert_eq!(status, StatusCode::BAD_REQUEST, "{answer}");
	assert!(
		answer["error"]
			.as_str()
			.unwrap()
			.contains("unsupported format .doc")
	);
	let (status, answer) = http
		.replace(&uri, &[("file", "other.doc", LEGACY_DOC)])
		.await;
	assert_eq!(status, StatusCode::BAD_REQUEST, "{answer}");
	let (status, _) = http.replace(&uri, &[("note", "x.txt", b"x")]).await;
	assert_eq!(status, StatusCode::BAD_REQUEST);
	let (status, _) = http
		.replace(&uri, &[("file", "a.txt", b"a"), ("file", "b.txt", b"b")])
		.await;
	assert_eq!(status, StatusCode::BAD_REQUEST);
	let (status, _) = http
		.replace(
			&format!("/api/documents/{}/replace", Uuid::new_v4()),
			&[("file", "a.txt", b"a")],
		)
		.await;
	assert_eq!(status, StatusCode::NOT_FOUND);
	let (status, _) = http.post("/api/documents/nope/reread").await;
	assert_eq!(status, StatusCode::BAD_REQUEST);
	assert_eq!(harness.documents(), before);
	assert_eq!(harness.entries().len(), entries);

	let rtf = br"{\rtf1\ansi Dear neighbour, the garden party is on Saturday.\par}";
	let (status, answer) = http.replace(&uri, &[("file", "letter.rtf", rtf)]).await;
	assert_eq!(status, StatusCode::OK, "{answer}");
	assert_eq!(
		answer["path"],
		"Personal/Undated/Garden party invitation.rtf"
	);
	assert_eq!(answer["sidecar"]["id"], id.to_string());
	let sources = answer["sidecar"]["sources"].as_array().unwrap();
	assert_eq!(sources.len(), 2);
	assert_eq!(sources[1]["original_name"], "letter.rtf");
	assert_eq!(sources[1]["sha256"], hash::sha256(rtf));
	assert!(
		answer["sidecar"]["pages"][0]["text"]
			.as_str()
			.unwrap()
			.contains("garden party")
	);
	let entry = harness.last_entry();
	assert_eq!(answer["entry"], entry.id.to_string());
	assert_eq!((kind(&entry), entry.actor.as_str()), ("replace", "user"));
	assert_eq!(
		entry.summary,
		"Replaced Unsorted/Undated/letter.doc with \"letter.rtf\", now Personal/Undated/Garden party invitation.rtf"
	);
	assert!(
		harness
			.originals()
			.contains(&format!("{}.doc", hash::sha256(LEGACY_DOC)))
	);
	assert_eq!(fake.ocr_requests(), 0);

	let (status, answer) = http.post(&format!("/api/activity/{}/undo", entry.id)).await;
	assert_eq!(status, StatusCode::OK, "{answer}");
	assert_eq!(harness.documents(), before);
	assert_eq!(
		harness.file_sha(Path::new("Unsorted/Undated/letter.doc")),
		hash::sha256(LEGACY_DOC)
	);
}
