use std::collections::HashSet;

use serde::{Deserialize, Deserializer};
use serde_json::Value;
use violet_core::{DatePrecision, Fact};

use crate::amount::{currencies_in, currency_fits, decimals, normalize_currency, parse_amount};
use crate::chat::{Message, Part, Request, Schema};
use crate::date::{Stated, day_and_month, only_a_year, read_date_in, read_stated};
use crate::grounding::{TOPIC_TAGS, TagGrounds, title_fits_text, title_from_text};
use crate::issuer::{acronym_printed, canonical_issuer, distinct_issuers};
use crate::text::{
	collapse_whitespace, extract_json, fold_name, fold_plain, fold_width, truncate_head,
	truncate_middle,
};
use crate::{Analysis, AnalyzeInput, Client, Error, Limits, PageInput, Result, Rotation, Segment};

const ANALYZE_PROMPT: &str = include_str!("prompts/analyze.txt");
const BOUNDARY_PROMPT: &str = include_str!("prompts/boundary.txt");
const ANALYZE_MAX_TOKENS: u32 = 4096;
const BOUNDARY_MAX_TOKENS: u32 = 1024;
const MIN_PAGE_CHARS: usize = 200;
const KNOWN_VALUES: usize = 60;
const KNOWN_CHARS: usize = 900;
const ECHOED_ANSWER_CHARS: usize = 3000;
const MAX_TITLE_CHARS: usize = 120;
const MAX_LABEL_CHARS: usize = 32;
const MAX_TAGS: usize = 8;
const MAX_FACTS: usize = 40;
const REPAIRED_CONFIDENCE: f32 = 0.5;
const DEFAULT_CONFIDENCE: f32 = 0.5;
const UNPRINTED_ISSUER_CONFIDENCE: f32 = 0.7;
const TOTAL_KEY: &str = "total";
/// The issuers in the prompt's example answers, which a model sometimes repeats.
const EXAMPLE_ISSUERS: [&str; 2] = ["株式会社サンプル", "Mairie de Valmont"];
/// A title in the prompt's example answers that names no other document, so that a model
/// repeating it is caught.
const EXAMPLE_TITLE: &str = "Vide-grenier du quartier";
const TOPICS_PLACEHOLDER: &str = "{topics}";
const LOGGED_ANSWER_CHARS: usize = 2000;
const MIN_DATE_CHARS: usize = 6;
const MAX_DATE_CHARS: usize = 20;
const DATE_STARTS: &str = "令平昭大明〇一二三四五六七八九";
const PERIOD_MARKS: [&str; 8] = ["以降", "以前", "から", "まで", "より", "～", "〜", "~"];
const OTHER_DATE_LABELS: [&str; 8] = [
	"期間",
	"期限",
	"生年月日",
	"始期日",
	"始期",
	"満期日",
	"満了日",
	"有効",
];
const SCHEMA_MAX_FACTS: usize = 16;
const SCHEMA_MAX_TAGS: usize = 5;
const MIN_AMOUNT_DIGITS: usize = 3;
const ADDRESSEE_SUFFIXES: [&str; 4] = ["様", "殿", "御中", "さま"];
const ADDRESSEE_PREFIXES: [&str; 2] = ["to:", "dear "];
const FALLBACK_AREA: &str = "Unsorted";
const FALLBACK_DOC_TYPE: &str = "document";
const TITLE_SEPARATORS: [char; 12] = [
	' ', '-', '–', '—', '_', ':', '：', '・', '/', '|', '、', ',',
];

pub(crate) async fn analyze(client: &Client, input: &AnalyzeInput) -> Result<Analysis> {
	let limits = &client.limits;
	let mut analysis = Analysis {
		segments: Vec::new(),
		blank_pages: Vec::new(),
	};
	if input.pages.is_empty() {
		return Ok(analysis);
	}
	let text_chars: usize = input
		.pages
		.iter()
		.map(|page| page.text.chars().count().min(limits.page_chars))
		.sum();
	let windowed = text_chars > limits.text_budget_chars && !input.one_document;
	let chunks = if windowed {
		split_documents(client, &input.pages).await?
	} else {
		vec![&input.pages[..]]
	};
	let mut orphans: Vec<u32> = Vec::new();
	for chunk in chunks {
		match analyze_chunk(client, input, chunk, windowed).await? {
			Chunk::Settled(part) => {
				let mut segments = part.segments;
				if let Some(first) = segments.first_mut().filter(|_| !orphans.is_empty()) {
					first.pages.splice(0..0, orphans.drain(..));
					first.confidence *= REPAIRED_CONFIDENCE;
				}
				analysis.segments.extend(segments);
				analysis.blank_pages.extend(part.blank_pages);
			}
			Chunk::Degenerate(complaint) if windowed => {
				let pages = chunk.iter().map(|page| page.number);
				tracing::warn!(
					file = input.file_name,
					first = chunk[0].number,
					problems = complaint,
					"no document in the answer for these pages, joining them to a neighbour"
				);
				match analysis.segments.last_mut() {
					Some(last) => {
						last.pages.extend(pages);
						last.confidence *= REPAIRED_CONFIDENCE;
					}
					None => orphans.extend(pages),
				}
			}
			Chunk::Degenerate(complaint) => return Err(no_document(&complaint)),
		}
	}
	if !orphans.is_empty() {
		return Err(no_document(
			"no part of the file was answered with a document",
		));
	}
	analysis.blank_pages.sort_unstable();
	Ok(analysis)
}

fn no_document(complaint: &str) -> Error {
	Error::Answer(format!(
		"the analysis put no page in a document after a retry:\n{complaint}"
	))
}

enum Chunk {
	Settled(Analysis),
	/// The model kept no document although the pages have text or are a photo.
	Degenerate(String),
}

fn boundary_schema() -> Schema {
	Schema::Object(vec![("new_document", Schema::Boolean)])
}

/// Cuts the file where a page starts a new document, judged on pairs of adjacent pages.
/// Pages without text stay with the pages before them.
async fn split_documents<'a>(
	client: &Client,
	pages: &'a [PageInput],
) -> Result<Vec<&'a [PageInput]>> {
	let mut starts = vec![0];
	let mut previous: Option<&PageInput> = None;
	for (index, page) in pages.iter().enumerate() {
		if page.text.trim().is_empty() {
			continue;
		}
		if let Some(previous) = previous
			&& starts_new_document(client, previous, page).await?
		{
			starts.push(index);
		}
		previous = Some(page);
	}
	starts.push(pages.len());
	Ok(starts
		.windows(2)
		.map(|bounds| &pages[bounds[0]..bounds[1]])
		.collect())
}

async fn starts_new_document(
	client: &Client,
	previous: &PageInput,
	next: &PageInput,
) -> Result<bool> {
	let chars = client.limits.boundary_page_chars;
	let user = format!(
		"<page n=\"{}\">\n{}\n</page>\n<page n=\"{}\">\n{}\n</page>\n\nDoes page {} start a new document?",
		previous.number,
		truncate_middle(previous.text.trim(), chars),
		next.number,
		truncate_head(next.text.trim(), chars),
		next.number,
	);
	let messages = [
		Message::text("system", BOUNDARY_PROMPT.trim()),
		Message::text("user", user),
	];
	let schema = boundary_schema();
	let request = Request {
		messages: &messages,
		schema: Some(("boundary", &schema)),
		max_tokens: BOUNDARY_MAX_TOKENS,
		images_optional: true,
		penalize_repetition: false,
	};
	let answer = client.complete(&request).await?.text;
	let decision = extract_json(&answer).and_then(|value| value["new_document"].as_bool());
	if decision.is_none() {
		tracing::warn!(
			page = next.number,
			answer,
			"unusable boundary answer, keeping pages together"
		);
	}
	Ok(decision.unwrap_or(false))
}

fn analysis_schema(require_segment: bool) -> Schema {
	let rotation = Schema::Object(vec![
		("page", Schema::Integer),
		("degrees", Schema::IntegerIn(&[90, 180, 270])),
	]);
	let fact = Schema::Object(vec![
		("key", Schema::String),
		("value", Schema::String),
		("amount", Schema::NullableNumber),
		("currency", Schema::NullableString),
	]);
	let segment = Schema::Object(vec![
		("pages", Schema::array(Schema::Integer)),
		("rotations", Schema::array(rotation)),
		("title", Schema::String),
		("area", Schema::String),
		("doc_type", Schema::String),
		("issuer", Schema::NullableString),
		("date_printed", Schema::NullableString),
		("total_printed", Schema::NullableString),
		("currency", Schema::NullableString),
		("language", Schema::NullableString),
		("summary", Schema::NullableString),
		("tags", Schema::array_up_to(Schema::String, SCHEMA_MAX_TAGS)),
		("facts", Schema::array_up_to(fact, SCHEMA_MAX_FACTS)),
		("confidence", Schema::Number),
	]);
	Schema::Object(vec![
		(
			"segments",
			if require_segment {
				Schema::array_at_least(segment, 1)
			} else {
				Schema::array(segment)
			},
		),
		("blank_pages", Schema::array(Schema::Integer)),
	])
}

struct Scope<'a> {
	input: &'a AnalyzeInput,
	pages: Vec<u32>,
	/// The answer must hold at least one segment: some page has text, or the pages are a photo.
	require_segment: bool,
	/// Text of each page, full-width forms folded and whitespace collapsed, to check that what
	/// the model reports is printed there.
	texts: Vec<(u32, String)>,
}

async fn analyze_chunk(
	client: &Client,
	input: &AnalyzeInput,
	chunk: &[PageInput],
	windowed: bool,
) -> Result<Chunk> {
	let scope = Scope::new(input, chunk);
	let schema = analysis_schema(scope.require_segment);
	let mut messages = vec![
		Message::text("system", analyze_prompt()),
		Message {
			role: "user",
			parts: chunk_parts(input, chunk, &client.limits, windowed, input.one_document),
		},
	];
	let mut repaired = None;
	let mut degenerate = false;
	let mut unconfirmed = None;
	let mut complaint = String::new();
	for attempt in 0..2 {
		let request = Request {
			messages: &messages,
			schema: Some(("analysis", &schema)),
			max_tokens: ANALYZE_MAX_TOKENS,
			images_optional: true,
			penalize_repetition: false,
		};
		let completion = client.complete(&request).await?;
		let answer = completion.text;
		let problems = match parse(&answer) {
			Err(_) if completion.truncated => vec![
				"the answer was cut off before its end; keep only the most useful facts and a short summary".into(),
			],
			Ok(raw) => {
				let settled = settle(raw, &scope);
				let valid = settled.problems.is_empty();
				if valid && (settled.doubts.is_empty() || attempt > 0) {
					return Ok(Chunk::Settled(settled.analysis));
				}
				if valid {
					unconfirmed = Some(settled.analysis);
				} else {
					repaired = Some(settled.analysis);
					degenerate = settled.degenerate;
				}
				settled.problems.into_iter().chain(settled.doubts).collect()
			}
			Err(problem) => vec![problem],
		};
		complaint = problems
			.iter()
			.map(|problem| format!("- {problem}\n"))
			.collect();
		tracing::warn!(
			file = input.file_name,
			attempt,
			problems = complaint,
			answer = truncate_head(&answer, LOGGED_ANSWER_CHARS),
			"invalid analysis"
		);
		if degenerate {
			messages[1].parts = chunk_parts(input, chunk, &client.limits, windowed, true);
		}
		messages.push(Message::text(
			"assistant",
			truncate_head(&answer, ECHOED_ANSWER_CHARS),
		));
		messages.push(Message::text(
			"user",
			format!(
				"That answer is invalid:\n{complaint}Answer again with the complete corrected JSON object only."
			),
		));
	}
	if let Some(analysis) = unconfirmed {
		return Ok(Chunk::Settled(analysis));
	}
	if degenerate {
		return Ok(Chunk::Degenerate(complaint));
	}
	let mut analysis = repaired
		.ok_or_else(|| Error::Answer(format!("no usable analysis after a retry:\n{complaint}")))?;
	for segment in &mut analysis.segments {
		segment.confidence *= REPAIRED_CONFIDENCE;
	}
	Ok(Chunk::Settled(analysis))
}

