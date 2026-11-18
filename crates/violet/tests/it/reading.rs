use std::path::PathBuf;
use std::time::Duration;

use serde_json::{Value, json};
use violet::watcher::{self, Scanner};
use violet::{Error, Models, ops, pipeline};
use violet_core::TextSource;

use crate::common::{Fake, Harness, analysis, is_ocr, text_pdf, user_text};

pub const NO_DOCUMENT: &str = r#"{"segments": [], "blank_pages": []}"#;
/// A white 1 x 1 GIF.
pub const GIF: &[u8] = b"GIF89a\x01\x00\x01\x00\x80\x00\x00\xff\xff\xff\x00\x00\x00,\x00\x00\x00\x00\x01\x00\x01\x00\x00\x02\x02D\x01\x00;";

pub fn photo_segment(title: &str) -> Value {
	json!({
		"pages": [1],
		"rotations": [],
		"title": title,
		"area": "Personal",
		"doc_type": "photo",
		"issuer": null,
		"date_printed": null,
		"total_printed": null,
		"currency": null,
		"language": null,
		"summary": "A view of the sea from a balcony.",
		"tags": [],
		"facts": [],
		"confidence": 0.7,
	})
}

/// Answers a photo prompt with a sea view and anything else without a document.
pub async fn photo_fake() -> Fake {
	Fake::start(|body| {
		if is_ocr(body) {
			return (200, "[BLANK]".into());
		}
		if user_text(body).contains("No text could be read") {
			analysis(vec![photo_segment("Sea view")], &[])
		} else {
			(200, NO_DOCUMENT.into())
		}
	})
	.await
}

pub fn blank_png() -> Vec<u8> {
	violet_pdf::render_png(&text_pdf(&[""]), 1, 64).unwrap()
}

#[tokio::test]
async fn an_answer_without_documents_fails_the_job_and_keeps_the_file() {
	let fake = Fake::start(|body| {
		if is_ocr(body) {
			(200, "[BLANK]".into())
		} else {
			(200, NO_DOCUMENT.into())
		}
	})
	.await;
	let harness = Harness::new(fake.models());
	let pdf = text_pdf(&[
		"Statement July 2018 closing balance 6551 yen",
		"page two of the statement",
	]);
	harness.drop_file("Statement_Jul 2018.pdf", &pdf);
	assert_eq!(
		Scanner::default().scan(&harness.app, Duration::ZERO).queued,
		1
	);
	let done = watcher::drain(&harness.app).await.unwrap();
	let error = done[0].1.as_ref().unwrap_err().to_string();
	assert!(error.contains("no page in a document"), "{error}");

	assert!(harness.documents().is_empty());
	assert!(harness.entries().is_empty());
	assert_eq!(harness.inbox(), ["Statement_Jul 2018.pdf"]);
	let status = ops::status(&harness.app, harness.app.model_status().await).unwrap();
	assert_eq!(status.jobs.failed, 1);
	let failed = status.failed[0].error.as_deref().unwrap();
	assert!(failed.contains("no page in a document"), "{failed}");
	assert_eq!(fake.analysis_requests(), 2);
	let retry = fake.analysis_prompts().pop().unwrap();
	assert!(retry.contains("exactly one segment"), "{retry}");
}

#[tokio::test]
async fn a_photo_without_text_is_analysed_from_its_image() {
	let fake = photo_fake().await;
	let harness = Harness::new(fake.models());
	let id = harness.one("DSC03470.JPG", &blank_png()).await;

	let (path, sidecar) = harness.app.index().get(id).unwrap().unwrap();
	assert_eq!(path, PathBuf::from("Personal/Undated/Sea view.pdf"));
	assert_eq!(
		(sidecar.area.as_str(), sidecar.doc_type.as_str()),
		("Personal", "photo")
	);
	assert_eq!(
		sidecar.summary.as_deref(),
		Some("A view of the sea from a balcony.")
	);
	assert_eq!(sidecar.pages.len(), 1);
	assert_eq!(
		(sidecar.pages[0].source, sidecar.pages[0].text.as_str()),
		(TextSource::Ocr, "")
	);
	assert_eq!(fake.analysis_requests(), 1);
	assert_eq!(fake.analysis_images(), [1]);
}

