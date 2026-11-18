//! Every change a user can make to a filed document, and its undo. The HTTP API and the CLI
//! both call these. Each one holds the index lock from start to end; reading a document
//! again, which waits for the model in between, is `pipeline::reanalyze`.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;
use violet_core::{
	Action, DatePrecision, Entry, Fact, Index, Job, JobQueue, JobState, Library, PageText, Pending,
	Sidecar, Source, Step, hash, naming,
};
use violet_model::{parse_amount, read_document_date};

use crate::app::ModelStatus;
use crate::cache::PageKey;
use crate::{App, Error, Result};

pub const USER: &str = "user";
pub const AGENT: &str = "agent";
const FAILED_JOBS_SHOWN: u32 = 20;

#[derive(Debug, Clone, Serialize)]
pub struct DocumentView {
	/// Relative to `library/`.
	pub path: PathBuf,
	pub size: u64,
	pub sidecar: Sidecar,
}

/// The answer to a change, with the journal entry that can undo it (none when nothing had to
/// change).
#[derive(Debug, Clone, Serialize)]
pub struct Changed<T> {
	pub entry: Option<Uuid>,
	#[serde(flatten)]
	pub result: T,
}

#[derive(Debug, Clone, Serialize)]
pub struct SplitResult {
	pub documents: Vec<Uuid>,
}

fn changed(
	app: &App,
	entry: &Entry,
	path: PathBuf,
	sidecar: Sidecar,
) -> Result<Changed<DocumentView>> {
	Ok(Changed {
		entry: Some(entry.id),
		result: view(&app.library, path, sidecar)?,
	})
}

/// A metadata change; fields left out stay as they are, `null` clears `issuer` and `date`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Patch {
	pub title: Option<String>,
	pub area: Option<String>,
	pub doc_type: Option<String>,
	#[serde(default, deserialize_with = "nullable")]
	pub issuer: Option<Option<String>>,
	#[serde(default, deserialize_with = "nullable")]
	pub date: Option<Option<String>>,
	pub tags: Option<Vec<String>>,
	pub facts: Option<Vec<Fact>>,
}

fn nullable<'de, D: Deserializer<'de>>(
	deserializer: D,
) -> Result<Option<Option<String>>, D::Error> {
	Option::<String>::deserialize(deserializer).map(Some)
}

#[derive(Debug, Clone, Serialize)]
pub struct ActivityItem {
	pub entry: Entry,
	pub undone: bool,
	pub undoable: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct JobCounts {
	pub pending: u64,
	pub running: u64,
	pub done: u64,
	pub failed: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Status {
	pub version: &'static str,
	/// Files waiting in `inbox/`.
	pub inbox: usize,
	pub jobs: JobCounts,
	pub failed: Vec<Job>,
	pub model: ModelStatus,
	/// Why changes are refused: the journal cannot be read, or a change that did not finish
	/// could not be rolled back.
	pub journal: Option<String>,
}

pub fn now() -> OffsetDateTime {
	OffsetDateTime::now_utc()
}

pub fn extension_of(path: &Path) -> String {
	path.extension()
		.map(|ext| ext.to_string_lossy().to_lowercase())
		.unwrap_or_default()
}

/// `wanted`, or the next free ` (n)` name when a document or sidecar is already there.
pub fn free_path(library: &Library, wanted: &Path, own: Option<&Path>) -> PathBuf {
	let root = library.library_dir();
	naming::deconflict(wanted, |candidate| {
		if own == Some(candidate) {
			return false;
		}
		let path = root.join(candidate);
		path.exists() || Library::sidecar_path(&path).exists()
	})
}

/// Name under `.originals/` for content with this hash that came from `path`.
pub fn original_name(sha256: &str, path: &Path) -> String {
	match path.extension() {
		Some(ext) => format!("{sha256}.{}", ext.to_string_lossy().to_lowercase()),
		None => sha256.to_string(),
	}
}

pub fn record(app: &App, actor: &str, action: Action, summary: String) -> Result<Entry> {
	let entry = Entry {
		id: Uuid::new_v4(),
		at: now(),
		actor: actor.to_string(),
		action,
		summary,
	};
	app.journal().append(&entry)?;
	tracing::info!(entry = %entry.id, actor, summary = entry.summary, "journal");
	Ok(entry)
}

/// One change to the library. Each step is recorded in `.pending.json` before it is made. The
/// journal entry is appended by `commit`; dropped before that, the steps made so far are
/// rolled back, and those of a process that died are rolled back when the library is opened
/// next.
pub struct Change<'a> {
	app: &'a App,
	index: &'a mut Index,
	pending: Pending,
	committed: bool,
}

impl<'a> Change<'a> {
	pub fn begin(app: &'a App, index: &'a mut Index) -> Result<Self> {
		if let Some(pending) = app.journal().pending()? {
			settle(app, index, &pending).map_err(|error| {
				Error::Conflict(format!(
					"an earlier change did not finish and could not be rolled back: {error}"
				))
			})?;
		}
		Ok(Self {
			app,
			index,
			pending: Pending {
				entry: Uuid::new_v4(),
				steps: Vec::new(),
			},
			committed: false,
		})
	}

	pub fn index(&mut self) -> &mut Index {
		self.index
	}

