//! The HTTP API of `docs/api.md` and the embedded UI.

use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::extract::multipart::{Field, MultipartRejection};
use axum::extract::rejection::QueryRejection;
use axum::extract::{DefaultBodyLimit, Multipart, Path, Query, State};
use axum::http::header::{
	CACHE_CONTROL, CONTENT_DISPOSITION, CONTENT_TYPE, ETAG, IF_NONE_MATCH, X_CONTENT_TYPE_OPTIONS,
};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::json;
use tokio::io::AsyncWriteExt;
use uuid::Uuid;
use violet_core::{Query as IndexQuery, naming};
use violet_model::normalize_date;

use crate::app::blocking;
use crate::ops::{self, Patch};
use crate::{App, Error, Result, pipeline};

const UPLOAD_LIMIT: usize = 200 * 1024 * 1024;
const DEFAULT_PAGE_SIDE: u32 = 1200;
const MAX_PAGE_SIDE: u32 = 4096;
const DEFAULT_ACTIVITY: usize = 50;
const MAX_ACTIVITY: usize = 500;
const MAX_SEARCH_LIMIT: u32 = 500;
const INLINE_TYPES: [&str; 6] = [
	"application/pdf",
	"image/png",
	"image/jpeg",
	"image/gif",
	"image/webp",
	"text/plain",
];

#[derive(rust_embed::RustEmbed)]
#[folder = "$CARGO_MANIFEST_DIR/../../web/build"]
#[allow_missing = true]
struct Ui;

type AppState = State<Arc<App>>;

impl IntoResponse for Error {
	fn into_response(self) -> Response {
		let status = match &self {
			Self::NotFound(_) => StatusCode::NOT_FOUND,
			Self::Invalid(_) => StatusCode::BAD_REQUEST,
			Self::Conflict(_) => StatusCode::CONFLICT,
			Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
		};
		if status.is_server_error() {
			tracing::error!(error = %self, "request failed");
		}
		(status, Json(json!({ "error": self.to_string() }))).into_response()
	}
}

pub fn router(app: Arc<App>) -> Router {
	let api = Router::new()
		.route("/status", get(status))
		.route("/search", get(search))
		.route("/facets", get(facets))
		.route(
			"/documents/{id}",
			get(document).patch(update).delete(delete),
		)
		.route("/documents/{id}/file", get(file))
		.route("/documents/{id}/pages/{page}", get(page))
		.route("/documents/{id}/split", post(split))
		.route("/documents/{id}/rotate", post(rotate))
		.route("/documents/{id}/delete-pages", post(delete_pages))
		.route("/documents/{id}/reanalyze", post(reanalyze))
		.route("/merge", post(merge))
		.route(
			"/upload",
			post(upload).layer(DefaultBodyLimit::max(UPLOAD_LIMIT)),
		)
		.route("/activity", get(activity))
		.route("/activity/{id}/undo", post(undo))
		.route("/mail", get(mail))
		.route("/mail/{account}/sync", post(mail_sync))
		.route("/mail/processed/{id}/reprocess", post(mail_reprocess))
		.fallback(|| async { Error::NotFound("no such endpoint".into()) });
	Router::new()
		.nest("/api", api)
		.route("/healthz", get(|| async { "ok" }))
		.route("/readyz", get(ready))
		.fallback(ui)
		.layer(tower_http::trace::TraceLayer::new_for_http())
		.with_state(app)
}

fn id(raw: &str) -> Result<Uuid> {
	Uuid::parse_str(raw).map_err(|_| Error::Invalid("the id is not a UUID".into()))
}

fn body<T: DeserializeOwned>(bytes: &Bytes) -> Result<T> {
	serde_json::from_slice(bytes).map_err(|error| Error::Invalid(format!("body: {error}")))
}

fn header_value(value: &str) -> HeaderValue {
	HeaderValue::from_str(value).unwrap_or_else(|_| HeaderValue::from_static(""))
}

async fn ready(State(app): AppState) -> Result<&'static str> {
	blocking(&app, |app| {
		app.index().facets()?;
		Ok("ready")
	})
	.await
}

async fn status(State(app): AppState) -> Result<Json<ops::Status>> {
	let model = app.model_status().await;
	Ok(Json(
		blocking(&app, move |app| ops::status(app, model)).await?,
	))
}

fn number<T: std::str::FromStr>(name: &str, value: &str) -> Result<T> {
	value
		.trim()
		.parse()
		.map_err(|_| Error::Invalid(format!("{name} is not a number")))
}

