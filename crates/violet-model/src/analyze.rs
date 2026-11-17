use std::collections::HashSet;

use serde::{Deserialize, Deserializer};
use serde_json::Value;
use violet_core::Fact;

use crate::amount::{decimals, normalize_currency, parse_amount};
use crate::chat::{Message, Part, Request, Schema};
use crate::date::{normalize_date, read_date};
use crate::text::{
	collapse_whitespace, extract_json, fold_name, fold_width, truncate_head, truncate_middle,
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
/// The issuer in the prompt's example answer, which a model sometimes repeats.
const EXAMPLE_ISSUER: &str = "株式会社サンプル";
const MIN_TEXT_CHARS: usize = 30;
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
	let windowed = text_chars > limits.text_budget_chars;
	let chunks = if windowed {
		split_documents(client, &input.pages).await?
	} else {
		vec![&input.pages[..]]
	};
	for chunk in chunks {
		let part = analyze_chunk(client, input, chunk, windowed).await?;
		analysis.segments.extend(part.segments);
		analysis.blank_pages.extend(part.blank_pages);
	}
	analysis.blank_pages.sort_unstable();
	Ok(analysis)
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

fn analysis_schema() -> Schema {
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
		("segments", Schema::array(segment)),
		("blank_pages", Schema::array(Schema::Integer)),
	])
}

struct Scope<'a> {
	input: &'a AnalyzeInput,
	pages: Vec<u32>,
	/// Text of each page, full-width forms folded and whitespace collapsed, to check that what
	/// the model reports is printed there.
	texts: Vec<(u32, String)>,
}

async fn analyze_chunk(
	client: &Client,
	input: &AnalyzeInput,
	chunk: &[PageInput],
	windowed: bool,
) -> Result<Analysis> {
	let scope = Scope::new(input, chunk);
	let schema = analysis_schema();
	let mut messages = vec![
		Message::text("system", ANALYZE_PROMPT.trim()),
		Message {
			role: "user",
			parts: chunk_parts(input, chunk, &client.limits, windowed),
		},
	];
	let mut repaired = None;
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
					return Ok(settled.analysis);
				}
				if valid {
					unconfirmed = Some(settled.analysis);
				} else {
					repaired = Some(settled.analysis);
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
		return Ok(analysis);
	}
	let mut analysis = repaired
		.ok_or_else(|| Error::Answer(format!("no usable analysis after a retry:\n{complaint}")))?;
	for segment in &mut analysis.segments {
		segment.confidence *= REPAIRED_CONFIDENCE;
	}
	Ok(analysis)
}

