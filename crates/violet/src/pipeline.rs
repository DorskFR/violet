//! One inbox file to filed documents: read, OCR what has no text, analyse, split, rotate,
//! name, file, index, journal. And a filed document analysed again from the text it has,
//! read again from its file, or given another file.

use std::collections::HashSet;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use uuid::Uuid;
use violet_core::{Action, PageText, Sidecar, Source, TextSource, hash, naming, sidecar};
use violet_model::{AnalyzeInput, Client, Limits, OCR_TILE_PIXELS, PageInput, Rotation, Segment};

use crate::app::blocking;
use crate::extract::{self, Extracted};
use crate::ops::{self, AGENT, Changed, DocumentView};
use crate::{App, Error, Result};

const THUMBNAIL_SIDE: u32 = 512;
const CUT_OFF_CONFIDENCE: f32 = 0.6;
const NEAR_DUPLICATE: f32 = 0.9;
const NEAR_DUPLICATE_TAG: &str = "near-duplicate";
const FALLBACK_AREA: &str = "Unsorted";
const FALLBACK_DOC_TYPE: &str = "document";
const NO_TEXT_TAG: &str = "no-text";

#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
	Filed {
		entry: Uuid,
		documents: Vec<(Uuid, PathBuf)>,
	},
	/// The same bytes were ingested before; the file was only stashed.
	Duplicate { entry: Uuid, of: Uuid },
	/// The file is no longer in the inbox.
	Missing,
}

struct Input {
	path: PathBuf,
	name: String,
	bytes: Arc<Vec<u8>>,
	sha256: String,
	/// Documents from this file are already in the library without an ingest entry: an
	/// earlier run stopped half way.
	resume: bool,
}

enum Start {
	Done(Outcome),
	Fresh(Input),
}

enum Content {
	/// Split and rotated as needed; `converted` when the PDF was made from an image.
	Pdf { pdf: Arc<Vec<u8>>, converted: bool },
	/// Stored as it is, with text taken out of it.
	Text { ext: String, extracted: Extracted },
}

struct Page {
	number: u32,
	text: String,
	source: TextSource,
	/// The OCR ran into its output limit, so the end of the page may be missing.
	cut_off: bool,
}

struct Plan {
	pages: Vec<u32>,
	rotations: Vec<Rotation>,
	title: String,
	area: String,
	doc_type: String,
	issuer: Option<String>,
	date: Option<String>,
	language: Option<String>,
	summary: Option<String>,
	tags: Vec<String>,
	facts: Vec<violet_core::Fact>,
	confidence: f32,
}

/// Processes `relative` (a path under `inbox/`) to the end.
pub async fn ingest(app: &Arc<App>, relative: &Path) -> Result<Outcome> {
	let relative = relative.to_path_buf();
	let input = match blocking(app, move |app| start(app, &relative)).await? {
		Start::Done(outcome) => return Ok(outcome),
		Start::Fresh(input) => input,
	};
	tracing::info!(file = input.name, sha256 = input.sha256, "ingesting");
	let content = read(&input.name, input.bytes.clone()).await?;
	let (pages, plans, blank) = understand(app, &input.name, &content, false).await?;
	blocking(app, move |app| {
		apply(app, &input, &content, &pages, plans, blank)
	})
	.await
}

fn inbox_path(app: &App, relative: &Path) -> Result<PathBuf> {
	let normal = relative
		.components()
		.all(|component| matches!(component, Component::Normal(_)));
	if !normal || relative.as_os_str().is_empty() {
		return Err(Error::Invalid(format!(
			"{} is not a path inside the inbox",
			relative.display()
		)));
	}
	Ok(app.library.inbox().join(relative))
}

