use std::collections::{HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};

use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, params, params_from_iter};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

/// The SQLite index at `.index.sqlite`: documents, tags, facts and two FTS5 tables over the
/// metadata (title, issuer, type, tags, summary) and the page text: `words` for whole words
/// without case or accents, `grams` for substrings (CJK). WAL mode. `rebuild` recreates all of
/// it except the jobs and the mail tables (which mails were fetched, and the provenance of files
/// waiting in the inbox); deleting the file loses those.
pub struct Index {
	conn: rusqlite::Connection,
}

#[derive(Debug, Clone, Default)]
pub struct Query {
	/// Words in any case and with or without accents; from four letters on also as prefixes, a
	/// trailing `*` makes any word a prefix, quotes keep phrases. CJK and text without letters or
	/// digits match anywhere. Every term, or a synonym of it, must appear somewhere in the
	/// document; short French and English stop-words are ignored next to other terms. When that
	/// finds nothing, documents with any of the terms are `partial` matches.
	pub text: Option<String>,
	pub area: Option<String>,
	pub doc_type: Option<String>,
	pub issuer: Option<String>,
	pub tags: Vec<String>,
	/// Inclusive `YYYY-MM-DD` bounds on the document date.
	pub date_from: Option<String>,
	pub date_to: Option<String>,
	/// Bounds on any fact amount in minor units (any currency unless `currency` is set).
	pub amount_min: Option<i64>,
	pub amount_max: Option<i64>,
	pub currency: Option<String>,
	/// The month of the document date, 1 to 12, in any year.
	pub month: Option<u32>,
	/// Newest first even with `text`, instead of by rank.
	pub newest: bool,
	/// 0 means 50.
	pub limit: u32,
	pub offset: u32,
	/// Whose search it is: the documents of areas hidden from them are left out.
	pub viewer: Viewer,
}

/// The areas a request cannot see. Every query that answers with documents, counts or text
/// filters through `visible`; the default sees everything.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Viewer {
	hidden: Vec<String>,
}

impl Viewer {
	pub fn everything() -> Self {
		Self::default()
	}
	pub fn hiding(areas: impl IntoIterator<Item = String>) -> Self {
		let mut hidden: Vec<String> = areas.into_iter().collect();
		hidden.sort();
		hidden.dedup();
		Self { hidden }
	}
	pub fn hidden(&self) -> &[String] {
		&self.hidden
	}
	pub fn sees(&self, area: &str) -> bool {
		let area = area.trim().to_lowercase();
		!self
			.hidden
			.iter()
			.any(|hidden| hidden.trim().to_lowercase() == area)
	}
	/// Whether a path relative to `library/` lies outside the folders of the hidden areas.
	pub fn sees_path(&self, path: &Path) -> bool {
		let Some(top) = path.components().next() else {
			return true;
		};
		let top = top.as_os_str().to_string_lossy().to_lowercase();
		!self
			.hidden
			.iter()
			.any(|hidden| crate::naming::sanitize(hidden).to_lowercase() == top)
	}
}

/// The SQL condition that keeps the rows of `documents` (as `alias`) a viewer may see.
fn visible(viewer: &Viewer, alias: &str, values: &mut Vec<Value>) -> String {
	if viewer.hidden.is_empty() {
		return "1".into();
	}
	let mut slots = Vec::with_capacity(viewer.hidden.len());
	for area in &viewer.hidden {
		values.push(area.trim().to_string().into());
		slots.push(format!("?{}", values.len()));
	}
	format!(
		"trim({alias}.area) COLLATE NOCASE NOT IN ({})",
		slots.join(", ")
	)
}

#[derive(Debug, Clone, Serialize)]
pub struct Hit {
	pub id: Uuid,
	/// Relative to `library/`.
	pub path: PathBuf,
	pub title: String,
	pub area: String,
	pub doc_type: String,
	pub issuer: Option<String>,
	pub date: Option<String>,
	pub tags: Vec<String>,
	/// The `total` fact if present, else the largest amount.
	pub amount_minor: Option<i64>,
	pub currency: Option<String>,
	pub pages: u32,
	/// Matching text with the terms wrapped in `<mark>`; HTML-escaped otherwise.
	pub snippet: Option<String>,
}

