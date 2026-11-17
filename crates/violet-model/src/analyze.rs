use std::collections::HashSet;

use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};
use violet_core::Fact;

use crate::amount::{decimals, normalize_currency, parse_amount};
use crate::chat::{Message, Part, Request};
use crate::date::normalize_date;
use crate::text::{collapse_whitespace, extract_json, fold_name, truncate_head, truncate_middle};
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

fn boundary_schema() -> Value {
	json!({
		"type": "object",
		"properties": {"new_document": {"type": "boolean"}},
		"required": ["new_document"],
		"additionalProperties": false,
	})
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
	};
	let answer = client.complete(&request).await?;
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

fn analysis_schema() -> Value {
	let nullable_string = json!({"type": ["string", "null"]});
	let pages = json!({"type": "array", "items": {"type": "integer"}});
	let strings = json!({"type": "array", "items": {"type": "string"}});
	let object = |properties: Value| {
		let required: Vec<String> = properties
			.as_object()
			.map(|map| map.keys().cloned().collect())
			.unwrap_or_default();
		json!({
			"type": "object",
			"properties": properties,
			"required": required,
			"additionalProperties": false,
		})
	};
	let rotation = object(json!({
		"page": {"type": "integer"},
		"degrees": {"type": "integer", "enum": [90, 180, 270]},
	}));
	let fact = object(json!({
		"key": {"type": "string"},
		"value": {"type": "string"},
		"amount": {"type": ["number", "null"]},
		"currency": nullable_string,
	}));
	let segment = object(json!({
		"pages": pages,
		"rotations": {"type": "array", "items": rotation},
		"title": {"type": "string"},
		"area": {"type": "string"},
		"doc_type": {"type": "string"},
		"issuer": nullable_string,
		"date": nullable_string,
		"language": nullable_string,
		"summary": nullable_string,
		"tags": strings,
		"facts": {"type": "array", "items": fact},
		"confidence": {"type": "number"},
	}));
	object(json!({
		"segments": {"type": "array", "items": segment},
		"blank_pages": pages,
	}))
}

struct Scope<'a> {
	input: &'a AnalyzeInput,
	pages: Vec<u32>,
}

async fn analyze_chunk(
	client: &Client,
	input: &AnalyzeInput,
	chunk: &[PageInput],
	windowed: bool,
) -> Result<Analysis> {
	let scope = Scope {
		input,
		pages: chunk.iter().map(|page| page.number).collect(),
	};
	let schema = analysis_schema();
	let mut messages = vec![
		Message::text("system", ANALYZE_PROMPT.trim()),
		Message {
			role: "user",
			parts: chunk_parts(input, chunk, &client.limits, windowed),
		},
	];
	let mut repaired = None;
	let mut complaint = String::new();
	for attempt in 0..2 {
		let request = Request {
			messages: &messages,
			schema: Some(("analysis", &schema)),
			max_tokens: ANALYZE_MAX_TOKENS,
			images_optional: true,
		};
		let answer = client.complete(&request).await?;
		let problems = match parse(&answer) {
			Ok(raw) => {
				let (analysis, problems) = settle(raw, &scope);
				if problems.is_empty() {
					return Ok(analysis);
				}
				repaired = Some(analysis);
				problems
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
	date: Option<String>,
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

/// Checks the model's answer against the pages it was given and returns a valid analysis
/// together with every rule the answer broke. The analysis is already repaired: broken
/// values are dropped or replaced and unassigned pages join the document before them.
fn settle(raw: RawAnalysis, scope: &Scope<'_>) -> (Analysis, Vec<String>) {
	let mut problems = Vec::new();
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
		));
	}

	let mut blank_pages = Vec::new();
	for page in raw.blank_pages {
		match scope.page(page) {
			None => problems.push(format!(
				"blank_pages: page {page} is not one of the listed pages"
			)),
			Some(page) if blank_pages.contains(&page) => {}
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
	(
		Analysis {
			segments,
			blank_pages,
		},
		problems,
	)
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

impl Scope<'_> {
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

	let issuer = raw
		.issuer
		.map(|issuer| collapse_whitespace(&issuer))
		.filter(|issuer| !issuer.is_empty() && !is_null_word(issuer))
		.map(|issuer| known(&issuer, &input.known_issuers).unwrap_or(issuer));

	let date = raw
		.date
		.filter(|date| !date.trim().is_empty() && !is_null_word(date))
		.and_then(|printed| {
			let date = normalize_date(&printed);
			if date.is_none() {
				problems.push(format!(
					"segment {label}: date \"{printed}\" is not a real YYYY-MM-DD date"
				));
			}
			date
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

	let facts = raw
		.facts
		.into_iter()
		.filter_map(|fact| settle_fact(fact, label, problems))
		.take(MAX_FACTS)
		.collect();

	let confidence = match raw.confidence {
		Some(confidence) if confidence.is_finite() => confidence.clamp(0.0, 1.0) as f32,
		_ => DEFAULT_CONFIDENCE,
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

/// The known value that is the same name apart from case, width and spacing.
fn known(value: &str, known_values: &[String]) -> Option<String> {
	let key = fold_name(value);
	known_values
		.iter()
		.find(|candidate| fold_name(candidate) == key)
		.cloned()
}

fn settle_area(raw: &str, known_areas: &[String]) -> Option<String> {
	let area = collapse_whitespace(raw);
	if area.is_empty() {
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
	use super::*;

	fn input(pages: u32) -> AnalyzeInput {
		AnalyzeInput {
			file_name: "scan 0042.pdf".into(),
			pages: (1..=pages)
				.map(|number| PageInput {
					number,
					text: format!("page {number}"),
					thumbnail_png: None,
				})
				.collect(),
			known_areas: vec!["Health".into(), "Tax".into()],
			known_issuers: vec!["NTTドコモ".into(), "東京電力".into()],
			known_tags: vec!["Electricity".into()],
		}
	}

	fn settled(answer: Value, pages: u32) -> (Analysis, Vec<String>) {
		let input = input(pages);
		let scope = Scope {
			input: &input,
			pages: (1..=pages).collect(),
		};
		settle(parse(&answer.to_string()).unwrap(), &scope)
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
					"date": "令和6年3月14日",
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
			3,
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
			6,
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
		let (analysis, problems) = settled(json!({"segments": [], "blank_pages": [2]}), 3);
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
					"date": "2024-02-30",
					"facts": [{"key": "total", "value": "12,340円", "amount": 1234, "currency": "JPY"}],
				}],
				"blank_pages": [],
			}),
			1,
		);
		assert_eq!(
			problems,
			[
				"segment 1: rotation of 45 degrees is not 90, 180 or 270",
				"segment 1: rotation for page 2, which is not in this segment",
				"segment 1: date \"2024-02-30\" is not a real YYYY-MM-DD date",
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
	fn schema_requires_every_property() {
		let schema = analysis_schema();
		let segment = &schema["properties"]["segments"]["items"];
		assert_eq!(schema["required"], json!(["blank_pages", "segments"]));
		assert_eq!(segment["required"].as_array().map(Vec::len), Some(12));
		assert_eq!(segment["additionalProperties"], json!(false));
		assert_eq!(
			segment["properties"]["date"]["type"],
			json!(["string", "null"])
		);
	}
}