	fn step(&mut self, step: Step) -> Result<()> {
		self.pending.steps.push(step);
		Ok(self.app.journal().set_pending(&self.pending)?)
	}

	/// Writes a new document at `path`, which must be free.
	pub fn create(&mut self, path: &Path, bytes: &[u8], sidecar: &Sidecar) -> Result<()> {
		self.step(Step::Create {
			document: (sidecar.id, path.to_path_buf()),
		})?;
		self.app.library.write_document(path, bytes, sidecar)?;
		self.index.upsert(path, sidecar)?;
		Ok(())
	}

	/// Moves a document into `.originals/` and returns its name there.
	pub fn remove(&mut self, path: &Path, sidecar: &Sidecar) -> Result<String> {
		let sha256 = hash::sha256_file(&self.app.library.library_dir().join(path))?;
		let original = original_name(&sha256, path);
		self.step(Step::Remove {
			original: original.clone(),
			document: (sidecar.id, path.to_path_buf()),
			sidecar: Box::new(sidecar.clone()),
		})?;
		self.app.library.stash_original(path)?;
		self.index.remove(sidecar.id)?;
		Ok(original)
	}

	/// Rewrites a document in place; `original` already holds the bytes it had.
	fn replace(
		&mut self,
		path: &Path,
		bytes: &[u8],
		sidecar: &Sidecar,
		before: &Sidecar,
		original: String,
	) -> Result<()> {
		self.step(Step::Replace {
			original,
			document: (sidecar.id, path.to_path_buf()),
			before: Box::new(before.clone()),
		})?;
		self.app.library.write_document(path, bytes, sidecar)?;
		self.index.upsert(path, sidecar)?;
		Ok(())
	}

	fn relocate(
		&mut self,
		from: &Path,
		to: &Path,
		sidecar: &Sidecar,
		before: &Sidecar,
	) -> Result<()> {
		self.step(Step::Move {
			document: sidecar.id,
			from: from.to_path_buf(),
			to: to.to_path_buf(),
			before: Box::new(before.clone()),
		})?;
		self.app.library.move_document(from, to)?;
		self.app.library.write_sidecar(to, sidecar)?;
		self.index.upsert(to, sidecar)?;
		Ok(())
	}

	/// Moves a document written while the ones it replaces were still there to the name it
	/// wanted, now that they are gone.
	fn rename(&mut self, path: &Path, wanted: &Path, sidecar: &Sidecar) -> Result<PathBuf> {
		let target = free_path(&self.app.library, wanted, Some(path));
		if target != path {
			self.relocate(path, &target, sidecar, sidecar)?;
		}
		Ok(target)
	}

	pub fn commit(mut self, actor: &str, action: Action, summary: String) -> Result<Entry> {
		let entry = Entry {
			id: self.pending.entry,
			at: now(),
			actor: actor.to_string(),
			action,
			summary,
		};
		self.app.journal().append(&entry)?;
		self.committed = true;
		if let Err(error) = self.app.journal().clear_pending() {
			tracing::warn!(%error, "pending change left behind after its journal entry");
		}
		tracing::info!(entry = %entry.id, actor, summary = entry.summary, "journal");
		Ok(entry)
	}
}

impl Drop for Change<'_> {
	fn drop(&mut self) {
		if self.committed || self.pending.steps.is_empty() {
			return;
		}
		if let Err(error) = settle(self.app, self.index, &self.pending) {
			tracing::error!(%error, entry = %self.pending.entry, "a failed change could not be rolled back");
		}
	}
}

/// Finishes a pending change: kept when its entry reached the journal, rolled back otherwise.
fn settle(app: &App, index: &mut Index, pending: &Pending) -> Result<()> {
	let journal = app.journal();
	let journaled = journal
		.entries()?
		.iter()
		.any(|entry| entry.id == pending.entry);
	if !journaled {
		for step in pending.steps.iter().rev() {
			roll_back(app, index, step)?;
		}
		tracing::warn!(entry = %pending.entry, steps = pending.steps.len(), "rolled back a change that did not finish");
	}
	Ok(journal.clear_pending()?)
}

fn roll_back(app: &App, index: &mut Index, step: &Step) -> Result<()> {
	let library = &app.library;
	let exists = |path: &Path| library.library_dir().join(path).is_file();
	match step {
		Step::Create {
			document: (id, path),
		} => {
			library.discard(path, *id)?;
			index.remove(*id)?;
		}
		Step::Remove {
			original,
			document: (_, path),
			sidecar,
		} => {
			if !exists(path) {
				library.write_document(path, &original_bytes(library, original)?, sidecar)?;
			} else if !Library::sidecar_path(&library.library_dir().join(path)).is_file() {
				library.write_sidecar(path, sidecar)?;
			}
			index.upsert(path, sidecar)?;
		}
		Step::Replace {
			original,
			document: (_, path),
			before,
		} => {
			library.write_document(path, &original_bytes(library, original)?, before)?;
			index.upsert(path, before)?;
		}
		Step::Move {
			from, to, before, ..
		} => {
			if !exists(from) && exists(to) {
				library.move_document(to, from)?;
			}
			library.write_sidecar(from, before)?;
			index.upsert(from, before)?;
		}
	}
	Ok(())
}