/// One side of a pair of near-duplicates, with what tells it apart from the other.
#[derive(Debug, Clone, Serialize)]
pub struct DuplicateCopy {
	#[serde(flatten)]
	pub hit: Hit,
	#[serde(with = "time::serde::rfc3339")]
	pub created_at: OffsetDateTime,
	pub sha256: String,
	pub sources: Vec<SourceSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourceSummary {
	pub original_name: String,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub mail: Option<MailSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MailSummary {
	pub account: String,
	pub subject: Option<String>,
	pub date: Option<String>,
}

impl DuplicateCopy {
	fn new(hit: Hit, sidecar: crate::Sidecar) -> Self {
		Self {
			hit,
			created_at: sidecar.created_at,
			sha256: sidecar.sha256,
			sources: sidecar
				.sources
				.into_iter()
				.map(|source| SourceSummary {
					original_name: source.original_name,
					mail: source.mail.map(|mail| MailSummary {
						account: mail.account,
						subject: mail.subject,
						date: mail.date,
					}),
				})
				.collect(),
		}
	}
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchResult {
	pub total: u64,
	pub hits: Vec<Hit>,
	/// No document has every term; these have some.
	pub partial: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Facet {
	pub value: String,
	pub count: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Facets {
	pub areas: Vec<Facet>,
	pub doc_types: Vec<Facet>,
	pub issuers: Vec<Facet>,
	pub tags: Vec<Facet>,
	pub years: Vec<Facet>,
	/// Currencies of the facts, counted by document.
	pub currencies: Vec<Facet>,
}

const DEFAULT_LIMIT: u32 = 50;
const SHINGLE_WORDS: usize = 3;
const SNIPPET_BEFORE: usize = 40;
const SNIPPET_CHARS: usize = 200;
/// The trigram tokenizer cannot match anything shorter.
const FTS_MIN_CHARS: usize = 3;
/// Shorter words match whole only: `car` is not `carrefour`.
const PREFIX_MIN_CHARS: usize = 4;
const META_WEIGHT: f64 = 5.0;
const BODY_WEIGHT: f64 = 1.0;
const ISSUER_EXACT_BOOST: f64 = 1.0;
const ISSUER_BOOST: f64 = 0.5;
const TYPE_BOOST: f64 = 0.5;
/// Scores within about 1% of each other tie and fall back to newest first.
const SCORE_STEP: f64 = 0.01;
const STOP_WORDS: &[&str] = &[
	"a", "an", "at", "au", "by", "d", "de", "des", "du", "en", "et", "in", "is", "l", "la", "le",
	"les", "of", "on", "or", "the", "to", "un", "une", "y", "à",
];
const DIACRITICS: &[(&str, char)] = &[
	("àáâãäåāăą", 'a'),
	("çćĉċč", 'c'),
	("ď", 'd'),
	("èéêëēĕėęě", 'e'),
	("ĝğġģ", 'g'),
	("ĥ", 'h'),
	("ìíîïĩīĭį", 'i'),
	("ĵ", 'j'),
	("ķ", 'k'),
	("ĺļľ", 'l'),
	("ñńņň", 'n'),
	("òóôõöōŏő", 'o'),
	("ŕŗř", 'r'),
	("śŝşš", 's'),
	("ţť", 't'),
	("ùúûüũūŭůűų", 'u'),
	("ŵ", 'w'),
	("ýÿŷ", 'y'),
	("źżž", 'z'),
];
/// The migration that replaced the single trigram table; it refills the text from the stored
/// sidecars so an upgraded library needs no reindex.
const TEXT_SCHEMA: usize = 3;
/// The migrations that added near-duplicates and then flagged identical files; both find them
/// among the stored sidecars.
const DUPLICATES_SCHEMAS: [usize; 2] = [4, 5];

const MIGRATIONS: &[&str] = &[
	"
	CREATE TABLE documents (
		id TEXT PRIMARY KEY,
		path TEXT NOT NULL,
		title TEXT NOT NULL,
		area TEXT NOT NULL,
		doc_type TEXT NOT NULL,
		issuer TEXT,
		date TEXT,
		sha256 TEXT NOT NULL,
		pages INTEGER NOT NULL,
		amount_minor INTEGER,
		currency TEXT,
		created_at INTEGER NOT NULL,
		body TEXT NOT NULL,
		sidecar TEXT NOT NULL
	);
	CREATE INDEX documents_sha256 ON documents (sha256);
	CREATE INDEX documents_date ON documents (date);
	CREATE TABLE tags (doc_id TEXT NOT NULL, pos INTEGER NOT NULL, tag TEXT NOT NULL);
	CREATE INDEX tags_doc ON tags (doc_id);
	CREATE INDEX tags_tag ON tags (tag);
	CREATE TABLE facts (
		doc_id TEXT NOT NULL,
		key TEXT NOT NULL,
		value TEXT NOT NULL,
		amount_minor INTEGER,
		currency TEXT
	);
	CREATE INDEX facts_doc ON facts (doc_id);
	CREATE TABLE sources (doc_id TEXT NOT NULL, sha256 TEXT NOT NULL);
	CREATE INDEX sources_doc ON sources (doc_id);
	CREATE INDEX sources_sha256 ON sources (sha256);
	CREATE VIRTUAL TABLE pages USING fts5 (
		doc_id UNINDEXED,
		page UNINDEXED,
		text,
		tokenize = 'trigram'
	);
",
	"
	CREATE TABLE mail_processed (
		id INTEGER PRIMARY KEY,
		account TEXT NOT NULL,
		folder TEXT NOT NULL,
		uidvalidity INTEGER NOT NULL,
		uid INTEGER NOT NULL,
		message_id TEXT,
		sender TEXT,
		subject TEXT,
		received TEXT,
		status TEXT NOT NULL,
		files INTEGER NOT NULL,
		detail TEXT,
		error TEXT,
		processed_at INTEGER NOT NULL,
		UNIQUE (account, folder, uidvalidity, uid)
	);
	CREATE INDEX mail_processed_message_id ON mail_processed (message_id);
	CREATE TABLE mail_files (sha256 TEXT PRIMARY KEY, hint TEXT NOT NULL);
",
	"
	DROP TABLE pages;
	CREATE VIRTUAL TABLE words USING fts5 (
		doc_id UNINDEXED,
		page UNINDEXED,
		meta,
		body,
		tokenize = 'unicode61 remove_diacritics 2'
	);
	CREATE VIRTUAL TABLE grams USING fts5 (
		doc_id UNINDEXED,
		page UNINDEXED,
		meta,
		body,
		tokenize = 'trigram remove_diacritics 1'
	);
	CREATE TABLE texts (doc_id TEXT NOT NULL, row INTEGER NOT NULL);
	CREATE INDEX texts_doc ON texts (doc_id);
",
	"
	CREATE TABLE near_duplicates (a TEXT NOT NULL, b TEXT NOT NULL, PRIMARY KEY (a, b));
	CREATE INDEX near_duplicates_b ON near_duplicates (b);
",
	"DELETE FROM near_duplicates;",
];

const HIT_COLUMNS: &str = "d.id, d.path, d.title, d.area, d.doc_type, d.issuer, d.date, \
	d.amount_minor, d.currency, d.pages";
const NEWEST_FIRST: &str = "d.date IS NULL, d.date DESC, d.created_at DESC, d.id";

impl Index {
	/// Opens or creates the index and runs migrations.
	pub fn open(path: &Path) -> crate::Result<Self> {
		let mut conn = Connection::open(path)?;
		conn.busy_timeout(std::time::Duration::from_secs(5))?;
		conn.pragma_update(None, "journal_mode", "WAL")?;
		conn.pragma_update(None, "synchronous", "NORMAL")?;
		migrate(&mut conn)?;
		Ok(Self { conn })
	}
	/// Replaces the documents with the library's sidecars. Jobs are kept.
	pub fn rebuild(&mut self, library: &crate::Library) -> crate::Result<usize> {
		let documents = library.sidecars()?;
		let tx = self.conn.transaction()?;
		tx.execute_batch(
			"DELETE FROM documents; DELETE FROM tags; DELETE FROM facts; DELETE FROM sources;
			 DELETE FROM words; DELETE FROM grams; DELETE FROM texts; DELETE FROM near_duplicates;",
		)?;
		for (path, sidecar) in &documents {
			delete(&tx, sidecar.id)?;
			insert(&tx, path, sidecar)?;
		}
		let count: i64 = tx.query_row("SELECT COUNT(*) FROM documents", [], |row| row.get(0))?;
		tx.commit()?;
		Ok(count as usize)
	}
	/// Inserts or replaces one document by `sidecar.id`.
	pub fn upsert(&mut self, path: &Path, sidecar: &crate::Sidecar) -> crate::Result<()> {
		let tx = self.conn.transaction()?;
		delete(&tx, sidecar.id)?;
		insert(&tx, path, sidecar)?;
		tx.commit()?;
		Ok(())
	}
	pub fn remove(&mut self, id: Uuid) -> crate::Result<()> {
		let tx = self.conn.transaction()?;
		delete(&tx, id)?;
		tx.commit()?;
		Ok(())
	}
	/// The document's current path and sidecar.
	pub fn get(&self, id: Uuid) -> crate::Result<Option<(PathBuf, crate::Sidecar)>> {
		let row: Option<(String, String)> = self
			.conn
			.query_row(
				"SELECT path, sidecar FROM documents WHERE id = ?1",
				params![id.to_string()],
				|row| Ok((row.get(0)?, row.get(1)?)),
			)
			.optional()?;
		match row {
			Some((path, sidecar)) => {
				Ok(Some((PathBuf::from(path), serde_json::from_str(&sidecar)?)))
			}
			None => Ok(None),
		}
	}
	/// The document's current path and sidecar, when the viewer may see it.
	pub fn get_visible(
		&self,
		id: Uuid,
		viewer: &Viewer,
	) -> crate::Result<Option<(PathBuf, crate::Sidecar)>> {
		let mut values: Vec<Value> = vec![id.to_string().into()];
		let filter = visible(viewer, "d", &mut values);
		let row: Option<(String, String)> = self
			.conn
			.query_row(
				&format!("SELECT path, sidecar FROM documents d WHERE id = ?1 AND {filter}"),
				params_from_iter(values.iter()),
				|row| Ok((row.get(0)?, row.get(1)?)),
			)
			.optional()?;
		match row {
			Some((path, sidecar)) => {
				Ok(Some((PathBuf::from(path), serde_json::from_str(&sidecar)?)))
			}
			None => Ok(None),
		}
	}
	/// A document with exactly this file sha256 or source sha256.
	pub fn find_by_sha256(&self, sha256: &str) -> crate::Result<Option<Uuid>> {
		let id: Option<String> = self
			.conn
			.query_row(
				"SELECT id FROM (
					SELECT id, 0 AS via FROM documents WHERE sha256 = ?1
					UNION ALL
					SELECT doc_id, 1 FROM sources WHERE sha256 = ?1
				) ORDER BY via, id LIMIT 1",
				params![sha256],
				|row| row.get(0),
			)
			.optional()?;
		id.as_deref().map(parse_id).transpose()
	}
	/// The most similar document by text (word-shingle Jaccard), if at or above `threshold`.
	/// Words are compared in lowercase; each CJK character counts as a word.
	pub fn similar(&self, text: &str, threshold: f32) -> crate::Result<Option<(Uuid, f32)>> {
		let wanted = shingles(text);
		if wanted.is_empty() {
			return Ok(None);
		}
		let mut statement = self
			.conn
			.prepare("SELECT id, body FROM documents ORDER BY id")?;
		let mut rows = statement.query([])?;
		let mut best: Option<(String, f32)> = None;
		while let Some(row) = rows.next()? {
			let body = row.get_ref(1)?.as_str().map_err(rusqlite::Error::from)?;
			let score = jaccard(&wanted, &shingles(body));
			if score >= threshold && best.as_ref().is_none_or(|(_, top)| score > *top) {
				best = Some((row.get(0)?, score));
			}
		}
		best.map(|(id, score)| Ok((parse_id(&id)?, score)))
			.transpose()
	}
	/// Newest documents first when `query.text` is empty, else by FTS rank.
	pub fn search(&self, query: &Query) -> crate::Result<SearchResult> {
		let groups = parse_terms(query.text.as_deref().unwrap_or_default());
		let result = self.search_terms(query, &groups, false)?;
		if result.total > 0 || groups.len() < 2 {
			return Ok(result);
		}
		let mut partial = self.search_terms(query, &groups, true)?;
		partial.partial = partial.total > 0;
		Ok(partial)
	}

	/// Documents with a term of every group, or of `any` group.
	fn search_terms(
		&self,
		query: &Query,
		groups: &[Vec<Term>],
		any: bool,
	) -> crate::Result<SearchResult> {
		let terms: Vec<Term> = groups.concat();
		let (filter, values) = filter_sql(query, groups, any);
		let limit = if query.limit == 0 {
			DEFAULT_LIMIT
		} else {
			query.limit
		};

		let (total, mut hits) = if terms.is_empty() {
			let total: i64 = self.conn.query_row(
				&format!("SELECT COUNT(*) FROM documents d WHERE {filter}"),
				params_from_iter(values.iter()),
				|row| row.get(0),
			)?;
			let sql = format!(
				"SELECT {HIT_COLUMNS} FROM documents d WHERE {filter}
				 ORDER BY {NEWEST_FIRST} LIMIT {limit} OFFSET {}",
				query.offset
			);
			(total.unsigned_abs(), self.hits(&sql, &values)?)
		} else {
			let sql = format!(
				"SELECT {HIT_COLUMNS} FROM documents d WHERE {filter} ORDER BY {NEWEST_FIRST}"
			);
			let mut matching = self.hits(&sql, &values)?;
			if !query.newest {
				let scores = self.scores(&terms)?;
				matching.sort_by_cached_key(|hit| {
					let score =
						scores.get(&hit.id).copied().unwrap_or_default() * boost(hit, groups);
					if score < 0.0 {
						-((-score).ln() / SCORE_STEP).round() as i64
					} else {
						i64::MAX
					}
				});
			}
			let total = matching.len() as u64;
			let page = matching
				.into_iter()
				.skip(query.offset as usize)
				.take(limit as usize)
				.collect();
			(total, page)
		};

		for hit in &mut hits {
			hit.tags = self.strings(
				"SELECT tag FROM tags WHERE doc_id = ?1 ORDER BY pos",
				params![hit.id.to_string()],
			)?;
			if !terms.is_empty() {
				hit.snippet = self.snippet(hit.id, groups)?;
			}
		}
		Ok(SearchResult {
			total,
			hits,
			partial: false,
		})
	}
	pub fn facets(&self, viewer: &Viewer) -> crate::Result<Facets> {
		let mut values = Vec::new();
		let seen = visible(viewer, "d", &mut values);
		let facet = |sql: &str| self.facet(&sql.replace("{seen}", &seen), &values);
		Ok(Facets {
			areas: facet(
				"SELECT area, COUNT(*) FROM documents d WHERE {seen} GROUP BY 1 ORDER BY 2 DESC, 1",
			)?,
			doc_types: facet(
				"SELECT doc_type, COUNT(*) FROM documents d WHERE {seen}
				 GROUP BY 1 ORDER BY 2 DESC, 1",
			)?,
			issuers: facet(
				"SELECT issuer, COUNT(*) FROM documents d
				 WHERE issuer IS NOT NULL AND issuer <> '' AND {seen}
				 GROUP BY 1 ORDER BY 2 DESC, 1",
			)?,
			tags: facet(
				"SELECT t.tag, COUNT(DISTINCT t.doc_id) FROM tags t
				 JOIN documents d ON d.id = t.doc_id WHERE {seen}
				 GROUP BY 1 ORDER BY 2 DESC, 1",
			)?,
			years: facet(
				"SELECT substr(date, 1, 4), COUNT(*) FROM documents d
				 WHERE date IS NOT NULL AND {seen}
				 GROUP BY 1 ORDER BY 1 DESC",
			)?,
			currencies: facet(
				"SELECT f.currency, COUNT(DISTINCT f.doc_id) FROM facts f
				 JOIN documents d ON d.id = f.doc_id
				 WHERE f.currency IS NOT NULL AND f.currency <> '' AND {seen}
				 GROUP BY 1 ORDER BY 2 DESC, 1",
			)?,
		})
	}
	/// Known values, most used first, to keep the analysis consistent with the existing library.
	pub fn areas(&self) -> crate::Result<Vec<String>> {
		Ok(self
			.facets(&Viewer::everything())?
			.areas
			.into_iter()
			.map(|facet| facet.value)
			.collect())
	}
	pub fn issuers(&self) -> crate::Result<Vec<String>> {
		Ok(self
			.facets(&Viewer::everything())?
			.issuers
			.into_iter()
			.map(|facet| facet.value)
			.collect())
	}
	pub fn tags(&self) -> crate::Result<Vec<String>> {
		Ok(self
			.facets(&Viewer::everything())?
			.tags
			.into_iter()
			.map(|facet| facet.value)
			.collect())
	}
	/// Shared connection for the job queue, which lives in the same file.
	/// The pairs of near-duplicates whose documents the viewer both sees, newest first.
	pub fn duplicates(&self, viewer: &Viewer) -> crate::Result<Vec<[DuplicateCopy; 2]>> {
		let mut values = Vec::new();
		let first = visible(viewer, "d", &mut values);
		let second = visible(viewer, "e", &mut values);
		let pairs: Vec<(String, String)> = {
			let mut statement = self.conn.prepare(&format!(
				"SELECT n.a, n.b FROM near_duplicates n
				 JOIN documents d ON d.id = n.a JOIN documents e ON e.id = n.b
				 WHERE {first} AND {second}
				 ORDER BY max(coalesce(d.date, ''), coalesce(e.date, '')) DESC, n.a, n.b"
			))?;
			statement
				.query_map(params_from_iter(values.iter()), |row| {
					Ok((row.get(0)?, row.get(1)?))
				})?
				.collect::<Result<_, _>>()?
		};
		const PAIRED: &str = "(SELECT a FROM near_duplicates UNION SELECT b FROM near_duplicates)";
		let mut sidecars: HashMap<String, String> = {
			let mut statement = self.conn.prepare(&format!(
				"SELECT id, sidecar FROM documents WHERE id IN {PAIRED}"
			))?;
			statement
				.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
				.collect::<Result<_, _>>()?
		};
		let mut copies: HashMap<Uuid, DuplicateCopy> = HashMap::new();
		for hit in self.hits(
			&format!("SELECT {HIT_COLUMNS} FROM documents d WHERE d.id IN {PAIRED}"),
			&[],
		)? {
			if let Some(sidecar) = sidecars.remove(&hit.id.to_string()) {
				copies.insert(
					hit.id,
					DuplicateCopy::new(hit, serde_json::from_str(&sidecar)?),
				);
			}
		}
		let mut out = Vec::with_capacity(pairs.len());
		for (a, b) in pairs {
			if let (Some(a), Some(b)) = (copies.get(&parse_id(&a)?), copies.get(&parse_id(&b)?)) {
				out.push([a.clone(), b.clone()]);
			}
		}
		Ok(out)
	}
	/// The near-duplicates of a document that the viewer sees.
	pub fn duplicates_of(&self, id: Uuid, viewer: &Viewer) -> crate::Result<Vec<Uuid>> {
		let mut values: Vec<Value> = vec![id.to_string().into()];
		let filter = visible(viewer, "d", &mut values);
		let ids = self.strings(
			&format!(
				"SELECT d.id FROM near_duplicates n
				 JOIN documents d ON d.id = CASE WHEN n.a = ?1 THEN n.b ELSE n.a END
				 WHERE (n.a = ?1 OR n.b = ?1) AND {filter} ORDER BY d.id"
			),
			params_from_iter(values.iter()),
		)?;
		ids.iter().map(|id| parse_id(id)).collect()
	}
	pub fn connection(&self) -> &rusqlite::Connection {
		&self.conn
	}

	fn hits(&self, sql: &str, values: &[Value]) -> crate::Result<Vec<Hit>> {
		let mut statement = self.conn.prepare(sql)?;
		let mut rows = statement.query(params_from_iter(values.iter()))?;
		let mut hits = Vec::new();
		while let Some(row) = rows.next()? {
			hits.push(Hit {
				id: parse_id(&row.get::<_, String>(0)?)?,
				path: PathBuf::from(row.get::<_, String>(1)?),
				title: row.get(2)?,
				area: row.get(3)?,
				doc_type: row.get(4)?,
				issuer: row.get(5)?,
				date: row.get(6)?,
				tags: Vec::new(),
				amount_minor: row.get(7)?,
				currency: row.get(8)?,
				pages: row.get(9)?,
				snippet: None,
			});
		}
		Ok(hits)
	}

	/// Per document: the bm25 of its metadata row plus that of its best page, in both tables.
	fn scores(&self, terms: &[Term]) -> crate::Result<HashMap<Uuid, f64>> {
		let mut scores = HashMap::new();
		for (table, kind) in [("words", Kind::Words), ("grams", Kind::Grams)] {
			let expressions: Vec<String> = terms
				.iter()
				.filter(|term| term.kind == kind)
				.map(Term::rank_expression)
				.collect();
			if expressions.is_empty() {
				continue;
			}
			let mut statement = self.conn.prepare(&format!(
				"SELECT doc_id, page, bm25({table}, 0, 0, {META_WEIGHT}, {BODY_WEIGHT}) \
				 FROM {table} WHERE {table} MATCH ?1"
			))?;
			let mut rows = statement.query(params![expressions.join(" OR ")])?;
			let mut best: HashMap<Uuid, (f64, f64)> = HashMap::new();
			while let Some(row) = rows.next()? {
				let id = parse_id(&row.get::<_, String>(0)?)?;
				let page: i64 = row.get(1)?;
				let rank: f64 = row.get(2)?;
				let (meta, body) = best.entry(id).or_default();
				let slot = if page == 0 { meta } else { body };
				*slot = slot.min(rank);
			}
			for (id, (meta, body)) in best {
				*scores.entry(id).or_default() += meta + body;
			}
		}
		Ok(scores)
	}

	/// The page matching the most groups of terms, pages before the metadata row, then the
	/// densest.
	fn snippet(&self, id: Uuid, groups: &[Vec<Term>]) -> crate::Result<Option<String>> {
		let mut statement = self.conn.prepare_cached(
			"SELECT g.page, g.meta, g.body FROM texts t JOIN grams g ON g.rowid = t.row
			 WHERE t.doc_id = ?1 ORDER BY g.page",
		)?;
		let rows: Vec<(i64, String, String)> = statement
			.query_map(params![id.to_string()], |row| {
				Ok((row.get(0)?, row.get(1)?, row.get(2)?))
			})?
			.collect::<Result<_, _>>()?;
		let mut best: Option<((usize, bool, f64), String)> = None;
		for (page, meta, body) in rows {
			let chars: Vec<char> = if page == 0 { meta } else { body }.chars().collect();
			let (marked, found, count) = highlights(&chars, groups);
			let key = (found, page != 0, count as f64 / chars.len().max(1) as f64);
			if found > 0 && best.as_ref().is_none_or(|(top, _)| key > *top) {
				best = Some((key, mark(&chars, &marked)));
			}
		}
		Ok(best.map(|(_, snippet)| snippet))
	}

	fn facet(&self, sql: &str, values: &[Value]) -> crate::Result<Vec<Facet>> {
		let mut statement = self.conn.prepare(sql)?;
		let facets = statement
			.query_map(params_from_iter(values.iter()), |row| {
				Ok(Facet {
					value: row.get(0)?,
					count: row.get::<_, i64>(1)?.unsigned_abs(),
				})
			})?
			.collect::<Result<_, _>>()?;
		Ok(facets)
	}

	fn strings(&self, sql: &str, params: impl rusqlite::Params) -> crate::Result<Vec<String>> {
		let mut statement = self.conn.prepare(sql)?;
		let values = statement
			.query_map(params, |row| row.get(0))?
			.collect::<Result<_, _>>()?;
		Ok(values)
	}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MailStatus {
	/// Its files were written to the inbox.
	Ingested,
	/// Nothing in it to take, or already taken from another folder.
	Skipped,
	Failed,
}

impl MailStatus {
	fn as_str(self) -> &'static str {
		match self {
			Self::Ingested => "ingested",
			Self::Skipped => "skipped",
			Self::Failed => "failed",
		}
	}
	fn parse(value: &str) -> Self {
		match value {
			"ingested" => Self::Ingested,
			"skipped" => Self::Skipped,
			_ => Self::Failed,
		}
	}
}

/// A mail to remember as handled.
#[derive(Debug, Clone)]
pub struct MailRecord {
	pub account: String,
	pub folder: String,
	pub uidvalidity: u32,
	pub uid: u32,
	pub message_id: Option<String>,
	pub sender: Option<String>,
	pub subject: Option<String>,
	/// RFC 3339, when the server received it.
	pub received: Option<String>,
	pub status: MailStatus,
	pub files: u32,
	pub detail: Option<String>,
	pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProcessedMail {
	pub id: i64,
	pub account: String,
	pub folder: String,
	pub uidvalidity: u32,
	pub uid: u32,
	pub message_id: Option<String>,
	pub sender: Option<String>,
	pub subject: Option<String>,
	pub received: Option<String>,
	pub status: MailStatus,
	pub files: u32,
	pub detail: Option<String>,
	pub error: Option<String>,
	#[serde(with = "time::serde::rfc3339")]
	pub processed_at: OffsetDateTime,
}

/// What the pipeline records about an inbox file that came from a mail.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MailHint {
	pub mail: crate::MailSource,
	pub area: Option<String>,
	pub tags: Vec<String>,
}

const MAIL_COLUMNS: &str = "id, account, folder, uidvalidity, uid, message_id, sender, subject, \
	received, status, files, detail, error, processed_at";

fn processed_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProcessedMail> {
	let status: String = row.get(9)?;
	let at: i64 = row.get(13)?;
	Ok(ProcessedMail {
		id: row.get(0)?,
		account: row.get(1)?,
		folder: row.get(2)?,
		uidvalidity: row.get(3)?,
		uid: row.get(4)?,
		message_id: row.get(5)?,
		sender: row.get(6)?,
		subject: row.get(7)?,
		received: row.get(8)?,
		status: MailStatus::parse(&status),
		files: row.get(10)?,
		detail: row.get(11)?,
		error: row.get(12)?,
		processed_at: OffsetDateTime::from_unix_timestamp(at).unwrap_or(OffsetDateTime::UNIX_EPOCH),
	})
}

impl Index {
	/// Of `uids`, the ones already handled in this folder at this UIDVALIDITY.
	pub fn mail_handled(
		&self,
		account: &str,
		folder: &str,
		uidvalidity: u32,
		uids: &[u32],
	) -> crate::Result<HashSet<u32>> {
		let mut statement = self.conn.prepare_cached(
			"SELECT uid FROM mail_processed WHERE account = ?1 AND folder = ?2 AND uidvalidity = ?3",
		)?;
		let known: HashSet<u32> = statement
			.query_map(params![account, folder, uidvalidity], |row| row.get(0))?
			.collect::<Result<_, _>>()?;
		Ok(uids
			.iter()
			.copied()
			.filter(|uid| known.contains(uid))
			.collect())
	}

	/// Whether a mail with this Message-ID was handled, in any folder.
	pub fn mail_message_handled(&self, message_id: &str) -> crate::Result<bool> {
		Ok(self
			.conn
			.query_row(
				"SELECT 1 FROM mail_processed WHERE message_id = ?1 LIMIT 1",
				[message_id],
				|_| Ok(()),
			)
			.optional()?
			.is_some())
	}

	/// Records a handled mail and the provenance of the files it put in the inbox, keyed by
	/// their sha256. Returns the row id.
	pub fn mail_record(
		&mut self,
		record: &MailRecord,
		hints: &[(String, MailHint)],
	) -> crate::Result<i64> {
		let now = OffsetDateTime::now_utc().unix_timestamp();
		let tx = self.conn.transaction()?;
		tx.execute(
			"INSERT OR REPLACE INTO mail_processed (account, folder, uidvalidity, uid, message_id, \
			 sender, subject, received, status, files, detail, error, processed_at) \
			 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
			params![
				record.account,
				record.folder,
				record.uidvalidity,
				record.uid,
				record.message_id,
				record.sender,
				record.subject,
				record.received,
				record.status.as_str(),
				record.files,
				record.detail,
				record.error,
				now
			],
		)?;
		let id = tx.last_insert_rowid();
		for (sha256, hint) in hints {
			tx.execute(
				"INSERT OR REPLACE INTO mail_files (sha256, hint) VALUES (?1, ?2)",
				params![sha256, serde_json::to_string(hint)?],
			)?;
		}
		tx.commit()?;
		Ok(id)
	}

	pub fn mail_set_error(&self, id: i64, error: &str) -> crate::Result<()> {
		self.conn.execute(
			"UPDATE mail_processed SET error = ?2 WHERE id = ?1",
			params![id, error],
		)?;
		Ok(())
	}

	/// The newest handled mails, of one account or all.
	pub fn mail_processed(
		&self,
		account: Option<&str>,
		limit: usize,
	) -> crate::Result<Vec<ProcessedMail>> {
		let mut statement = self.conn.prepare_cached(&format!(
			"SELECT {MAIL_COLUMNS} FROM mail_processed WHERE ?1 IS NULL OR account = ?1 \
			 ORDER BY processed_at DESC, id DESC LIMIT ?2"
		))?;
		let rows = statement
			.query_map(params![account, limit as i64], processed_row)?
			.collect::<Result<_, _>>()?;
		Ok(rows)
	}

	/// How many mails of an account ended in each status.
	pub fn mail_counts(&self, account: &str) -> crate::Result<Vec<(MailStatus, u64)>> {
		let mut statement = self.conn.prepare_cached(
			"SELECT status, COUNT(*) FROM mail_processed WHERE account = ?1 GROUP BY status",
		)?;
		let counts = statement
			.query_map([account], |row| {
				let status: String = row.get(0)?;
				let count: i64 = row.get(1)?;
				Ok((MailStatus::parse(&status), count as u64))
			})?
			.collect::<Result<_, _>>()?;
		Ok(counts)
	}

	/// Forgets a handled mail so the next sync takes it again. Returns the row.
	pub fn mail_forget(&self, id: i64) -> crate::Result<Option<ProcessedMail>> {
		let row = self
			.conn
			.query_row(
				&format!("SELECT {MAIL_COLUMNS} FROM mail_processed WHERE id = ?1"),
				[id],
				processed_row,
			)
			.optional()?;
		if row.is_some() {
			self.conn
				.execute("DELETE FROM mail_processed WHERE id = ?1", [id])?;
		}
		Ok(row)
	}

	/// The provenance of an inbox file that came from a mail.
	pub fn mail_hint(&self, sha256: &str) -> crate::Result<Option<MailHint>> {
		let hint: Option<String> = self
			.conn
			.query_row(
				"SELECT hint FROM mail_files WHERE sha256 = ?1 AND json_extract(hint, '$.mail') IS NOT NULL",
				[sha256],
				|row| row.get(0),
			)
			.optional()?;
		Ok(hint.map(|hint| serde_json::from_str(&hint)).transpose()?)
	}

	/// Records who put a file with this content in the inbox, next to its mail provenance.
	pub fn set_uploader(&self, sha256: &str, user: &str) -> crate::Result<()> {
		self.conn.execute(
			"INSERT INTO mail_files (sha256, hint) VALUES (?1, json_object('uploader', ?2))
			 ON CONFLICT (sha256) DO UPDATE SET hint = json_set(hint, '$.uploader', ?2)",
			params![sha256, user],
		)?;
		Ok(())
	}

	/// Who put a file with this content in the inbox, when known.
	pub fn uploader(&self, sha256: &str) -> crate::Result<Option<String>> {
		Ok(self
			.conn
			.query_row(
				"SELECT json_extract(hint, '$.uploader') FROM mail_files WHERE sha256 = ?1",
				[sha256],
				|row| row.get::<_, Option<String>>(0),
			)
			.optional()?
			.flatten())
	}
}

fn migrate(conn: &mut Connection) -> crate::Result<()> {
	let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
	let version = usize::try_from(version).unwrap_or(usize::MAX);
	if version > MIGRATIONS.len() {
		return Err(crate::Error::Invalid(format!(
			"index schema {version} is newer than this binary ({})",
			MIGRATIONS.len()
		)));
	}
	for (done, sql) in MIGRATIONS.iter().enumerate().skip(version) {
		let tx = conn.transaction()?;
		tx.execute_batch(sql)?;
		if done + 1 == TEXT_SCHEMA {
			refill_text(&tx)?;
		}
		if DUPLICATES_SCHEMAS.contains(&(done + 1)) {
			relink_duplicates(&tx)?;
		}
		tx.pragma_update(None, "user_version", done as i64 + 1)?;
		tx.commit()?;
	}
	Ok(())
}

fn delete(conn: &Connection, id: Uuid) -> crate::Result<()> {
	let id = id.to_string();
	for table in ["words", "grams"] {
		conn.execute(
			&format!(
				"DELETE FROM {table} WHERE rowid IN (SELECT row FROM texts WHERE doc_id = ?1)"
			),
			params![id],
		)?;
	}
	for table in ["tags", "facts", "sources", "texts"] {
		conn.execute(
			&format!("DELETE FROM {table} WHERE doc_id = ?1"),
			params![id],
		)?;
	}
	conn.execute(
		"DELETE FROM near_duplicates WHERE a = ?1 OR b = ?1",
		params![id],
	)?;
	conn.execute("DELETE FROM documents WHERE id = ?1", params![id])?;
	Ok(())
}

fn insert(conn: &Connection, path: &Path, sidecar: &crate::Sidecar) -> crate::Result<()> {
	let id = sidecar.id.to_string();
	let (amount_minor, currency) = headline_amount(&sidecar.facts);
	let body = sidecar
		.pages
		.iter()
		.map(|page| page.text.as_str())
		.collect::<Vec<_>>()
		.join("\n");
	conn.execute(
		"INSERT INTO documents (id, path, title, area, doc_type, issuer, date, sha256, pages,
			amount_minor, currency, created_at, body, sidecar)
		 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
		params![
			id,
			path.to_string_lossy(),
			sidecar.title,
			sidecar.area,
			sidecar.doc_type,
			sidecar.issuer,
			sidecar.date,
			sidecar.sha256,
			sidecar.pages.len() as i64,
			amount_minor,
			currency,
			sidecar.created_at.unix_timestamp(),
			body,
			serde_json::to_string(sidecar)?,
		],
	)?;
	for (pos, tag) in sidecar.tags.iter().enumerate() {
		conn.execute(
			"INSERT INTO tags (doc_id, pos, tag) VALUES (?1, ?2, ?3)",
			params![id, pos as i64, tag],
		)?;
	}
	for fact in &sidecar.facts {
		conn.execute(
			"INSERT INTO facts (doc_id, key, value, amount_minor, currency)
			 VALUES (?1, ?2, ?3, ?4, ?5)",
			params![id, fact.key, fact.value, fact.amount_minor, fact.currency],
		)?;
	}
	for source in &sidecar.sources {
		conn.execute(
			"INSERT INTO sources (doc_id, sha256) VALUES (?1, ?2)",
			params![id, source.sha256],
		)?;
	}

	insert_text(conn, &id, sidecar)?;
	link_duplicates(conn, sidecar)
}

/// Records the near-duplicates of a document among the others of its date or with its file,
/// each pair once.
fn link_duplicates(conn: &Connection, sidecar: &crate::Sidecar) -> crate::Result<()> {
	let date = crate::duplicates::group_key(sidecar).and(sidecar.date.as_deref());
	let id = sidecar.id.to_string();
	let mut candidates = conn.prepare_cached(
		"SELECT sidecar FROM documents WHERE (date = ?1 OR sha256 = ?2) AND id != ?3",
	)?;
	let others: Vec<String> = candidates
		.query_map(params![date, sidecar.sha256, id], |row| row.get(0))?
		.collect::<Result<_, _>>()?;
	let mut link =
		conn.prepare_cached("INSERT OR IGNORE INTO near_duplicates (a, b) VALUES (?1, ?2)")?;
	for other in others {
		let other: crate::Sidecar = serde_json::from_str(&other)?;
		if crate::duplicates::near_duplicates(sidecar, &other) {
			let other = other.id.to_string();
			let (a, b) = if id < other {
				(&id, &other)
			} else {
				(&other, &id)
			};
			link.execute(params![a, b])?;
		}
	}
	Ok(())
}

fn relink_duplicates(conn: &Connection) -> crate::Result<()> {
	conn.execute("DELETE FROM near_duplicates", [])?;
	let mut statement = conn.prepare("SELECT sidecar FROM documents ORDER BY id")?;
	let rows: Vec<String> = statement
		.query_map([], |row| row.get(0))?
		.collect::<Result<_, _>>()?;
	for sidecar in rows {
		link_duplicates(conn, &serde_json::from_str(&sidecar)?)?;
	}
	Ok(())
}

/// A metadata row (page 0) and a row per page with text, in both text tables under the same
/// rowid, listed in `texts` because `doc_id` is not indexed in them.
fn insert_text(conn: &Connection, id: &str, sidecar: &crate::Sidecar) -> crate::Result<()> {
	let tags = sidecar.tags.join(", ");
	let meta: Vec<String> = [
		Some(sidecar.title.as_str()),
		sidecar.issuer.as_deref(),
		Some(sidecar.doc_type.as_str()),
		Some(tags.as_str()),
		sidecar.summary.as_deref(),
	]
	.into_iter()
	.flatten()
	.map(normalize)
	.filter(|part| !part.is_empty())
	.collect();
	let mut rows = vec![(0, meta.join(" | "), String::new())];
	for page in &sidecar.pages {
		let text = normalize(&page.text);
		if !text.is_empty() {
			rows.push((page.number.max(1), String::new(), text));
		}
	}
	let mut words = conn
		.prepare_cached("INSERT INTO words (doc_id, page, meta, body) VALUES (?1, ?2, ?3, ?4)")?;
	let mut grams = conn.prepare_cached(
		"INSERT INTO grams (rowid, doc_id, page, meta, body) VALUES (?1, ?2, ?3, ?4, ?5)",
	)?;
	let mut texts = conn.prepare_cached("INSERT INTO texts (doc_id, row) VALUES (?1, ?2)")?;
	for (page, meta, body) in &rows {
		let row = words.insert(params![id, page, meta, body])?;
		grams.execute(params![row, id, page, meta, body])?;
		texts.execute(params![id, row])?;
	}
	Ok(())
}

fn refill_text(conn: &Connection) -> crate::Result<()> {
	let mut statement = conn.prepare("SELECT id, sidecar FROM documents ORDER BY id")?;
	let rows: Vec<(String, String)> = statement
		.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
		.collect::<Result<_, _>>()?;
	for (id, sidecar) in rows {
		insert_text(conn, &id, &serde_json::from_str(&sidecar)?)?;
	}
	Ok(())
}

fn headline_amount(facts: &[crate::Fact]) -> (Option<i64>, Option<&str>) {
	let amounts = || {
		facts
			.iter()
			.filter_map(|fact| Some((fact.amount_minor?, fact)))
	};
	let total = amounts().find(|(_, fact)| fact.key.eq_ignore_ascii_case("total"));
	let mut largest: Option<(i64, &crate::Fact)> = None;
	for candidate in amounts() {
		if largest.is_none_or(|(amount, _)| candidate.0 > amount) {
			largest = Some(candidate);
		}
	}
	match total.or(largest) {
		Some((amount, fact)) => (Some(amount), fact.currency.as_deref()),
		None => (None, None),
	}
}

fn filter_sql(query: &Query, groups: &[Vec<Term>], any: bool) -> (String, Vec<Value>) {
	let mut values: Vec<Value> = Vec::new();
	let mut clauses = vec![visible(&query.viewer, "d", &mut values)];
	let mut bind = |value: Value| {
		values.push(value);
		format!("?{}", values.len())
	};

	for (column, wanted) in [
		("area", &query.area),
		("doc_type", &query.doc_type),
		("issuer", &query.issuer),
	] {
		if let Some(wanted) = wanted {
			clauses.push(format!("d.{column} = {}", bind(wanted.clone().into())));
		}
	}
	for tag in &query.tags {
		clauses.push(format!(
			"EXISTS (SELECT 1 FROM tags t WHERE t.doc_id = d.id AND t.tag = {})",
			bind(tag.clone().into())
		));
	}
	if let Some(from) = &query.date_from {
		clauses.push(format!("d.date >= {}", bind(from.clone().into())));
	}
	if let Some(to) = &query.date_to {
		clauses.push(format!("d.date <= {}", bind(to.clone().into())));
	}
	if let Some(month) = query.month {
		clauses.push(format!(
			"substr(d.date, 6, 2) = {}",
			bind(format!("{month:02}").into())
		));
	}
	if query.amount_min.is_some() || query.amount_max.is_some() || query.currency.is_some() {
		let mut fact = vec!["f.doc_id = d.id AND f.amount_minor IS NOT NULL".to_string()];
		if let Some(min) = query.amount_min {
			fact.push(format!("f.amount_minor >= {}", bind(min.into())));
		}
		if let Some(max) = query.amount_max {
			fact.push(format!("f.amount_minor <= {}", bind(max.into())));
		}
		if let Some(currency) = &query.currency {
			fact.push(format!(
				"f.currency = {} COLLATE NOCASE",
				bind(currency.clone().into())
			));
		}
		clauses.push(format!(
			"EXISTS (SELECT 1 FROM facts f WHERE {})",
			fact.join(" AND ")
		));
	}
	let mut rows = |term: &Term| match term.kind {
		Kind::Words | Kind::Grams => {
			let table = if term.kind == Kind::Words {
				"words"
			} else {
				"grams"
			};
			let expression = bind(term.match_expression().into());
			format!("SELECT doc_id FROM {table} WHERE {table} MATCH {expression}")
		}
		Kind::Like => {
			let pattern = bind(like_pattern(&term.text).into());
			format!(
				"SELECT doc_id FROM grams \
				 WHERE meta LIKE {pattern} ESCAPE '\\' OR body LIKE {pattern} ESCAPE '\\'"
			)
		}
	};
	let mut union = |terms: &mut dyn Iterator<Item = &Term>| {
		let selects: Vec<String> = terms.map(&mut rows).collect();
		format!("d.id IN ({})", selects.join(" UNION "))
	};
	if any && !groups.is_empty() {
		clauses.push(union(&mut groups.iter().flatten()));
	} else {
		for group in groups {
			clauses.push(union(&mut group.iter()));
		}
	}
	(clauses.join(" AND "), values)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
	/// Whole words in the `words` table, the last one as a prefix when `Term::prefix`.
	Words,
	/// A substring in the `grams` table: CJK, or no letters or digits at all.
	Grams,
	/// A substring shorter than a trigram, by LIKE.
	Like,
}

#[derive(Debug, Clone)]
struct Term {
	text: String,
	folded: Vec<char>,
	/// Folded words, for `Kind::Words`.
	words: Vec<Vec<char>>,
	prefix: bool,
	kind: Kind,
}

impl Term {
	fn new(text: String, quoted: bool, star: bool) -> Self {
		let folded = fold(&text);
		let words: Vec<Vec<char>> = tokens(&folded)
			.into_iter()
			.map(|(start, end)| folded[start..end].to_vec())
			.collect();
		let kind = if words.is_empty() || text.chars().any(is_cjk) {
			if folded.len() >= FTS_MIN_CHARS {
				Kind::Grams
			} else {
				Kind::Like
			}
		} else {
			Kind::Words
		};
		let prefix = kind == Kind::Words
			&& (star
				|| !quoted
					&& words
						.last()
						.is_some_and(|word| word.len() >= PREFIX_MIN_CHARS));
		Self {
			text,
			folded,
			words,
			prefix,
			kind,
		}
	}

	fn phrase(&self) -> String {
		let words: Vec<String> = self
			.words
			.iter()
			.map(|word| word.iter().collect())
			.collect();
		format!("\"{}\"", words.join(" "))
	}

	fn match_expression(&self) -> String {
		match self.kind {
			Kind::Words if self.prefix => format!("{}*", self.phrase()),
			Kind::Words => self.phrase(),
			Kind::Grams | Kind::Like => fts_phrase(&self.text),
		}
	}

	/// A whole word counts twice, so `facture` ranks above `factures`.
	fn rank_expression(&self) -> String {
		if self.prefix {
			format!("({} OR {})", self.match_expression(), self.phrase())
		} else {
			self.match_expression()
		}
	}

	fn is_stop_word(&self) -> bool {
		self.kind == Kind::Words
			&& self.words.len() == 1
			&& STOP_WORDS
				.iter()
				.any(|stop| stop.chars().eq(self.words[0].iter().copied()))
	}

	/// Whether `words` (folded) contain every word of the term, or `folded` its substring.
	fn found_in(&self, folded: &[char], words: &[&[char]]) -> bool {
		match self.kind {
			Kind::Words => self
				.words
				.iter()
				.all(|word| words.contains(&word.as_slice())),
			Kind::Grams | Kind::Like => find(folded, &self.folded, 0).is_some(),
		}
	}
}

/// Words and quoted phrases of a search box, each first in a group with its synonyms; whitespace
/// collapsed, duplicates dropped, stop-words dropped unless there is nothing else. Quoted
/// phrases and `word*` have no synonyms.
fn parse_terms(text: &str) -> Vec<Vec<Term>> {
	let mut groups: Vec<Vec<Term>> = Vec::new();
	let mut push = |part: &str, quoted: bool, synonyms: &[&str]| {
		let star = !quoted && part.ends_with('*');
		let part = normalize(if quoted {
			part
		} else {
			part.trim_end_matches('*')
		});
		if part.is_empty() || groups.iter().any(|known| known[0].folded == fold(&part)) {
			return;
		}
		let mut group = vec![Term::new(part, quoted, star)];
		for synonym in synonyms {
			let term = Term::new((*synonym).to_string(), false, false);
			if !group.iter().any(|known| known.folded == term.folded) {
				group.push(term);
			}
		}
		groups.push(group);
	};
	for (index, part) in text.split('"').enumerate() {
		if index % 2 == 1 {
			push(part, true, &[]);
			continue;
		}
		let words: Vec<&str> = part.split_whitespace().collect();
		let mut at = 0;
		while at < words.len() {
			match crate::synonyms::lookup(&words[at..]).filter(|_| !words[at].ends_with('*')) {
				Some((len, synonyms)) => {
					push(&words[at..at + len].join(" "), false, synonyms);
					at += len;
				}
				None => {
					push(words[at], false, &[]);
					at += 1;
				}
			}
		}
	}
	let stop = |group: &Vec<Term>| group.len() == 1 && group[0].is_stop_word();
	if groups.iter().any(|group| !stop(group)) {
		groups.retain(|group| !stop(group));
	}
	groups
}

/// Rank multiplier for documents whose issuer or type the query names.
fn boost(hit: &Hit, groups: &[Vec<Term>]) -> f64 {
	let typed: Vec<&Term> = groups.iter().map(|group| &group[0]).collect();
	let mut boost = 1.0;
	if let Some(issuer) = hit.issuer.as_deref() {
		let folded = fold(issuer);
		let spans = tokens(&folded);
		let words: Vec<&[char]> = spans
			.iter()
			.map(|&(start, end)| &folded[start..end])
			.collect();
		let asked: Vec<&[char]> = typed
			.iter()
			.flat_map(|term| term.words.iter().map(Vec::as_slice))
			.collect();
		if !asked.is_empty() && asked == words {
			boost += ISSUER_EXACT_BOOST;
		} else if typed.iter().any(|term| term.found_in(&folded, &words)) {
			boost += ISSUER_BOOST;
		}
	}
	let folded = fold(&hit.doc_type);
	let spans = tokens(&folded);
	let words: Vec<&[char]> = spans
		.iter()
		.map(|&(start, end)| &folded[start..end])
		.collect();
	if groups
		.iter()
		.flatten()
		.any(|term| term.kind == Kind::Words && term.found_in(&folded, &words))
	{
		boost += TYPE_BOOST;
	}
	boost
}

fn fts_phrase(term: &str) -> String {
	format!("\"{}\"", term.replace('"', "\"\""))
}

fn like_pattern(term: &str) -> String {
	let mut pattern = String::from("%");
	for c in term.chars() {
		if matches!(c, '\\' | '%' | '_') {
			pattern.push('\\');
		}
		pattern.push(c);
	}
	pattern.push('%');
	pattern
}

fn normalize(text: &str) -> String {
	let mut out = String::with_capacity(text.len());
	for word in text.split(|c: char| c.is_whitespace() || c.is_control()) {
		if word.is_empty() {
			continue;
		}
		if !out.is_empty() {
			out.push(' ');
		}
		out.push_str(word);
	}
	out
}

/// Lowercase without accents, one char per char so positions carry over to the original.
pub(crate) fn fold(text: &str) -> Vec<char> {
	text.chars().map(fold_char).collect()
}

fn fold_char(c: char) -> char {
	let lower = c.to_lowercase().next().unwrap_or(c);
	if !('\u{e0}'..='\u{17f}').contains(&lower) {
		return lower;
	}
	DIACRITICS
		.iter()
		.find(|(accented, _)| accented.contains(lower))
		.map_or(lower, |(_, base)| *base)
}

/// Runs of letters and digits, as the unicode61 tokenizer splits them.
pub(crate) fn tokens(chars: &[char]) -> Vec<(usize, usize)> {
	let mut spans = Vec::new();
	let mut start = None;
	for (at, c) in chars.iter().enumerate() {
		match (c.is_alphanumeric(), start) {
			(true, None) => start = Some(at),
			(false, Some(from)) => {
				spans.push((from, at));
				start = None;
			}
			_ => {}
		}
	}
	if let Some(from) = start {
		spans.push((from, chars.len()));
	}
	spans
}

fn find(haystack: &[char], needle: &[char], from: usize) -> Option<usize> {
	if needle.is_empty() || haystack.len() < needle.len() {
		return None;
	}
	(from..=haystack.len() - needle.len()).find(|&at| haystack[at..at + needle.len()] == *needle)
}

/// Which chars are inside a match, how many groups of terms matched and how many matches there
/// are. Words match whole words, substrings anywhere.
fn highlights(chars: &[char], groups: &[Vec<Term>]) -> (Vec<bool>, usize, usize) {
	let folded: Vec<char> = chars.iter().copied().map(fold_char).collect();
	let words = tokens(&folded);
	let mut marked = vec![false; chars.len()];
	let (mut found, mut count) = (0, 0);
	for group in groups {
		let before = count;
		for term in group {
			count += mark_term(&folded, &words, term, &mut marked);
		}
		if count > before {
			found += 1;
		}
	}
	(marked, found, count)
}

/// Marks the matches of one term, returns how many there are.
fn mark_term(folded: &[char], words: &[(usize, usize)], term: &Term, marked: &mut [bool]) -> usize {
	let mut spans = Vec::new();
	match term.kind {
		Kind::Words => {
			let last = term.words.len() - 1;
			for window in words.windows(term.words.len()) {
				let hit = window.iter().zip(&term.words).enumerate().all(
					|(at, (&(start, end), wanted))| {
						let word = &folded[start..end];
						if term.prefix && at == last {
							word.starts_with(wanted)
						} else {
							word == wanted.as_slice()
						}
					},
				);
				if hit {
					spans.push((window[0].0, window[last].1));
				}
			}
		}
		Kind::Grams | Kind::Like => {
			let mut from = 0;
			while let Some(at) = find(folded, &term.folded, from) {
				spans.push((at, at + term.folded.len()));
				from = at + term.folded.len();
			}
		}
	}
	for &(start, end) in &spans {
		marked[start..end].fill(true);
	}
	spans.len()
}

/// A window of `chars` around the first mark, HTML-escaped, with the marks in `<mark>`.
fn mark(chars: &[char], marked: &[bool]) -> String {
	let first = marked.iter().position(|hit| *hit).unwrap_or(0);
	let start = first.saturating_sub(SNIPPET_BEFORE);
	let end = (start + SNIPPET_CHARS).min(chars.len());

	let mut out = String::new();
	if start > 0 {
		out.push('…');
	}
	let mut open = false;
	for at in start..end {
		if marked[at] != open {
			out.push_str(if open { "</mark>" } else { "<mark>" });
			open = !open;
		}
		match chars[at] {
			'&' => out.push_str("&amp;"),
			'<' => out.push_str("&lt;"),
			'>' => out.push_str("&gt;"),
			'"' => out.push_str("&quot;"),
			'\'' => out.push_str("&#39;"),
			c => out.push(c),
		}
	}
	if open {
		out.push_str("</mark>");
	}
	if end < chars.len() {
		out.push('…');
	}
	out
}

fn shingles(text: &str) -> HashSet<u64> {
	let mut words: Vec<String> = Vec::new();
	let mut word = String::new();
	for c in text.chars() {
		let solo = is_cjk(c);
		if (solo || !c.is_alphanumeric()) && !word.is_empty() {
			words.push(std::mem::take(&mut word));
		}
		if solo {
			words.push(c.to_string());
		} else if c.is_alphanumeric() {
			word.extend(c.to_lowercase());
		}
	}
	if !word.is_empty() {
		words.push(word);
	}
	if words.is_empty() {
		return HashSet::new();
	}
	words
		.windows(SHINGLE_WORDS.min(words.len()))
		.map(|window| {
			let mut hasher = DefaultHasher::new();
			window.hash(&mut hasher);
			hasher.finish()
		})
		.collect()
}

fn is_cjk(c: char) -> bool {
	matches!(u32::from(c),
		0x3040..=0x30FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xAC00..=0xD7AF
		| 0xF900..=0xFAFF | 0xFF66..=0xFF9F | 0x20000..=0x2FA1F)
}

fn jaccard(a: &HashSet<u64>, b: &HashSet<u64>) -> f32 {
	let shared = a.intersection(b).count();
	let union = a.len() + b.len() - shared;
	if union == 0 {
		0.0
	} else {
		shared as f32 / union as f32
	}
}

fn parse_id(id: &str) -> crate::Result<Uuid> {
	Uuid::parse_str(id).map_err(|error| crate::Error::Invalid(format!("document id {id}: {error}")))
}

#[cfg(test)]
mod tests {
	use serde_json::json;
	use time::OffsetDateTime;

	use super::*;
	use crate::testutil::sidecar;
	use crate::{Fact, JobKind, JobQueue, Library, PageText, Sidecar, Source, TextSource};

	fn index() -> (tempfile::TempDir, Index) {
		let dir = tempfile::tempdir().unwrap();
		let index = Index::open(&dir.path().join(".index.sqlite")).unwrap();
		(dir, index)
	}

	fn doc(title: &str, area: &str, date: Option<&str>, pages: &[&str]) -> Sidecar {
		let mut s = sidecar(title);
		s.area = area.into();
		s.date = date.map(str::to_string);
		s.pages = pages
			.iter()
			.enumerate()
			.map(|(i, text)| PageText {
				number: i as u32 + 1,
				text: text.to_string(),
				source: TextSource::Ocr,
			})
			.collect();
		s.sha256 = crate::hash::sha256(title.as_bytes());
		s
	}

	fn amount(key: &str, minor: i64, currency: &str) -> Fact {
		Fact {
			key: key.into(),
			value: minor.to_string(),
			amount_minor: Some(minor),
			currency: Some(currency.into()),
		}
	}

	fn add(index: &mut Index, sidecar: &Sidecar) -> PathBuf {
		let path = crate::naming::document_path(sidecar, "pdf");
		index.upsert(&path, sidecar).unwrap();
		path
	}

	fn text(query: &str) -> Query {
		Query {
			text: Some(query.into()),
			..Query::default()
		}
	}

	fn titles(index: &Index, query: &Query) -> Vec<String> {
		index
			.search(query)
			.unwrap()
			.hits
			.into_iter()
			.map(|hit| hit.title)
			.collect()
	}

	fn sorted(mut values: Vec<String>) -> Vec<String> {
		values.sort();
		values
	}

	fn corpus(index: &mut Index) -> [Sidecar; 4] {
		let mut insurance = doc(
			"健康保険料 領収書",
			"Health",
			Some("2024-04-10"),
			&[
				"令和6年度 国民健康保険料の納付を確認しました。\n合計 12,300円",
				"二枚目 口座振替のご案内",
			],
		);
		insurance.issuer = Some("渋谷区役所".into());
		insurance.doc_type = "receipt".into();
		insurance.tags = vec!["insurance".into(), "tax-deductible".into()];
		insurance.facts = vec![amount("total", 12_300, "JPY"), amount("tax", 0, "JPY")];

		let mut invoice = doc(
			"Electricity Invoice",
			"Housing",
			Some("2023-11-02"),
			&[
				"Invoice number 4411 for electricity usage in October. Amount due <b>8,450</b> & fees",
			],
		);
		invoice.issuer = Some("Tokyo Power".into());
		invoice.doc_type = "invoice".into();
		invoice.tags = vec!["utilities".into()];
		invoice.summary = Some("Monthly electricity bill".into());
		invoice.facts = vec![amount("net", 7_682, "JPY"), amount("due", 8_450, "JPY")];

		let mut payslip = doc(
			"Payslip March",
			"Work",
			Some("2024-03-25"),
			&["Gross pay 5,000.00 net pay 3,900.50"],
		);
		payslip.issuer = Some("Acme Inc.".into());
		payslip.doc_type = "payslip".into();
		payslip.tags = vec!["insurance".into(), "salary".into()];
		payslip.facts = vec![
			amount("net_pay", 390_050, "USD"),
			amount("gross", 500_000, "USD"),
		];

		let mut lease = doc(
			"Lease Agreement",
			"Housing",
			None,
			&["The tenant agrees to pay rent"],
		);
		lease.doc_type = "contract".into();
		lease.created_at = OffsetDateTime::from_unix_timestamp(1_600_000_000).unwrap();

		let all = [insurance, invoice, payslip, lease];
		for sidecar in &all {
			add(index, sidecar);
		}
		all
	}

	#[test]
	fn open_uses_wal_and_keeps_data_across_reopens() {
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join(".index.sqlite");
		let s = doc("Kept", "A", None, &["text"]);
		{
			let mut index = Index::open(&path).unwrap();
			let mode: String = index
				.connection()
				.query_row("PRAGMA journal_mode", [], |row| row.get(0))
				.unwrap();
			assert_eq!(mode, "wal");
			add(&mut index, &s);
		}
		let index = Index::open(&path).unwrap();
		let version: i64 = index
			.connection()
			.query_row("PRAGMA user_version", [], |row| row.get(0))
			.unwrap();
		assert_eq!(version, MIGRATIONS.len() as i64);
		assert_eq!(index.get(s.id).unwrap().unwrap().1, s);
	}

	#[test]
	fn open_refuses_a_newer_schema() {
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join(".index.sqlite");
		Index::open(&path)
			.unwrap()
			.connection()
			.pragma_update(None, "user_version", 99)
			.unwrap();
		assert!(matches!(Index::open(&path), Err(crate::Error::Invalid(_))));
	}

	#[test]
	fn upsert_get_and_remove() {
		let (_dir, mut index) = index();
		let mut s = doc("First", "A", Some("2024-01-01"), &["alpha beta gamma"]);
		s.extra.insert("future".into(), json!(true));
		let path = add(&mut index, &s);
		assert_eq!(index.get(s.id).unwrap(), Some((path, s.clone())));
		assert_eq!(index.get(Uuid::new_v4()).unwrap(), None);

		s.title = "Second".into();
		s.pages[0].text = "delta epsilon".into();
		s.tags = vec!["new".into()];
		let moved = PathBuf::from("B/2024/Second.pdf");
		index.upsert(&moved, &s).unwrap();
		assert_eq!(index.get(s.id).unwrap(), Some((moved.clone(), s.clone())));
		assert!(titles(&index, &text("alpha")).is_empty());
		let found = index.search(&text("epsilon")).unwrap();
		assert_eq!(found.total, 1);
		assert_eq!(found.hits[0].path, moved);
		assert_eq!(found.hits[0].tags, ["new"]);

		index.remove(s.id).unwrap();
		index.remove(s.id).unwrap();
		assert_eq!(index.get(s.id).unwrap(), None);
		assert_eq!(index.search(&Query::default()).unwrap().total, 0);
		assert!(titles(&index, &text("epsilon")).is_empty());
		assert!(index.tags().unwrap().is_empty());
	}

	#[test]
	fn find_by_file_or_source_sha256() {
		let (_dir, mut index) = index();
		let mut s = doc("Doc", "A", None, &["x"]);
		s.sources = vec![Source {
			sha256: "source-hash".into(),
			original_name: "scan.pdf".into(),
			pages: vec![],
			mail: None,
		}];
		add(&mut index, &s);
		assert_eq!(index.find_by_sha256(&s.sha256).unwrap(), Some(s.id));
		assert_eq!(index.find_by_sha256("source-hash").unwrap(), Some(s.id));
		assert_eq!(index.find_by_sha256("source").unwrap(), None);
		index.remove(s.id).unwrap();
		assert_eq!(index.find_by_sha256("source-hash").unwrap(), None);
	}

	#[test]
	fn empty_query_lists_newest_first() {
		let (_dir, mut index) = index();
		corpus(&mut index);
		let result = index.search(&Query::default()).unwrap();
		assert_eq!(result.total, 4);
		let order: Vec<&str> = result.hits.iter().map(|hit| hit.title.as_str()).collect();
		assert_eq!(
			order,
			[
				"健康保険料 領収書",
				"Payslip March",
				"Electricity Invoice",
				"Lease Agreement"
			]
		);
		assert!(result.hits.iter().all(|hit| hit.snippet.is_none()));
		assert_eq!(titles(&index, &text("   ")).len(), 4);
	}

	#[test]
	fn hits_carry_the_document_summary_fields() {
		let (_dir, mut index) = index();
		let [insurance, invoice, _, lease] = corpus(&mut index);
		let result = index.search(&Query::default()).unwrap();
		let hit = &result.hits[0];
		assert_eq!(hit.id, insurance.id);
		assert_eq!(
			hit.path,
			PathBuf::from("Health/2024/2024-04-10 渋谷区役所 健康保険料 領収書.pdf")
		);
		assert_eq!(hit.area, "Health");
		assert_eq!(hit.doc_type, "receipt");
		assert_eq!(hit.issuer.as_deref(), Some("渋谷区役所"));
		assert_eq!(hit.date.as_deref(), Some("2024-04-10"));
		assert_eq!(hit.tags, ["insurance", "tax-deductible"]);
		assert_eq!(hit.amount_minor, Some(12_300));
		assert_eq!(hit.currency.as_deref(), Some("JPY"));
		assert_eq!(hit.pages, 2);

		let by_id = |id: Uuid| result.hits.iter().find(|hit| hit.id == id).unwrap();
		assert_eq!(by_id(invoice.id).amount_minor, Some(8_450));
		assert_eq!(by_id(lease.id).amount_minor, None);
		assert_eq!(by_id(lease.id).currency, None);
		assert_eq!(by_id(lease.id).date, None);
	}

	#[test]
	fn total_fact_wins_over_larger_amounts() {
		let (_dir, mut index) = index();
		let mut s = doc("Receipt", "A", None, &["x"]);
		s.facts = vec![
			amount("deposit", 90_000, "EUR"),
			Fact {
				key: "total".into(),
				value: "n/a".into(),
				amount_minor: None,
				currency: None,
			},
			amount("total", 1_200, "JPY"),
		];
		add(&mut index, &s);
		let hit = &index.search(&Query::default()).unwrap().hits[0];
		assert_eq!(
			(hit.amount_minor, hit.currency.as_deref()),
			(Some(1_200), Some("JPY"))
		);
	}

	#[test]
	fn english_words_match_as_prefixes_in_any_case() {
		let (_dir, mut index) = index();
		corpus(&mut index);
		assert_eq!(titles(&index, &text("electr")), ["Electricity Invoice"]);
		assert_eq!(
			titles(&index, &text("ELECTRICITY")),
			["Electricity Invoice"]
		);
		assert_eq!(titles(&index, &text("invoice*")), ["Electricity Invoice"]);
		assert_eq!(titles(&index, &text("tenant rent")), ["Lease Agreement"]);
		let partial = index.search(&text("tenant electricity")).unwrap();
		assert!(partial.partial);
		assert_eq!(partial.total, 2);
		assert!(titles(&index, &text("zzzznothing")).is_empty());
	}

	#[test]
	fn words_may_match_across_pages_and_metadata() {
		let (_dir, mut index) = index();
		corpus(&mut index);
		assert_eq!(
			titles(&index, &text("口座振替 国民健康")),
			["健康保険料 領収書"]
		);
		assert_eq!(
			titles(&index, &text("power october")),
			["Electricity Invoice"]
		);
		assert_eq!(titles(&index, &text("monthly")), ["Electricity Invoice"]);
		assert_eq!(titles(&index, &text("utilities")), ["Electricity Invoice"]);
		assert_eq!(titles(&index, &text("acme gross")), ["Payslip March"]);
	}

	#[test]
	fn japanese_substrings_are_found() {
		let (_dir, mut index) = index();
		corpus(&mut index);
		for query in [
			"健康保険",
			"国民健康保険料",
			"渋谷区",
			"領収書",
			"納付を確認",
		] {
			assert_eq!(
				titles(&index, &text(query)),
				["健康保険料 領収書"],
				"{query}"
			);
		}
		assert!(titles(&index, &text("厚生年金")).is_empty());
		let hit = &index.search(&text("健康保険")).unwrap().hits[0];
		assert_eq!(
			hit.snippet.as_deref(),
			Some("令和6年度 国民<mark>健康保険</mark>料の納付を確認しました。 合計 12,300円")
		);
	}

	#[test]
	fn short_queries_match_cjk_substrings_and_latin_words() {
		let (_dir, mut index) = index();
		corpus(&mut index);
		assert_eq!(titles(&index, &text("健康")), ["健康保険料 領収書"]);
		assert_eq!(titles(&index, &text("円")), ["健康保険料 領収書"]);
		assert!(titles(&index, &text("pa")).is_empty());
		assert_eq!(
			sorted(titles(&index, &text("pay"))),
			["Lease Agreement", "Payslip March"]
		);
		assert_eq!(
			titles(&index, &text("保険 納付を確認")),
			["健康保険料 領収書"]
		);
		let hit = &index.search(&text("保険")).unwrap().hits[0];
		assert!(
			hit.snippet
				.as_deref()
				.unwrap()
				.contains("国民健康<mark>保険</mark>料")
		);
		assert!(titles(&index, &text("%")).is_empty());
		assert!(titles(&index, &text("_")).is_empty());
	}

	#[test]
	fn quoted_phrases_must_be_adjacent() {
		let (_dir, mut index) = index();
		corpus(&mut index);
		assert_eq!(
			titles(&index, &text("\"amount due\"")),
			["Electricity Invoice"]
		);
		assert!(titles(&index, &text("\"due amount\"")).is_empty());
		assert_eq!(titles(&index, &text("due amount")), ["Electricity Invoice"]);
		assert_eq!(titles(&index, &text("\"net pay\" acme")), ["Payslip March"]);
		assert_eq!(
			titles(&index, &text("\"tenant agrees")),
			["Lease Agreement"]
		);
	}

	#[test]
	fn query_syntax_is_taken_literally() {
		let (_dir, mut index) = index();
		corpus(&mut index);
		for query in [
			"AND",
			"NOT tenant",
			"text:tenant",
			"(tenant",
			"ten^ant",
			"-tenant",
			"\"\"",
			"*",
		] {
			index.search(&text(query)).unwrap();
		}
		assert_eq!(titles(&index, &text("3,900.50")), ["Payslip March"]);
		assert_eq!(
			titles(&index, &text("<b>8,450</b>")),
			["Electricity Invoice"]
		);
	}

	#[test]
	fn snippets_are_escaped_except_for_mark() {
		let (_dir, mut index) = index();
		corpus(&mut index);
		let hit = &index.search(&text("amount fees")).unwrap().hits[0];
		assert_eq!(
			hit.snippet.as_deref(),
			Some(
				"… 4411 for electricity usage in October. <mark>Amount</mark> due \
				 &lt;b&gt;8,450&lt;/b&gt; &amp; <mark>fees</mark>"
			)
		);
		let hit = &index.search(&text("<b>")).unwrap().hits[0];
		assert!(
			hit.snippet
				.as_deref()
				.unwrap()
				.contains("&lt;<mark>b</mark>&gt;8,450")
		);

		let mut quotes = doc(
			"Quotes",
			"A",
			None,
			&["she said \"it's <script>alert(1)</script>\""],
		);
		quotes.summary = Some("x".into());
		add(&mut index, &quotes);
		let hit = &index.search(&text("said")).unwrap().hits[0];
		assert_eq!(
			hit.snippet.as_deref(),
			Some(
				"she <mark>said</mark> &quot;it&#39;s &lt;script&gt;alert(1)&lt;/script&gt;&quot;"
			)
		);
	}

	#[test]
	fn snippets_prefer_page_text_and_are_bounded() {
		let (_dir, mut index) = index();
		let filler = "lorem ipsum ".repeat(100);
		let s = doc(
			"Needle report",
			"A",
			None,
			&[
				"nothing here",
				&format!("{filler}the needle is here {filler}"),
			],
		);
		add(&mut index, &s);
		let snippet = index.search(&text("needle")).unwrap().hits[0]
			.snippet
			.clone()
			.unwrap();
		assert!(snippet.starts_with('…') && snippet.ends_with('…'));
		assert!(snippet.contains("the <mark>needle</mark> is here"));
		assert!(snippet.chars().count() < 230);

		let snippet = index.search(&text("report")).unwrap().hits[0]
			.snippet
			.clone()
			.unwrap();
		assert_eq!(snippet, "Needle <mark>report</mark> | letter");
	}

	#[test]
	fn text_results_are_ranked_and_paged() {
		let (_dir, mut index) = index();
		let strong = doc(
			"Insurance",
			"A",
			Some("2020-01-01"),
			&["insurance insurance insurance"],
		);
		let filler = "unrelated words about many other things ".repeat(40);
		let weak = doc(
			"Other",
			"A",
			Some("2024-01-01"),
			&[&format!("{filler} insurance {filler}")],
		);
		add(&mut index, &weak);
		add(&mut index, &strong);
		assert_eq!(titles(&index, &text("insurance")), ["Insurance", "Other"]);

		let page = |limit, offset| {
			let query = Query {
				limit,
				offset,
				..text("insurance")
			};
			let result = index.search(&query).unwrap();
			assert_eq!(result.total, 2);
			result
				.hits
				.into_iter()
				.map(|hit| hit.title)
				.collect::<Vec<_>>()
		};
		assert_eq!(page(1, 0), ["Insurance"]);
		assert_eq!(page(1, 1), ["Other"]);
		assert!(page(1, 2).is_empty());
	}

	#[test]
	fn listing_is_paged_with_a_default_limit() {
		let (_dir, mut index) = index();
		for n in 0..60 {
			let date = format!("2024-01-{:02}", n % 28 + 1);
			add(
				&mut index,
				&doc(&format!("Doc {n}"), "A", Some(&date), &["x"]),
			);
		}
		let all = index.search(&Query::default()).unwrap();
		assert_eq!((all.total, all.hits.len()), (60, 50));
		let rest = index
			.search(&Query {
				offset: 50,
				..Query::default()
			})
			.unwrap();
		assert_eq!((rest.total, rest.hits.len()), (60, 10));
		let few = index
			.search(&Query {
				limit: 7,
				offset: 3,
				..Query::default()
			})
			.unwrap();
		assert_eq!(few.hits.len(), 7);
		assert_eq!(few.hits[0].id, all.hits[3].id);
	}

	#[test]
	fn filters_narrow_the_results() {
		let (_dir, mut index) = index();
		corpus(&mut index);
		let run = |query: Query| sorted(titles(&index, &query));
		let tags = |tags: &[&str]| tags.iter().map(|tag| tag.to_string()).collect::<Vec<_>>();

		assert_eq!(
			run(Query {
				area: Some("Housing".into()),
				..Query::default()
			}),
			["Electricity Invoice", "Lease Agreement"]
		);
		assert_eq!(
			run(Query {
				doc_type: Some("payslip".into()),
				..Query::default()
			}),
			["Payslip March"]
		);
		assert_eq!(
			run(Query {
				issuer: Some("Tokyo Power".into()),
				..Query::default()
			}),
			["Electricity Invoice"]
		);
		assert_eq!(
			run(Query {
				tags: tags(&["insurance"]),
				..Query::default()
			}),
			["Payslip March", "健康保険料 領収書"]
		);
		assert_eq!(
			run(Query {
				tags: tags(&["insurance", "salary"]),
				..Query::default()
			}),
			["Payslip March"]
		);
		assert_eq!(
			run(Query {
				date_from: Some("2024-01-01".into()),
				..Query::default()
			}),
			["Payslip March", "健康保険料 領収書"]
		);
		assert_eq!(
			run(Query {
				date_from: Some("2023-11-02".into()),
				date_to: Some("2024-03-25".into()),
				..Query::default()
			}),
			["Electricity Invoice", "Payslip March"]
		);
		assert_eq!(
			run(Query {
				date_to: Some("2023-12-31".into()),
				..Query::default()
			}),
			["Electricity Invoice"]
		);
		assert_eq!(
			run(Query {
				area: Some("Housing".into()),
				text: Some("tenant".into()),
				..Query::default()
			}),
			["Lease Agreement"]
		);
		assert!(
			run(Query {
				area: Some("Work".into()),
				text: Some("tenant".into()),
				..Query::default()
			})
			.is_empty()
		);
	}

	#[test]
	fn amount_filters_look_at_every_fact() {
		let (_dir, mut index) = index();
		corpus(&mut index);
		let run = |min, max, currency: Option<&str>| {
			sorted(titles(
				&index,
				&Query {
					amount_min: min,
					amount_max: max,
					currency: currency.map(str::to_string),
					..Query::default()
				},
			))
		};
		assert_eq!(
			run(Some(10_000), None, None),
			["Payslip March", "健康保険料 領収書"]
		);
		assert_eq!(run(Some(10_000), None, Some("JPY")), ["健康保険料 領収書"]);
		assert_eq!(run(Some(7_000), Some(8_000), None), ["Electricity Invoice"]);
		assert_eq!(run(None, Some(0), None), ["健康保険料 領収書"]);
		assert_eq!(run(None, None, Some("usd")), ["Payslip March"]);
		assert_eq!(
			run(Some(12_300), Some(12_300), Some("JPY")),
			["健康保険料 領収書"]
		);
		assert!(run(Some(8_451), Some(12_299), Some("JPY")).is_empty());
		assert!(run(Some(1), None, Some("EUR")).is_empty());
	}

	#[test]
	fn facets_count_values() {
		let (_dir, mut index) = index();
		corpus(&mut index);
		let facets = index.facets(&Viewer::everything()).unwrap();
		let pairs = |facets: &[Facet]| {
			facets
				.iter()
				.map(|facet| (facet.value.clone(), facet.count))
				.collect::<Vec<_>>()
		};
		let owned = |pairs: &[(&str, u64)]| {
			pairs
				.iter()
				.map(|(value, count)| (value.to_string(), *count))
				.collect::<Vec<_>>()
		};
		assert_eq!(
			pairs(&facets.areas),
			owned(&[("Housing", 2), ("Health", 1), ("Work", 1)])
		);
		assert_eq!(
			pairs(&facets.doc_types),
			owned(&[
				("contract", 1),
				("invoice", 1),
				("payslip", 1),
				("receipt", 1)
			])
		);
		assert_eq!(
			pairs(&facets.issuers),
			owned(&[("Acme Inc.", 1), ("Tokyo Power", 1), ("渋谷区役所", 1)])
		);
		assert_eq!(
			pairs(&facets.tags),
			owned(&[
				("insurance", 2),
				("salary", 1),
				("tax-deductible", 1),
				("utilities", 1)
			])
		);
		assert_eq!(pairs(&facets.years), owned(&[("2024", 2), ("2023", 1)]));
		assert_eq!(pairs(&facets.currencies), owned(&[("JPY", 2), ("USD", 1)]));

		assert_eq!(index.areas().unwrap(), ["Housing", "Health", "Work"]);
		assert_eq!(
			index.issuers().unwrap(),
			["Acme Inc.", "Tokyo Power", "渋谷区役所"]
		);
		assert_eq!(
			index.tags().unwrap(),
			["insurance", "salary", "tax-deductible", "utilities"]
		);
		let json = serde_json::to_value(&facets).unwrap();
		assert_eq!(json["areas"][0], json!({"value": "Housing", "count": 2}));
	}

	#[test]
	fn duplicate_tags_count_once() {
		let (_dir, mut index) = index();
		let mut s = doc("Doc", "A", None, &["x"]);
		s.tags = vec!["dup".into(), "dup".into()];
		add(&mut index, &s);
		assert_eq!(
			index.facets(&Viewer::everything()).unwrap().tags[0].count,
			1
		);
		assert_eq!(
			index.search(&Query::default()).unwrap().hits[0].tags,
			["dup", "dup"]
		);
	}

	#[test]
	fn similar_finds_near_duplicates() {
		let (_dir, mut index) = index();
		let [insurance, invoice, ..] = corpus(&mut index);

		let (id, score) = index
			.similar(
				"INVOICE number 4411, for electricity usage in October: amount due b 8,450 b fees",
				0.9,
			)
			.unwrap()
			.unwrap();
		assert_eq!(id, invoice.id);
		assert!((score - 1.0).abs() < f32::EPSILON);

		let (id, score) = index
			.similar(
				"Invoice number 4411 for electricity usage in November. Amount due 9,100",
				0.2,
			)
			.unwrap()
			.unwrap();
		assert_eq!(id, invoice.id);
		assert!(score > 0.2 && score < 0.5, "{score}");
		assert_eq!(
			index
				.similar(
					"Invoice number 4411 for electricity usage in November. Amount due 9,100",
					0.95
				)
				.unwrap(),
			None
		);

		let (id, score) = index
			.similar(
				"令和6年度国民健康保険料の納付を確認しました。合計12,300円 二枚目口座振替のご案内",
				0.8,
			)
			.unwrap()
			.unwrap();
		assert_eq!(id, insurance.id);
		assert!(score > 0.8, "{score}");

		assert_eq!(
			index
				.similar("completely different words appear in this text", 0.1)
				.unwrap(),
			None
		);
		assert_eq!(index.similar("", 0.0).unwrap(), None);
		assert_eq!(index.similar(" ... ", 0.0).unwrap(), None);
	}

	#[test]
	fn similar_handles_very_short_texts() {
		let (_dir, mut index) = index();
		let s = doc("Short", "A", None, &["hello world"]);
		add(&mut index, &s);
		assert_eq!(
			index
				.similar("Hello, World!", 1.0)
				.unwrap()
				.map(|(id, _)| id),
			Some(s.id)
		);
		assert_eq!(index.similar("hello", 0.5).unwrap(), None);
	}

	#[test]
	fn rebuild_replaces_documents_and_keeps_jobs() {
		let dir = tempfile::tempdir().unwrap();
		let library = Library::open(dir.path()).unwrap();
		let mut index = Index::open(&library.index_path()).unwrap();

		let stale = doc("Stale", "Old", None, &["stale text"]);
		add(&mut index, &stale);
		JobQueue::new(index.connection())
			.unwrap()
			.enqueue(JobKind::Ingest, "abc", &json!({"path": "a.pdf"}))
			.unwrap();

		let [insurance, invoice, ..] = corpus(&mut Index::open(&dir.path().join("other")).unwrap());
		for sidecar in [&insurance, &invoice] {
			let path = crate::naming::document_path(sidecar, "pdf");
			library.write_document(&path, b"%PDF", sidecar).unwrap();
		}

		assert_eq!(index.rebuild(&library).unwrap(), 2);
		assert_eq!(index.rebuild(&library).unwrap(), 2);
		assert_eq!(index.get(stale.id).unwrap(), None);
		assert!(titles(&index, &text("stale")).is_empty());
		assert_eq!(index.areas().unwrap(), ["Health", "Housing"]);
		assert_eq!(titles(&index, &text("健康保険")), ["健康保険料 領収書"]);
		let (path, stored) = index.get(invoice.id).unwrap().unwrap();
		assert_eq!(
			path,
			PathBuf::from("Housing/2023/2023-11-02 Tokyo Power Electricity Invoice.pdf")
		);
		assert_eq!(stored, invoice);

		let queue = JobQueue::new(index.connection()).unwrap();
		assert_eq!(queue.claim().unwrap().unwrap().key, "abc");
	}

	#[test]
	fn hits_serialise_for_the_api() {
		let (_dir, mut index) = index();
		corpus(&mut index);
		let result = serde_json::to_value(index.search(&text("tenant")).unwrap()).unwrap();
		assert_eq!(result["total"], 1);
		let hit = &result["hits"][0];
		assert_eq!(hit["title"], "Lease Agreement");
		assert_eq!(hit["path"], "Housing/Undated/Lease Agreement.pdf");
		assert_eq!(hit["snippet"], "The <mark>tenant</mark> agrees to pay rent");
		assert_eq!(hit["issuer"], serde_json::Value::Null);
		assert_eq!(hit["tags"], json!([]));
	}

	struct Household {
		docs: Vec<Sidecar>,
	}

	impl Household {
		fn title(&self, id: Uuid) -> &str {
			&self.docs.iter().find(|doc| doc.id == id).unwrap().title
		}
	}

	#[allow(clippy::too_many_arguments)]
	fn household_doc(
		title: &str,
		issuer: Option<&str>,
		doc_type: &str,
		date: Option<&str>,
		summary: Option<&str>,
		tags: &[&str],
		pages: &[&str],
	) -> Sidecar {
		let mut s = doc(title, "Home", date, pages);
		s.issuer = issuer.map(str::to_string);
		s.doc_type = doc_type.into();
		s.summary = summary.map(str::to_string);
		s.tags = tags.iter().map(|tag| tag.to_string()).collect();
		s
	}

	/// The cases of the search audit on the live library, French household papers.
	fn household(index: &mut Index) -> Household {
		let filler = "Les présentes conditions s'appliquent au contrat souscrit. ".repeat(30);
		let docs = vec![
			household_doc(
				"Avis d'impôt sur le revenu 2024",
				Some("Direction générale des Finances publiques"),
				"tax_notice",
				Some("2024-08-01"),
				Some("Income tax notice for 2023 income"),
				&["impôts"],
				&["Vos impôts sur les revenus de 2023. Montant de l'impôt 1 234 €"],
			),
			household_doc(
				"Guide fiscal",
				None,
				"leaflet",
				None,
				None,
				&[],
				&["Comment déclarer vos impôts en ligne. Identité du déclarant"],
			),
			household_doc(
				"Contrat d'électricité",
				Some("Lampiris"),
				"contract",
				Some("2022-02-01"),
				Some("Electricity supply contract"),
				&["électricité"],
				&["Fourniture d'électricité et de gaz. Prix de l'électricité au kWh."],
			),
			household_doc(
				"Conditions Générales",
				Some("TOTAL SPRING"),
				"terms",
				Some("2021-05-01"),
				None,
				&[],
				&[&format!("ELECTRICITE {filler}")],
			),
			household_doc(
				"Votre devis auto",
				Some("Direct Assurance"),
				"quote",
				Some("2023-03-10"),
				Some("Car insurance quote"),
				&["car", "insurance"],
				&["Devis assurance auto pour votre véhicule. Prime annuelle 420 €"],
			),
			household_doc(
				"Relevé Carrefour Banque",
				Some("Carrefour Banque"),
				"statement",
				Some("2024-01-31"),
				Some("Credit card statement with payment insurance"),
				&["insurance", "bank"],
				&["CARREFOUR BANQUE carte PASS. Assurance moyens de paiement. carnet de chèques"],
			),
			household_doc(
				"Conditions générales habitation",
				Some("Matmut"),
				"terms",
				Some("2020-01-01"),
				Some("Home insurance terms"),
				&["insurance"],
				&[&format!(
					"{filler} The policy does not cover your car. {filler}"
				)],
			),
			household_doc(
				"Relevé de compte",
				Some("SGCB"),
				"statement",
				Some("2015-06-30"),
				Some("Bank statement for June 2015"),
				&["bank"],
				&["SGCB relevé de compte n° 123 solde créditeur"],
			),
			household_doc(
				"Calendrier de travail",
				Some("Ponant"),
				"calendar",
				Some("2016-01-04"),
				None,
				&[],
				&["Réunion SGCB lundi. Réunion SGCB mardi. Participants SGCB"],
			),
			household_doc(
				"Échéancier EDF",
				Some("EDF"),
				"schedule",
				Some("2017-01-15"),
				None,
				&[],
				&["EDF échéancier électricité 2017"],
			),
			household_doc(
				"O nool 1 8-ooz",
				Some("KDDI"),
				"contract",
				Some("2024-05-01"),
				None,
				&[],
				&["EdffiHA w+hwlnffi6r 44edf 0x3fedf"],
			),
			household_doc(
				"Bulletin de paie",
				Some("SGCB"),
				"payslip",
				None,
				Some("Payslip for March 2014"),
				&["salary"],
				&["Bulletin de paie mars 2014 salaire net"],
			),
		];
		for sidecar in &docs {
			add(index, sidecar);
		}
		Household { docs }
	}

	fn ranked(index: &Index, household: &Household, query: &str) -> Vec<String> {
		index
			.search(&text(query))
			.unwrap()
			.hits
			.iter()
			.map(|hit| household.title(hit.id).to_string())
			.collect()
	}

	#[test]
	fn accents_and_case_do_not_matter() {
		let (_dir, mut index) = index();
		let household = household(&mut index);
		let expected = ranked(&index, &household, "électricité");
		assert_eq!(
			sorted(expected.clone()),
			[
				"Conditions Générales",
				"Contrat d'électricité",
				"Échéancier EDF"
			]
		);
		assert_eq!(expected[0], "Contrat d'électricité");
		for query in ["electricite", "Electricité", "électricite", "ELECTRICITE"] {
			assert_eq!(ranked(&index, &household, query), expected, "{query}");
		}
		for query in ["impots", "impôts", "IMPOTS", "Impôt"] {
			assert_eq!(
				ranked(&index, &household, query),
				["Avis d'impôt sur le revenu 2024", "Guide fiscal"],
				"{query}"
			);
		}
		assert_eq!(
			ranked(&index, &household, "impots 2024"),
			["Avis d'impôt sur le revenu 2024"]
		);
		let hit = &index.search(&text("electricite")).unwrap().hits[0];
		assert_eq!(
			hit.snippet.as_deref(),
			Some(
				"Fourniture d&#39;<mark>électricité</mark> et de gaz. Prix de l&#39;<mark>électricité</mark> au kWh."
			)
		);
	}

	#[test]
	fn short_latin_words_match_whole_words_only() {
		let (_dir, mut index) = index();
		let household = household(&mut index);
		let results = ranked(&index, &household, "car insurance");
		assert_eq!(
			results,
			["Votre devis auto", "Conditions générales habitation"]
		);
		assert_eq!(ranked(&index, &household, "EDF"), ["Échéancier EDF"]);
		assert!(ranked(&index, &household, "ban").is_empty());
		assert_eq!(
			ranked(&index, &household, "carre"),
			["Relevé Carrefour Banque"]
		);
		assert_eq!(
			ranked(&index, &household, "carrefour"),
			["Relevé Carrefour Banque"]
		);
		let prefixed = ranked(&index, &household, "car*");
		assert!(
			prefixed.contains(&"Relevé Carrefour Banque".to_string()),
			"{prefixed:?}"
		);
	}

	#[test]
	fn stop_words_are_ignored_next_to_other_words() {
		let (_dir, mut index) = index();
		let household = household(&mut index);
		assert_eq!(
			ranked(&index, &household, "bulletin de paie"),
			["Bulletin de paie"]
		);
		assert_eq!(ranked(&index, &household, "identite de"), ["Guide fiscal"]);
		let hit = &index.search(&text("identite de")).unwrap().hits[0];
		assert_eq!(
			hit.snippet.as_deref(),
			Some("Comment déclarer vos impôts en ligne. <mark>Identité</mark> du déclarant")
		);
		let alone = ranked(&index, &household, "de");
		assert!(alone.contains(&"Relevé de compte".to_string()));
		assert!(!alone.contains(&"Guide fiscal".to_string()));
	}

	#[test]
	fn the_issuer_outranks_a_mention() {
		let (_dir, mut index) = index();
		let household = household(&mut index);
		assert_eq!(
			ranked(&index, &household, "SGCB"),
			[
				"Relevé de compte",
				"Bulletin de paie",
				"Calendrier de travail"
			]
		);
		assert_eq!(
			ranked(&index, &household, "statement")[..2],
			["Relevé de compte", "Relevé Carrefour Banque"].map(String::from)
		);
	}

	#[test]
	fn synonyms_find_the_words_documents_use() {
		let (_dir, mut index) = index();
		let household = household(&mut index);
		let bill = household_doc(
			"Facture d'électricité",
			Some("EDF"),
			"invoice",
			Some("2023-11-30"),
			None,
			&[],
			&["Votre facture d'électricité de novembre. Total 133,90 €"],
		);
		add(&mut index, &bill);
		for query in [
			"fiche de paie",
			"Fiches de paie",
			"給与明細",
			"bulletin de salaire",
		] {
			assert_eq!(
				index
					.search(&text(query))
					.unwrap()
					.hits
					.iter()
					.map(|hit| hit.title.as_str())
					.collect::<Vec<_>>(),
				["Bulletin de paie"],
				"{query}"
			);
		}
		let car = ranked(&index, &household, "assurance voiture");
		assert_eq!(car[0], "Votre devis auto");
		assert!(!car.contains(&"Relevé Carrefour Banque".to_string()));
		let bills = index.search(&text("電気 請求書")).unwrap();
		assert!(!bills.partial);
		assert_eq!(bills.hits[0].title, "Facture d'électricité");
		assert_eq!(
			sorted(ranked(&index, &household, "statement")),
			["Relevé Carrefour Banque", "Relevé de compte"]
		);
		let quoted = index.search(&text("\"fiche de paie\"")).unwrap();
		assert_eq!((quoted.total, quoted.partial), (0, false));
	}

	#[test]
	fn without_a_document_having_every_word_any_word_is_a_partial_match() {
		let (_dir, mut index) = index();
		let household = household(&mut index);
		let strict = index.search(&text("échéancier edf")).unwrap();
		assert_eq!((strict.total, strict.partial), (1, false));

		let partial = index.search(&text("échéancier zzznothing")).unwrap();
		assert!(partial.partial);
		assert_eq!(partial.hits[0].title, "Échéancier EDF");
		assert_eq!(
			partial.hits[0].snippet.as_deref(),
			Some("EDF <mark>échéancier</mark> électricité 2017")
		);
		assert_eq!(
			ranked(&index, &household, "dernier bulletin de paie"),
			["Bulletin de paie"]
		);

		let alone = index.search(&text("zzznothing")).unwrap();
		assert_eq!((alone.total, alone.partial), (0, false));
		let filtered = index
			.search(&Query {
				area: Some("Nowhere".into()),
				..text("échéancier zzznothing")
			})
			.unwrap();
		assert_eq!((filtered.total, filtered.partial), (0, false));
	}

	#[test]
	fn month_filter_and_newest_first_with_text() {
		let (_dir, mut index) = index();
		let household = household(&mut index);
		let march = Query {
			month: Some(3),
			..Query::default()
		};
		assert_eq!(titles(&index, &march), ["Votre devis auto"]);
		let june = Query {
			month: Some(6),
			..text("relevé")
		};
		assert_eq!(titles(&index, &june), ["Relevé de compte"]);

		assert_eq!(ranked(&index, &household, "sgcb")[0], "Relevé de compte");
		let newest = Query {
			newest: true,
			..text("sgcb")
		};
		assert_eq!(
			titles(&index, &newest),
			[
				"Calendrier de travail",
				"Relevé de compte",
				"Bulletin de paie"
			]
		);
	}

	#[test]
	fn equal_scores_list_the_newest_first_and_undated_last() {
		let (_dir, mut index) = index();
		let mut older = doc("Facture", "A", Some("2021-01-01"), &["Facture eau"]);
		older.created_at = OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap();
		let mut undated = doc("Facture", "A", None, &["Facture eau"]);
		undated.created_at = OffsetDateTime::from_unix_timestamp(1_800_000_000).unwrap();
		let mut newer = doc("Facture", "A", Some("2023-01-01"), &["Facture eau"]);
		newer.created_at = OffsetDateTime::from_unix_timestamp(1_600_000_000).unwrap();
		for sidecar in [&older, &undated, &newer] {
			add(&mut index, sidecar);
		}
		let ids = |query: &Query| -> Vec<Uuid> {
			index
				.search(query)
				.unwrap()
				.hits
				.iter()
				.map(|hit| hit.id)
				.collect()
		};
		let expected = [newer.id, older.id, undated.id];
		assert_eq!(ids(&text("facture")), expected);
		assert_eq!(ids(&Query::default()), expected);
	}

	#[test]
	fn snippets_come_from_the_best_page() {
		let (_dir, mut index) = index();
		let s = doc(
			"Contrat",
			"A",
			None,
			&[
				"Assurance: voir les conditions.",
				"Le devis assurance auto Carrefour car est joint.",
			],
		);
		add(&mut index, &s);
		let hit = &index.search(&text("assurance devis")).unwrap().hits[0];
		assert_eq!(
			hit.snippet.as_deref(),
			Some("Le <mark>devis</mark> <mark>assurance</mark> auto Carrefour car est joint.")
		);
		let hit = &index.search(&text("car")).unwrap().hits[0];
		assert_eq!(
			hit.snippet.as_deref(),
			Some("Le devis assurance <mark>auto</mark> Carrefour <mark>car</mark> est joint.")
		);
		let hit = &index.search(&text("\"assurance auto\"")).unwrap().hits[0];
		assert_eq!(
			hit.snippet.as_deref(),
			Some("Le devis <mark>assurance auto</mark> Carrefour car est joint.")
		);
	}

	#[test]
	fn upgrading_the_schema_refills_the_text_from_the_sidecars() {
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join(".index.sqlite");
		let mut s = doc("Avis d'impôt", "Tax", None, &["Vos impôts 2024"]);
		s.issuer = Some("DGFiP".into());
		{
			let conn = Connection::open(&path).unwrap();
			for sql in &MIGRATIONS[..TEXT_SCHEMA - 1] {
				conn.execute_batch(sql).unwrap();
			}
			conn.pragma_update(None, "user_version", TEXT_SCHEMA as i64 - 1)
				.unwrap();
			conn.execute(
				"INSERT INTO documents (id, path, title, area, doc_type, sha256, pages,
					created_at, body, sidecar)
				 VALUES (?1, 'Tax/a.pdf', ?2, 'Tax', 'letter', 'x', 1, 0, '', ?3)",
				params![
					s.id.to_string(),
					s.title,
					serde_json::to_string(&s).unwrap()
				],
			)
			.unwrap();
			conn.execute(
				"INSERT INTO pages (doc_id, page, text) VALUES (?1, 1, 'Vos impôts 2024')",
				params![s.id.to_string()],
			)
			.unwrap();
		}
		let index = Index::open(&path).unwrap();
		assert_eq!(titles(&index, &text("impots")), ["Avis d'impôt"]);
		assert_eq!(titles(&index, &text("dgfip")), ["Avis d'impôt"]);
	}

	fn mail(uid: u32, message_id: &str, status: MailStatus) -> MailRecord {
		MailRecord {
			account: "home".into(),
			folder: "INBOX".into(),
			uidvalidity: 7,
			uid,
			message_id: Some(message_id.into()),
			sender: Some("billing@example.com".into()),
			subject: Some("Invoice".into()),
			received: None,
			status,
			files: 1,
			detail: None,
			error: None,
		}
	}

	#[test]
	fn mail_tables_dedupe_and_survive_a_rebuild() {
		let dir = tempfile::tempdir().unwrap();
		let library = Library::open(dir.path()).unwrap();
		let mut index = Index::open(&library.index_path()).unwrap();
		let hint = MailHint {
			mail: crate::MailSource {
				account: "home".into(),
				folder: "INBOX".into(),
				uidvalidity: 7,
				uid: 3,
				message_id: Some("a@example.com".into()),
				from: None,
				subject: None,
				date: None,
			},
			area: Some("Tax".into()),
			tags: vec!["mail".into()],
		};
		let first = index
			.mail_record(
				&mail(3, "a@example.com", MailStatus::Ingested),
				&[("abc".into(), hint.clone())],
			)
			.unwrap();
		index
			.mail_record(&mail(4, "b@example.com", MailStatus::Skipped), &[])
			.unwrap();
		index.mail_set_error(first, "store refused").unwrap();
		index.rebuild(&library).unwrap();

		let handled = index.mail_handled("home", "INBOX", 7, &[2, 3, 4]).unwrap();
		assert_eq!(handled, HashSet::from([3, 4]));
		assert!(
			index
				.mail_handled("home", "INBOX", 8, &[3])
				.unwrap()
				.is_empty()
		);
		assert!(
			index
				.mail_handled("home", "Archive", 7, &[3])
				.unwrap()
				.is_empty()
		);
		assert!(index.mail_message_handled("a@example.com").unwrap());
		assert!(!index.mail_message_handled("c@example.com").unwrap());
		assert_eq!(index.mail_hint("abc").unwrap(), Some(hint));
		assert_eq!(index.mail_hint("def").unwrap(), None);

		let rows = index.mail_processed(Some("home"), 10).unwrap();
		assert_eq!(rows.len(), 2);
		let row = rows.iter().find(|row| row.id == first).unwrap();
		assert_eq!(row.error.as_deref(), Some("store refused"));
		assert_eq!(row.status, MailStatus::Ingested);
		assert!(index.mail_processed(Some("work"), 10).unwrap().is_empty());
		let mut counts = index.mail_counts("home").unwrap();
		counts.sort_by_key(|(status, _)| status.as_str());
		assert_eq!(
			counts,
			[(MailStatus::Ingested, 1), (MailStatus::Skipped, 1)]
		);

		assert_eq!(index.mail_forget(first).unwrap().unwrap().uid, 3);
		assert!(index.mail_forget(first).unwrap().is_none());
		assert!(!index.mail_message_handled("a@example.com").unwrap());
		assert_eq!(
			index.mail_handled("home", "INBOX", 7, &[3, 4]).unwrap(),
			HashSet::from([4])
		);
	}

	#[test]
	fn a_viewer_sees_nothing_of_a_hidden_area() {
		let (_dir, mut index) = index();
		let mut private = doc("Blood test", "Health", Some("2024-03-01"), &["cholesterol"]);
		private.tags = vec!["lab".into()];
		private.facts = vec![amount("total", 4200, "EUR")];
		let public = doc(
			"Lease",
			"Housing",
			Some("2023-01-01"),
			&["cholesterol rent"],
		);
		let private_path = add(&mut index, &private);
		add(&mut index, &public);

		let outsider = Viewer::hiding(["health ".to_string()]);
		assert!(!outsider.sees("Health"));
		assert!(outsider.sees("Housing"));
		assert!(!outsider.sees_path(&private_path));
		assert!(outsider.sees_path(Path::new("Housing/2023/x.pdf")));

		let search = |viewer: &Viewer, text: Option<&str>| {
			let query = Query {
				text: text.map(str::to_string),
				viewer: viewer.clone(),
				..Query::default()
			};
			titles(&index, &query)
		};
		for text in [None, Some("cholesterol")] {
			assert_eq!(search(&outsider, text), ["Lease"]);
			assert_eq!(search(&Viewer::everything(), text).len(), 2);
		}

		let facets = index.facets(&outsider).unwrap();
		let values = |facets: &[Facet]| facets.iter().map(|f| f.value.clone()).collect::<Vec<_>>();
		assert_eq!(values(&facets.areas), ["Housing"]);
		assert!(facets.tags.is_empty());
		assert!(facets.currencies.is_empty());
		assert_eq!(values(&facets.years), ["2023"]);
		let all = index.facets(&Viewer::everything()).unwrap();
		assert_eq!(values(&all.tags), ["lab"]);

		assert!(index.get_visible(private.id, &outsider).unwrap().is_none());
		assert!(
			index
				.get_visible(private.id, &Viewer::everything())
				.unwrap()
				.is_some()
		);
		assert!(index.get_visible(public.id, &outsider).unwrap().is_some());
	}

	#[test]
	fn the_uploader_sits_next_to_the_mail_provenance() {
		let (_dir, index) = index();
		assert_eq!(index.uploader("aa").unwrap(), None);
		index.set_uploader("aa", "sam").unwrap();
		assert_eq!(index.uploader("aa").unwrap().as_deref(), Some("sam"));
		assert_eq!(index.mail_hint("aa").unwrap(), None);
		index.set_uploader("aa", "alex").unwrap();
		assert_eq!(index.uploader("aa").unwrap().as_deref(), Some("alex"));
	}

	fn copy(title: &str, area: &str, text: &str) -> Sidecar {
		let mut s = doc(title, area, Some("2026-07-01"), &[text]);
		s.issuer = Some("SCI Malical".into());
		s.doc_type = "invoice".into();
		s
	}

	const RENT: &str = "Facture N° 07 2026 loyer de juillet 2026, total 1.400,00 EUR à régler \
		avant le 05/07/2026 par virement sur le compte habituel de la SCI.";

	fn pair_ids(index: &Index, viewer: &Viewer) -> Vec<[Uuid; 2]> {
		index
			.duplicates(viewer)
			.unwrap()
			.into_iter()
			.map(|[a, b]| [a.hit.id, b.hit.id])
			.collect()
	}

	fn ordered(a: &Sidecar, b: &Sidecar) -> [Uuid; 2] {
		if a.id.to_string() < b.id.to_string() {
			[a.id, b.id]
		} else {
			[b.id, a.id]
		}
	}

	#[test]
	fn near_duplicates_follow_every_upsert_and_removal() {
		let (_dir, mut index) = index();
		let a = copy("Facture", "Housing", RENT);
		let b = copy("Facture (2)", "Housing", &format!("{RENT}  "));
		let august = copy(
			"Facture août",
			"Housing",
			&RENT.replace("07 2026", "08 2026").replace("05/07", "05/08"),
		);
		for sidecar in [&a, &b, &august] {
			add(&mut index, sidecar);
		}
		let everyone = Viewer::everything();
		assert_eq!(pair_ids(&index, &everyone), [ordered(&a, &b)]);
		let [first, second] = &index.duplicates(&everyone).unwrap()[0];
		assert_eq!(first.hit.pages, 1);
		assert!([&a.title, &b.title].contains(&&second.hit.title));
		assert_eq!(index.duplicates_of(a.id, &everyone).unwrap(), [b.id]);
		assert_eq!(index.duplicates_of(b.id, &everyone).unwrap(), [a.id]);
		assert!(
			index
				.duplicates_of(august.id, &everyone)
				.unwrap()
				.is_empty()
		);

		let mut dismissed = a.clone();
		dismissed.not_duplicates = vec![b.id];
		add(&mut index, &dismissed);
		assert!(pair_ids(&index, &everyone).is_empty());
		add(&mut index, &a);
		assert_eq!(pair_ids(&index, &everyone), [ordered(&a, &b)]);

		let mut moved = b.clone();
		moved.issuer = Some("Someone else".into());
		add(&mut index, &moved);
		assert!(pair_ids(&index, &everyone).is_empty());
		add(&mut index, &b);
		index.remove(a.id).unwrap();
		assert!(pair_ids(&index, &everyone).is_empty());
		assert!(index.duplicates_of(b.id, &everyone).unwrap().is_empty());
	}

	#[test]
	fn a_pair_is_listed_only_to_who_sees_both() {
		let (_dir, mut index) = index();
		let public = copy("Facture", "Housing", RENT);
		let private = copy("Facture (2)", "Health", RENT);
		add(&mut index, &public);
		add(&mut index, &private);
		assert_eq!(pair_ids(&index, &Viewer::everything()).len(), 1);
		let outsider = Viewer::hiding(["Health".to_string()]);
		assert!(pair_ids(&index, &outsider).is_empty());
		assert!(
			index
				.duplicates_of(public.id, &outsider)
				.unwrap()
				.is_empty()
		);
		assert_eq!(
			index
				.duplicates_of(public.id, &Viewer::everything())
				.unwrap(),
			[private.id]
		);
	}

	#[test]
	fn rebuild_recomputes_near_duplicates_and_keeps_dismissals() {
		let dir = tempfile::tempdir().unwrap();
		let library = Library::open(dir.path()).unwrap();
		let mut index = Index::open(&library.index_path()).unwrap();
		let a = copy("Facture", "Housing", RENT);
		let mut b = copy("Facture (2)", "Housing", RENT);
		let c = copy("Facture (3)", "Housing", RENT);
		b.not_duplicates = vec![c.id];
		for sidecar in [&a, &b, &c] {
			let path = crate::naming::document_path(sidecar, "pdf");
			let path = crate::naming::deconflict(&path, |p| library.library_dir().join(p).exists());
			library.write_document(&path, b"%PDF", sidecar).unwrap();
		}
		index
			.connection()
			.execute(
				"INSERT INTO near_duplicates (a, b) VALUES ('stale', 'pair')",
				[],
			)
			.unwrap();
		assert_eq!(index.rebuild(&library).unwrap(), 3);
		let mut pairs = pair_ids(&index, &Viewer::everything());
		pairs.sort();
		let mut wanted = vec![ordered(&a, &b), ordered(&a, &c)];
		wanted.sort();
		assert_eq!(pairs, wanted);
		let stale: i64 = index
			.connection()
			.query_row("SELECT COUNT(*) FROM near_duplicates", [], |row| row.get(0))
			.unwrap();
		assert_eq!(stale, 2);
	}

	fn identical(title: &str, issuer: &str, date: Option<&str>, of: &Sidecar) -> Sidecar {
		let mut s = doc(title, "Bank", date, &["Relevé"]);
		s.issuer = Some(issuer.into());
		s.doc_type = "statement".into();
		s.sha256.clone_from(&of.sha256);
		s
	}

	#[test]
	fn the_same_file_is_paired_whatever_its_metadata_and_tells_its_copies_apart() {
		let (_dir, mut index) = index();
		let mut a = doc("Relevé", "Bank", Some("2026-03-31"), &["Relevé"]);
		a.issuer = Some("Société Générale Calédonienne de Banque".into());
		a.created_at = time::macros::datetime!(2026-04-02 09:00 UTC);
		a.sources = vec![crate::Source {
			sha256: "aa".into(),
			original_name: "releve.pdf".into(),
			pages: Vec::new(),
			mail: Some(crate::MailSource {
				account: "home".into(),
				folder: "INBOX".into(),
				uidvalidity: 1,
				uid: 7,
				message_id: Some("<m@x>".into()),
				from: Some("bank@x".into()),
				subject: Some("Votre relevé".into()),
				date: Some("2026-04-01T08:00:00Z".into()),
			}),
		}];
		let b = identical("Relevé (2)", "SOCIETE GENERALE", None, &a);
		let other = doc("Autre", "Bank", None, &["Relevé"]);
		for sidecar in [&a, &b, &other] {
			add(&mut index, sidecar);
		}
		let everyone = Viewer::everything();
		assert_eq!(pair_ids(&index, &everyone), [ordered(&a, &b)]);
		let pair = &index.duplicates(&everyone).unwrap()[0];
		let first = pair.iter().find(|copy| copy.hit.id == a.id).unwrap();
		assert_eq!(first.created_at, a.created_at);
		assert_eq!(first.sha256, a.sha256);
		assert_eq!(
			first.sources,
			[crate::SourceSummary {
				original_name: "releve.pdf".into(),
				mail: Some(crate::MailSummary {
					account: "home".into(),
					subject: Some("Votre relevé".into()),
					date: Some("2026-04-01T08:00:00Z".into()),
				}),
			}]
		);
		let json = serde_json::to_value(first).unwrap();
		assert_eq!(json["title"], "Relevé");
		assert_eq!(json["created_at"], "2026-04-02T09:00:00Z");
		assert!(json["sources"][0]["mail"].get("uid").is_none());

		let mut dismissed = b.clone();
		dismissed.not_duplicates = vec![a.id];
		add(&mut index, &dismissed);
		assert!(pair_ids(&index, &everyone).is_empty());
	}

	#[test]
	fn upgrading_the_schema_finds_the_near_duplicates() {
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join(".index.sqlite");
		let a = copy("Facture", "Housing", RENT);
		let b = copy("Facture (2)", "Housing", RENT);
		{
			let mut index = Index::open(&path).unwrap();
			add(&mut index, &a);
			add(&mut index, &b);
			let conn = index.connection();
			conn.execute_batch("DROP TABLE near_duplicates").unwrap();
			conn.pragma_update(None, "user_version", DUPLICATES_SCHEMAS[0] as i64 - 1)
				.unwrap();
		}
		let index = Index::open(&path).unwrap();
		assert_eq!(pair_ids(&index, &Viewer::everything()), [ordered(&a, &b)]);
	}

	#[test]
	fn upgrading_the_schema_pairs_the_identical_files() {
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join(".index.sqlite");
		let a = identical(
			"Relevé",
			"SGCB",
			Some("2026-03-31"),
			&doc("x", "Bank", None, &[]),
		);
		let b = identical("Relevé (2)", "SOCIETE GENERALE", None, &a);
		let mut c = identical("Relevé (3)", "SGCB", None, &a);
		c.not_duplicates = vec![a.id];
		{
			let mut index = Index::open(&path).unwrap();
			for sidecar in [&a, &b, &c] {
				add(&mut index, sidecar);
			}
			let conn = index.connection();
			conn.execute_batch("DELETE FROM near_duplicates").unwrap();
			conn.pragma_update(None, "user_version", DUPLICATES_SCHEMAS[1] as i64 - 1)
				.unwrap();
		}
		let index = Index::open(&path).unwrap();
		let mut pairs = pair_ids(&index, &Viewer::everything());
		pairs.sort();
		let mut wanted = vec![ordered(&a, &b), ordered(&b, &c)];
		wanted.sort();
		assert_eq!(pairs, wanted);
	}
}