fn search_query(pairs: Vec<(String, String)>) -> Result<IndexQuery> {
	let mut query = IndexQuery::default();
	for (name, value) in pairs {
		if value.trim().is_empty() {
			continue;
		}
		let date = |value: &str| {
			normalize_date(value).ok_or_else(|| Error::Invalid(format!("{name} is not a date")))
		};
		match name.as_str() {
			"q" => query.text = Some(value),
			"area" => query.area = Some(value),
			"type" => query.doc_type = Some(value),
			"issuer" => query.issuer = Some(value),
			"tag" => query.tags.push(value),
			"from" => query.date_from = Some(date(&value)?),
			"to" => query.date_to = Some(date(&value)?),
			"min" => query.amount_min = Some(number(&name, &value)?),
			"max" => query.amount_max = Some(number(&name, &value)?),
			"currency" => query.currency = Some(value.trim().to_uppercase()),
			"limit" => query.limit = number::<u32>(&name, &value)?.min(MAX_SEARCH_LIMIT),
			"offset" => query.offset = number(&name, &value)?,
			_ => {}
		}
	}
	Ok(query)
}

async fn search(
	State(app): AppState,
	pairs: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Result<Json<violet_core::SearchResult>> {
	let Query(pairs) = pairs.map_err(|error| Error::Invalid(error.body_text()))?;
	let query = search_query(pairs)?;
	Ok(Json(
		blocking(&app, move |app| Ok(app.index().search(&query)?)).await?,
	))
}

async fn facets(State(app): AppState) -> Result<Json<violet_core::Facets>> {
	Ok(Json(blocking(&app, |app| Ok(app.index().facets()?)).await?))
}

async fn document(
	State(app): AppState,
	Path(raw): Path<String>,
) -> Result<Json<ops::DocumentView>> {
	let id = id(&raw)?;
	Ok(Json(
		blocking(&app, move |app| ops::document(app, id)).await?,
	))
}

/// `filename="..."` with only plain ASCII, plus the real name as `filename*`.
fn disposition(kind: &str, path: &FsPath) -> String {
	let name = path
		.file_name()
		.map(|name| name.to_string_lossy().into_owned())
		.unwrap_or_default();
	let ascii: String = name
		.chars()
		.map(|c| match c {
			'"' | '\\' | '/' => '_',
			c if c.is_ascii_graphic() || c == ' ' => c,
			_ => '_',
		})
		.collect();
	let mut encoded = String::new();
	for byte in name.bytes() {
		if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
			encoded.push(byte as char);
		} else {
			encoded.push_str(&format!("%{byte:02X}"));
		}
	}
	format!("{kind}; filename=\"{ascii}\"; filename*=UTF-8''{encoded}")
}