fn analyze_prompt() -> String {
	ANALYZE_PROMPT
		.trim()
		.replace(TOPICS_PLACEHOLDER, &TOPIC_TAGS.join(", "))
}

fn chunk_parts(
	input: &AnalyzeInput,
	chunk: &[PageInput],
	limits: &Limits,
	windowed: bool,
	one_document: bool,
) -> Vec<Part> {
	let longest = chunk
		.iter()
		.map(|page| page.text.chars().count())
		.max()
		.unwrap_or(0);
	let total: usize = chunk
		.iter()
		.map(|page| page.text.chars().count().min(limits.page_chars))
		.sum();
	let least = if one_document { 0 } else { MIN_PAGE_CHARS };
	let page_chars = if total > limits.text_budget_chars {
		(limits.text_budget_chars / chunk.len())
			.max(least)
			.min(limits.page_chars)
	} else {
		limits.page_chars
	};
	let all_text = fold_name(
		&chunk
			.iter()
			.map(|page| page.text.as_str())
			.collect::<String>(),
	);
	let numbers: Vec<String> = chunk.iter().map(|page| page.number.to_string()).collect();

	let mut text = format!("File: {}\n", input.file_name);
	let issuers = distinct_issuers(&input.known_issuers);
	for (label, values) in [
		("Known areas", &input.known_areas),
		("Known issuers", &issuers),
		("Known tags", &input.known_tags),
	] {
		if let Some(line) = known_line(values, &all_text) {
			text.push_str(&format!("{label}: {line}\n"));
		}
	}
	text.push_str(&format!("Pages: {}\n", numbers.join(", ")));
	if windowed {
		text.push_str("These pages were grouped as probably one document.\n");
	}
	if input.photo {
		text.push_str(
			"No text could be read from these pages: they are a photo or a picture. Judge them from the image alone. Answer with exactly one segment for all of them, never blank_pages. Use doc_type \"photo\" unless the image shows another kind of document, a short English title that names what it shows, and a summary of one English sentence describing it.\n",
		);
	} else if one_document {
		text.push_str(
			"These pages are one document: answer with exactly one segment for all of them.\n",
		);
	}
	if longest > page_chars {
		text.push_str("[...] marks text left out of a long page.\n");
	}
	for page in chunk {
		text.push_str(&format!(
			"\n<page n=\"{}\">\n{}\n</page>\n",
			page.number,
			truncate_middle(page.text.trim(), page_chars)
		));
	}

	let thumbnails: Vec<(u32, &[u8])> = chunk
		.iter()
		.filter_map(|page| Some((page.number, page.thumbnail_png.as_deref()?)))
		.collect();
	let shown = if !windowed && thumbnails.len() <= limits.max_thumbnails {
		thumbnails.len()
	} else {
		limits.max_thumbnails.min(1)
	};
	let mut parts = vec![Part::Text(text)];
	for (number, png) in thumbnails.into_iter().take(shown) {
		parts.push(Part::Text(format!("Image of page {number}:")));
		parts.push(Part::png(png));
	}
	parts
}

/// Known values for the prompt, those that occur in the page text first, capped in number
/// and length.
fn known_line(values: &[String], folded_text: &str) -> Option<String> {
	let (present, absent): (Vec<&String>, Vec<&String>) = values
		.iter()
		.filter(|value| !value.trim().is_empty())
		.partition(|value| folded_text.contains(&fold_name(value)));
	let mut line = String::new();
	for value in present.into_iter().chain(absent).take(KNOWN_VALUES) {
		if line.chars().count() + value.chars().count() > KNOWN_CHARS {
			break;
		}
		if !line.is_empty() {
			line.push_str("; ");
		}
		line.push_str(value.trim());
	}
	(!line.is_empty()).then_some(line)
}