fn start(app: &App, relative: &Path) -> Result<Start> {
	let path = inbox_path(app, relative)?;
	if !path.is_file() {
		return Ok(Start::Done(Outcome::Missing));
	}
	let bytes = fs::read(&path)?;
	let sha256 = hash::sha256(&bytes);
	let name = path
		.file_name()
		.map(|name| name.to_string_lossy().into_owned())
		.unwrap_or_default();
	let index = app.index();
	let known = index.find_by_sha256(&sha256)?;
	if let Some(of) = known
		&& ingested(app, &sha256)?
	{
		let original = app.library.stash_original(&path)?;
		let summary = format!("\"{name}\" is a duplicate, kept aside");
		let action = Action::Duplicate {
			original,
			original_name: name,
			of,
		};
		let entry = ops::record(app, AGENT, action, summary)?;
		return Ok(Start::Done(Outcome::Duplicate {
			entry: entry.id,
			of,
		}));
	}
	Ok(Start::Fresh(Input {
		path,
		name,
		bytes: Arc::new(bytes),
		sha256,
		resume: known.is_some(),
	}))
}

/// Whether the journal has an ingest of this content that was not undone.
fn ingested(app: &App, sha256: &str) -> Result<bool> {
	let entries = app.journal().entries()?;
	let undone: HashSet<Uuid> = entries
		.iter()
		.filter_map(|entry| match entry.action {
			Action::Undo { entry } => Some(entry),
			_ => None,
		})
		.collect();
	Ok(entries.iter().any(|entry| match &entry.action {
		Action::Ingest { original, .. } => {
			!undone.contains(&entry.id) && original.split('.').next() == Some(sha256)
		}
		_ => false,
	}))
}

/// What a file named `name` holds, from its bytes first and then its extension. A format
/// violet cannot read is an error, so that it is never filed without its text.
async fn read(name: &str, bytes: Arc<Vec<u8>>) -> Result<Content> {
	let ext = extract::extension(name);
	tokio::task::spawn_blocking(move || {
		let kind = violet_pdf::detect(&bytes);
		let ext = match extract::sniff(&bytes) {
			Some(sniffed) if ext.is_empty() => sniffed.to_string(),
			_ => ext,
		};
		Ok(match kind {
			violet_pdf::Kind::Pdf => Content::Pdf {
				pdf: bytes,
				converted: false,
			},
			kind if kind.is_image() => Content::Pdf {
				pdf: Arc::new(violet_pdf::image_to_pdf(&bytes)?),
				converted: true,
			},
			_ if extract::is_office(&ext) => Content::Text {
				extracted: extract::office(&ext, &bytes)?,
				ext,
			},
			_ if ext == "json" => Content::Text {
				extracted: extract::plain(&ext, &bytes),
				ext: "json.txt".into(),
			},
			_ if extract::is_text(&ext) => Content::Text {
				extracted: extract::plain(&ext, &bytes),
				ext,
			},
			_ if ext.is_empty() => {
				return Err(Error::Invalid(
					"unsupported format: no extension and not a mail".into(),
				));
			}
			_ => return Err(Error::Invalid(format!("unsupported format .{ext}"))),
		})
	})
	.await?
}

/// The pages of the content, and the documents and the number of blank pages the analysis
/// finds in them. Text content is always one document without pages of its own.
async fn understand(
	app: &App,
	name: &str,
	content: &Content,
	one_document: bool,
) -> Result<(Vec<Page>, Vec<Plan>, usize)> {
	match content {
		Content::Pdf { pdf, .. } => {
			let (pages, thumbnails) = read_pages(app, pdf).await?;
			let (plans, blank) = analyze(app, name, &pages, thumbnails, None, one_document).await?;
			Ok((pages, plans, blank))
		}
		Content::Text { extracted, .. } => {
			let pages = vec![Page {
				number: 1,
				text: extracted.text.clone(),
				source: TextSource::Extracted,
				cut_off: false,
			}];
			let mut plans = if extracted.text.trim().is_empty() {
				tracing::warn!(file = name, "no text in the file, filing it under its name");
				let mut plan = fallback(name, &pages, Some(extracted));
				plan.tags.push(NO_TEXT_TAG.into());
				vec![plan]
			} else {
				analyze(app, name, &pages, Vec::new(), Some(extracted), true)
					.await?
					.0
			};
			plans.truncate(1);
			plans[0].pages.clear();
			plans[0].rotations.clear();
			Ok((pages, plans, 0))
		}
	}
}