async fn file(State(app): AppState, Path(raw): Path<String>) -> Result<Response> {
	let id = id(&raw)?;
	let (absolute, relative, _) = blocking(&app, move |app| ops::document_file(app, id)).await?;
	let bytes = tokio::fs::read(&absolute).await?;
	let guessed = mime_guess::from_path(&relative).first_or_octet_stream();
	let guessed = match guessed.essence_str() {
		"text/markdown" => "text/plain",
		other => other,
	}
	.to_string();
	let (kind, content_type) = if INLINE_TYPES.contains(&guessed.as_str()) {
		let charset = if guessed == "text/plain" {
			"; charset=utf-8"
		} else {
			""
		};
		("inline", format!("{guessed}{charset}"))
	} else {
		("attachment", "application/octet-stream".to_string())
	};
	Ok((
		[
			(CONTENT_TYPE, header_value(&content_type)),
			(
				CONTENT_DISPOSITION,
				header_value(&disposition(kind, &relative)),
			),
			(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff")),
			(CACHE_CONTROL, HeaderValue::from_static("no-cache")),
		],
		bytes,
	)
		.into_response())
}

#[derive(Deserialize)]
struct PageQuery {
	w: Option<String>,
	/// Start of the file's sha256; a request that names the version it wants can be cached
	/// for good.
	v: Option<String>,
}

async fn page(
	State(app): AppState,
	Path((raw, page)): Path<(String, String)>,
	query: Result<Query<PageQuery>, QueryRejection>,
	headers: HeaderMap,
) -> Result<Response> {
	let id = id(&raw)?;
	let page: u32 = page
		.parse()
		.map_err(|_| Error::Invalid("the page is not a number".into()))?;
	let Query(query) = query.map_err(|error| Error::Invalid(error.body_text()))?;
	let width = match query.w.as_deref() {
		Some(w) => number("w", w)?,
		None => DEFAULT_PAGE_SIDE,
	};
	if !(1..=MAX_PAGE_SIDE).contains(&width) {
		return Err(Error::Invalid(format!("w must be in 1..={MAX_PAGE_SIDE}")));
	}
	let _permit = app
		.renders
		.acquire()
		.await
		.map_err(|error| Error::Internal(error.to_string()))?;
	let (png, tag) = blocking(&app, move |app| ops::page_png(app, id, page, width)).await?;
	let pinned = query
		.v
		.as_deref()
		.is_some_and(|version| version.len() >= 8 && tag[1..].starts_with(version));
	let caching = [
		(ETAG, header_value(&tag)),
		(
			CACHE_CONTROL,
			HeaderValue::from_static(if pinned {
				"private, max-age=31536000, immutable"
			} else {
				"private, no-cache"
			}),
		),
	];
	if headers
		.get(IF_NONE_MATCH)
		.is_some_and(|known| known.as_bytes() == tag.as_bytes())
	{
		return Ok((StatusCode::NOT_MODIFIED, caching).into_response());
	}
	Ok((
		caching,
		[(CONTENT_TYPE, HeaderValue::from_static("image/png"))],
		png.to_vec(),
	)
		.into_response())
}

async fn update(
	State(app): AppState,
	Path(raw): Path<String>,
	bytes: Bytes,
) -> Result<Json<ops::Changed<ops::DocumentView>>> {
	let id = id(&raw)?;
	let patch: Patch = body(&bytes)?;
	Ok(Json(
		blocking(&app, move |app| ops::update(app, id, patch)).await?,
	))
}

async fn reanalyze(
	State(app): AppState,
	Path(raw): Path<String>,
) -> Result<Json<ops::Changed<ops::DocumentView>>> {
	Ok(Json(pipeline::reanalyze(&app, id(&raw)?).await?))
}

#[derive(Deserialize)]
struct SplitBody {
	groups: Vec<Vec<u32>>,
}

async fn split(
	State(app): AppState,
	Path(raw): Path<String>,
	bytes: Bytes,
) -> Result<Json<ops::Changed<ops::SplitResult>>> {
	let id = id(&raw)?;
	let request: SplitBody = body(&bytes)?;
	Ok(Json(
		blocking(&app, move |app| ops::split(app, id, &request.groups)).await?,
	))
}

#[derive(Deserialize)]
struct RotateBody {
	pages: Vec<u32>,
	degrees: i32,
}

async fn rotate(
	State(app): AppState,
	Path(raw): Path<String>,
	bytes: Bytes,
) -> Result<Json<ops::Changed<ops::DocumentView>>> {
	let id = id(&raw)?;
	let request: RotateBody = body(&bytes)?;
	Ok(Json(
		blocking(&app, move |app| {
			ops::rotate(app, id, &request.pages, request.degrees)
		})
		.await?,
	))
}

#[derive(Deserialize)]
struct PagesBody {
	pages: Vec<u32>,
}

async fn delete_pages(
	State(app): AppState,
	Path(raw): Path<String>,
	bytes: Bytes,
) -> Result<Json<ops::Changed<ops::DocumentView>>> {
	let id = id(&raw)?;
	let request: PagesBody = body(&bytes)?;
	Ok(Json(
		blocking(&app, move |app| ops::delete_pages(app, id, &request.pages)).await?,
	))
}

async fn delete(State(app): AppState, Path(raw): Path<String>) -> Result<Json<serde_json::Value>> {
	let id = id(&raw)?;
	let entry = blocking(&app, move |app| ops::delete(app, id)).await?;
	Ok(Json(json!({ "entry": entry })))
}

#[derive(Deserialize)]
struct MergeBody {
	ids: Vec<String>,
}

async fn merge(
	State(app): AppState,
	bytes: Bytes,
) -> Result<Json<ops::Changed<ops::DocumentView>>> {
	let request: MergeBody = body(&bytes)?;
	let ids = request
		.ids
		.iter()
		.map(|raw| id(raw))
		.collect::<Result<Vec<_>>>()?;
	Ok(Json(
		blocking(&app, move |app| ops::merge(app, &ids)).await?,
	))
}

/// A name for an uploaded file that is one plain path component.
fn upload_name(given: Option<&str>) -> String {
	let last = given
		.unwrap_or_default()
		.rsplit(['/', '\\'])
		.next()
		.unwrap_or_default();
	let name = naming::sanitize(last);
	if name.is_empty() {
		"upload".into()
	} else {
		name
	}
}

async fn save_upload(app: &App, mut field: Field<'_>) -> Result<()> {
	let inbox = app.library.inbox();
	let name = upload_name(field.file_name());
	let temp = inbox.join(format!(".upload-{}.part", Uuid::new_v4().simple()));
	let written: Result<()> = async {
		let mut file = tokio::fs::File::create(&temp).await?;
		while let Some(chunk) = field
			.chunk()
			.await
			.map_err(|error| Error::Invalid(format!("upload: {}", error.body_text())))?
		{
			file.write_all(&chunk).await?;
		}
		file.sync_all().await?;
		let target = naming::deconflict(&PathBuf::from(&name), |candidate| {
			inbox.join(candidate).exists()
		});
		tokio::fs::rename(&temp, inbox.join(target)).await?;
		Ok(())
	}
	.await;
	if written.is_err() {
		let _ = tokio::fs::remove_file(&temp).await;
	}
	written
}

async fn upload(
	State(app): AppState,
	multipart: Result<Multipart, MultipartRejection>,
) -> Result<Json<serde_json::Value>> {
	let mut multipart = multipart.map_err(|error| Error::Invalid(error.body_text()))?;
	let mut queued = 0;
	while let Some(field) = multipart
		.next_field()
		.await
		.map_err(|error| Error::Invalid(format!("upload: {}", error.body_text())))?
	{
		if field.name() != Some("file") {
			continue;
		}
		save_upload(&app, field).await?;
		queued += 1;
	}
	if queued == 0 {
		return Err(Error::Invalid("no file field in the upload".into()));
	}
	app.wake_watcher.notify_one();
	Ok(Json(json!({ "queued": queued })))
}

#[derive(Deserialize)]
struct ActivityQuery {
	limit: Option<String>,
}

async fn activity(
	State(app): AppState,
	query: Result<Query<ActivityQuery>, QueryRejection>,
) -> Result<Json<Vec<ops::ActivityItem>>> {
	let Query(query) = query.map_err(|error| Error::Invalid(error.body_text()))?;
	let limit = match query.limit.as_deref() {
		Some(limit) => number::<usize>("limit", limit)?.min(MAX_ACTIVITY),
		None => DEFAULT_ACTIVITY,
	};
	Ok(Json(
		blocking(&app, move |app| ops::activity(app, limit)).await?,
	))
}

async fn undo(State(app): AppState, Path(raw): Path<String>) -> Result<Json<serde_json::Value>> {
	let id = id(&raw)?;
	let entry = blocking(&app, move |app| ops::undo(app, id)).await?;
	Ok(Json(json!({ "entry": entry })))
}

async fn mail(State(app): AppState) -> Result<Json<crate::mail::MailView>> {
	Ok(Json(blocking(&app, crate::mail::view).await?))
}

async fn mail_sync(
	State(app): AppState,
	Path(account): Path<String>,
) -> Result<(StatusCode, Json<serde_json::Value>)> {
	let Some(mail) = app.mail() else {
		return Err(Error::NotFound("mail sync is off".into()));
	};
	if !mail.wake(&account) {
		return Err(Error::NotFound(format!("mail account {account}")));
	}
	Ok((
		StatusCode::ACCEPTED,
		Json(json!({ "account": account, "started": true })),
	))
}

async fn mail_reprocess(
	State(app): AppState,
	Path(raw): Path<String>,
) -> Result<Json<violet_core::ProcessedMail>> {
	let id: i64 = number("id", &raw)?;
	Ok(Json(
		blocking(&app, move |app| crate::mail::reprocess(app, id)).await?,
	))
}

async fn ui(method: Method, uri: Uri) -> Response {
	let path = uri.path().trim_start_matches('/');
	if path == "api" || path.starts_with("api/") {
		return Error::NotFound("no such endpoint".into()).into_response();
	}
	if method != Method::GET && method != Method::HEAD {
		return StatusCode::METHOD_NOT_ALLOWED.into_response();
	}
	let (name, asset) = match Ui::get(path) {
		Some(asset) if !path.is_empty() => (path, asset),
		_ => match Ui::get("index.html") {
			Some(index) => ("index.html", index),
			None => {
				return (StatusCode::NOT_FOUND, "the UI is not part of this build").into_response();
			}
		},
	};
	let caching = if name.contains("/immutable/") {
		"public, max-age=31536000, immutable"
	} else {
		"no-cache"
	};
	let content_type = mime_guess::from_path(name).first_or_octet_stream();
	(
		[
			(CONTENT_TYPE, header_value(content_type.as_ref())),
			(CACHE_CONTROL, HeaderValue::from_static(caching)),
		],
		Body::from(asset.data.into_owned()),
	)
		.into_response()
}