/// Rolls back a change that a process left unfinished; true when there was one.
pub fn recover(app: &App) -> Result<bool> {
	let mut index = app.index();
	let Some(pending) = app.journal().pending()? else {
		return Ok(false);
	};
	settle(app, &mut index, &pending)?;
	Ok(true)
}

fn load(index: &Index, id: Uuid) -> Result<(PathBuf, Sidecar)> {
	index
		.get(id)?
		.ok_or_else(|| Error::NotFound(format!("document {id}")))
}

fn view(library: &Library, path: PathBuf, sidecar: Sidecar) -> Result<DocumentView> {
	let size = fs::metadata(library.library_dir().join(&path))?.len();
	Ok(DocumentView {
		path,
		size,
		sidecar,
	})
}

fn require_pdf(path: &Path) -> Result<()> {
	if extension_of(path) == "pdf" {
		Ok(())
	} else {
		Err(Error::Invalid(format!("{} is not a PDF", path.display())))
	}
}

fn write_atomic(target: &Path, bytes: &[u8]) -> Result<()> {
	let temp = target.with_file_name(format!(".tmp-{}", Uuid::new_v4().simple()));
	let written = fs::write(&temp, bytes).and_then(|()| fs::rename(&temp, target));
	if written.is_err() {
		let _ = fs::remove_file(&temp);
	}
	Ok(written?)
}

/// Copies a document and its sidecar into `.originals/` and leaves it in place.
fn preserve(library: &Library, path: &Path, bytes: &[u8], sidecar: &Sidecar) -> Result<String> {
	let sha256 = hash::sha256(bytes);
	let name = original_name(&sha256, path);
	let originals = library.originals();
	fs::create_dir_all(&originals)?;
	let target = originals.join(&name);
	if !target.is_file() {
		write_atomic(&target, bytes)?;
	}
	let mut json = serde_json::to_vec_pretty(sidecar)?;
	json.push(b'\n');
	write_atomic(&originals.join(format!("{sha256}.json")), &json)?;
	Ok(name)
}

fn original_bytes(library: &Library, name: &str) -> Result<Vec<u8>> {
	let plain = Path::new(name).file_name().is_some_and(|file| file == name);
	if !plain {
		return Err(Error::Conflict(format!("{name} is not a stashed original")));
	}
	fs::read(library.originals().join(name))
		.map_err(|error| Error::Conflict(format!("original {name} is not available: {error}")))
}

fn original_sidecar(library: &Library, name: &str, id: Uuid) -> Result<Sidecar> {
	let sha256 = name.split('.').next().unwrap_or(name);
	let bytes = original_bytes(library, &format!("{sha256}.json"))?;
	let sidecar: Sidecar = serde_json::from_slice(&bytes)
		.map_err(|error| Error::Conflict(format!("stashed sidecar of {name}: {error}")))?;
	if sidecar.id != id {
		return Err(Error::Conflict(format!(
			"the stashed sidecar of {name} belongs to another document"
		)));
	}
	Ok(sidecar)
}