async fn render(pdf: &Arc<Vec<u8>>, page: u32, side: u32) -> Result<Vec<u8>> {
	let pdf = pdf.clone();
	Ok(tokio::task::spawn_blocking(move || violet_pdf::render_png(&pdf, page, side)).await??)
}

/// The text of every page, from the text layer or by OCR, and the thumbnails to show the
/// analyst.
async fn read_pages(app: &App, pdf: &Arc<Vec<u8>>) -> Result<(Vec<Page>, Vec<(u32, Vec<u8>)>)> {
	let layers = {
		let pdf = pdf.clone();
		tokio::task::spawn_blocking(move || -> Result<_> {
			let count = violet_pdf::page_count(&pdf)?;
			let mut layers = violet_pdf::text_layer(&pdf)?;
			layers.resize(count as usize, None);
			Ok(layers)
		})
		.await??
	};
	let mut pages = Vec::with_capacity(layers.len());
	for (position, layer) in layers.into_iter().enumerate() {
		let number = position as u32 + 1;
		let (text, source, cut_off) = match (layer, &app.vision) {
			(Some(text), _) => (text, TextSource::Layer, false),
			(None, Some(vision)) => {
				let tiles = {
					let pdf = pdf.clone();
					tokio::task::spawn_blocking(move || {
						violet_pdf::render_tiles(&pdf, number, OCR_TILE_PIXELS)
					})
					.await??
				};
				let read = vision.ocr_page(&tiles).await?;
				if read.truncated {
					tracing::warn!(page = number, "the transcription was cut off");
				}
				(read.text, TextSource::Ocr, read.truncated)
			}
			(None, None) => (String::new(), TextSource::None, false),
		};
		pages.push(Page {
			number,
			text,
			source,
			cut_off,
		});
	}
	let thumbnails = if app.analyst().is_some() {
		thumbnails(pdf, &pages).await?
	} else {
		Vec::new()
	};
	Ok((pages, thumbnails))
}

fn fallback(name: &str, pages: &[Page], extracted: Option<&Extracted>) -> Plan {
	let stem = match name.rsplit_once('.') {
		Some((stem, _)) if !stem.is_empty() => stem,
		_ => name,
	};
	let carried = extracted.and_then(|extracted| extracted.title.as_deref());
	Plan {
		pages: pages.iter().map(|page| page.number).collect(),
		rotations: Vec::new(),
		title: naming::sanitize(carried.unwrap_or(stem)),
		area: FALLBACK_AREA.into(),
		doc_type: FALLBACK_DOC_TYPE.into(),
		issuer: None,
		date: extracted
			.and_then(|extracted| extracted.date.clone())
			.or_else(|| extract::date_in_name(stem)),
		language: None,
		summary: None,
		tags: Vec::new(),
		facts: Vec::new(),
		confidence: 0.0,
	}
}

fn plan(segment: Segment, count: u32) -> Plan {
	let mut seen = HashSet::new();
	let pages: Vec<u32> = segment
		.pages
		.into_iter()
		.filter(|page| (1..=count).contains(page) && seen.insert(*page))
		.collect();
	Plan {
		rotations: segment
			.rotations
			.into_iter()
			.filter(|rotation| pages.contains(&rotation.page) && rotation.degrees % 90 == 0)
			.collect(),
		pages,
		title: segment.title,
		area: segment.area,
		doc_type: segment.doc_type,
		issuer: segment.issuer,
		date: segment.date,
		language: segment.language,
		summary: segment.summary,
		tags: segment.tags,
		facts: segment.facts,
		confidence: segment.confidence.clamp(0.0, 1.0),
	}
}

