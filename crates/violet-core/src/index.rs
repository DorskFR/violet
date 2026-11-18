use std::collections::{HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};

use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, params, params_from_iter};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

/// The SQLite index at `.index.sqlite`: documents, tags, facts and an FTS5 table over page
/// text, title, issuer, tags and summary. WAL mode. `rebuild` recreates all of it except the
/// jobs and the mail tables (which mails were fetched, and the provenance of files waiting in
/// the inbox); deleting the file loses those.
pub struct Index {
	conn: rusqlite::Connection,
}

#[derive(Debug, Clone, Default)]
pub struct Query {
	/// FTS5 text; plain words are matched as prefixes, quotes kept as phrases. Every word must
	/// appear somewhere in the document, in any case and anywhere in a word, so Japanese works.
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
	/// 0 means 50.
	pub limit: u32,
	pub offset: u32,
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

#[derive(Debug, Clone, Serialize)]
pub struct SearchResult {
	pub total: u64,
	pub hits: Vec<Hit>,
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
];

const HIT_COLUMNS: &str = "d.id, d.path, d.title, d.area, d.doc_type, d.issuer, d.date, \
	d.amount_minor, d.currency, d.pages";
const NEWEST_FIRST: &str = "COALESCE(d.date, date(d.created_at, 'unixepoch')) DESC, \
	d.created_at DESC, d.id";

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
			 DELETE FROM pages;",
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
		let terms = parse_terms(query.text.as_deref().unwrap_or_default());
		let (filter, values) = filter_sql(query, &terms);
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
			let ranks = self.ranks(&terms)?;
			let rank = |hit: &Hit| ranks.get(&hit.id).copied().unwrap_or(f64::INFINITY);
			matching.sort_by(|a, b| rank(a).total_cmp(&rank(b)));
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
				hit.snippet = self.snippet(hit.id, &terms)?;
			}
		}
		Ok(SearchResult { total, hits })
	}
	pub fn facets(&self) -> crate::Result<Facets> {
		Ok(Facets {
			areas: self
				.facet("SELECT area, COUNT(*) FROM documents GROUP BY 1 ORDER BY 2 DESC, 1")?,
			doc_types: self
				.facet("SELECT doc_type, COUNT(*) FROM documents GROUP BY 1 ORDER BY 2 DESC, 1")?,
			issuers: self.facet(
				"SELECT issuer, COUNT(*) FROM documents WHERE issuer IS NOT NULL AND issuer <> ''
				 GROUP BY 1 ORDER BY 2 DESC, 1",
			)?,
			tags: self.facet(
				"SELECT tag, COUNT(DISTINCT doc_id) FROM tags GROUP BY 1 ORDER BY 2 DESC, 1",
			)?,
			years: self.facet(
				"SELECT substr(date, 1, 4), COUNT(*) FROM documents WHERE date IS NOT NULL
				 GROUP BY 1 ORDER BY 1 DESC",
			)?,
			currencies: self.facet(
				"SELECT currency, COUNT(DISTINCT doc_id) FROM facts
				 WHERE currency IS NOT NULL AND currency <> ''
				 GROUP BY 1 ORDER BY 2 DESC, 1",
			)?,
		})
	}
	/// Known values, most used first, to keep the analysis consistent with the existing library.
	pub fn areas(&self) -> crate::Result<Vec<String>> {
		Ok(self
			.facets()?
			.areas
			.into_iter()
			.map(|facet| facet.value)
			.collect())
	}
	pub fn issuers(&self) -> crate::Result<Vec<String>> {
		Ok(self
			.facets()?
			.issuers
			.into_iter()
			.map(|facet| facet.value)
			.collect())
	}
	pub fn tags(&self) -> crate::Result<Vec<String>> {
		Ok(self
			.facets()?
			.tags
			.into_iter()
			.map(|facet| facet.value)
			.collect())
	}
	/// Shared connection for the job queue, which lives in the same file.
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

	fn ranks(&self, terms: &[String]) -> crate::Result<HashMap<Uuid, f64>> {
		let phrases: Vec<String> = terms
			.iter()
			.filter(|term| uses_fts(term))
			.map(|term| fts_phrase(term))
			.collect();
		let mut ranks = HashMap::new();
		if phrases.is_empty() {
			return Ok(ranks);
		}
		let mut statement = self
			.conn
			.prepare("SELECT doc_id, rank FROM pages WHERE pages MATCH ?1")?;
		let mut rows = statement.query(params![phrases.join(" OR ")])?;
		while let Some(row) = rows.next()? {
			let id = parse_id(&row.get::<_, String>(0)?)?;
			let rank: f64 = row.get(1)?;
			ranks
				.entry(id)
				.and_modify(|best: &mut f64| *best = best.min(rank))
				.or_insert(rank);
		}
		Ok(ranks)
	}

	fn snippet(&self, id: Uuid, terms: &[String]) -> crate::Result<Option<String>> {
		let texts = self.strings(
			"SELECT text FROM pages WHERE doc_id = ?1 ORDER BY page = 0, page",
			params![id.to_string()],
		)?;
		let needles: Vec<Vec<char>> = terms.iter().map(|term| fold(term)).collect();
		let mut best: Option<(usize, &str)> = None;
		for text in &texts {
			let folded = fold(text);
			let found = needles
				.iter()
				.filter(|needle| find(&folded, needle, 0).is_some())
				.count();
			if found > best.map_or(0, |(count, _)| count) {
				best = Some((found, text));
			}
		}
		Ok(best.map(|(_, text)| mark(text, &needles)))
	}

	fn facet(&self, sql: &str) -> crate::Result<Vec<Facet>> {
		let mut statement = self.conn.prepare(sql)?;
		let facets = statement
			.query_map([], |row| {
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
				"SELECT hint FROM mail_files WHERE sha256 = ?1",
				[sha256],
				|row| row.get(0),
			)
			.optional()?;
		Ok(hint.map(|hint| serde_json::from_str(&hint)).transpose()?)
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
		tx.pragma_update(None, "user_version", done as i64 + 1)?;
		tx.commit()?;
	}
	Ok(())
}