fn null_as_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
	D: Deserializer<'de>,
	T: Default + Deserialize<'de>,
{
	Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawAnalysis {
	#[serde(deserialize_with = "null_as_default")]
	segments: Vec<RawSegment>,
	#[serde(deserialize_with = "null_as_default")]
	blank_pages: Vec<i64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawSegment {
	#[serde(deserialize_with = "null_as_default")]
	pages: Vec<i64>,
	#[serde(deserialize_with = "null_as_default")]
	rotations: Vec<RawRotation>,
	title: Option<String>,
	area: Option<String>,
	doc_type: Option<String>,
	issuer: Option<String>,
	date_printed: Option<String>,
	total_printed: Option<String>,
	currency: Option<String>,
	language: Option<String>,
	summary: Option<String>,
	#[serde(deserialize_with = "null_as_default")]
	tags: Vec<String>,
	#[serde(deserialize_with = "null_as_default")]
	facts: Vec<RawFact>,
	confidence: Option<f64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawRotation {
	page: i64,
	degrees: i64,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawFact {
	key: Option<String>,
	value: Value,
	amount: Value,
	currency: Option<String>,
}

fn parse(answer: &str) -> Result<RawAnalysis, String> {
	let value = extract_json(answer).ok_or("the answer contains no JSON object")?;
	if value.get("segments").is_none() {
		return Err("the JSON object has no \"segments\" array".into());
	}
	serde_json::from_value(value).map_err(|error| format!("the JSON has the wrong shape: {error}"))
}

struct Settled {
	/// Already repaired: broken values are dropped or replaced and unassigned pages join the
	/// document before them.
	analysis: Analysis,
	/// Every rule the answer broke.
	problems: Vec<String>,
	/// What is worth asking the model once more about, but is kept if it insists.
	doubts: Vec<String>,
	/// No segment came from the model although one was needed: any segment in `analysis` is
	/// a stand-in named after the file.
	degenerate: bool,
}

/// Checks the model's answer against the pages it was given.
fn settle(raw: RawAnalysis, scope: &Scope<'_>) -> Settled {
	let mut problems = Vec::new();
	let mut doubts = Vec::new();
	let mut assigned = HashSet::new();
	let mut segments = Vec::new();
	for (index, raw_segment) in raw.segments.into_iter().enumerate() {
		let label = index + 1;
		let mut pages = Vec::new();
		for &page in &raw_segment.pages {
			match scope.page(page) {
				None => problems.push(format!(
					"segment {label}: page {page} is not one of the listed pages"
				)),
				Some(page) if !assigned.insert(page) => {
					problems.push(format!("page {page} is in more than one place"));
				}
				Some(page) => pages.push(page),
			}
		}
		if pages.is_empty() {
			problems.push(format!("segment {label} has no valid pages"));
			continue;
		}
		segments.push(settle_segment(
			raw_segment,
			pages,
			label,
			scope,
			&mut problems,
			&mut doubts,
		));
	}

	let answered = !segments.is_empty();
	if !answered && scope.require_segment {
		problems.push(if scope.input.photo {
			"segments is empty: the image is the document; answer with one segment for it".into()
		} else {
			"segments is empty but the pages have text: they are at least one document; answer with one segment for all of them unless they are clearly separate documents".into()
		});
	}

	let mut blank_pages = Vec::new();
	for page in raw.blank_pages {
		match scope.page(page) {
			None => problems.push(format!(
				"blank_pages: page {page} is not one of the listed pages"
			)),
			Some(page) if blank_pages.contains(&page) => {}
			Some(page) if scope.input.photo => problems.push(format!(
				"page {page} is in blank_pages but it is the photo; put it in a segment"
			)),
			Some(page) if scope.has_text(page) => problems.push(format!(
				"page {page} is in blank_pages but has text; put it in a segment"
			)),
			Some(page) if assigned.contains(&page) => {
				problems.push(format!(
					"page {page} is both in a segment and in blank_pages"
				));
			}
			Some(page) => blank_pages.push(page),
		}
	}
	blank_pages.sort_unstable();

	let missing: Vec<u32> = scope
		.pages
		.iter()
		.copied()
		.filter(|page| !assigned.contains(page) && !blank_pages.contains(page))
		.collect();
	if !missing.is_empty() {
		let list: Vec<String> = missing.iter().map(u32::to_string).collect();
		problems.push(format!(
			"pages missing from both segments and blank_pages: {}",
			list.join(", ")
		));
	}
	let degenerate = !answered && (scope.require_segment || !missing.is_empty());
	for page in missing {
		adopt(&mut segments, page, scope);
	}
	Settled {
		analysis: Analysis {
			segments,
			blank_pages,
		},
		problems,
		doubts,
		degenerate,
	}
}

/// Puts an unassigned page right after the closest earlier page that has a document, or
/// before the closest later one, or in a document of its own when there is none.
fn adopt(segments: &mut Vec<Segment>, page: u32, scope: &Scope<'_>) {
	let position = scope
		.pages
		.iter()
		.position(|&candidate| candidate == page)
		.unwrap_or(0);
	let locate = |segments: &[Segment], neighbour: u32| {
		segments
			.iter()
			.enumerate()
			.find_map(|(segment, candidate)| {
				let offset = candidate.pages.iter().position(|&p| p == neighbour)?;
				Some((segment, offset))
			})
	};
	let earlier = scope.pages[..position]
		.iter()
		.rev()
		.find_map(|&p| locate(segments, p));
	if let Some((segment, offset)) = earlier {
		segments[segment].pages.insert(offset + 1, page);
		return;
	}
	let later = scope.pages[position + 1..]
		.iter()
		.find_map(|&p| locate(segments, p));
	if let Some((segment, offset)) = later {
		segments[segment].pages.insert(offset, page);
		return;
	}
	segments.push(Segment {
		pages: vec![page],
		rotations: Vec::new(),
		title: scope.fallback_title(),
		area: FALLBACK_AREA.into(),
		doc_type: FALLBACK_DOC_TYPE.into(),
		issuer: None,
		date: None,
		date_precision: DatePrecision::Day,
		language: None,
		summary: None,
		tags: Vec::new(),
		facts: Vec::new(),
		confidence: 0.0,
	});
}

impl<'a> Scope<'a> {
	fn new(input: &'a AnalyzeInput, chunk: &[PageInput]) -> Self {
		Self {
			input,
			pages: chunk.iter().map(|page| page.number).collect(),
			require_segment: input.photo || chunk.iter().any(|page| !page.text.trim().is_empty()),
			texts: chunk
				.iter()
				.map(|page| (page.number, collapse_whitespace(&fold_width(&page.text))))
				.collect(),
		}
	}

	fn has_text(&self, page: u32) -> bool {
		self.texts
			.iter()
			.any(|(number, text)| *number == page && !text.trim().is_empty())
	}

	fn text_of(&self, pages: &[u32]) -> String {
		self.texts
			.iter()
			.filter(|(number, _)| pages.contains(number))
			.map(|(_, text)| text.as_str())
			.collect::<Vec<_>>()
			.join("\n")
	}

	/// The text of the pages with its lines as given.
	fn raw_text_of(&self, pages: &[u32]) -> String {
		self.input
			.pages
			.iter()
			.filter(|page| pages.contains(&page.number))
			.map(|page| page.text.as_str())
			.collect::<Vec<_>>()
			.join("\n")
	}

	fn page(&self, number: i64) -> Option<u32> {
		u32::try_from(number)
			.ok()
			.filter(|page| self.pages.contains(page))
	}

	fn fallback_title(&self) -> String {
		let name = self.input.file_name.trim();
		let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem).trim();
		if stem.is_empty() {
			"Untitled".into()
		} else {
			stem.into()
		}
	}
}

fn settle_segment(
	raw: RawSegment,
	pages: Vec<u32>,
	label: usize,
	scope: &Scope<'_>,
	problems: &mut Vec<String>,
	doubts: &mut Vec<String>,
) -> Segment {
	let input = scope.input;
	let mut rotations: Vec<Rotation> = Vec::new();
	for rotation in raw.rotations {
		let page = u32::try_from(rotation.page)
			.ok()
			.filter(|page| pages.contains(page));
		let degrees = rotation.degrees.rem_euclid(360) as i32;
		match page {
			None => problems.push(format!(
				"segment {label}: rotation for page {}, which is not in this segment",
				rotation.page
			)),
			Some(_) if degrees % 90 != 0 => problems.push(format!(
				"segment {label}: rotation of {} degrees is not 90, 180 or 270",
				rotation.degrees
			)),
			Some(page) if degrees != 0 && rotations.iter().all(|kept| kept.page != page) => {
				rotations.push(Rotation { page, degrees });
			}
			Some(_) => {}
		}
	}

	let spaced = scope.text_of(&pages);
	let printed = fold_name(&spaced);

	let plain = fold_plain(&spaced);
	let stated_issuer = raw
		.issuer
		.map(|issuer| collapse_whitespace(&issuer))
		.filter(|issuer| !issuer.is_empty() && !is_null_word(issuer))
		.filter(|issuer| !is_addressee(issuer) && !is_address(issuer));
	let canonical = stated_issuer
		.as_deref()
		.and_then(|issuer| canonical_issuer(issuer, &input.known_issuers));
	let issuer_printed = stated_issuer.as_deref().is_none_or(|issuer| {
		is_printed_plain(issuer, &plain)
			|| canonical
				.as_deref()
				.is_some_and(|known| is_printed_plain(known, &plain))
			|| (canonical.is_some() && acronym_printed(issuer, &spaced))
	});
	let stated_issuer = stated_issuer.filter(|issuer| {
		issuer_printed
			|| (canonical.is_none()
				&& EXAMPLE_ISSUERS
					.iter()
					.all(|example| fold_name(issuer) != fold_name(example)))
	});
	if let Some(issuer) = stated_issuer
		.as_deref()
		.filter(|_| !issuer_printed && canonical.is_none())
	{
		doubts.push(format!(
			"segment {label}: issuer \"{issuer}\" is not in the page text; copy the name of the sender exactly as printed, or use null when the page does not name it"
		));
	}
	let issuer = stated_issuer.map(|issuer| canonical.unwrap_or(issuer));

	let language = raw.language.as_deref().and_then(settle_language);
	let short_years = !language
		.as_deref()
		.is_some_and(|language| YEAR_FIRST_LANGUAGES.contains(&language));
	let (date, date_precision) = raw
		.date_printed
		.filter(|date| !date.trim().is_empty() && !is_null_word(date))
		.and_then(|stated| {
			let texts = DateTexts {
				spaced: &spaced,
				plain: &plain,
				short_years,
			};
			settle_date(&stated, &texts, label, problems)
		})
		.map_or((None, DatePrecision::Day), |(date, precision)| {
			(Some(date), precision)
		});

	let doc_type = match raw.doc_type.as_deref().and_then(settle_doc_type) {
		Some(doc_type) => doc_type,
		None => {
			problems.push(format!(
				"segment {label}: doc_type must be a short lowercase English word, got {:?}",
				raw.doc_type.unwrap_or_default()
			));
			FALLBACK_DOC_TYPE.into()
		}
	};

	let area = match raw
		.area
		.as_deref()
		.and_then(|area| settle_area(area, &input.known_areas))
	{
		Some(area) => area,
		None => {
			problems.push(format!(
				"segment {label}: area must be a known area or a short English Title-case noun, got {:?}",
				raw.area.unwrap_or_default()
			));
			FALLBACK_AREA.into()
		}
	};

	let title = raw
		.title
		.map(|title| collapse_whitespace(&title))
		.unwrap_or_default();
	let title = if title.is_empty() {
		problems.push(format!("segment {label}: title is empty"));
		scope.fallback_title()
	} else {
		let bare = bare_title(&title, issuer.as_deref(), date.as_deref());
		let bare = truncate_head(&bare, MAX_TITLE_CHARS).trim_end();
		if fold_name(bare) == fold_name(EXAMPLE_TITLE) && !is_printed(bare, &printed) {
			problems.push(format!(
				"segment {label}: title \"{bare}\" is the title of the example, not of this document; name this document from its own page text"
			));
			title_from_text(&scope.raw_text_of(&pages)).unwrap_or_else(|| scope.fallback_title())
		} else if title_fits_text(bare, &spaced) {
			bare.to_string()
		} else {
			problems.push(format!(
				"segment {label}: title \"{bare}\" is not in the language of the page text; write the title in the same language and script as the page, never translated"
			));
			title_from_text(&scope.raw_text_of(&pages)).unwrap_or_else(|| scope.fallback_title())
		}
	};

	let periodic =
		date_precision == DatePrecision::Month || PERIODIC_TYPES.contains(&doc_type.as_str());
	let title = if periodic {
		specific_title(
			title,
			issuer.as_deref(),
			date.as_deref(),
			language.as_deref(),
		)
	} else {
		title
	};

	let grounds = TagGrounds::new(&spaced);
	let mut tags: Vec<String> = Vec::new();
	for tag in raw.tags {
		let tag = collapse_whitespace(&tag);
		let tag = known(&tag, &input.known_tags).unwrap_or_else(|| tag.to_lowercase());
		if grounds.holds(&tag) && !tags.contains(&tag) && tags.len() < MAX_TAGS {
			tags.push(tag);
		}
	}

	let total = raw
		.total_printed
		.filter(|total| !total.trim().is_empty() && !is_null_word(total))
		.and_then(|total| {
			let stated = RawFact {
				key: Some(TOTAL_KEY.into()),
				value: Value::String(total),
				amount: Value::Null,
				currency: raw.currency,
			};
			settle_fact(stated, label, problems)
		})
		.filter(|fact| fact.amount_minor.is_some());
	let has_total = total.is_some();
	let facts = total
		.into_iter()
		.chain(
			raw.facts
				.into_iter()
				.filter_map(|fact| settle_fact(fact, label, problems))
				.filter(|fact| !(has_total && fact.key == TOTAL_KEY)),
		)
		.filter(|fact| fact_is_printed(fact, &printed))
		.map(|fact| plausible_currency(fact, &spaced))
		.take(MAX_FACTS)
		.collect();

	let confidence = match raw.confidence {
		Some(confidence) if confidence.is_finite() => confidence.clamp(0.0, 1.0) as f32,
		_ => DEFAULT_CONFIDENCE,
	};
	let confidence = if issuer_printed {
		confidence
	} else {
		confidence * UNPRINTED_ISSUER_CONFIDENCE
	};

	Segment {
		pages,
		rotations,
		title,
		area,
		doc_type,
		issuer,
		date,
		date_precision,
		language,
		summary: raw
			.summary
			.map(|summary| collapse_whitespace(&summary))
			.filter(|s| !s.is_empty()),
		tags,
		facts,
		confidence,
	}
}

fn is_null_word(text: &str) -> bool {
	matches!(
		text.trim().to_ascii_lowercase().as_str(),
		"null" | "none" | "n/a" | "unknown" | "不明"
	)
}

/// Whether `value` occurs in the folded page text, whatever its width, case and spacing.
fn is_printed(value: &str, folded_text: &str) -> bool {
	let key = fold_name(value);
	!key.is_empty() && folded_text.contains(&key)
}

/// [`is_printed`] that also ignores accents, for names.
fn is_printed_plain(value: &str, plain_text: &str) -> bool {
	let key = fold_plain(value);
	!key.is_empty() && plain_text.contains(&key)
}

/// Languages that write a short date year first (`24/03/14`).
const YEAR_FIRST_LANGUAGES: [&str; 3] = ["ja", "zh", "ko"];

/// The page text a stated date is checked against.
struct DateTexts<'a> {
	spaced: &'a str,
	plain: &'a str,
	/// Day-first dates with a two-digit year can be read.
	short_years: bool,
}

/// The document date the model states, kept only when the page prints it: a day as an issue
/// date (not the start or the end of a period, not under another label), a month as a month
/// alone, both ends of a period, a date in words as written. A day and a month without a year
/// take the year of the one printed date with that day and month.
fn settle_date(
	stated: &str,
	texts: &DateTexts<'_>,
	label: usize,
	problems: &mut Vec<String>,
) -> Option<(String, DatePrecision)> {
	let short_years = texts.short_years;
	let printed = printed_dates(texts.spaced, short_years);
	let issue_day = |date: &str| {
		printed
			.iter()
			.any(|printed| printed.date == date && printed.day && !printed.qualified())
	};
	let cleaned = collapse_whitespace(&fold_width(stated));
	let reading = read_stated(stated, short_years).or_else(|| {
		let date = printed_dates(&cleaned, short_years).into_iter().next()?;
		Some(Stated::Date {
			date: date.date,
			day: date.day,
		})
	});
	if let Some(reading) = reading {
		let grounded = match &reading {
			Stated::Date { date, day: true } => issue_day(date),
			Stated::Date { date, day: false } => printed
				.iter()
				.any(|printed| printed.date == *date && !printed.day && !printed.marked),
			Stated::Range { start, end } => [start, end]
				.iter()
				.all(|end| printed.iter().any(|printed| printed.date == **end)),
			Stated::Words { .. } => is_printed_plain(stated, texts.plain),
		};
		return grounded.then(|| reading.document_date());
	}
	if !short_years && !printed_dates(&cleaned, true).is_empty() {
		problems.push(format!(
			"segment {label}: date_printed \"{stated}\" has a two-digit year, which cannot be told apart from the day in this language; copy the full date with its four-digit year or in 和暦 as printed (\"2024/03/14\", \"令和6年3月14日\"), or use null"
		));
		return None;
	}
	if let Some((month, day)) = day_and_month(stated) {
		let mut years: Vec<&str> = printed
			.iter()
			.filter(|printed| printed.day && !printed.qualified())
			.filter(|printed| printed.month_day() == Some((month, day)))
			.map(|printed| printed.date.as_str())
			.collect();
		years.dedup();
		if let [date] = years.as_slice() {
			return Some((date.to_string(), DatePrecision::Day));
		}
		problems.push(format!(
			"segment {label}: date_printed \"{stated}\" has no year; copy the full date with its year as printed, or use null"
		));
		return None;
	}
	problems.push(if only_a_year(stated) {
		format!(
			"segment {label}: date_printed \"{stated}\" is only a year; give the issue date, or for a pay or account period its month and year as printed (\"mars 2014\", \"令和6年3月分\"), or null"
		)
	} else {
		format!(
			"segment {label}: date_printed \"{stated}\" cannot be read as a date; copy the date exactly as printed or use null"
		)
	});
	None
}

/// Kinds of document issued every month or period, which their month tells apart.
const PERIODIC_TYPES: [&str; 2] = ["payslip", "statement"];
const GENERIC_TITLE_WORDS: usize = 4;
const GENERIC_TITLE_CHARS: usize = 40;
const MONTH_NAMES_FR: [&str; 12] = [
	"janvier",
	"février",
	"mars",
	"avril",
	"mai",
	"juin",
	"juillet",
	"août",
	"septembre",
	"octobre",
	"novembre",
	"décembre",
];
const MONTH_NAMES_EN: [&str; 12] = [
	"January",
	"February",
	"March",
	"April",
	"May",
	"June",
	"July",
	"August",
	"September",
	"October",
	"November",
	"December",
];

/// A short title that names only the kind of document (`Bulletin de paie`, `給与明細`) gets
/// the issuer and the month after it, in the language of the document:
/// `Bulletin de paie SGCB mars 2014`, `給与明細 株式会社サンプル 2024年3月`.
fn specific_title(
	title: String,
	issuer: Option<&str>,
	date: Option<&str>,
	language: Option<&str>,
) -> String {
	let generic = !title.chars().any(|c| c.is_ascii_digit())
		&& title.split_whitespace().count() <= GENERIC_TITLE_WORDS
		&& title.chars().count() <= GENERIC_TITLE_CHARS;
	if !generic {
		return title;
	}
	let plain = fold_plain(&title);
	let mut parts = vec![title.clone()];
	if let Some(issuer) = issuer.filter(|issuer| !plain.contains(&fold_plain(issuer))) {
		parts.push(issuer.to_string());
	}
	if let Some(month) = date.and_then(|date| month_label(date, language)) {
		parts.push(month);
	}
	let composed = parts.join(" ");
	if composed.chars().count() > MAX_TITLE_CHARS {
		return title;
	}
	composed
}

fn month_label(date: &str, language: Option<&str>) -> Option<String> {
	let year: i32 = date.get(..4)?.parse().ok()?;
	let month: usize = date.get(5..7)?.parse().ok()?;
	let index = month.checked_sub(1).filter(|index| *index < 12)?;
	Some(match language {
		Some("fr") => format!("{} {year}", MONTH_NAMES_FR[index]),
		Some("en") => format!("{} {year}", MONTH_NAMES_EN[index]),
		Some("ja") => format!("{year}年{month}月"),
		_ => format!("{year}-{month:02}"),
	})
}

/// A money fact whose currency comes from the model alone, not from its printed value, is
/// checked against the currencies the document names: it takes the only one named when it
/// is not among them, and loses its amount when several others are.
fn plausible_currency(fact: Fact, text: &str) -> Fact {
	let Some(currency) = fact.currency.as_deref() else {
		return fact;
	};
	if parse_amount(&fact.value, None).is_some() {
		return fact;
	}
	let named = currencies_in(text);
	if named.is_empty() || currency_fits(currency, &named, text) {
		return fact;
	}
	let corrected = match named.as_slice() {
		[only] => parse_amount(&fact.value, Some(only)),
		_ => None,
	};
	tracing::debug!(
		key = fact.key,
		currency,
		named = ?named,
		corrected = corrected.is_some(),
		"currency not named in the document"
	);
	Fact {
		amount_minor: corrected.as_ref().map(|amount| amount.minor),
		currency: corrected.map(|amount| amount.currency),
		..fact
	}
}

struct PrintedDate {
	date: String,
	/// The day is printed, not only the month.
	day: bool,
	/// The date starts or ends a period ("2024年4月1日以降", "～ 2026年3月31日").
	marked: bool,
	/// A label makes it something else than the day the document was issued ("有効期限").
	labelled: bool,
}

impl PrintedDate {
	fn qualified(&self) -> bool {
		self.marked || self.labelled
	}

	fn month_day(&self) -> Option<(u32, u32)> {
		Some((
			self.date.get(5..7)?.parse().ok()?,
			self.date.get(8..10)?.parse().ok()?,
		))
	}
}

/// Every date in the text, in any notation, by position and longest reading first. A
/// candidate that is the start of a longer number or date does not count.
fn printed_dates(spaced_text: &str, short_years: bool) -> Vec<PrintedDate> {
	let chars: Vec<char> = spaced_text.chars().collect();
	let digit = |c: &char| c.is_ascii_digit();
	let continues = |rest: &[char], complete: bool| match rest {
		[next, ..] if digit(next) || matches!(next, '年' | '月' | '日') => true,
		['/' | '-' | '.', next, ..] => digit(next),
		[' ', next, ..] => !complete && digit(next),
		_ => false,
	};
	let starts_date = |start: usize| {
		let c = chars[start];
		let previous = start.checked_sub(1).map(|at| chars[at]);
		let word_start = !previous.is_some_and(|p| p.is_ascii_alphabetic());
		(digit(&c) && !previous.is_some_and(|p| digit(&p)))
			|| (c.is_ascii_alphabetic() && word_start)
			|| DATE_STARTS.contains(c)
	};
	let mut dates = Vec::new();
	for start in (0..chars.len()).filter(|&start| starts_date(start)) {
		let longest = MAX_DATE_CHARS.min(chars.len() - start);
		for length in (MIN_DATE_CHARS..=longest).rev() {
			let end = start + length;
			if chars[start..end - 1].contains(&'日') {
				continue;
			}
			let candidate: String = chars[start..end].iter().collect();
			let Some((date, day_printed)) = read_date_in(&candidate, short_years) else {
				continue;
			};
			let complete = day_printed || chars[end - 1] == '分';
			if chars[end - 1] != '日' && continues(&chars[end..], complete) {
				continue;
			}
			let before: String = chars[..start].iter().collect();
			let after: String = chars[end..].iter().collect();
			let after = after.trim_start_matches(['日', ' ', ')']);
			let before = before.trim_end_matches([' ', ':', '・', '|', '(']);
			let marked = PERIOD_MARKS
				.iter()
				.any(|mark| after.starts_with(mark) || before.ends_with(mark));
			let labelled = OTHER_DATE_LABELS
				.iter()
				.any(|label| before.ends_with(label));
			dates.push(PrintedDate {
				date,
				day: day_printed,
				marked,
				labelled,
			});
		}
	}
	dates
}

/// A fact is kept when its value is printed on its pages, or, for money, when its number is
/// (a table cell often holds the number without the currency).
fn fact_is_printed(fact: &Fact, folded_text: &str) -> bool {
	if is_printed(&fact.value, folded_text) {
		return true;
	}
	if fact.amount_minor.is_none() {
		return false;
	}
	let folded = fold_name(&fact.value);
	let start = folded
		.find(|c: char| c.is_ascii_digit())
		.unwrap_or(folded.len());
	let number: String = folded[start..]
		.chars()
		.take_while(|c| c.is_ascii_digit() || matches!(c, ',' | '.'))
		.filter(|c| *c != ',')
		.collect();
	let number = number.trim_end_matches('.');
	if number.chars().filter(char::is_ascii_digit).count() < MIN_AMOUNT_DIGITS {
		return false;
	}
	let text = folded_text.replace(',', "");
	text.match_indices(number).any(|(start, _)| {
		let before = text[..start].chars().next_back();
		let after = text[start + number.len()..].chars().next();
		!before.is_some_and(|c| c.is_ascii_digit() || c == '.')
			&& !after.is_some_and(|c| c.is_ascii_digit())
	})
}

/// A name with an honorific or a salutation is whom the document is addressed to, not who
/// issued it.
fn is_addressee(name: &str) -> bool {
	let name = name.trim_end_matches(|c: char| c.is_whitespace() || matches!(c, ')' | '）'));
	ADDRESSEE_SUFFIXES
		.iter()
		.any(|suffix| name.ends_with(suffix))
		|| ADDRESSEE_PREFIXES.iter().any(|prefix| {
			name.get(..prefix.len())
				.is_some_and(|start| start.eq_ignore_ascii_case(prefix))
		})
}

/// A postal mark or code, or prefecture, municipality and a block number in that order.
fn is_address(name: &str) -> bool {
	let folded = fold_width(name);
	if folded.contains('〒') || has_postal_code(&folded) {
		return true;
	}
	let Some(prefecture) = folded.find(['都', '道', '府', '県']) else {
		return false;
	};
	let rest = &folded[prefecture..];
	let Some(municipality) = rest.find(['区', '市', '町', '村', '郡']) else {
		return false;
	};
	let rest = &rest[municipality..];
	rest.contains(|c: char| c.is_ascii_digit()) || rest.contains("丁目") || rest.contains("番地")
}

fn has_postal_code(folded: &str) -> bool {
	let chars: Vec<char> = folded.chars().collect();
	let digits = |from: usize| {
		chars
			.iter()
			.skip(from)
			.take_while(|c| c.is_ascii_digit())
			.count()
	};
	(0..chars.len()).any(|start| {
		(start == 0 || !chars[start - 1].is_ascii_digit() && chars[start - 1] != '-')
			&& digits(start) == 3
			&& matches!(chars.get(start + 3), Some('-' | '\u{2010}' | '\u{2212}'))
			&& digits(start + 4) == 4
			&& chars.get(start + 8) != Some(&'-')
	})
}

/// The known value that is the same name apart from case, width and spacing.
fn known(value: &str, known_values: &[String]) -> Option<String> {
	let key = fold_name(value);
	known_values
		.iter()
		.find(|candidate| fold_name(candidate) == key)
		.cloned()
}

fn is_placeholder_area(area: &str) -> bool {
	matches!(
		area.to_ascii_lowercase().as_str(),
		"unsorted"
			| "misc"
			| "miscellaneous"
			| "other"
			| "others"
			| "general"
			| "document"
			| "documents"
			| "none"
			| "unknown"
	)
}

fn settle_area(raw: &str, known_areas: &[String]) -> Option<String> {
	let area = collapse_whitespace(raw);
	if area.is_empty() || is_placeholder_area(&area) {
		return None;
	}
	if let Some(area) = known(&area, known_areas) {
		return Some(area);
	}
	let allowed = |c: char| c.is_ascii_alphanumeric() || matches!(c, ' ' | '&' | '-');
	let valid = area.len() <= MAX_LABEL_CHARS
		&& area.starts_with(|c: char| c.is_ascii_alphabetic())
		&& area.chars().all(allowed);
	valid.then(|| {
		let words: Vec<String> = area
			.split(' ')
			.map(|word| {
				let mut chars = word.chars();
				match chars.next() {
					Some(first) if word.chars().all(|c| !c.is_ascii_uppercase()) => {
						first.to_ascii_uppercase().to_string() + chars.as_str()
					}
					_ => word.to_string(),
				}
			})
			.collect();
		words.join(" ")
	})
}

fn settle_doc_type(raw: &str) -> Option<String> {
	let doc_type = collapse_whitespace(raw)
		.to_ascii_lowercase()
		.replace('_', " ");
	let allowed = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, ' ' | '-');
	let valid = !doc_type.is_empty()
		&& doc_type.len() <= MAX_LABEL_CHARS
		&& doc_type.starts_with(|c: char| c.is_ascii_lowercase())
		&& doc_type.chars().all(allowed);
	valid.then_some(doc_type)
}

fn settle_language(raw: &str) -> Option<String> {
	let language = raw.trim().to_ascii_lowercase();
	let code = match language.as_str() {
		"japanese" | "日本語" | "jp" | "jpn" => "ja",
		"english" | "eng" => "en",
		"french" | "français" | "francais" | "fra" | "fre" => "fr",
		code if code.len() == 2 && code.chars().all(|c| c.is_ascii_lowercase()) => code,
		_ => return None,
	};
	Some(code.to_string())
}

/// The title without the issuer or the date at either end, since the file name carries both.
fn bare_title(title: &str, issuer: Option<&str>, date: Option<&str>) -> String {
	let mut bare = title;
	loop {
		let shorter = [date, issuer]
			.into_iter()
			.flatten()
			.flat_map(|affix| [bare.strip_prefix(affix), bare.strip_suffix(affix)])
			.flatten()
			.map(|stripped| stripped.trim_matches(TITLE_SEPARATORS))
			.find(|stripped| !stripped.is_empty());
		match shorter {
			Some(shorter) => bare = shorter,
			None => return bare.to_string(),
		}
	}
}

fn settle_fact(raw: RawFact, label: usize, problems: &mut Vec<String>) -> Option<Fact> {
	let key: String = collapse_whitespace(raw.key.as_deref().unwrap_or_default())
		.to_lowercase()
		.replace([' ', '-'], "_");
	let value = match &raw.value {
		Value::String(text) => text.trim().to_string(),
		Value::Number(number) => number.to_string(),
		_ => String::new(),
	};
	if key.is_empty() || value.is_empty() {
		return None;
	}
	let stated_currency = raw.currency.as_deref().and_then(normalize_currency);
	let stated_amount = match &raw.amount {
		Value::Number(number) => number.as_f64(),
		Value::String(text) => text.trim().replace(',', "").parse().ok(),
		_ => None,
	}
	.filter(|amount: &f64| amount.is_finite());
	let in_minor = |amount: f64, currency: &str| {
		let minor = (amount * 10f64.powi(decimals(currency) as i32)).round();
		(minor.abs() < 9.0e15).then_some(minor as i64)
	};

	let (amount_minor, currency) = match parse_amount(&value, stated_currency.as_deref()) {
		Some(printed) => {
			let stated = stated_amount.and_then(|amount| in_minor(amount, &printed.currency));
			if stated.is_some_and(|stated| stated.abs() != printed.minor.abs()) {
				problems.push(format!(
					"segment {label}: fact \"{key}\" has amount {} but its value \"{value}\" reads as {} {}",
					stated_amount.unwrap_or_default(),
					printed.minor as f64 / 10f64.powi(decimals(&printed.currency) as i32),
					printed.currency
				));
			}
			(Some(printed.minor), Some(printed.currency))
		}
		None => match (stated_amount, stated_currency) {
			(Some(amount), Some(currency)) => match in_minor(amount, &currency) {
				Some(minor) => (Some(minor), Some(currency)),
				None => (None, None),
			},
			_ => (None, None),
		},
	};
	Some(Fact {
		key,
		value,
		amount_minor,
		currency,
	})
}

#[cfg(test)]
mod tests {
	#[test]
	fn placeholder_areas_are_refused() {
		let known = ["Health".to_string(), "Unsorted".to_string()];
		for raw in [
			"Unsorted",
			"misc",
			"Other",
			"General",
			"documents",
			"unknown",
		] {
			assert_eq!(super::settle_area(raw, &known), None, "{raw}");
		}
		assert_eq!(
			super::settle_area("travel", &known).as_deref(),
			Some("Travel")
		);
		assert_eq!(
			super::settle_area("health", &known).as_deref(),
			Some("Health")
		);
	}

	use serde_json::json;

	use super::*;

	const BILL: &str = "東京電力 ご請求書 令和6年3月14日\n合計 | ¥12,340\n手取り 1万2千円\n期限 令和6年3月31日\nfee: twelve dollars\n控除 ▲1,200円";

	fn input(texts: &[&str]) -> AnalyzeInput {
		AnalyzeInput {
			file_name: "scan 0042.pdf".into(),
			pages: (1..)
				.zip(texts)
				.map(|(number, text)| PageInput {
					number,
					text: text.to_string(),
					thumbnail_png: None,
				})
				.collect(),
			one_document: false,
			photo: false,
			known_areas: vec!["Health".into(), "Tax".into()],
			known_issuers: vec!["NTTドコモ".into(), "東京電力".into()],
			known_tags: vec!["Electricity".into()],
		}
	}

	fn settled(answer: Value, texts: &[&str]) -> (Analysis, Vec<String>) {
		let input = input(texts);
		let scope = Scope::new(&input, &input.pages);
		let settled = settle(parse(&answer.to_string()).unwrap(), &scope);
		(settled.analysis, settled.problems)
	}

	fn settled_segment(fields: Value, text: &str) -> Segment {
		let mut answer = segment(&[1]);
		for (name, value) in fields.as_object().unwrap() {
			answer[name] = value.clone();
		}
		let (mut analysis, problems) =
			settled(json!({"segments": [answer], "blank_pages": []}), &[text]);
		assert_eq!(problems, Vec::<String>::new());
		analysis.segments.remove(0)
	}

	fn segment(pages: &[u32]) -> Value {
		json!({"pages": pages, "title": "領収書", "area": "Health", "doc_type": "receipt", "confidence": 0.8})
	}

	#[test]
	fn valid_answer_is_normalised() {
		let (analysis, problems) = settled(
			json!({
				"segments": [{
					"pages": [1, 2],
					"rotations": [{"page": 2, "degrees": -90}, {"page": 1, "degrees": 0}],
					"title": "東京電力  2024-03-14 電気料金 ",
					"area": "health",
					"doc_type": "Tax_Notice",
					"issuer": "東京 電力",
					"date_printed": "令和6年3月14日",
					"language": "Japanese",
					"summary": "  Bill.  ",
					"tags": ["electricity", "Bill", "bill", ""],
					"facts": [
						{"key": "Total", "value": "￥１２，３４０", "amount": 12340, "currency": "JPY"},
						{"key": "net pay", "value": "1万2千円", "amount": null, "currency": null},
						{"key": "due_date", "value": "令和6年3月31日", "amount": null, "currency": "JPY"},
						{"key": "fee", "value": "twelve dollars", "amount": 12.5, "currency": "usd"},
						{"key": "deduction", "value": "▲1,200円", "amount": 1200, "currency": "円"},
						{"key": "", "value": "dropped"},
					],
					"confidence": 1.7,
				}],
				"blank_pages": [3, 3],
			}),
			&[BILL, "", ""],
		);
		assert_eq!(problems, Vec::<String>::new());
		assert_eq!(analysis.blank_pages, [3]);
		let segment = &analysis.segments[0];
		assert_eq!(segment.pages, [1, 2]);
		assert_eq!(
			segment.rotations,
			[Rotation {
				page: 2,
				degrees: 270
			}]
		);
		assert_eq!(segment.title, "電気料金");
		assert_eq!(segment.area, "Health");
		assert_eq!(segment.doc_type, "tax notice");
		assert_eq!(segment.issuer.as_deref(), Some("東京電力"));
		assert_eq!(segment.date.as_deref(), Some("2024-03-14"));
		assert_eq!(segment.language.as_deref(), Some("ja"));
		assert_eq!(segment.summary.as_deref(), Some("Bill."));
		assert_eq!(segment.tags, ["Electricity", "bill"]);
		assert_eq!(segment.confidence, 1.0);
		let facts: Vec<_> = segment
			.facts
			.iter()
			.map(|fact| {
				(
					fact.key.as_str(),
					fact.amount_minor,
					fact.currency.as_deref(),
				)
			})
			.collect();
		assert_eq!(
			facts,
			[
				("total", Some(12_340), Some("JPY")),
				("net_pay", Some(12_000), Some("JPY")),
				("due_date", None, None),
				("fee", Some(1_250), Some("USD")),
				("deduction", Some(-1_200), Some("JPY")),
			]
		);
		assert_eq!(segment.facts[0].value, "￥１２，３４０");
	}

	#[test]
	fn new_area_is_title_cased() {
		let known = ["Health".to_string()];
		assert_eq!(
			settle_area("real estate", &known).as_deref(),
			Some("Real Estate")
		);
		assert_eq!(
			settle_area("ＨＥＡＬＴＨ", &known).as_deref(),
			Some("Health")
		);
		assert_eq!(settle_area("医療", &known), None);
		assert_eq!(settle_area("Health/Dental", &known), None);
		assert_eq!(settle_area(" ", &known), None);
	}

	#[test]
	fn page_violations_are_reported_and_repaired() {
		let (analysis, problems) = settled(
			json!({
				"segments": [segment(&[2, 3, 9]), segment(&[3, 5]), segment(&[9])],
				"blank_pages": [5, 0],
			}),
			&[""; 6],
		);
		assert_eq!(
			problems,
			[
				"segment 1: page 9 is not one of the listed pages",
				"page 3 is in more than one place",
				"segment 3: page 9 is not one of the listed pages",
				"segment 3 has no valid pages",
				"page 5 is both in a segment and in blank_pages",
				"blank_pages: page 0 is not one of the listed pages",
				"pages missing from both segments and blank_pages: 1, 4, 6",
			]
		);
		assert_eq!(analysis.segments.len(), 2);
		assert_eq!(analysis.segments[0].pages, [1, 2, 3, 4]);
		assert_eq!(analysis.segments[1].pages, [5, 6]);
		assert!(analysis.blank_pages.is_empty());
	}

	#[test]
	fn pages_without_any_segment_get_a_document() {
		let (analysis, problems) = settled(json!({"segments": [], "blank_pages": [2]}), &[""; 3]);
		assert_eq!(
			problems,
			["pages missing from both segments and blank_pages: 1, 3"]
		);
		assert_eq!(analysis.blank_pages, [2]);
		assert_eq!(analysis.segments.len(), 1);
		let segment = &analysis.segments[0];
		assert_eq!(segment.pages, [1, 3]);
		assert_eq!(
			(
				segment.title.as_str(),
				segment.area.as_str(),
				segment.doc_type.as_str()
			),
			("scan 0042", "Unsorted", "document")
		);
		assert_eq!(segment.confidence, 0.0);
		let input = input(&[""; 3]);
		let answer = json!({"segments": [], "blank_pages": [2]});
		assert!(settle_with(answer, &input).degenerate);
	}

	#[test]
	fn field_violations_are_reported_and_repaired() {
		let (analysis, problems) = settled(
			json!({
				"segments": [{
					"pages": [1],
					"rotations": [{"page": 1, "degrees": 45}, {"page": 2, "degrees": 90}],
					"title": " ",
					"area": "医療",
					"doc_type": "領収書",
					"issuer": "null",
					"date_printed": "2024-02-30",
					"facts": [{"key": "total", "value": "12,340円", "amount": 1234, "currency": "JPY"}],
				}],
				"blank_pages": [],
			}),
			&["合計 12,340円"],
		);
		assert_eq!(
			problems,
			[
				"segment 1: rotation of 45 degrees is not 90, 180 or 270",
				"segment 1: rotation for page 2, which is not in this segment",
				"segment 1: date_printed \"2024-02-30\" cannot be read as a date; copy the date exactly as printed or use null",
				"segment 1: doc_type must be a short lowercase English word, got \"領収書\"",
				"segment 1: area must be a known area or a short English Title-case noun, got \"医療\"",
				"segment 1: title is empty",
				"segment 1: fact \"total\" has amount 1234 but its value \"12,340円\" reads as 12340 JPY",
			]
		);
		let segment = &analysis.segments[0];
		assert!(segment.rotations.is_empty());
		assert_eq!(segment.title, "scan 0042");
		assert_eq!(segment.area, "Unsorted");
		assert_eq!(segment.doc_type, "document");
		assert_eq!(segment.issuer, None);
		assert_eq!(segment.date, None);
		assert_eq!(segment.facts[0].amount_minor, Some(12_340));
		assert_eq!(segment.confidence, DEFAULT_CONFIDENCE);
	}

	#[test]
	fn date_is_kept_only_when_printed_on_the_page() {
		let date = |stated: &str, text: &str| {
			let title = if text.is_ascii() {
				"Invoice"
			} else {
				"領収書"
			};
			settled_segment(json!({"date_printed": stated, "title": title}), text).date
		};
		let page = "発行日 令和 ６年10月 7日\n受付 1987年04月15日";
		assert_eq!(date("令和6年10月7日", page).as_deref(), Some("2024-10-07"));
		assert_eq!(date("2024年10月7日", page).as_deref(), Some("2024-10-07"));
		assert_eq!(date("1987/4/15", page).as_deref(), Some("1987-04-15"));
		assert_eq!(date("令和6年10月1日", page), None);
		assert_eq!(date("2022-12-01", page), None);
		assert_eq!(date("令和6年10月", page), None);
		assert_eq!(date("2024-03-14", ""), None);
		assert_eq!(
			date("Jan 20, 2023", "Invoice date: Jan 20, 2023").as_deref(),
			Some("2023-01-20")
		);
		assert_eq!(
			date("R6.3.14", "領収証 R6.3.14 10:18").as_deref(),
			Some("2024-03-14")
		);
		assert_eq!(
			date("29 Sep 2024", "Issued 29 Sep 2024. Thank you").as_deref(),
			Some("2024-09-29")
		);
		assert_eq!(
			date("2013/01/05", "領収証 2013 01 05 10:18").as_deref(),
			Some("2013-01-05")
		);
		assert_eq!(date("2013/01", "領収証 2013 01 05 10:18"), None);
		assert_eq!(
			date("令和6年3月分", "給与明細 令和6年3月分 312,450円").as_deref(),
			Some("2024-03-01")
		);
		assert_eq!(date("1/3/2024", "11/3/2024"), None);
		assert_eq!(date("2024/3/1", "2024/3/14"), None);
		assert_eq!(date("2024-03", "2024/3/14"), None);
		assert_eq!(
			date(
				"Jan 1, 2025 5:31 PM (PST)",
				"Invoice Date Jan 1, 2025 5:31 PM (PST)"
			)
			.as_deref(),
			Some("2025-01-01")
		);
		assert_eq!(
			date("発行日 2024年3月14日（木）", "発行日 2024年3月14日（木）").as_deref(),
			Some("2024-03-14")
		);
		for period in [
			"2022年10月1日以降始期契約用",
			"契約期間 2022年10月1日 ～ 2024年9月30日",
			"契約期間 2021年10月2日～2022年10月1日",
			"2022年10月1日から適用",
			"R4.10.1 より",
		] {
			assert_eq!(date("2022年10月1日", period), None, "{period}");
		}
		assert_eq!(
			date(
				"2022年10月1日",
				"2022年10月1日\n保険期間 2022年10月1日から2年間"
			)
			.as_deref(),
			Some("2022-10-01")
		);
		for labelled in [
			"契約期間 2024年11月30日",
			"有効期限: 2024年11月30日",
			"生年月日 | 2024年11月30日",
			"始期日・2024年11月30日",
		] {
			assert_eq!(date("2024年11月30日", labelled), None, "{labelled}");
		}
		assert_eq!(
			date("令和4年11月14日", "作成日時・令和 4年11月14日15時15分").as_deref(),
			Some("2022-11-14")
		);
	}

	fn settled_problems(fields: Value, text: &str) -> (Segment, Vec<String>) {
		let mut answer = segment(&[1]);
		for (name, value) in fields.as_object().unwrap() {
			answer[name] = value.clone();
		}
		let (mut analysis, problems) =
			settled(json!({"segments": [answer], "blank_pages": []}), &[text]);
		(analysis.segments.remove(0), problems)
	}

	#[test]
	fn periods_and_short_years_are_document_dates() {
		let date = |stated: &str, text: &str| {
			let segment =
				settled_segment(json!({"date_printed": stated, "title": "Bulletin"}), text);
			segment.date.map(|date| (date, segment.date_precision))
		};
		let month = |date: &str| Some((date.to_string(), DatePrecision::Month));
		let day = |date: &str| Some((date.to_string(), DatePrecision::Day));
		let payslip = "BULLETIN DE PAIE Période : mars 2014 Net à payer 470 075 F CFP";
		assert_eq!(date("mars 2014", payslip), month("2014-03-01"));
		assert_eq!(date("Mars 2014", payslip), month("2014-03-01"));
		assert_eq!(date("avril 2014", payslip), None);
		let ranged = "Bulletin de salaire Période du 01/03/2014 au 31/03/2014 Paiement";
		assert_eq!(
			date("du 01/03/2014 au 31/03/2014", ranged),
			month("2014-03-01")
		);
		assert_eq!(date("du 01/03/2014 au 31/03/2015", ranged), None);
		assert_eq!(
			date(
				"du 31 mai 2015 au 30 juin 2015",
				"Relevé du 31 mai 2015 au 30 juin 2015"
			),
			day("2015-06-30")
		);
		assert_eq!(
			date("27/04/18", "Nouméa, le 27/04/18 Facture"),
			day("2018-04-27")
		);
		assert_eq!(
			date("08 JAN 16", "Date: 08 JAN 16 Total"),
			day("2016-01-08")
		);
		assert_eq!(date("22MAY17", "ISSUED 22MAY17 TOKYO"), day("2017-05-22"));
		assert_eq!(
			date(
				"L'AN DEUX MIL VINGT-TROIS LE ONZE AVRIL",
				"Et l'an deux mil vingt-trois le onze avril, par devant"
			),
			day("2023-04-11")
		);
		assert_eq!(
			date("L'AN DEUX MIL VINGT-TROIS LE ONZE AVRIL", "Acte notarié"),
			None
		);
	}

	#[test]
	fn two_digit_years_are_refused_where_the_year_comes_first() {
		let stated = |stated: &str, language: &str, text: &str| {
			settled_problems(
				json!({"date_printed": stated, "language": language, "title": "Facture 領収書"}),
				text,
			)
		};
		for (date, text) in [
			("24/03/14", "領収書 24/03/14 合計 1,200円 Facture"),
			("24.03.14", "領収書 24.03.14 合計 1,200円 Facture"),
			("発行日 24/03/14", "領収書 発行日 24/03/14 Facture"),
		] {
			for language in ["ja", "zh", "ko"] {
				let (segment, problems) = stated(date, language, text);
				assert_eq!(segment.date, None, "{date} {language}");
				assert_eq!(problems.len(), 1, "{date} {language}: {problems:?}");
				assert!(problems[0].contains("two-digit year"), "{problems:?}");
			}
			let (segment, problems) = stated(date, "fr", text);
			assert_eq!(segment.date.as_deref(), Some("2014-03-24"), "{date}");
			assert_eq!(problems, Vec::<String>::new());
		}
		let (segment, problems) = stated("27/04/18", "fr", "Nouméa, le 27/04/18 Facture 領収書");
		assert_eq!(
			(segment.date.as_deref(), problems.len()),
			(Some("2018-04-27"), 0)
		);
		let (segment, problems) = stated("2024/03/14", "ja", "領収書 2024/03/14 Facture");
		assert_eq!(
			(segment.date.as_deref(), problems.len()),
			(Some("2024-03-14"), 0)
		);
		let (segment, problems) = stated("令和6年3月14日", "ja", "領収書 令和6年3月14日 Facture");
		assert_eq!(
			(segment.date.as_deref(), problems.len()),
			(Some("2024-03-14"), 0)
		);
		let (segment, problems) = stated("08 JAN 16", "ja", "ISSUED 08 JAN 16 領収書 Facture");
		assert_eq!(
			(segment.date.as_deref(), problems.len()),
			(Some("2016-01-08"), 0)
		);
	}

	#[test]
	fn a_day_without_a_year_takes_the_printed_year_or_is_refused() {
		let stated = |stated: &str, text: &str| {
			settled_problems(json!({"date_printed": stated, "title": "Billet"}), text)
		};
		let (segment, problems) = stated("7 juil.", "Billet Lyon 7 juil. Émis le 7 juillet 2016");
		assert_eq!(segment.date.as_deref(), Some("2016-07-07"));
		assert_eq!(problems, Vec::<String>::new());
		let (segment, problems) = stated("7 juil.", "Billet Lyon 7 juil. départ");
		assert_eq!(segment.date, None);
		assert!(problems[0].contains("has no year"), "{problems:?}");
		let (segment, problems) = stated("7 juil.", "Billet 7 juillet 2016, retour 7 juillet 2017");
		assert_eq!(segment.date, None);
		assert_eq!(problems.len(), 1);
		let (segment, problems) = stated("Solde 2017", "Avis Solde 2017 à payer");
		assert_eq!(segment.date, None);
		assert!(problems[0].contains("only a year"), "{problems:?}");
	}

	#[test]
	fn periodic_titles_get_the_issuer_and_the_month() {
		let title = |title: &str, doc_type: &str, language: &str, date: Value| {
			settled_segment(
				json!({"title": title, "doc_type": doc_type, "language": language, "issuer": "SGCB", "date_printed": date}),
				"SGCB Bulletin de paie Facture Payslip 給与明細 mars 2014, payé le 2014-03-25",
			)
			.title
		};
		assert_eq!(
			title("Bulletin de paie", "payslip", "fr", json!("mars 2014")),
			"Bulletin de paie SGCB mars 2014"
		);
		assert_eq!(
			title("Payslip", "payslip", "en", json!("2014-03-25")),
			"Payslip SGCB March 2014"
		);
		assert_eq!(
			title("給与明細", "payslip", "ja", json!("2014-03-25")),
			"給与明細 SGCB 2014年3月"
		);
		assert_eq!(
			title("Facture", "invoice", "fr", json!("mars 2014")),
			"Facture SGCB mars 2014"
		);
		assert_eq!(
			title("Facture", "invoice", "fr", json!("2014-03-25")),
			"Facture"
		);
		assert_eq!(
			title("Bulletin de paie", "payslip", "fr", json!(null)),
			"Bulletin de paie SGCB"
		);
		assert_eq!(
			title("Bulletin de paie SGCB", "payslip", "fr", json!("mars 2014")),
			"Bulletin de paie SGCB mars 2014"
		);
		assert_eq!(
			title(
				"Bulletin de paie n° 2014-118 du service",
				"payslip",
				"fr",
				json!("mars 2014")
			),
			"Bulletin de paie n° 2014-118 du service"
		);
	}

	#[test]
	fn issuers_map_to_the_known_spelling() {
		let mut input =
			input(&["SOCIETE GENERALE CALEDONIENNE DE BANQUE - Agence Nouméa\nRelevé de compte"]);
		input.known_issuers = vec![
			"SOCIETE GENERALE CALEDONIENNE DE BANQUE".into(),
			"SGCB".into(),
			"Société Générale Calédonienne de Banque".into(),
		];
		let issuer = |stated: &str, text: &str| {
			let mut input = input.clone();
			input.pages[0].text = text.into();
			let scope = Scope::new(&input, &input.pages);
			let mut answer = segment(&[1]);
			answer["title"] = json!("Relevé de compte");
			answer["issuer"] = json!(stated);
			let settled = settle(
				parse(&json!({"segments": [answer], "blank_pages": []}).to_string()).unwrap(),
				&scope,
			);
			let segment = &settled.analysis.segments[0];
			(
				segment.issuer.clone(),
				settled.doubts.len(),
				segment.confidence,
			)
		};
		let full = "SOCIETE GENERALE CALEDONIENNE DE BANQUE - Agence Nouméa\nRelevé de compte";
		let canonical = Some("SOCIETE GENERALE CALEDONIENNE DE BANQUE".to_string());
		assert_eq!(
			issuer("Société Générale Calédonienne de Banque", full),
			(canonical.clone(), 0, 0.8)
		);
		assert_eq!(
			issuer("La Société Générale Calédonienne de Banque – Nouméa", full).0,
			canonical
		);
		let acronym = "SGCB Agence Nouméa\nRelevé de compte";
		assert_eq!(
			issuer("Société Générale Calédonienne de Banque", acronym),
			(canonical.clone(), 0, 0.8)
		);
		assert_eq!(issuer("SGCB", acronym).0.as_deref(), Some("SGCB"));
		assert_eq!(
			issuer(
				"Société Générale Calédonienne de Banque",
				"Relevé de compte"
			)
			.0,
			None
		);
	}

	#[test]
	fn currencies_follow_the_document() {
		let facts = |facts: Value, text: &str| -> Vec<(Option<i64>, Option<String>)> {
			settled_segment(json!({"title": "Bulletin de paie", "facts": facts}), text)
				.facts
				.into_iter()
				.map(|fact| (fact.amount_minor, fact.currency))
				.collect()
		};
		let xpf = "BULLETIN DE PAIE Net à payer 470 075 F CFP Brut 520 000";
		assert_eq!(
			facts(
				json!([{"key": "net_pay", "value": "470 075", "amount": 470075, "currency": "JPY"}]),
				xpf
			),
			[(Some(470_075), Some("XPF".into()))]
		);
		assert_eq!(
			facts(
				json!([{"key": "gross_pay", "value": "520 000", "amount": 520000, "currency": "XPF"}]),
				xpf
			),
			[(Some(520_000), Some("XPF".into()))]
		);
		let mixed = "Montant 1 000 XPF soit 8,38 EUR. Frais 250";
		assert_eq!(
			facts(
				json!([{"key": "fee", "value": "250", "amount": 250, "currency": "JPY"}]),
				mixed
			),
			[(None, None)]
		);
		assert_eq!(
			facts(
				json!([{"key": "fee", "value": "250", "amount": 250, "currency": "JPY"}]),
				"Frais 250"
			),
			[(Some(250), Some("JPY".into()))]
		);
		assert_eq!(
			facts(
				json!([{"key": "usage", "value": "350", "amount": 350, "currency": "KWH"}]),
				"Consommation 350 kWh"
			),
			[(None, None)]
		);
	}

	#[test]
	fn facts_not_printed_on_the_page_are_dropped() {
		let segment = settled_segment(
			json!({"facts": [
				{"key": "total", "value": "12,340円", "amount": 12340, "currency": "JPY"},
				{"key": "tax", "value": "1,121円", "amount": 1121, "currency": "JPY"},
				{"key": "subtotal", "value": "99,999円", "amount": 99999, "currency": "JPY"},
				{"key": "deposit", "value": "234円", "amount": 234, "currency": "JPY"},
				{"key": "points", "value": "75円", "amount": 75, "currency": "JPY"},
				{"key": "account_number", "value": "ＡＢ－１２３４", "amount": null, "currency": null},
				{"key": "policy_number", "value": "P-777", "amount": null, "currency": null},
			]}),
			"合計 | 12,340 |\n消費税 1,121 円\nポイント 75\nお客様番号 AB-1234",
		);
		let keys: Vec<&str> = segment.facts.iter().map(|fact| fact.key.as_str()).collect();
		assert_eq!(keys, ["total", "tax", "account_number"]);
	}

	#[test]
	fn printed_total_becomes_the_total_fact() {
		let page = "領収証\n小計 | 28,381\n合計 | ¥31,219\nポイント 75";
		let total = |fields: Value| {
			let segment = settled_segment(fields, page);
			let facts: Vec<_> = segment
				.facts
				.iter()
				.map(|fact| (fact.key.clone(), fact.amount_minor, fact.currency.clone()))
				.collect();
			facts
		};
		let jpy = |key: &str, amount: i64| (key.to_string(), Some(amount), Some("JPY".to_string()));
		assert_eq!(
			total(json!({
				"total_printed": "31,219",
				"currency": "JPY",
				"facts": [
					{"key": "total", "value": "28,381円", "amount": 28381, "currency": "JPY"},
					{"key": "subtotal", "value": "28,381円", "amount": 28381, "currency": "JPY"},
				],
			})),
			[jpy("total", 31_219), jpy("subtotal", 28_381)]
		);
		assert_eq!(
			total(json!({"total_printed": "￥31,219", "currency": null})),
			[jpy("total", 31_219)]
		);
		assert_eq!(
			total(json!({"total_printed": "¥99,999", "currency": "JPY"})),
			[]
		);
		assert_eq!(
			total(json!({"total_printed": "合計", "currency": "JPY"})),
			[]
		);
		assert_eq!(total(json!({"total_printed": null, "currency": "JPY"})), []);
		assert_eq!(
			total(json!({
				"total_printed": "null",
				"facts": [{"key": "total", "value": "¥31,219", "amount": 31219, "currency": "JPY"}],
			})),
			[jpy("total", 31_219)]
		);
	}

	#[test]
	fn example_issuers_are_not_an_answer() {
		for (issuer, title, other, own) in [
			(
				EXAMPLE_ISSUERS[0],
				"契約明細",
				"契約明細書",
				"株式会社サンプル 御請求書",
			),
			(
				EXAMPLE_ISSUERS[1],
				"Avis",
				"Avis aux habitants",
				"Mairie de Valmont, avis aux habitants",
			),
		] {
			assert!(ANALYZE_PROMPT.contains(issuer));
			let answer = json!({"issuer": issuer, "title": title});
			assert_eq!(settled_segment(answer.clone(), other).issuer, None);
			assert_eq!(settled_segment(answer, own).issuer.as_deref(), Some(issuer));
		}
	}

	const LEAFLET: &str = "Venez planter avec nous !\nJournée de reboisement au parc du Mont Vénus\nSamedi 2 mars, Ville de Port-Céleste";

	#[test]
	fn title_in_another_script_is_reported_and_replaced_by_a_line_of_the_page() {
		let answer = |title: &str| {
			let mut answer = segment(&[1]);
			answer["title"] = json!(title);
			json!({"segments": [answer], "blank_pages": []})
		};
		let (analysis, problems) = settled(answer("イベント告知"), &[LEAFLET]);
		assert_eq!(problems.len(), 1);
		assert!(problems[0].contains("title \"イベント告知\" is not in the language"));
		assert_eq!(analysis.segments[0].title, "Venez planter avec nous");

		let (analysis, problems) = settled(answer("Journée de reboisement"), &[LEAFLET]);
		assert_eq!(problems, Vec::<String>::new());
		assert_eq!(analysis.segments[0].title, "Journée de reboisement");

		let (analysis, problems) = settled(
			answer("Receipt"),
			&["領収書 合計 1,200円 毎度ありがとうございます"],
		);
		assert_eq!(problems.len(), 1);
		assert_eq!(
			analysis.segments[0].title,
			"領収書 合計 1,200円 毎度ありがとうございます"
		);

		let (analysis, _) = settled(answer("イベント告知"), &["12 34 abcdefgh\n56"]);
		assert_eq!(analysis.segments[0].title, "12 34 abcdefgh");
	}

	#[test]
	fn example_title_is_not_an_answer() {
		assert!(ANALYZE_PROMPT.contains(EXAMPLE_TITLE));
		let answer = || {
			let mut answer = segment(&[1]);
			answer["title"] = json!(EXAMPLE_TITLE);
			json!({"segments": [answer], "blank_pages": []})
		};
		let (analysis, problems) = settled(answer(), &[LEAFLET]);
		assert_eq!(problems.len(), 1);
		assert!(problems[0].contains("is the title of the example"));
		assert_eq!(analysis.segments[0].title, "Venez planter avec nous");

		let printed = "Grand vide-grenier du quartier, dimanche 14 juin";
		let (analysis, problems) = settled(answer(), &[printed]);
		assert_eq!(problems, Vec::<String>::new());
		assert_eq!(analysis.segments[0].title, EXAMPLE_TITLE);
	}

	#[test]
	fn tags_not_printed_and_not_topics_are_dropped() {
		let tags = |tags: Value, text: &str| {
			let title = if text == LEAFLET {
				"Reboisement"
			} else {
				"領収書"
			};
			settled_segment(json!({"tags": tags, "title": title}), text).tags
		};
		assert_eq!(
			tags(
				json!([
					"Travel",
					"event",
					"france",
					"paris",
					"Port-Céleste",
					"mont venus"
				]),
				LEAFLET
			),
			["travel", "event", "port-céleste", "mont venus"]
		);
		assert_eq!(
			tags(
				json!(["electricity", "東京電力", "関西電力", "japan"]),
				BILL
			),
			["Electricity", "東京電力"]
		);
	}

	#[test]
	fn one_document_is_announced_and_fits_the_budget() {
		let mut input = input(&["あ".repeat(500).as_str(); 40]);
		input.one_document = true;
		let limits = Limits {
			text_budget_chars: 2000,
			..Limits::default()
		};
		let parts = chunk_parts(&input, &input.pages, &limits, false, true);
		let Part::Text(text) = &parts[0] else {
			panic!("no text part");
		};
		assert!(text.contains("exactly one segment"));
		assert!(text.contains("<page n=\"40\">"));
		assert!(text.matches('あ').count() <= 2000);
	}

	#[test]
	fn prompt_lists_the_topics() {
		let prompt = analyze_prompt();
		assert!(!prompt.contains(TOPICS_PLACEHOLDER));
		assert!(prompt.contains("receipt, invoice, bill"));
	}

	#[test]
	fn a_known_issuer_not_printed_on_the_page_is_not_borrowed() {
		let page = "植樹イベントのお知らせ\n3月2日 午前8時から";
		let borrowed = settled_segment(json!({"issuer": "NTT ドコモ"}), page);
		assert_eq!(borrowed.issuer, None);
		let printed = settled_segment(json!({"issuer": "東京電力"}), "東京電力 ご使用量のお知らせ");
		assert_eq!(printed.issuer.as_deref(), Some("東京電力"));
	}

	#[test]
	fn issuer_not_printed_on_the_page_lowers_confidence() {
		let page = "ＫＤＤＩ 株式会社\nご利用料金のお知らせ";
		let printed = settled_segment(json!({"issuer": "KDDI株式会社"}), page);
		assert_eq!(printed.issuer.as_deref(), Some("KDDI株式会社"));
		assert_eq!(printed.confidence, 0.8);

		let unprinted = settled_segment(json!({"issuer": "Sakura Clinic"}), page);
		assert_eq!(unprinted.issuer.as_deref(), Some("Sakura Clinic"));
		assert!((unprinted.confidence - 0.8 * UNPRINTED_ISSUER_CONFIDENCE).abs() < 1e-6);

		let doubts = |issuer: &str| {
			let input = input(&[page]);
			let mut answer = segment(&[1]);
			answer["issuer"] = json!(issuer);
			let answer = json!({"segments": [answer], "blank_pages": []});
			let scope = Scope::new(&input, &input.pages);
			settle(parse(&answer.to_string()).unwrap(), &scope).doubts
		};
		assert_eq!(doubts("KDDI株式会社"), Vec::<String>::new());
		assert_eq!(doubts("NTT ドコモ"), Vec::<String>::new());
		assert_eq!(doubts("Sakura Clinic").len(), 1);
		assert_eq!(doubts("ソフトバンク").len(), 1);

		let none = settled_segment(json!({"issuer": null}), page);
		assert_eq!((none.issuer, none.confidence), (None, 0.8));
	}

	#[test]
	fn addressees_and_addresses_are_not_issuers() {
		let page = "〒100-0005 東京都千代田区丸の内1－1－1 山田 太郎 様 株式会社サンプル 御中 東京都渋谷区役所";
		for rejected in [
			"山田 太郎 様",
			"山田太郎殿",
			"株式会社サンプル 御中",
			"To: Taro Yamada",
			"Dear Mr Yamada",
			"〒100-0005",
			"東京都千代田区丸の内1－1－1 サンプルビル3F",
			"神奈川県横浜市中区山下町一丁目",
			"長野県北佐久郡軽井沢町大字軽井沢番地",
			"千代田区丸の内 100-0005",
		] {
			let segment = settled_segment(json!({"issuer": rejected}), page);
			assert_eq!(segment.issuer, None, "{rejected}");
		}
		for kept in [
			"東京都渋谷区役所",
			"横浜市中区長",
			"京都府警察",
			"株式会社サンプル",
			"Tokyo Gas",
			"0120-995-881 サポート",
		] {
			let segment = settled_segment(json!({"issuer": kept}), page);
			assert_eq!(segment.issuer.as_deref(), Some(kept));
		}
	}

	#[test]
	fn page_with_text_is_not_blank() {
		let (analysis, problems) = settled(
			json!({"segments": [], "blank_pages": [1, 2, 3]}),
			&[
				"領収証 Apple Store, Shibuya 2013 01 05 合計 ¥31,219",
				"領収済 2024.04.10",
				"  \n ",
			],
		);
		assert_eq!(problems.len(), 4);
		assert!(problems[0].starts_with("segments is empty but the pages have text"));
		assert_eq!(
			problems[1..],
			[
				"page 1 is in blank_pages but has text; put it in a segment",
				"page 2 is in blank_pages but has text; put it in a segment",
				"pages missing from both segments and blank_pages: 1, 2",
			]
		);
		assert_eq!(analysis.blank_pages, [3]);
		assert_eq!(analysis.segments.len(), 1);
		assert_eq!(analysis.segments[0].pages, [1, 2]);
	}

	fn settle_with(answer: Value, input: &AnalyzeInput) -> Settled {
		let scope = Scope::new(input, &input.pages);
		settle(parse(&answer.to_string()).unwrap(), &scope)
	}

	#[test]
	fn no_segment_for_pages_with_text_is_degenerate() {
		let empty = json!({"segments": [], "blank_pages": []});
		let settled = settle_with(empty.clone(), &input(&["Statement July 2018", ""]));
		assert!(settled.degenerate);
		assert!(settled.problems[0].starts_with("segments is empty"));

		let blank = settle_with(
			json!({"segments": [], "blank_pages": [1, 2]}),
			&input(&["", " "]),
		);
		assert!(!blank.degenerate);
		assert_eq!(blank.problems, Vec::<String>::new());
		assert!(blank.analysis.segments.is_empty());

		let unlisted = settle_with(empty, &input(&["", ""]));
		assert!(unlisted.degenerate);

		let answered = settle_with(
			json!({"segments": [segment(&[1])], "blank_pages": []}),
			&input(&["領収書"]),
		);
		assert!(!answered.degenerate);
	}

	#[test]
	fn a_photo_is_never_blank() {
		let mut photo = input(&[""]);
		photo.photo = true;
		let settled = settle_with(json!({"segments": [], "blank_pages": [1]}), &photo);
		assert!(settled.degenerate);
		assert_eq!(
			settled.problems,
			[
				"segments is empty: the image is the document; answer with one segment for it",
				"page 1 is in blank_pages but it is the photo; put it in a segment",
				"pages missing from both segments and blank_pages: 1",
			]
		);

		let mut answer = segment(&[1]);
		answer["title"] = json!("Sea view from a balcony");
		answer["doc_type"] = json!("photo");
		answer["area"] = json!("Personal");
		let settled = settle_with(json!({"segments": [answer], "blank_pages": []}), &photo);
		assert_eq!(settled.problems, Vec::<String>::new());
		let segment = &settled.analysis.segments[0];
		assert_eq!(
			(segment.title.as_str(), segment.doc_type.as_str()),
			("Sea view from a balcony", "photo")
		);

		let parts = chunk_parts(&photo, &photo.pages, &Limits::default(), false, true);
		let Part::Text(text) = &parts[0] else {
			panic!("no text part");
		};
		assert!(text.contains("No text could be read"));
		assert!(!text.contains("These pages are one document"));
	}

	#[test]
	fn unusable_json_is_refused() {
		assert!(parse("I cannot do that").is_err());
		assert!(parse("{\"documents\": []}").is_err());
		assert!(parse("{\"segments\": [{\"pages\": \"1-3\"}]}").is_err());
		assert!(parse("```json\n{\"segments\": null, \"blank_pages\": null}\n```").is_ok());
	}

	#[test]
	fn known_values_in_the_text_come_first() {
		let values: Vec<String> = ["Alpha", "ＮＴＴ ドコモ", "", "Gamma"]
			.map(String::from)
			.into();
		assert_eq!(
			known_line(&values, &fold_name("ご請求 NTTドコモ")).as_deref(),
			Some("ＮＴＴ ドコモ; Alpha; Gamma")
		);
		assert_eq!(known_line(&[], ""), None);
	}

	#[test]
	fn schema_requires_every_property_in_reading_order() {
		let schema = serde_json::to_string(&analysis_schema(false)).unwrap();
		let order: Vec<usize> = [
			"\"segments\":{",
			"\"pages\":{",
			"\"title\":{",
			"\"issuer\":{",
			"\"date_printed\":{",
			"\"total_printed\":{",
			"\"facts\":{",
			"\"key\":{",
			"\"value\":{",
			"\"amount\":{",
			"\"confidence\":{",
			"\"blank_pages\":{",
		]
		.iter()
		.map(|property| schema.find(property).expect(property))
		.collect();
		assert!(order.is_sorted(), "{schema}");

		let schema: Value = serde_json::from_str(&schema).unwrap();
		let segment = &schema["properties"]["segments"]["items"];
		assert_eq!(schema["required"], json!(["segments", "blank_pages"]));
		assert_eq!(segment["required"].as_array().map(Vec::len), Some(14));
		assert_eq!(segment["additionalProperties"], json!(false));
		assert_eq!(
			segment["properties"]["date_printed"]["type"],
			json!(["string", "null"])
		);
		assert_eq!(
			segment["properties"]["rotations"]["items"]["properties"]["degrees"]["enum"],
			json!([90, 180, 270])
		);
		assert_eq!(segment["properties"]["facts"]["maxItems"], json!(16));
		assert!(segment["properties"]["pages"].get("maxItems").is_none());
		assert!(schema["properties"]["segments"].get("minItems").is_none());

		let required: Value = serde_json::to_value(analysis_schema(true)).unwrap();
		assert_eq!(required["properties"]["segments"]["minItems"], json!(1));
		assert!(
			required["properties"]["blank_pages"]
				.get("minItems")
				.is_none()
		);
	}
}