#[tokio::test]
async fn text_formats_mails_without_extension_and_gifs_are_read() {
	let harness = Harness::new(Models::default());
	harness
		.one("bank.csv", b"date,amount\n2024-03-14,1200\n")
		.await;
	harness
		.one(
			"clinic.vcf",
			b"BEGIN:VCARD\r\nFN:Sakura\r\n  Clinic\r\nEND:VCARD\r\n",
		)
		.await;
	harness
		.one(
			"dentist.ics",
			b"BEGIN:VEVENT\nSUMMARY:Dentist\nEND:VEVENT\n",
		)
		.await;
	harness
		.one(
			"letter.rtf",
			br"{\rtf1\ansi{\fonttbl{\f0 Arial;}}\f0 Dear neighbour,\par see you\'e9}",
		)
		.await;
	harness
		.one(
			"message",
			b"From: a@b.test\nSubject: Rent receipt\nDate: Thu, 14 Mar 2024 10:00:00 +0900\n\nPaid.\n",
		)
		.await;
	harness.one("drawing.gif", GIF).await;
	harness.one("blank.txt", b" \n").await;

	let mut documents = harness.documents();
	documents.sort_by(|a, b| a.0.cmp(&b.0));
	let paths: Vec<String> = documents
		.iter()
		.map(|(path, _)| path.to_string_lossy().into_owned())
		.collect();
	assert_eq!(
		paths,
		[
			"Unsorted/2024/2024-03-14 Rent receipt.eml",
			"Unsorted/Undated/bank.csv",
			"Unsorted/Undated/blank.txt",
			"Unsorted/Undated/clinic.vcf",
			"Unsorted/Undated/dentist.ics",
			"Unsorted/Undated/drawing.pdf",
			"Unsorted/Undated/letter.rtf",
		]
	);
	let text = |index: usize| documents[index].1.pages[0].text.clone();
	assert!(text(0).contains("Paid."));
	assert_eq!(documents[0].1.pages[0].source, TextSource::Extracted);
	assert_eq!(text(1), "date,amount\n2024-03-14,1200");
	assert_eq!(documents[2].1.tags, ["no-text"]);
	assert!(documents[1].1.tags.is_empty());
	assert!(text(3).contains("FN:Sakura Clinic"));
	assert!(text(4).contains("SUMMARY:Dentist"));
	let gif = harness.app.library.library_dir().join(&documents[5].0);
	assert_eq!(
		violet_pdf::detect(&std::fs::read(gif).unwrap()),
		violet_pdf::Kind::Pdf
	);
	assert_eq!(text(6), "Dear neighbour,\nsee youé");
	assert!(harness.inbox().is_empty());
}

#[tokio::test]
async fn unsupported_formats_fail_and_stay_in_the_inbox() {
	let harness = Harness::new(Models::default());
	for (name, bytes, expected) in [
		(
			"report.doc",
			&b"\xd0\xcf\x11\xe0\xa1\xb1\x1a\xe1legacy"[..],
			"unsupported format .doc",
		),
		(
			"photo.heic",
			b"\0\0\0\x18ftypheic",
			"unsupported format .heic",
		),
		("scan.tif", b"II*\0rest", "image"),
		("blob", &[0, 1, 2, 3], "no extension and not a mail"),
		("broken.jpg", b"\xff\xd8\xff\xe0 truncated", "image"),
	] {
		let relative = harness.drop_file(name, bytes);
		let error = pipeline::ingest(&harness.app, &relative).await.unwrap_err();
		assert!(
			matches!(&error, Error::Invalid(message) if message.contains(expected)),
			"{name}: {error}"
		);
	}
	assert!(harness.documents().is_empty());
	assert!(harness.entries().is_empty());
	assert_eq!(
		harness.inbox(),
		["blob", "broken.jpg", "photo.heic", "report.doc", "scan.tif"]
	);
}