fn delete(conn: &Connection, id: Uuid) -> crate::Result<()> {
	let id = id.to_string();
	for table in ["tags", "facts", "sources", "pages"] {
		conn.execute(
			&format!("DELETE FROM {table} WHERE doc_id = ?1"),
			params![id],
		)?;
	}
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

	let mut page_insert =
		conn.prepare_cached("INSERT INTO pages (doc_id, page, text) VALUES (?1, ?2, ?3)")?;
	let mut metadata = vec![sidecar.title.as_str()];
	metadata.extend(sidecar.issuer.as_deref());
	let tags = sidecar.tags.join(", ");
	metadata.push(&tags);
	metadata.extend(sidecar.summary.as_deref());
	let metadata: Vec<String> = metadata
		.into_iter()
		.map(normalize)
		.filter(|part| !part.is_empty())
		.collect();
	page_insert.execute(params![id, 0, metadata.join(" | ")])?;
	for page in &sidecar.pages {
		let text = normalize(&page.text);
		if !text.is_empty() {
			page_insert.execute(params![id, page.number.max(1), text])?;
		}
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

fn filter_sql(query: &Query, terms: &[String]) -> (String, Vec<Value>) {
	let mut clauses = vec!["1".to_string()];
	let mut values: Vec<Value> = Vec::new();
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
	for term in terms {
		let condition = if uses_fts(term) {
			format!("pages MATCH {}", bind(fts_phrase(term).into()))
		} else {
			format!("text LIKE {} ESCAPE '\\'", bind(like_pattern(term).into()))
		};
		clauses.push(format!(
			"d.id IN (SELECT doc_id FROM pages WHERE {condition})"
		));
	}
	(clauses.join(" AND "), values)
}

/// Words and quoted phrases of a search box, whitespace collapsed, duplicates dropped.
fn parse_terms(text: &str) -> Vec<String> {
	let mut terms: Vec<String> = Vec::new();
	let mut push = |term: &str| {
		let term = normalize(term);
		if !term.is_empty() && !terms.iter().any(|known| fold(known) == fold(&term)) {
			terms.push(term);
		}
	};
	for (index, part) in text.split('"').enumerate() {
		if index % 2 == 1 {
			push(part);
		} else {
			for word in part.split_whitespace() {
				push(word.trim_end_matches('*'));
			}
		}
	}
	terms
}

fn uses_fts(term: &str) -> bool {
	term.chars().count() >= FTS_MIN_CHARS
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

fn fold(text: &str) -> Vec<char> {
	text.chars().map(fold_char).collect()
}

fn fold_char(c: char) -> char {
	c.to_lowercase().next().unwrap_or(c)
}

fn find(haystack: &[char], needle: &[char], from: usize) -> Option<usize> {
	if needle.is_empty() || haystack.len() < needle.len() {
		return None;
	}
	(from..=haystack.len() - needle.len()).find(|&at| haystack[at..at + needle.len()] == *needle)
}

/// A window of `text` around the first match, HTML-escaped, with every match in `<mark>`.
fn mark(text: &str, needles: &[Vec<char>]) -> String {
	let chars: Vec<char> = text.chars().collect();
	let folded: Vec<char> = chars.iter().copied().map(fold_char).collect();
	let mut marked = vec![false; chars.len()];
	for needle in needles {
		let mut from = 0;
		while let Some(at) = find(&folded, needle, from) {
			marked[at..at + needle.len()].fill(true);
			from = at + needle.len();
		}
	}
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
		assert!(titles(&index, &text("tenant electricity")).is_empty());
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
	fn short_queries_fall_back_to_substring_search() {
		let (_dir, mut index) = index();
		corpus(&mut index);
		assert_eq!(titles(&index, &text("保険")), ["健康保険料 領収書"]);
		assert_eq!(titles(&index, &text("円")), ["健康保険料 領収書"]);
		assert_eq!(
			titles(&index, &text("pa")),
			["Payslip March", "Lease Agreement"]
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
				.contains("<mark>&lt;b&gt;</mark>8,450")
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
		assert_eq!(snippet, "Needle <mark>report</mark>");
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
		let facets = index.facets().unwrap();
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
		assert_eq!(index.facets().unwrap().tags[0].count, 1);
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
}