fn clean(text: &str) -> String {
	text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn required(field: &str, value: &str) -> Result<String> {
	let value = clean(value);
	if value.is_empty() {
		return Err(Error::Invalid(format!("{field} must not be empty")));
	}
	Ok(value)
}

fn clean_tags(tags: &[String]) -> Vec<String> {
	let mut seen = HashSet::new();
	tags.iter()
		.map(|tag| clean(tag))
		.filter(|tag| !tag.is_empty() && seen.insert(tag.to_lowercase()))
		.collect()
}

fn clean_facts(facts: Vec<Fact>) -> Result<Vec<Fact>> {
	facts
		.into_iter()
		.map(|fact| {
			let key = required("fact key", &fact.key)?;
			let value = clean(&fact.value);
			let mut currency = fact
				.currency
				.map(|currency| clean(&currency).to_uppercase())
				.filter(|currency| !currency.is_empty());
			let mut amount_minor = fact.amount_minor;
			if amount_minor.is_none()
				&& let Some(amount) = parse_amount(&value, currency.as_deref())
			{
				amount_minor = Some(amount.minor);
				currency = Some(amount.currency);
			}
			Ok(Fact {
				key,
				value,
				amount_minor,
				currency,
			})
		})
		.collect()
}

pub fn document(app: &App, id: Uuid) -> Result<DocumentView> {
	let (path, sidecar) = load(&app.index(), id)?;
	view(&app.library, path, sidecar)
}

/// Absolute path, library-relative path and sidecar of a document.
pub fn document_file(app: &App, id: Uuid) -> Result<(PathBuf, PathBuf, Sidecar)> {
	let (path, sidecar) = load(&app.index(), id)?;
	let absolute = app.library.library_dir().join(&path);
	if !absolute.is_file() {
		return Err(Error::NotFound(format!("file of document {id}")));
	}
	Ok((absolute, path, sidecar))
}

/// One page as PNG with its longest side `width` pixels, and a tag that changes with the
/// file.
pub fn page_png(
	app: &App,
	id: Uuid,
	page: u32,
	width: u32,
) -> Result<(std::sync::Arc<Vec<u8>>, String)> {
	let (absolute, path, sidecar) = document_file(app, id)?;
	if extension_of(&path) != "pdf" {
		return Err(Error::NotFound(format!(
			"document {id} has no pages to show"
		)));
	}
	if width == 0 || width > violet_pdf::MAX_RENDER_SIDE {
		return Err(Error::Invalid(format!(
			"w must be in 1..={}",
			violet_pdf::MAX_RENDER_SIDE
		)));
	}
	let tag = format!("\"{}-{page}-{width}\"", sidecar.sha256);
	let key = PageKey {
		id,
		page,
		width,
		sha256: sidecar.sha256,
	};
	if let Some(png) = app.pages.get(&key) {
		return Ok((png, tag));
	}
	let bytes = fs::read(absolute)?;
	let png = match violet_pdf::render_png(&bytes, page, width) {
		Ok(png) => std::sync::Arc::new(png),
		Err(error @ violet_pdf::Error::PageOutOfRange(..)) => {
			return Err(Error::NotFound(error.to_string()));
		}
		Err(error) => return Err(error.into()),
	};
	app.pages.put(key, png.clone());
	Ok((png, tag))
}

pub fn update(app: &App, id: Uuid, patch: Patch) -> Result<Changed<DocumentView>> {
	let mut index = app.index();
	let (from, before) = load(&index, id)?;
	let mut sidecar = before.clone();
	if let Some(title) = &patch.title {
		sidecar.title = required("title", title)?;
	}
	if let Some(area) = &patch.area {
		sidecar.area = required("area", area)?;
	}
	if let Some(doc_type) = &patch.doc_type {
		sidecar.doc_type = required("doc_type", doc_type)?;
	}
	if let Some(issuer) = &patch.issuer {
		sidecar.issuer = issuer
			.as_deref()
			.map(clean)
			.filter(|issuer| !issuer.is_empty());
	}
	if let Some(date) = &patch.date {
		let (date, precision) = match date.as_deref().map(str::trim) {
			None | Some("") => (None, DatePrecision::Day),
			Some(date) => read_document_date(date)
				.map(|(date, precision)| (Some(date), precision))
				.ok_or_else(|| Error::Invalid(format!("{date} is not a date")))?,
		};
		sidecar.date = date;
		sidecar.date_precision = precision;
	}
	if let Some(tags) = &patch.tags {
		sidecar.tags = clean_tags(tags);
	}
	if let Some(facts) = patch.facts {
		sidecar.facts = clean_facts(facts)?;
	}
	if sidecar == before {
		return Ok(Changed {
			entry: None,
			result: view(&app.library, from, sidecar)?,
		});
	}
	sidecar.confidence = 1.0;
	sidecar.updated_at = now();
	let summary = |from: &Path, to: &Path| {
		if from == to {
			format!("Edited {}", to.display())
		} else {
			format!("Moved {} to {}", from.display(), to.display())
		}
	};
	store_metadata(app, &mut index, USER, from, before, sidecar, summary)
}

/// Writes changed metadata, renames and moves the file to match and journals the change.
fn store_metadata(
	app: &App,
	index: &mut Index,
	actor: &str,
	from: PathBuf,
	before: Sidecar,
	sidecar: Sidecar,
	summary: impl FnOnce(&Path, &Path) -> String,
) -> Result<Changed<DocumentView>> {
	let wanted = naming::document_path(&sidecar, &extension_of(&from));
	let to = free_path(&app.library, &wanted, Some(&from));
	let mut change = Change::begin(app, index)?;
	change.relocate(&from, &to, &sidecar, &before)?;
	let summary = summary(&from, &to);
	let action = Action::Update {
		document: sidecar.id,
		from,
		to: to.clone(),
		before: Box::new(before),
	};
	let entry = change.commit(actor, action, summary)?;
	changed(app, &entry, to, sidecar)
}

/// Replaces the metadata of a document with what a new analysis of its text decided.
/// Refused with a conflict when the document is no longer the one that was analysed.
pub fn apply_analysis(
	app: &App,
	analysed: &Sidecar,
	mut sidecar: Sidecar,
) -> Result<Changed<DocumentView>> {
	let mut index = app.index();
	let (from, before) = load(&index, analysed.id)?;
	if before != *analysed {
		return Err(Error::Conflict(format!(
			"{} changed while it was read again",
			from.display()
		)));
	}
	if sidecar == before {
		return Ok(Changed {
			entry: None,
			result: view(&app.library, from, sidecar)?,
		});
	}
	sidecar.updated_at = now();
	let summary = |from: &Path, to: &Path| {
		if from == to {
			format!("Read {} again", to.display())
		} else {
			format!("Read {} again, now {}", from.display(), to.display())
		}
	};
	store_metadata(app, &mut index, AGENT, from, before, sidecar, summary)
}

/// Files a document again from a new reading of its file, or of the file that replaces it:
/// `bytes` with extension `ext` and the metadata of `sidecar`, under the same id. Only the
/// sidecar changes when the bytes and the extension are the same; otherwise the old file
/// goes to `.originals/`. Refused with a conflict when the document is no longer the one
/// that was read.
pub fn refile(
	app: &App,
	analysed: &Sidecar,
	mut sidecar: Sidecar,
	bytes: &[u8],
	ext: &str,
	replaced_by: Option<&str>,
) -> Result<Changed<DocumentView>> {
	let mut index = app.index();
	let (from, before) = unchanged(app, &index, analysed.id)?;
	if before != *analysed {
		return Err(Error::Conflict(format!(
			"{} changed while it was read again",
			from.display()
		)));
	}
	let actor = if replaced_by.is_some() { USER } else { AGENT };
	let describe = |from: &Path, to: &Path| {
		let now = if from == to {
			String::new()
		} else {
			format!(", now {}", to.display())
		};
		match replaced_by {
			Some(name) => format!("Replaced {} with \"{name}\"{now}", from.display()),
			None => format!("Read the file of {} again{now}", from.display()),
		}
	};
	sidecar.sha256 = hash::sha256(bytes);
	if sidecar.sha256 == before.sha256 && extension_of(&from) == ext {
		if sidecar == before {
			return Ok(Changed {
				entry: None,
				result: view(&app.library, from, sidecar)?,
			});
		}
		sidecar.updated_at = now();
		return store_metadata(app, &mut index, actor, from, before, sidecar, describe);
	}
	sidecar.updated_at = now();
	let mut change = Change::begin(app, &mut index)?;
	let original = change.remove(&from, &before)?;
	let to = free_path(&app.library, &naming::document_path(&sidecar, ext), None);
	change.create(&to, bytes, &sidecar)?;
	let summary = describe(&from, &to);
	let action = Action::Replace {
		original,
		document: sidecar.id,
		from,
		to: to.clone(),
		before: Box::new(before),
	};
	let entry = change.commit(actor, action, summary)?;
	changed(app, &entry, to, sidecar)
}

/// The sources of a document made of the pages `keep` (1-based, in order) of `parent`, which
/// has `total` pages.
fn sources_for(parent: &Sidecar, keep: &[u32], total: u32) -> Vec<Source> {
	let [source] = parent.sources.as_slice() else {
		return parent.sources.clone();
	};
	let origin: Vec<u32> = if source.pages.is_empty() {
		(1..=total).collect()
	} else {
		source.pages.clone()
	};
	if origin.len() != total as usize {
		return parent.sources.clone();
	}
	let pages = keep
		.iter()
		.filter_map(|page| origin.get(page.checked_sub(1)? as usize).copied())
		.collect();
	vec![Source {
		pages,
		..source.clone()
	}]
}

fn pages_for(parent: &Sidecar, keep: &[u32]) -> Vec<PageText> {
	keep.iter()
		.enumerate()
		.filter_map(|(position, number)| {
			let page = parent.pages.iter().find(|page| page.number == *number)?;
			Some(PageText {
				number: position as u32 + 1,
				..page.clone()
			})
		})
		.collect()
}

fn list(pages: &[u32]) -> String {
	pages
		.iter()
		.map(u32::to_string)
		.collect::<Vec<_>>()
		.join(", ")
}

/// Cuts a PDF into one new document per group of pages; the original goes to `.originals/`.
/// Split groups must hold every page exactly once: leaving pages out is `delete_pages`.
fn covers_once(groups: &[Vec<u32>], total: u32) -> Result<()> {
	let mut seen = vec![false; total as usize];
	for group in groups {
		if group.is_empty() {
			return Err(Error::Invalid("a split group is empty".into()));
		}
		for &page in group {
			let Some(slot) = page.checked_sub(1).and_then(|at| seen.get_mut(at as usize)) else {
				return Err(Error::Invalid(format!(
					"page {page} is not in the document (1..={total})"
				)));
			};
			if std::mem::replace(slot, true) {
				return Err(Error::Invalid(format!(
					"page {page} is in more than one group"
				)));
			}
		}
	}
	let missing: Vec<String> = (1..=total)
		.filter(|page| !seen[*page as usize - 1])
		.map(|page| page.to_string())
		.collect();
	if !missing.is_empty() {
		return Err(Error::Invalid(format!(
			"pages missing from the groups: {}; remove pages with delete-pages",
			missing.join(", ")
		)));
	}
	Ok(())
}

pub fn split(app: &App, id: Uuid, groups: &[Vec<u32>]) -> Result<Changed<SplitResult>> {
	let mut index = app.index();
	let (from, parent) = load(&index, id)?;
	require_pdf(&from)?;
	if groups.is_empty() {
		return Err(Error::Invalid("groups must not be empty".into()));
	}
	let bytes = fs::read(app.library.library_dir().join(&from))?;
	let total = violet_pdf::page_count(&bytes)?;
	covers_once(groups, total)?;
	let parts = violet_pdf::split(&bytes, groups)?;
	let at = now();
	let mut change = Change::begin(app, &mut index)?;
	let mut into = Vec::with_capacity(parts.len());
	for (position, (part, pages)) in parts.iter().zip(groups).enumerate() {
		let mut sidecar = parent.clone();
		sidecar.id = Uuid::new_v4();
		if groups.len() > 1 {
			sidecar.title = format!("{} ({}/{})", parent.title, position + 1, groups.len());
		}
		sidecar.pages = pages_for(&parent, pages);
		sidecar.sources = sources_for(&parent, pages, total);
		sidecar.sha256 = hash::sha256(part);
		sidecar.created_at = at;
		sidecar.updated_at = at;
		let path = free_path(&app.library, &naming::document_path(&sidecar, "pdf"), None);
		change.create(&path, part, &sidecar)?;
		into.push((sidecar, path));
	}
	let original = change.remove(&from, &parent)?;
	let into = into
		.into_iter()
		.map(|(sidecar, path)| {
			let wanted = naming::document_path(&sidecar, "pdf");
			Ok((sidecar.id, change.rename(&path, &wanted, &sidecar)?))
		})
		.collect::<Result<Vec<_>>>()?;
	let summary = format!("Split {} into {} documents", from.display(), into.len());
	let documents = into.iter().map(|(id, _)| *id).collect();
	let action = Action::Split {
		original,
		from: (id, from),
		into,
	};
	let entry = change.commit(USER, action, summary)?;
	Ok(Changed {
		entry: Some(entry.id),
		result: SplitResult { documents },
	})
}

fn edit_pages(
	app: &App,
	id: Uuid,
	detail: String,
	edit: impl FnOnce(&[u8], &mut Sidecar) -> Result<Vec<u8>>,
) -> Result<Changed<DocumentView>> {
	let mut index = app.index();
	let (path, mut sidecar) = load(&index, id)?;
	require_pdf(&path)?;
	let bytes = fs::read(app.library.library_dir().join(&path))?;
	let before = sidecar.clone();
	let edited = edit(&bytes, &mut sidecar)?;
	let original = preserve(&app.library, &path, &bytes, &before)?;
	sidecar.sha256 = hash::sha256(&edited);
	sidecar.updated_at = now();
	let mut change = Change::begin(app, &mut index)?;
	change.replace(&path, &edited, &sidecar, &before, original.clone())?;
	let summary = format!("{detail} of {}", path.display());
	let action = Action::EditPages {
		original,
		document: (id, path.clone()),
		detail,
	};
	let entry = change.commit(USER, action, summary)?;
	changed(app, &entry, path, sidecar)
}

pub fn rotate(app: &App, id: Uuid, pages: &[u32], degrees: i32) -> Result<Changed<DocumentView>> {
	if pages.is_empty() {
		return Err(Error::Invalid("pages must not be empty".into()));
	}
	if degrees % 360 == 0 {
		return Err(Error::Invalid("degrees must turn the page".into()));
	}
	let detail = format!("Rotated pages {} by {degrees}°", list(pages));
	edit_pages(app, id, detail, |bytes, _| {
		Ok(violet_pdf::rotate(bytes, pages, degrees)?)
	})
}

pub fn delete_pages(app: &App, id: Uuid, pages: &[u32]) -> Result<Changed<DocumentView>> {
	if pages.is_empty() {
		return Err(Error::Invalid("pages must not be empty".into()));
	}
	let detail = format!("Deleted pages {}", list(pages));
	edit_pages(app, id, detail, |bytes, sidecar| {
		let total = violet_pdf::page_count(bytes)?;
		let edited = violet_pdf::delete_pages(bytes, pages)?;
		let keep: Vec<u32> = (1..=total).filter(|page| !pages.contains(page)).collect();
		sidecar.sources = sources_for(sidecar, &keep, total);
		sidecar.pages = pages_for(sidecar, &keep);
		Ok(edited)
	})
}

/// Takes a document out of the library; its file and sidecar go to `.originals/`. Returns
/// the journal entry.
pub fn delete(app: &App, id: Uuid) -> Result<Uuid> {
	let mut index = app.index();
	let (path, sidecar) = load(&index, id)?;
	let mut change = Change::begin(app, &mut index)?;
	let original = change.remove(&path, &sidecar)?;
	let summary = format!("Deleted {}", path.display());
	let action = Action::Delete {
		original,
		document: (id, path),
		sidecar: Box::new(sidecar),
	};
	Ok(change.commit(USER, action, summary)?.id)
}

/// Joins PDFs in the order given into a new document that takes the metadata of the first.
pub fn merge(app: &App, ids: &[Uuid]) -> Result<Changed<DocumentView>> {
	let distinct: HashSet<&Uuid> = ids.iter().collect();
	if ids.len() < 2 || distinct.len() != ids.len() {
		return Err(Error::Invalid(
			"merge needs at least two different documents".into(),
		));
	}
	let mut index = app.index();
	let mut parents = Vec::with_capacity(ids.len());
	for id in ids {
		let (path, sidecar) = load(&index, *id)?;
		require_pdf(&path)?;
		let bytes = fs::read(app.library.library_dir().join(&path))?;
		parents.push((path, sidecar, bytes));
	}
	let slices: Vec<&[u8]> = parents
		.iter()
		.map(|(_, _, bytes)| bytes.as_slice())
		.collect();
	let merged = violet_pdf::merge(&slices)?;

	let at = now();
	let mut sidecar = parents[0].1.clone();
	sidecar.id = Uuid::new_v4();
	sidecar.pages.clear();
	sidecar.sources.clear();
	sidecar.sha256 = hash::sha256(&merged);
	sidecar.created_at = at;
	sidecar.updated_at = at;
	let mut offset = 0;
	for (_, parent, bytes) in &parents {
		sidecar
			.pages
			.extend(parent.pages.iter().map(|page| PageText {
				number: page.number + offset,
				..page.clone()
			}));
		offset += violet_pdf::page_count(bytes)?;
		for source in &parent.sources {
			if !sidecar.sources.contains(source) {
				sidecar.sources.push(source.clone());
			}
		}
		for tag in &parent.tags {
			if !sidecar.tags.contains(tag) {
				sidecar.tags.push(tag.clone());
			}
		}
	}

	let wanted = naming::document_path(&sidecar, "pdf");
	let path = free_path(&app.library, &wanted, None);
	let mut change = Change::begin(app, &mut index)?;
	change.create(&path, &merged, &sidecar)?;
	let mut originals = Vec::with_capacity(parents.len());
	for (parent_path, parent, _) in &parents {
		let original = change.remove(parent_path, parent)?;
		originals.push((original, parent.id, parent_path.clone()));
	}
	let path = change.rename(&path, &wanted, &sidecar)?;
	let summary = format!(
		"Merged {} documents into {}",
		originals.len(),
		path.display()
	);
	let action = Action::Merge {
		originals,
		into: (sidecar.id, path.clone()),
	};
	let entry = change.commit(USER, action, summary)?;
	changed(app, &entry, path, sidecar)
}

fn touched(action: &Action) -> Vec<Uuid> {
	match action {
		Action::Ingest { documents, .. } => documents.iter().map(|(id, _)| *id).collect(),
		Action::Split { from, into, .. } => std::iter::once(from.0)
			.chain(into.iter().map(|(id, _)| *id))
			.collect(),
		Action::Merge { originals, into } => originals
			.iter()
			.map(|(_, id, _)| *id)
			.chain([into.0])
			.collect(),
		Action::EditPages { document, .. } | Action::Delete { document, .. } => vec![document.0],
		Action::Update { document, .. } | Action::Replace { document, .. } => vec![*document],
		Action::Duplicate { .. } | Action::Undo { .. } => Vec::new(),
	}
}

fn undone(entries: &[Entry]) -> HashSet<Uuid> {
	entries
		.iter()
		.filter_map(|entry| match entry.action {
			Action::Undo { entry } => Some(entry),
			_ => None,
		})
		.collect()
}

/// The first later change, not undone, to a document that entry `position` touched.
fn blocker<'a>(entries: &'a [Entry], undone: &HashSet<Uuid>, position: usize) -> Option<&'a Entry> {
	let ids = touched(&entries[position].action);
	entries[position + 1..].iter().find(|later| {
		!undone.contains(&later.id) && touched(&later.action).iter().any(|id| ids.contains(id))
	})
}

