//! Scores the real OCR and analysis calls against human ground truth, one page image at a time,
//! on the path the pipeline takes for an image dropped in the inbox: `image_to_pdf`, the page
//! rendered in tiles for `Client::ocr_page`, a thumbnail and the text for `Client::analyze`.
//!
//! `cargo run -p violet-model --example eval -- <base_url> <model> <samples_dir> <ground_truth.json> [dump_dir]`
//!
//! `ground_truth.json` is a list of `{"page", "documents", "document_type", "issuer", "date",
//! "total_amount", "currency", "key_text"}` (`key_text`: strings the OCR should find); the image of a page is `<samples_dir>/<page>.png`. With
//! `dump_dir`, the OCR text and the analysis of every page are written there.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::Value;
use violet_model::{
	AnalyzeInput, Client, ModelConfig, OCR_TILE_PIXELS, PageInput, Segment, parse_amount,
};

const THUMBNAIL_SIDE: u32 = 512;
const COMPANY_WORDS: [&str; 7] = [
	"株式会社",
	"合同会社",
	"有限会社",
	"(株)",
	"㈱",
	"inc",
	"ltd",
];

type Failure = Box<dyn std::error::Error>;

struct Read {
	ocr_keys: usize,
	ocr_chars: usize,
	truncated: bool,
	segments: Vec<Segment>,
	seconds: f32,
}

#[derive(Default)]
struct Totals {
	pages: u32,
	date: u32,
	issuer: u32,
	ocr_keys: usize,
	keys: usize,
	amount: u32,
	doc_type: u32,
	documents: u32,
	truncated: u32,
	failed: u32,
}

fn lower(text: &str) -> String {
	text.chars()
		.map(|c| match c {
			'\u{ff01}'..='\u{ff5e}' => char::from_u32(c as u32 - 0xfee0).unwrap_or(c),
			_ => c,
		})
		.flat_map(char::to_lowercase)
		.collect()
}

fn plain(text: &str) -> String {
	let mut plain = lower(text);
	plain.retain(char::is_alphanumeric);
	plain
}

fn fold(text: &str) -> String {
	let mut folded = lower(text);
	for word in COMPANY_WORDS {
		folded = folded.replace(word, "");
	}
	plain(&folded)
}

fn issuer_matches(got: Option<&str>, expected: Option<&str>) -> bool {
	let (got, expected) = (fold(got.unwrap_or("")), fold(expected.unwrap_or("")));
	if got.is_empty() || expected.is_empty() {
		return got == expected;
	}
	got.contains(&expected) || expected.contains(&got)
}

fn date_matches(got: Option<&str>, expected: Option<&str>) -> bool {
	match expected {
		Some(expected) => got.is_some_and(|got| got.starts_with(expected)),
		None => got.is_none(),
	}
}

fn expected_total(truth: &Value) -> Option<(i64, String)> {
	let currency = truth["currency"].as_str()?;
	let amount = parse_amount(&truth["total_amount"].as_f64()?.to_string(), Some(currency))?;
	Some((amount.minor, amount.currency))
}

fn total(segment: &Segment) -> Option<(i64, String)> {
	segment
		.facts
		.iter()
		.filter(|fact| fact.key == "total")
		.find_map(|fact| Some((fact.amount_minor?, fact.currency.clone()?)))
}

fn amount_matches(got: Option<&(i64, String)>, expected: Option<&(i64, String)>) -> bool {
	match expected {
		Some(expected) => got == Some(expected),
		None => got.is_none_or(|(minor, _)| *minor == 0),
	}
}

fn shown_amount(amount: Option<&(i64, String)>) -> String {
	amount.map_or("-".into(), |(minor, currency)| {
		format!("{minor} {currency}")
	})
}

fn mark(ok: bool) -> &'static str {
	if ok { "ok" } else { "**x**" }
}

async fn read_page(
	client: &Client,
	page: &str,
	image: &Path,
	keys: &[&str],
	dump: Option<&Path>,
) -> Result<Read, Failure> {
	let started = Instant::now();
	let pdf = violet_pdf::image_to_pdf(&std::fs::read(image)?)?;
	let transcription = client
		.ocr_page(&violet_pdf::render_tiles(&pdf, 1, OCR_TILE_PIXELS)?)
		.await?;
	let (text, truncated) = (transcription.text, transcription.truncated);
	let input = AnalyzeInput {
		file_name: format!("{page}.png"),
		pages: vec![PageInput {
			number: 1,
			text: text.clone(),
			thumbnail_png: Some(violet_pdf::render_png(&pdf, 1, THUMBNAIL_SIDE)?),
		}],
		known_areas: Vec::new(),
		known_issuers: Vec::new(),
		known_tags: Vec::new(),
	};
	let analysis = client.analyze(&input).await?;
	if let Some(dump) = dump {
		std::fs::write(dump.join(format!("{page}.txt")), &text)?;
		std::fs::write(
			dump.join(format!("{page}.json")),
			serde_json::to_string_pretty(&analysis)?,
		)?;
	}
	Ok(Read {
		ocr_keys: keys
			.iter()
			.filter(|key| plain(&text).contains(&plain(key)))
			.count(),
		ocr_chars: text.chars().count(),
		truncated,
		segments: analysis.segments,
		seconds: started.elapsed().as_secs_f32(),
	})
}