fn chunk_parts(
	input: &AnalyzeInput,
	chunk: &[PageInput],
	limits: &Limits,
	windowed: bool,
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
	let page_chars = if total > limits.text_budget_chars {
		(limits.text_budget_chars / chunk.len())
			.max(MIN_PAGE_CHARS)
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
	for (label, values) in [
		("Known areas", &input.known_areas),
		("Known issuers", &input.known_issuers),
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

	let mut blank_pages = Vec::new();
	for page in raw.blank_pages {
		match scope.page(page) {
			None => problems.push(format!(
				"blank_pages: page {page} is not one of the listed pages"
			)),
			Some(page) if blank_pages.contains(&page) => {}
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
			texts: chunk
				.iter()
				.map(|page| (page.number, collapse_whitespace(&fold_width(&page.text))))
				.collect(),
		}
	}

	fn has_text(&self, page: u32) -> bool {
		self.texts
			.iter()
			.any(|(number, text)| *number == page && text.chars().count() >= MIN_TEXT_CHARS)
	}

	fn text_of(&self, pages: &[u32]) -> String {
		self.texts
			.iter()
			.filter(|(number, _)| pages.contains(number))
			.map(|(_, text)| text.as_str())
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

	let stated_issuer = raw
		.issuer
		.map(|issuer| collapse_whitespace(&issuer))
		.filter(|issuer| !issuer.is_empty() && !is_null_word(issuer))
		.filter(|issuer| !is_addressee(issuer) && !is_address(issuer));
	let issuer_printed = stated_issuer
		.as_deref()
		.is_none_or(|issuer| is_printed(issuer, &printed));
	let stated_issuer = stated_issuer
		.filter(|issuer| issuer_printed || fold_name(issuer) != fold_name(EXAMPLE_ISSUER));
	if let Some(issuer) = stated_issuer
		.as_deref()
		.filter(|issuer| !issuer_printed && known(issuer, &input.known_issuers).is_none())
	{
		doubts.push(format!(
			"segment {label}: issuer \"{issuer}\" is not in the page text; copy the name of the sender exactly as printed, or use null when the page does not name it"
		));
	}
	let issuer = stated_issuer.map(|issuer| known(&issuer, &input.known_issuers).unwrap_or(issuer));

	let date = raw
		.date_printed
		.filter(|date| !date.trim().is_empty() && !is_null_word(date))
		.and_then(|stated| {
			let date = normalize_date(&stated).or_else(|| {
				let stated = collapse_whitespace(&fold_width(&stated));
				Some(printed_dates(&stated).into_iter().next()?.date)
			});
			if date.is_none() {
				problems.push(format!(
					"segment {label}: date_printed \"{stated}\" cannot be read as a date; copy the date exactly as printed or use null"
				));
			}
			date.filter(|date| {
				printed_dates(&spaced)
					.iter()
					.any(|printed| printed.date == *date && !printed.qualified)
			})
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
		truncate_head(&bare, MAX_TITLE_CHARS).trim_end().to_string()
	};

	let mut tags: Vec<String> = Vec::new();
	for tag in raw.tags {
		let tag = collapse_whitespace(&tag);
		let tag = known(&tag, &input.known_tags).unwrap_or_else(|| tag.to_lowercase());
		if !tag.is_empty() && !tags.contains(&tag) && tags.len() < MAX_TAGS {
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
		language: raw.language.as_deref().and_then(settle_language),
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

struct PrintedDate {
	date: String,
	/// The date starts or ends a period ("2024年4月1日以降", "～ 2026年3月31日") or carries a
	/// label that makes it something else than the day the document was issued ("有効期限").
	qualified: bool,
}

/// Every date in the text, in any notation, by position and longest reading first. A
/// candidate that is the start of a longer number or date does not count.
fn printed_dates(spaced_text: &str) -> Vec<PrintedDate> {
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
			let Some((date, day_printed)) = read_date(&candidate) else {
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
			let qualified = PERIOD_MARKS
				.iter()
				.any(|mark| after.starts_with(mark) || before.ends_with(mark))
				|| OTHER_DATE_LABELS
					.iter()
					.any(|label| before.ends_with(label));
			dates.push(PrintedDate { date, qualified });
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
		let date =
			|stated: &str, text: &str| settled_segment(json!({"date_printed": stated}), text).date;
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
	fn example_issuer_is_not_an_answer() {
		assert!(ANALYZE_PROMPT.contains(EXAMPLE_ISSUER));
		let echoed = settled_segment(json!({"issuer": EXAMPLE_ISSUER}), "契約明細書");
		assert_eq!(echoed.issuer, None);
		let printed = settled_segment(
			json!({"issuer": EXAMPLE_ISSUER}),
			"株式会社サンプル 御請求書",
		);
		assert_eq!(printed.issuer.as_deref(), Some(EXAMPLE_ISSUER));
	}

	#[test]
	fn issuer_not_printed_on_the_page_lowers_confidence() {
		let page = "ＫＤＤＩ 株式会社\nご利用料金のお知らせ";
		let printed = settled_segment(json!({"issuer": "KDDI株式会社"}), page);
		assert_eq!(printed.issuer.as_deref(), Some("KDDI株式会社"));
		assert_eq!(printed.confidence, 0.8);

		let unprinted = settled_segment(json!({"issuer": "NTT ドコモ"}), page);
		assert_eq!(unprinted.issuer.as_deref(), Some("NTTドコモ"));
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
			json!({"segments": [], "blank_pages": [1, 2]}),
			&[
				"領収証 Apple Store, Shibuya 2013 01 05 合計 ¥31,219",
				"裏面",
			],
		);
		assert_eq!(
			problems,
			[
				"page 1 is in blank_pages but has text; put it in a segment",
				"pages missing from both segments and blank_pages: 1",
			]
		);
		assert_eq!(analysis.blank_pages, [2]);
		assert_eq!(analysis.segments.len(), 1);
		assert_eq!(analysis.segments[0].pages, [1]);
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
		let schema = serde_json::to_string(&analysis_schema()).unwrap();
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
	}
}