/// Newest first; an entry is undoable until it is undone or a later change builds on it.
pub fn activity(app: &App, limit: usize) -> Result<Vec<ActivityItem>> {
	let entries = app.journal().entries()?;
	let undone = undone(&entries);
	Ok((0..entries.len())
		.rev()
		.take(limit)
		.map(|position| {
			let entry = &entries[position];
			let is_undone = undone.contains(&entry.id);
			let undoable = !is_undone
				&& !matches!(entry.action, Action::Undo { .. })
				&& blocker(&entries, &undone, position).is_none();
			ActivityItem {
				entry: entry.clone(),
				undone: is_undone,
				undoable,
			}
		})
		.collect())
}

/// The document as the index knows it, refusing when it is gone or its file is not the one
/// the sidecar describes.
fn unchanged(app: &App, index: &Index, id: Uuid) -> Result<(PathBuf, Sidecar)> {
	let (path, sidecar) = index
		.get(id)?
		.ok_or_else(|| Error::Conflict(format!("document {id} no longer exists")))?;
	let absolute = app.library.library_dir().join(&path);
	let actual = hash::sha256_file(&absolute)
		.map_err(|error| Error::Conflict(format!("{}: {error}", path.display())))?;
	if actual != sidecar.sha256 {
		return Err(Error::Conflict(format!("{} changed since", path.display())));
	}
	Ok((path, sidecar))
}