/// What to file and how many blank pages to drop. Without a model, the whole file becomes
/// one document named after the file. Pages without any text are shown to the model as a
/// photo. An analysis that keeps no page is an error: the file stays in the inbox.
async fn analyze(
	app: &App,
	name: &str,
	pages: &[Page],
	thumbnails: Vec<(u32, Vec<u8>)>,
	extracted: Option<&Extracted>,
	one_document: bool,
) -> Result<(Vec<Plan>, usize)> {
	let Some(analyst) = app.analyst() else {
		return Ok((vec![fallback(name, pages, extracted)], 0));
	};
	let photo = !thumbnails.is_empty() && pages.iter().all(|page| page.text.trim().is_empty());
	let plans = plans(app, analyst, name, pages, thumbnails, one_document, photo).await?;
	if plans.is_empty() {
		return Err(Error::Internal(format!(
			"the analysis of {name} kept no page"
		)));
	}
	let kept: usize = plans.iter().map(|plan| plan.pages.len()).sum();
	Ok((plans, pages.len().saturating_sub(kept)))
}

/// The documents the model sees in the pages; none when it keeps no page.
async fn plans(
	app: &App,
	analyst: &Client,
	name: &str,
	pages: &[Page],
	thumbnails: Vec<(u32, Vec<u8>)>,
	one_document: bool,
	photo: bool,
) -> Result<Vec<Plan>> {
	let (known_areas, known_issuers, known_tags) = {
		let index = app.index();
		let areas = index
			.areas()?
			.into_iter()
			.filter(|area| area != FALLBACK_AREA)
			.collect();
		(areas, index.issuers()?, index.tags()?)
	};
	let mut thumbnails = thumbnails;
	let input = AnalyzeInput {
		file_name: name.to_string(),
		pages: pages
			.iter()
			.map(|page| PageInput {
				number: page.number,
				text: page.text.clone(),
				thumbnail_png: thumbnails
					.iter_mut()
					.find(|(number, _)| *number == page.number)
					.map(|(_, png)| std::mem::take(png)),
			})
			.collect(),
		one_document: one_document || photo,
		photo,
		known_areas,
		known_issuers,
		known_tags,
	};
	let analysis = analyst.analyze(&input).await?;
	let count = pages.len() as u32;
	Ok(analysis
		.segments
		.into_iter()
		.map(|segment| plan(segment, count))
		.filter(|plan| !plan.pages.is_empty())
		.map(|mut plan| {
			let cut_off = pages
				.iter()
				.any(|page| page.cut_off && plan.pages.contains(&page.number));
			if cut_off {
				plan.confidence *= CUT_OFF_CONFIDENCE;
			}
			plan
		})
		.collect())
}

async fn thumbnails(pdf: &Arc<Vec<u8>>, pages: &[Page]) -> Result<Vec<(u32, Vec<u8>)>> {
	let shown = if pages.len() <= Limits::default().max_thumbnails {
		pages.len()
	} else {
		1
	};
	let mut thumbnails = Vec::with_capacity(shown);
	for page in pages.iter().take(shown) {
		thumbnails.push((page.number, render(pdf, page.number, THUMBNAIL_SIDE).await?));
	}
	Ok(thumbnails)
}