#[tokio::main]
async fn main() -> Result<(), Failure> {
	let args: Vec<String> = std::env::args().skip(1).collect();
	let [base_url, model, samples, truth, rest @ ..] = args.as_slice() else {
		return Err(
			"usage: eval <base_url> <model> <samples_dir> <ground_truth.json> [dump_dir]".into(),
		);
	};
	let dump = rest.first().map(PathBuf::from);
	if let Some(dump) = &dump {
		std::fs::create_dir_all(dump)?;
	}
	let truth: Vec<Value> = serde_json::from_slice(&std::fs::read(truth)?)?;
	let client = Client::new(ModelConfig {
		base_url: base_url.clone(),
		model: model.clone(),
		api_key: std::env::var("VIOLET_EVAL_API_KEY").ok(),
		timeout: Duration::from_secs(600),
	})?;

	println!(
		"| page | type (gt / got) | issuer | date (gt / got) | total (gt / got) | docs | conf | OCR keys | OCR chars | s |"
	);
	println!("|---|---|---|---|---|---|---|---|---|---|");
	let mut totals = Totals::default();
	for expected in &truth {
		let Some(page) = expected["page"].as_str() else {
			continue;
		};
		totals.pages += 1;
		let image = Path::new(samples).join(format!("{page}.png"));
		let keys: Vec<&str> = expected["key_text"]
			.as_array()
			.map(|keys| keys.iter().filter_map(Value::as_str).collect())
			.unwrap_or_default();
		totals.keys += keys.len();
		let read = match read_page(&client, page, &image, &keys, dump.as_deref()).await {
			Ok(read) => read,
			Err(error) => {
				totals.failed += 1;
				let error = error.to_string().replace(['|', '\n'], " ");
				println!("| {page} | failed: {error} | | | | | | | | |");
				continue;
			}
		};
		let first = read.segments.first();
		let doc_type = first.map(|segment| segment.doc_type.as_str());
		let issuer = first.and_then(|segment| segment.issuer.as_deref());
		let date = first.and_then(|segment| segment.date.as_deref());
		let amount = first.and_then(total);
		let wanted_amount = expected_total(expected);
		let wanted_type = expected["document_type"].as_str();
		let wanted_date = expected["date"].as_str();
		let wanted_documents = expected["documents"].as_u64().unwrap_or(1);

		let type_ok = doc_type
			.map(|doc_type| doc_type.replace(' ', "_"))
			.as_deref()
			== wanted_type;
		let issuer_ok = issuer_matches(issuer, expected["issuer"].as_str());
		let date_ok = date_matches(date, wanted_date);
		let amount_ok = amount_matches(amount.as_ref(), wanted_amount.as_ref());
		let documents_ok = read.segments.len() as u64 == wanted_documents;
		totals.doc_type += u32::from(type_ok);
		totals.issuer += u32::from(issuer_ok);
		totals.date += u32::from(date_ok);
		totals.amount += u32::from(amount_ok);
		totals.documents += u32::from(documents_ok);
		totals.truncated += u32::from(read.truncated);
		totals.ocr_keys += read.ocr_keys;

		println!(
			"| {page} | {} / {} {} | {} {} | {} / {} {} | {} / {} {} | {} / {} | {:.2} | {}/{} | {}{} | {:.1} |",
			wanted_type.unwrap_or("-"),
			doc_type.unwrap_or("-"),
			mark(type_ok),
			mark(issuer_ok),
			issuer.unwrap_or("-").replace('|', "/"),
			wanted_date.unwrap_or("-"),
			date.unwrap_or("-"),
			mark(date_ok),
			shown_amount(wanted_amount.as_ref()),
			shown_amount(amount.as_ref()),
			mark(amount_ok),
			wanted_documents,
			read.segments.len(),
			first.map_or(0.0, |segment| segment.confidence),
			read.ocr_keys,
			keys.len(),
			read.ocr_chars,
			if read.truncated { " (cut)" } else { "" },
			read.seconds,
		);
	}
	let n = totals.pages;
	println!(
		"\n**{n} pages**: date {}/{n}, issuer {}/{n}, total+currency {}/{n}, type {}/{n}, document count {}/{n}, OCR key strings {}/{}, OCR cut off {}/{n}, failed {}/{n}",
		totals.date,
		totals.issuer,
		totals.amount,
		totals.doc_type,
		totals.documents,
		totals.ocr_keys,
		totals.keys,
		totals.truncated,
		totals.failed
	);
	Ok(())
}