/// The documents as they are now, refusing when any changed since it was filed.
fn filed(
	app: &App,
	index: &Index,
	documents: &[(Uuid, PathBuf)],
) -> Result<Vec<(PathBuf, Sidecar)>> {
	documents
		.iter()
		.map(|(id, _)| unchanged(app, index, *id))
		.collect()
}

fn remove_all(change: &mut Change, current: &[(PathBuf, Sidecar)]) -> Result<()> {
	for (path, sidecar) in current {
		change.remove(path, sidecar)?;
	}
	Ok(())
}

fn restore(change: &mut Change, path: &Path, original: &str, sidecar: &Sidecar) -> Result<PathBuf> {
	let library = &change.app.library;
	let bytes = original_bytes(library, original)?;
	let target = free_path(library, path, None);
	change.create(&target, &bytes, sidecar)?;
	Ok(target)
}

/// Reverses one journal entry and appends the `undo` entry. Refused with a conflict when
/// the entry was already undone, a later change builds on it, or a file it would replace is
/// not the one it left.
pub fn undo(app: &App, entry_id: Uuid) -> Result<Entry> {
	let mut index = app.index();
	let entries = app.journal().entries()?;
	let position = entries
		.iter()
		.position(|entry| entry.id == entry_id)
		.ok_or_else(|| Error::NotFound(format!("entry {entry_id}")))?;
	let entry = &entries[position];
	let undone = undone(&entries);
	if undone.contains(&entry_id) {
		return Err(Error::Conflict("already undone".into()));
	}
	if let Some(later) = blocker(&entries, &undone, position) {
		return Err(Error::Conflict(format!(
			"changed since: undo \"{}\" first",
			later.summary
		)));
	}
	let library = &app.library;
	let summary = format!("Undid: {}", entry.summary);
	let undo = Action::Undo { entry: entry_id };
	match &entry.action {
		Action::Undo { .. } => Err(Error::Conflict("an undo cannot be undone".into())),
		Action::Duplicate { .. } => record(app, USER, undo, summary),
		Action::Ingest { documents, .. } => {
			let current = filed(app, &index, documents)?;
			let mut change = Change::begin(app, &mut index)?;
			remove_all(&mut change, &current)?;
			change.commit(USER, undo, summary)
		}
		Action::Split {
			original,
			from,
			into,
		} => {
			let sidecar = original_sidecar(library, original, from.0)?;
			original_bytes(library, original)?;
			let current = filed(app, &index, into)?;
			let mut change = Change::begin(app, &mut index)?;
			let restored = restore(&mut change, &from.1, original, &sidecar)?;
			remove_all(&mut change, &current)?;
			change.rename(&restored, &from.1, &sidecar)?;
			change.commit(USER, undo, summary)
		}
		Action::Merge { originals, into } => {
			let mut sidecars = Vec::with_capacity(originals.len());
			for (original, id, _) in originals {
				original_bytes(library, original)?;
				sidecars.push(original_sidecar(library, original, *id)?);
			}
			let current = filed(app, &index, std::slice::from_ref(into))?;
			let mut change = Change::begin(app, &mut index)?;
			let mut restored = Vec::with_capacity(originals.len());
			for ((original, _, path), sidecar) in originals.iter().zip(&sidecars) {
				restored.push(restore(&mut change, path, original, sidecar)?);
			}
			remove_all(&mut change, &current)?;
			for ((_, _, path), (at, sidecar)) in
				originals.iter().zip(restored.iter().zip(&sidecars))
			{
				change.rename(at, path, sidecar)?;
			}
			change.commit(USER, undo, summary)
		}
		Action::EditPages {
			original, document, ..
		} => {
			let before = original_sidecar(library, original, document.0)?;
			let bytes = original_bytes(library, original)?;
			let (path, edited) = unchanged(app, &index, document.0)?;
			let current = fs::read(library.library_dir().join(&path))?;
			let kept = preserve(library, &path, &current, &edited)?;
			let mut change = Change::begin(app, &mut index)?;
			change.replace(&path, &bytes, &before, &edited, kept)?;
			change.commit(USER, undo, summary)
		}
		Action::Update {
			document,
			from,
			to,
			before,
		} => {
			let (path, current) = index
				.get(*document)?
				.ok_or_else(|| Error::Conflict(format!("document {document} no longer exists")))?;
			if path != *to {
				return Err(Error::Conflict(format!("{} moved since", to.display())));
			}
			let target = free_path(library, from, Some(&path));
			let mut change = Change::begin(app, &mut index)?;
			change.relocate(&path, &target, before, &current)?;
			change.commit(USER, undo, summary)
		}
		Action::Replace {
			original,
			document,
			from,
			to,
			before,
		} => {
			original_bytes(library, original)?;
			let (path, current) = unchanged(app, &index, *document)?;
			if path != *to {
				return Err(Error::Conflict(format!("{} moved since", to.display())));
			}
			let mut change = Change::begin(app, &mut index)?;
			change.remove(&path, &current)?;
			restore(&mut change, from, original, before)?;
			change.commit(USER, undo, summary)
		}
		Action::Delete {
			original,
			document,
			sidecar,
		} => {
			if index.get(document.0)?.is_some() {
				return Err(Error::Conflict(format!(
					"document {} exists again",
					document.0
				)));
			}
			let mut change = Change::begin(app, &mut index)?;
			restore(&mut change, &document.1, original, sidecar)?;
			change.commit(USER, undo, summary)
		}
	}
}

pub fn status(app: &App, model: ModelStatus) -> Result<Status> {
	let index = app.index();
	let queue = JobQueue::new(index.connection())?;
	let mut jobs = JobCounts::default();
	for (state, count) in queue.counts()? {
		match state {
			JobState::Pending => jobs.pending = count,
			JobState::Running => jobs.running = count,
			JobState::Done => jobs.done = count,
			JobState::Failed => jobs.failed = count,
		}
	}
	let journal = match app.journal().entries() {
		Err(error) => Some(error.to_string()),
		Ok(_) if app.holds_lock() => app.journal().pending()?.map(|pending| {
			format!(
				"a change ({} steps) did not finish and could not be rolled back",
				pending.steps.len()
			)
		}),
		Ok(_) => None,
	};
	Ok(Status {
		version: env!("CARGO_PKG_VERSION"),
		inbox: crate::watcher::inbox_files(&app.library).len(),
		jobs,
		failed: queue.failed(FAILED_JOBS_SHOWN)?,
		model,
		journal,
	})
}