/// Analyses a filed document again from the page texts it already has, without reading the
/// pages anew, and replaces its metadata: the file and its pages stay as they are, the name
/// and the folder follow. The document is never split; when the model sees several
/// documents in it, the one with the most pages decides. The index lock is not held while
/// the model works, so the change is refused if the document changed in the meantime.
pub async fn reanalyze(app: &Arc<App>, id: Uuid) -> Result<Changed<DocumentView>> {
	let Some(analyst) = app.analyst() else {
		return Err(Error::Invalid("no model is configured".into()));
	};
	let (absolute, path, analysed) = blocking(app, move |app| ops::document_file(app, id)).await?;
	let pages: Vec<Page> = analysed
		.pages
		.iter()
		.map(|page| Page {
			number: page.number,
			text: page.text.clone(),
			source: page.source,
			cut_off: false,
		})
		.collect();
	if pages.iter().all(|page| page.text.trim().is_empty()) {
		return Err(Error::Invalid(format!(
			"{} has no text to read again",
			path.display()
		)));
	}
	let shown = if ops::extension_of(&path) == "pdf" {
		let pdf = Arc::new(tokio::fs::read(&absolute).await?);
		match thumbnails(&pdf, &pages).await {
			Ok(shown) => shown,
			Err(error) => {
				tracing::warn!(document = %id, %error, "no thumbnails, analysing the text alone");
				Vec::new()
			}
		}
	} else {
		Vec::new()
	};
	let name = source_name(&analysed, &path);
	let plans = plans(app, analyst, &name, &pages, shown, true, false).await?;
	let plan = decided(plans, &path)?;
	let sidecar = with_plan(&analysed, plan);
	blocking(app, move |app| ops::apply_analysis(app, &analysed, sidecar)).await
}

/// The plan with the most pages: the one document a file read as one is.
fn decided(plans: Vec<Plan>, path: &Path) -> Result<Plan> {
	let mut decided: Option<Plan> = None;
	for plan in plans {
		if decided
			.as_ref()
			.is_none_or(|kept| plan.pages.len() > kept.pages.len())
		{
			decided = Some(plan);
		}
	}
	decided
		.ok_or_else(|| Error::Internal(format!("the analysis of {} kept no page", path.display())))
}

/// The sidecar with the metadata of `plan`, keeping the near-duplicate mark.
fn with_plan(analysed: &Sidecar, plan: Plan) -> Sidecar {
	let mut sidecar = analysed.clone();
	let mut tags = plan.tags;
	if analysed.tags.iter().any(|tag| tag == NEAR_DUPLICATE_TAG)
		&& !tags.iter().any(|tag| tag == NEAR_DUPLICATE_TAG)
	{
		tags.push(NEAR_DUPLICATE_TAG.into());
	}
	sidecar.title = plan.title;
	sidecar.area = plan.area;
	sidecar.doc_type = plan.doc_type;
	sidecar.issuer = plan.issuer;
	sidecar.date = plan.date;
	sidecar.language = plan.language;
	sidecar.summary = plan.summary;
	sidecar.tags = tags;
	sidecar.facts = plan.facts;
	sidecar.confidence = plan.confidence;
	sidecar
}

fn source_name(sidecar: &Sidecar, path: &Path) -> String {
	match sidecar.sources.first() {
		Some(source) if !source.original_name.is_empty() => source.original_name.clone(),
		_ => path
			.file_name()
			.map(|name| name.to_string_lossy().into_owned())
			.unwrap_or_default(),
	}
}

/// Reads the file of a filed document again from the start (text layer or OCR, then the
/// analysis as one document) and files it again under the same id. An image or a mail kept
/// as it was becomes the PDF or the mail it is read as now. Journaled, undoable.
pub async fn reread(app: &Arc<App>, id: Uuid) -> Result<Changed<DocumentView>> {
	let (absolute, path, analysed) = blocking(app, move |app| ops::document_file(app, id)).await?;
	let bytes = tokio::fs::read(&absolute).await?;
	let name = source_name(&analysed, &path);
	refile(app, analysed, path, &name, bytes, false).await
}

