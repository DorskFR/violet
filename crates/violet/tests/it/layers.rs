use serde_json::Value;
use violet::{Models, pipeline};
use violet_core::TextSource;

use crate::common::{Fake, Harness, analysis, is_ocr, segment, text_pdf, user_text};

const GOOD: &str = "Invoice for March 2024, the total amount due is 133 euros, payable by transfer";
const GARBLED: &str = "w+hwlnffi6r]xffioHR H oa-oo ffifr H Y6+tr+F zOzt.tt.Ot#ltl il'tfrF:frffitt;EFtt \
	Ltf Iijrljilti:tirtq ili4AR7t,qffiffiffiH1f+ft I TE I 4# 5 6 ilr4gRtEfrZf;ffiElEa 1 TE 1 4 - 5";
/// Reads as text, but not well enough for a page of a Japanese document.
const BORDERLINE: &str = "Service contract tvlt for the subscriber Hffix rtfr the provider \
	agrees lltt to deliver the service every month";
const OCR: &str = "ご契約内容のお知らせ 月額料金 5,280円";

/// Reads every page as `ocr` and files the pages it is asked about as one document in
/// `language`.
async fn fake(ocr: &'static str, language: &'static str) -> Fake {
	Fake::start(move |body: &Value| {
		if is_ocr(body) {
			return (200, ocr.into());
		}
		let pages: Vec<u32> = user_text(body)
			.lines()
			.find_map(|line| line.strip_prefix("Pages: "))
			.unwrap()
			.split(", ")
			.map(|page| page.parse().unwrap())
			.collect();
		let mut segment = segment(&pages, "Contract", "Utilities", "KDDI", "2024-03-01");
		segment["language"] = language.into();
		analysis(vec![segment], &[])
	})
	.await
}

fn sources(harness: &Harness, id: uuid::Uuid) -> Vec<(TextSource, String)> {
	let (_, sidecar) = harness.app.index().get(id).unwrap().unwrap();
	sidecar
		.pages
		.into_iter()
		.map(|page| (page.source, page.text))
		.collect()
}

#[tokio::test]
async fn a_garbled_text_layer_is_read_by_ocr() {
	let fake = fake(OCR, "en").await;
	let harness = Harness::new(fake.models());
	let id = harness
		.one("contract.pdf", &text_pdf(&[GOOD, GARBLED]))
		.await;
	assert_eq!(
		sources(&harness, id),
		[
			(TextSource::Layer, GOOD.to_string()),
			(TextSource::Ocr, OCR.to_string())
		]
	);
	assert_eq!(fake.ocr_requests(), 1);
	let prompt = fake.analysis_prompts().pop().unwrap();
	assert!(
		prompt.contains(OCR) && !prompt.contains("ffioHR"),
		"{prompt}"
	);
}

#[tokio::test]
async fn a_garbled_layer_is_kept_when_nothing_reads_it_better() {
	let harness = Harness::new(Models::default());
	let id = harness.one("contract.pdf", &text_pdf(&[GARBLED])).await;
	assert_eq!(
		sources(&harness, id),
		[(TextSource::Layer, GARBLED.to_string())]
	);

	let fake = fake("[BLANK]", "en").await;
	let harness = Harness::new(fake.models());
	let id = harness.one("contract.pdf", &text_pdf(&[GARBLED])).await;
	assert_eq!(
		sources(&harness, id),
		[(TextSource::Layer, GARBLED.to_string())]
	);
	assert_eq!(fake.ocr_requests(), 1);
}

#[tokio::test]
async fn reread_judges_the_layer_by_the_document_language() {
	let fake = fake(OCR, "ja").await;
	let harness = Harness::new(fake.models());
	let id = harness
		.one("contract.pdf", &text_pdf(&[BORDERLINE, GOOD]))
		.await;
	assert_eq!(
		sources(&harness, id),
		[
			(TextSource::Layer, BORDERLINE.to_string()),
			(TextSource::Layer, GOOD.to_string())
		]
	);
	assert_eq!(fake.ocr_requests(), 0);

	pipeline::reread(&harness.app, id).await.unwrap();
	assert_eq!(
		sources(&harness, id),
		[
			(TextSource::Ocr, OCR.to_string()),
			(TextSource::Layer, GOOD.to_string())
		]
	);
	assert_eq!(fake.ocr_requests(), 1);
}

/// A TIFF of 8-bit gray pages, one strip each, little-endian.
fn tiff(pages: &[(u32, u32)]) -> Vec<u8> {
	let mut out = b"II*\0\0\0\0\0".to_vec();
	let mut previous_next = 4;
	for &(width, height) in pages {
		let strip = out.len() as u32;
		out.extend(std::iter::repeat_n(255u8, (width * height) as usize));
		if out.len() % 2 == 1 {
			out.push(0);
		}
		let directory = out.len() as u32;
		out[previous_next..previous_next + 4].copy_from_slice(&directory.to_le_bytes());
		let entries: [(u16, u16, u32); 9] = [
			(256, 4, width),
			(257, 4, height),
			(258, 3, 8),
			(259, 3, 1),
			(262, 3, 1),
			(273, 4, strip),
			(277, 3, 1),
			(278, 4, height),
			(279, 4, width * height),
		];
		out.extend((entries.len() as u16).to_le_bytes());
		for (tag, kind, value) in entries {
			out.extend(tag.to_le_bytes());
			out.extend(kind.to_le_bytes());
			out.extend(1u32.to_le_bytes());
			out.extend(value.to_le_bytes());
		}
		previous_next = out.len();
		out.extend(0u32.to_le_bytes());
	}
	out
}

#[tokio::test]
async fn each_page_of_a_tiff_becomes_a_page() {
	let fake = fake(OCR, "ja").await;
	let harness = Harness::new(fake.models());
	let id = harness
		.one("scan.tiff", &tiff(&[(80, 60), (90, 60), (100, 50)]))
		.await;
	let (path, sidecar) = harness.app.index().get(id).unwrap().unwrap();
	assert_eq!(path.extension().unwrap(), "pdf");
	assert_eq!(sidecar.pages.len(), 3);
	assert!(
		sidecar
			.pages
			.iter()
			.all(|page| page.source == TextSource::Ocr)
	);
	let filed = std::fs::read(harness.app.library.library_dir().join(path)).unwrap();
	assert_eq!(violet_pdf::page_count(&filed).unwrap(), 3);
	assert_eq!(fake.ocr_requests(), 3);
}