/// Puts `bytes`, a file named `name`, in the place of the file of a document: the old one
/// goes to `.originals/`, the new one is read and analysed as one document and filed under
/// the same id. Journaled, undoable.
pub async fn replace(
	app: &Arc<App>,
	id: Uuid,
	name: &str,
	bytes: Vec<u8>,
) -> Result<Changed<DocumentView>> {
	if bytes.is_empty() {
		return Err(Error::Invalid("the new file is empty".into()));
	}
	let (_, path, analysed) = blocking(app, move |app| ops::document_file(app, id)).await?;
	refile(app, analysed, path, name, bytes, true).await
}

async fn refile(
	app: &Arc<App>,
	analysed: Sidecar,
	path: PathBuf,
	name: &str,
	bytes: Vec<u8>,
	replacing: bool,
) -> Result<Changed<DocumentView>> {
	if app.analyst().is_none() {
		return Err(Error::Invalid("no model is configured".into()));
	}
	let source = hash::sha256(&bytes);
	let bytes = Arc::new(bytes);
	let content = read(name, bytes.clone()).await?;
	let (pages, plans, _) = understand(app, name, &content, true).await?;
	let plan = decided(plans, &path)?;
	let mut sidecar = with_plan(&analysed, plan);
	sidecar.pages = pages
		.iter()
		.map(|page| PageText {
			number: page.number,
			text: page.text.clone(),
			source: page.source,
		})
		.collect();
	if replacing && sidecar.sources.iter().all(|known| known.sha256 != source) {
		sidecar.sources.push(Source {
			sha256: source,
			original_name: name.to_string(),
			pages: Vec::new(),
			mail: None,
		});
	}
	let (bytes, ext) = match content {
		Content::Pdf { pdf, .. } => (pdf, "pdf".to_string()),
		Content::Text { ext, .. } => (bytes, ext),
	};
	let replaced_by = replacing.then(|| name.to_string());
	blocking(app, move |app| {
		ops::refile(
			app,
			&analysed,
			sidecar,
			&bytes,
			&ext,
			replaced_by.as_deref(),
		)
	})
	.await
}

fn segment_pdf(pdf: &[u8], plan: &Plan, count: u32, converted: bool) -> Result<Vec<u8>> {
	let whole = plan.pages.iter().copied().eq(1..=count);
	if whole && plan.rotations.is_empty() {
		return Ok(pdf.to_vec());
	}
	let mut bytes = if whole && converted {
		pdf.to_vec()
	} else {
		violet_pdf::split(pdf, std::slice::from_ref(&plan.pages))?
			.pop()
			.ok_or_else(|| Error::Internal("split returned nothing".into()))?
	};
	for rotation in &plan.rotations {
		if let Some(position) = plan.pages.iter().position(|page| *page == rotation.page) {
			bytes = violet_pdf::rotate(&bytes, &[position as u32 + 1], rotation.degrees)?;
		}
	}
	Ok(bytes)
}

/// Documents already in the library that came from this source, with the pages of it they
/// hold (none listed means all).
fn already_filed(app: &App, sha256: &str) -> Result<Vec<(Uuid, PathBuf, Vec<u32>)>> {
	let mut filed = Vec::new();
	for (path, sidecar) in app.library.sidecars()? {
		if let Some(source) = sidecar
			.sources
			.iter()
			.find(|source| source.sha256 == sha256)
		{
			filed.push((sidecar.id, path, source.pages.clone()));
		}
	}
	Ok(filed)
}

fn apply(
	app: &App,
	input: &Input,
	content: &Content,
	pages: &[Page],
	plans: Vec<Plan>,
	blank: usize,
) -> Result<Outcome> {
	let mut index = app.index();
	let filed = if input.resume {
		already_filed(app, &input.sha256)?
	} else {
		Vec::new()
	};
	let all_filed = filed.iter().any(|(_, _, pages)| pages.is_empty());
	let filed_pages: HashSet<u32> = filed
		.iter()
		.flat_map(|(_, _, pages)| pages.clone())
		.collect();
	let mut documents: Vec<(Uuid, PathBuf)> = filed
		.iter()
		.map(|(id, path, _)| (*id, path.clone()))
		.collect();

	let count = pages.len() as u32;
	let at = ops::now();
	let mail = index.mail_hint(&input.sha256)?;
	let mut change = ops::Change::begin(app, &mut index)?;
	for plan in plans {
		let covered = if plan.pages.is_empty() {
			!filed.is_empty()
		} else {
			all_filed || plan.pages.iter().all(|page| filed_pages.contains(page))
		};
		if covered {
			continue;
		}
		let (bytes, ext) = match content {
			Content::Pdf { pdf, converted } => (segment_pdf(pdf, &plan, count, *converted)?, "pdf"),
			Content::Text { ext, .. } => (input.bytes.to_vec(), ext.as_str()),
		};
		let texts: Vec<PageText> = match content {
			Content::Pdf { .. } => plan
				.pages
				.iter()
				.enumerate()
				.filter_map(|(position, number)| {
					let page = pages.iter().find(|page| page.number == *number)?;
					Some(PageText {
						number: position as u32 + 1,
						text: page.text.clone(),
						source: page.source,
					})
				})
				.collect(),
			_ => pages
				.iter()
				.map(|page| PageText {
					number: page.number,
					text: page.text.clone(),
					source: page.source,
				})
				.collect(),
		};
		let mut sidecar = Sidecar {
			version: sidecar::SIDECAR_VERSION,
			id: Uuid::new_v4(),
			title: plan.title,
			area: plan.area,
			doc_type: plan.doc_type,
			issuer: plan.issuer,
			date: plan.date,
			language: plan.language,
			summary: plan.summary,
			tags: plan.tags,
			facts: plan.facts,
			pages: texts,
			sources: vec![Source {
				sha256: input.sha256.clone(),
				original_name: input.name.clone(),
				pages: plan.pages,
				mail: mail.as_ref().map(|hint| hint.mail.clone()),
			}],
			sha256: hash::sha256(&bytes),
			confidence: plan.confidence,
			created_at: at,
			updated_at: at,
			extra: serde_json::Map::new(),
		};
		if let Some(hint) = &mail {
			if let Some(area) = &hint.area {
				sidecar.area.clone_from(area);
			}
			for tag in &hint.tags {
				if !sidecar.tags.contains(tag) {
					sidecar.tags.push(tag.clone());
				}
			}
		}
		let text: String = sidecar
			.pages
			.iter()
			.map(|page| page.text.as_str())
			.collect::<Vec<_>>()
			.join("\n");
		if let Some((of, score)) = change.index().similar(&text, NEAR_DUPLICATE)? {
			tracing::info!(document = %sidecar.id, %of, score, "near duplicate");
			if !sidecar.tags.iter().any(|tag| tag == NEAR_DUPLICATE_TAG) {
				sidecar.tags.push(NEAR_DUPLICATE_TAG.into());
			}
			sidecar
				.extra
				.insert("duplicate_of".into(), of.to_string().into());
		}
		let path = ops::free_path(&app.library, &naming::document_path(&sidecar, ext), None);
		change.create(&path, &bytes, &sidecar)?;
		documents.push((sidecar.id, path));
	}

	let mut summary = match documents.as_slice() {
		[(_, path)] => format!("Filed \"{}\" as {}", input.name, path.display()),
		many => format!("Filed \"{}\" as {} documents", input.name, many.len()),
	};
	match blank {
		0 => {}
		1 => summary.push_str(", dropped 1 blank page"),
		more => summary.push_str(&format!(", dropped {more} blank pages")),
	}
	let action = Action::Ingest {
		original: ops::original_name(&input.sha256, &input.path),
		original_name: input.name.clone(),
		documents: documents.clone(),
	};
	let entry = change.commit(AGENT, action, summary)?;
	app.library.stash_original(&input.path)?;
	Ok(Outcome::Filed {
		entry: entry.id,
		documents,
	})
}
