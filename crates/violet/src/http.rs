//! The HTTP API of `docs/api.md` and the embedded UI.

use std::path::Path as FsPath;
use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::extract::multipart::Field;
use axum::extract::rejection::QueryRejection;
use axum::extract::{
	ConnectInfo, DefaultBodyLimit, Extension, FromRequest, Multipart, Path, Query, Request, State,
};
use axum::http::header::{
	CACHE_CONTROL, CONTENT_DISPOSITION, CONTENT_TYPE, ETAG, HOST, IF_NONE_MATCH, ORIGIN,
	WWW_AUTHENTICATE, X_CONTENT_TYPE_OPTIONS,
};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, Uri};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete as remove, get, patch, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::json;
use tokio::io::AsyncWriteExt;
use uuid::Uuid;
use violet_core::{Query as IndexQuery, Viewer, hash, naming};
use violet_model::normalize_date;

use crate::app::blocking;
use crate::auth::{Areas, Role, Scope, User, Via};
use crate::ops::{self, Patch};
use crate::{App, Error, Result, pipeline, users};

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
			Self::Unauthorized(_) => StatusCode::UNAUTHORIZED,
			Self::Forbidden(_) => StatusCode::FORBIDDEN,
			Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
		};
		if status.is_server_error() {
			tracing::error!(error = %self, "request failed");
		}
		let mut response = (status, Json(json!({ "error": self.to_string() }))).into_response();
		if status == StatusCode::UNAUTHORIZED {
			response.headers_mut().insert(
				WWW_AUTHENTICATE,
				HeaderValue::from_static("Bearer realm=\"violet\""),
			);
		}
		response
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
		.route("/documents/{id}/reread", post(reread))
		.route("/documents/{id}/not-duplicate", post(not_duplicate))
		.route("/duplicates", get(duplicates))
		.route(
			"/documents/{id}/replace",
			post(replace).layer(DefaultBodyLimit::max(UPLOAD_LIMIT)),
		)
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
		.route("/me", get(me))
		.route("/users", get(list_users).post(add_user))
		.route("/users/{name}", patch(update_user).delete(remove_user))
		.route("/users/{name}/tokens", post(add_token))
		.route("/users/{name}/tokens/{token}", remove(revoke_token))
		.route("/areas", get(areas).put(set_areas))
		.fallback(|| async { Error::NotFound("no such endpoint".into()) })
		.layer(middleware::from_fn_with_state(app.clone(), identify))
		.layer(middleware::from_fn(same_origin));
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

fn has_type(headers: &HeaderMap, wanted: &str) -> bool {
	headers
		.get(CONTENT_TYPE)
		.and_then(|value| value.to_str().ok())
		.and_then(|value| value.split(';').next())
		.is_some_and(|essence| essence.trim().eq_ignore_ascii_case(wanted))
}

/// A JSON body, sent as `application/json`.
struct JsonBody<T>(T);

impl<T: DeserializeOwned, S: Send + Sync> FromRequest<S> for JsonBody<T> {
	type Rejection = Response;

	async fn from_request(request: Request, state: &S) -> std::result::Result<Self, Response> {
		if !has_type(request.headers(), "application/json") {
			return Err(refused(
				StatusCode::UNSUPPORTED_MEDIA_TYPE,
				"the body must be sent as Content-Type: application/json",
			));
		}
		let bytes = Bytes::from_request(request, state)
			.await
			.map_err(IntoResponse::into_response)?;
		serde_json::from_slice(&bytes)
			.map(Self)
			.map_err(|error| Error::Invalid(format!("body: {error}")).into_response())
	}
}

/// A `multipart/form-data` body.
struct Upload(Multipart);

impl<S: Send + Sync> FromRequest<S> for Upload {
	type Rejection = Response;

	async fn from_request(request: Request, state: &S) -> std::result::Result<Self, Response> {
		if !has_type(request.headers(), "multipart/form-data") {
			return Err(refused(
				StatusCode::UNSUPPORTED_MEDIA_TYPE,
				"files must be sent as Content-Type: multipart/form-data",
			));
		}
		Multipart::from_request(request, state)
			.await
			.map(Self)
			.map_err(|error| Error::Invalid(error.body_text()).into_response())
	}
}

fn refused(status: StatusCode, message: &str) -> Response {
	(status, Json(json!({ "error": message }))).into_response()
}

/// Refuses requests that change something when a browser says they come from another site.
/// Clients that send no `Origin`, like curl and the CLI, pass.
async fn same_origin(request: Request, next: Next) -> Response {
	let reads = matches!(
		*request.method(),
		Method::GET | Method::HEAD | Method::OPTIONS
	);
	let authority = request
		.uri()
		.authority()
		.map(|authority| authority.as_str());
	if !reads && let Some(reason) = cross_site(request.headers(), authority) {
		return refused(StatusCode::FORBIDDEN, reason);
	}
	next.run(request).await
}

/// What the request presented, to check that a change of the users file leaves its sender
/// signed in.
#[derive(Clone)]
struct Credentials {
	headers: HeaderMap,
	peer: Option<std::net::IpAddr>,
}

/// Finds the user of an API request and refuses what its scope does not allow.
async fn identify(State(app): AppState, mut request: Request, next: Next) -> Response {
	let peer = request
		.extensions()
		.get::<ConnectInfo<std::net::SocketAddr>>()
		.map(|ConnectInfo(address)| address.ip());
	let user = match app.auth.resolve(request.headers(), peer) {
		Ok(user) => user,
		Err(error) => return error.into_response(),
	};
	let reads = matches!(
		*request.method(),
		Method::GET | Method::HEAD | Method::OPTIONS
	);
	if !reads && !user.can_write() {
		return Error::Forbidden("this token can only read".into()).into_response();
	}
	let credentials = Credentials {
		headers: request.headers().clone(),
		peer,
	};
	request.extensions_mut().insert(user);
	request.extensions_mut().insert(credentials);
	next.run(request).await
}

fn admin(user: &User) -> Result<()> {
	if user.is_admin() {
		Ok(())
	} else {
		Err(Error::Forbidden("only an admin can do this".into()))
	}
}

const MISMATCH: &str = "the Origin does not match the host";

/// `authority` is the host of the request URI, which HTTP/2 sends instead of `Host`.
fn cross_site(headers: &HeaderMap, authority: Option<&str>) -> Option<&'static str> {
	let fetch_site = headers.get("sec-fetch-site");
	if fetch_site.is_some_and(|site| site.as_bytes().eq_ignore_ascii_case(b"cross-site")) {
		return Some("cross-site requests cannot change the library");
	}
	let origin = headers.get(ORIGIN)?;
	let Some((scheme, origin_authority)) = origin.to_str().ok().and_then(|origin| {
		let (scheme, authority) = origin.trim().split_once("://")?;
		(!authority.is_empty() && !authority.contains('/')).then_some((scheme, authority))
	}) else {
		return Some(MISMATCH);
	};
	let normal = |authority: &str| {
		let lower = authority.trim().to_ascii_lowercase();
		let default = if scheme.eq_ignore_ascii_case("https") {
			":443"
		} else {
			":80"
		};
		lower
			.strip_suffix(default)
			.map(str::to_string)
			.unwrap_or(lower)
	};
	let wanted = normal(origin_authority);
	let headers = [headers.get(HOST), headers.get("x-forwarded-host")];
	let matches = headers
		.into_iter()
		.flatten()
		.filter_map(|host| host.to_str().ok()?.split(',').next())
		.chain(authority)
		.any(|host| normal(host) == wanted);
	(!matches).then_some(MISMATCH)
}

fn header_value(value: &str) -> HeaderValue {
	HeaderValue::from_str(value).unwrap_or_else(|_| HeaderValue::from_static(""))
}

async fn ready(State(app): AppState) -> Result<&'static str> {
	blocking(&app, |app| {
		app.index().facets(&Viewer::everything())?;
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

#[derive(serde::Serialize)]
struct SearchAnswer {
	#[serde(flatten)]
	result: violet_core::SearchResult,
	understood: violet_model::Understood,
}

async fn search(
	State(app): AppState,
	Extension(user): Extension<User>,
	pairs: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Result<Json<SearchAnswer>> {
	let Query(pairs) = pairs.map_err(|error| Error::Invalid(error.body_text()))?;
	let mut query = search_query(pairs)?;
	query.viewer = user.viewer;
	let understood = violet_model::understand(&mut query);
	let result = blocking(&app, move |app| Ok(app.index().search(&query)?)).await?;
	Ok(Json(SearchAnswer { result, understood }))
}

async fn facets(
	State(app): AppState,
	Extension(user): Extension<User>,
) -> Result<Json<violet_core::Facets>> {
	Ok(Json(
		blocking(&app, move |app| Ok(app.index().facets(&user.viewer)?)).await?,
	))
}

async fn document(
	State(app): AppState,
	Extension(user): Extension<User>,
	Path(raw): Path<String>,
) -> Result<Json<ops::DocumentView>> {
	let id = id(&raw)?;
	Ok(Json(
		blocking(&app, move |app| ops::document_as(app, &user, id)).await?,
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

async fn file(
	State(app): AppState,
	Extension(user): Extension<User>,
	Path(raw): Path<String>,
) -> Result<Response> {
	let id = id(&raw)?;
	let (absolute, relative, _) =
		blocking(&app, move |app| ops::document_file_as(app, &user, id)).await?;
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
	Extension(user): Extension<User>,
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
	let (png, tag) = blocking(&app, move |app| ops::page_png(app, &user, id, page, width)).await?;
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
	Extension(user): Extension<User>,
	Path(raw): Path<String>,
	JsonBody(patch): JsonBody<Patch>,
) -> Result<Json<ops::Changed<ops::DocumentView>>> {
	let id = id(&raw)?;
	Ok(Json(
		blocking(&app, move |app| ops::update_as(app, &user, id, patch)).await?,
	))
}

async fn reanalyze(
	State(app): AppState,
	Extension(user): Extension<User>,
	Path(raw): Path<String>,
) -> Result<Json<ops::Changed<ops::DocumentView>>> {
	Ok(Json(pipeline::reanalyze_as(&app, &user, id(&raw)?).await?))
}

async fn reread(
	State(app): AppState,
	Extension(user): Extension<User>,
	Path(raw): Path<String>,
) -> Result<Json<ops::Changed<ops::DocumentView>>> {
	Ok(Json(pipeline::reread_as(&app, &user, id(&raw)?).await?))
}

async fn replace(
	State(app): AppState,
	Extension(user): Extension<User>,
	Path(raw): Path<String>,
	Upload(mut multipart): Upload,
) -> Result<Json<ops::Changed<ops::DocumentView>>> {
	let id = id(&raw)?;
	let mut file: Option<(String, Bytes)> = None;
	while let Some(field) = multipart
		.next_field()
		.await
		.map_err(|error| Error::Invalid(format!("upload: {}", error.body_text())))?
	{
		if field.name() != Some("file") {
			continue;
		}
		if file.is_some() {
			return Err(Error::Invalid("replace takes exactly one file".into()));
		}
		let name = upload_name(field.file_name());
		let bytes = field
			.bytes()
			.await
			.map_err(|error| Error::Invalid(format!("upload: {}", error.body_text())))?;
		file = Some((name, bytes));
	}
	let (name, bytes) = file.ok_or_else(|| Error::Invalid("no file field in the upload".into()))?;
	Ok(Json(
		pipeline::replace_as(&app, &user, id, &name, bytes.to_vec()).await?,
	))
}

#[derive(Deserialize)]
struct SplitBody {
	groups: Vec<Vec<u32>>,
}

async fn split(
	State(app): AppState,
	Extension(user): Extension<User>,
	Path(raw): Path<String>,
	JsonBody(request): JsonBody<SplitBody>,
) -> Result<Json<ops::Changed<ops::SplitResult>>> {
	let id = id(&raw)?;
	Ok(Json(
		blocking(&app, move |app| {
			ops::split_as(app, &user, id, &request.groups)
		})
		.await?,
	))
}

#[derive(Deserialize)]
struct RotateBody {
	pages: Vec<u32>,
	degrees: i32,
}

async fn rotate(
	State(app): AppState,
	Extension(user): Extension<User>,
	Path(raw): Path<String>,
	JsonBody(request): JsonBody<RotateBody>,
) -> Result<Json<ops::Changed<ops::DocumentView>>> {
	let id = id(&raw)?;
	Ok(Json(
		blocking(&app, move |app| {
			ops::rotate_as(app, &user, id, &request.pages, request.degrees)
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
	Extension(user): Extension<User>,
	Path(raw): Path<String>,
	JsonBody(request): JsonBody<PagesBody>,
) -> Result<Json<ops::Changed<ops::DocumentView>>> {
	let id = id(&raw)?;
	Ok(Json(
		blocking(&app, move |app| {
			ops::delete_pages_as(app, &user, id, &request.pages)
		})
		.await?,
	))
}

async fn delete(
	State(app): AppState,
	Extension(user): Extension<User>,
	Path(raw): Path<String>,
) -> Result<Json<serde_json::Value>> {
	let id = id(&raw)?;
	let entry = blocking(&app, move |app| ops::delete_as(app, &user, id)).await?;
	Ok(Json(json!({ "entry": entry })))
}

#[derive(serde::Serialize)]
struct DuplicatesAnswer {
	pairs: Vec<[violet_core::Hit; 2]>,
}

async fn duplicates(
	State(app): AppState,
	Extension(user): Extension<User>,
) -> Result<Json<DuplicatesAnswer>> {
	let pairs = blocking(&app, move |app| Ok(app.index().duplicates(&user.viewer)?)).await?;
	Ok(Json(DuplicatesAnswer { pairs }))
}

#[derive(Deserialize)]
struct NotDuplicateBody {
	of: String,
}

async fn not_duplicate(
	State(app): AppState,
	Extension(user): Extension<User>,
	Path(raw): Path<String>,
	JsonBody(request): JsonBody<NotDuplicateBody>,
) -> Result<Json<ops::Changed<ops::DocumentView>>> {
	let of = id(&request.of)?;
	let id = id(&raw)?;
	Ok(Json(
		blocking(&app, move |app| ops::not_duplicate_as(app, &user, id, of)).await?,
	))
}

#[derive(Deserialize)]
struct MergeBody {
	ids: Vec<String>,
}

async fn merge(
	State(app): AppState,
	Extension(user): Extension<User>,
	JsonBody(request): JsonBody<MergeBody>,
) -> Result<Json<ops::Changed<ops::DocumentView>>> {
	let ids = request
		.ids
		.iter()
		.map(|raw| id(raw))
		.collect::<Result<Vec<_>>>()?;
	Ok(Json(
		blocking(&app, move |app| ops::merge_as(app, &user, &ids)).await?,
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

async fn save_upload(app: &Arc<App>, user: &User, mut field: Field<'_>) -> Result<()> {
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
		let placed = temp.clone();
		let uploader = user.name.clone();
		blocking(app, move |app| {
			if let Some(uploader) = uploader {
				let sha256 = hash::sha256_file(&placed)?;
				app.index().set_uploader(&sha256, &uploader)?;
			}
			Ok(app.library.into_inbox(&placed, FsPath::new(&name))?)
		})
		.await?;
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
	Extension(user): Extension<User>,
	Upload(mut multipart): Upload,
) -> Result<Json<serde_json::Value>> {
	let mut queued = 0;
	while let Some(field) = multipart
		.next_field()
		.await
		.map_err(|error| Error::Invalid(format!("upload: {}", error.body_text())))?
	{
		if field.name() != Some("file") {
			continue;
		}
		save_upload(&app, &user, field).await?;
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
	Extension(user): Extension<User>,
	query: Result<Query<ActivityQuery>, QueryRejection>,
) -> Result<Json<Vec<ops::ActivityItem>>> {
	let Query(query) = query.map_err(|error| Error::Invalid(error.body_text()))?;
	let limit = match query.limit.as_deref() {
		Some(limit) => number::<usize>("limit", limit)?.min(MAX_ACTIVITY),
		None => DEFAULT_ACTIVITY,
	};
	Ok(Json(
		blocking(&app, move |app| ops::activity_as(app, &user, limit)).await?,
	))
}

async fn undo(
	State(app): AppState,
	Extension(user): Extension<User>,
	Path(raw): Path<String>,
) -> Result<Json<serde_json::Value>> {
	let id = id(&raw)?;
	let entry = blocking(&app, move |app| ops::undo_as(app, &user, id)).await?;
	Ok(Json(json!({ "entry": entry })))
}

async fn mail(
	State(app): AppState,
	Extension(user): Extension<User>,
) -> Result<Json<crate::mail::MailView>> {
	admin(&user)?;
	Ok(Json(blocking(&app, crate::mail::view).await?))
}

async fn mail_sync(
	State(app): AppState,
	Extension(user): Extension<User>,
	Path(account): Path<String>,
) -> Result<(StatusCode, Json<serde_json::Value>)> {
	admin(&user)?;
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
	Extension(user): Extension<User>,
	Path(raw): Path<String>,
) -> Result<Json<violet_core::ProcessedMail>> {
	admin(&user)?;
	let id: i64 = number("id", &raw)?;
	Ok(Json(
		blocking(&app, move |app| crate::mail::reprocess(app, id)).await?,
	))
}

#[derive(serde::Serialize)]
struct Me {
	name: Option<String>,
	role: Role,
	scope: Scope,
	via: Via,
	/// The areas of the documents this user can see.
	areas: Vec<String>,
	/// Whether the library has a users file.
	users: bool,
}

async fn me(State(app): AppState, Extension(user): Extension<User>) -> Result<Json<Me>> {
	blocking(&app, move |app| {
		let areas = app
			.index()
			.facets(&user.viewer)?
			.areas
			.into_iter()
			.map(|facet| facet.value)
			.collect();
		Ok(Json(Me {
			users: app.auth.users()?.is_some(),
			name: user.name,
			role: user.role,
			scope: user.scope,
			via: user.via,
			areas,
		}))
	})
	.await
}

/// Refuses a change of the users file after which the sender would no longer be an admin.
fn keeps_admin(app: &App, credentials: &Credentials, users: &crate::auth::Users) -> Result<()> {
	match app
		.auth
		.resolve_in(users, &credentials.headers, credentials.peer)
	{
		Ok(after) if after.is_admin() => Ok(()),
		_ => Err(Error::Conflict(
			"this change would leave you without admin access: sign in with a token or a \
			 certificate of an admin, or set VIOLET_DEFAULT_USER to an admin"
				.into(),
		)),
	}
}

#[derive(serde::Serialize)]
struct UsersAnswer {
	/// Whether the library has a users file; without one everyone is the implicit admin.
	configured: bool,
	users: Vec<users::UserView>,
}

async fn list_users(
	State(app): AppState,
	Extension(user): Extension<User>,
) -> Result<Json<UsersAnswer>> {
	admin(&user)?;
	let listed = blocking(&app, users::list).await?;
	Ok(Json(UsersAnswer {
		configured: listed.is_some(),
		users: listed.unwrap_or_default(),
	}))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewUser {
	name: String,
	role: Role,
	#[serde(default)]
	identities: Vec<String>,
}

async fn add_user(
	State(app): AppState,
	Extension(user): Extension<User>,
	Extension(credentials): Extension<Credentials>,
	JsonBody(body): JsonBody<NewUser>,
) -> Result<Json<serde_json::Value>> {
	admin(&user)?;
	let entry = blocking(&app, move |app| {
		let check = |users: &crate::auth::Users| keeps_admin(app, &credentials, users);
		users::add(app, &user, &check, &body.name, body.role, body.identities)
	})
	.await?;
	Ok(Json(json!({ "entry": entry.id })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UserPatch {
	role: Option<Role>,
	identities: Option<Vec<String>>,
}

async fn update_user(
	State(app): AppState,
	Extension(user): Extension<User>,
	Extension(credentials): Extension<Credentials>,
	Path(name): Path<String>,
	JsonBody(body): JsonBody<UserPatch>,
) -> Result<Json<serde_json::Value>> {
	admin(&user)?;
	let entry = blocking(&app, move |app| {
		let check = |users: &crate::auth::Users| keeps_admin(app, &credentials, users);
		users::update(app, &user, &check, &name, body.role, body.identities)
	})
	.await?;
	Ok(Json(json!({ "entry": entry.id })))
}

async fn remove_user(
	State(app): AppState,
	Extension(user): Extension<User>,
	Extension(credentials): Extension<Credentials>,
	Path(name): Path<String>,
) -> Result<Json<serde_json::Value>> {
	admin(&user)?;
	let entry = blocking(&app, move |app| {
		let check = |users: &crate::auth::Users| keeps_admin(app, &credentials, users);
		users::remove(app, &user, &check, &name)
	})
	.await?;
	Ok(Json(json!({ "entry": entry.id })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewToken {
	name: String,
	scope: Scope,
	#[serde(default, with = "time::serde::rfc3339::option")]
	expires: Option<time::OffsetDateTime>,
}

async fn add_token(
	State(app): AppState,
	Extension(user): Extension<User>,
	Extension(credentials): Extension<Credentials>,
	Path(name): Path<String>,
	JsonBody(body): JsonBody<NewToken>,
) -> Result<Json<serde_json::Value>> {
	admin(&user)?;
	let (secret, entry) = blocking(&app, move |app| {
		let check = |users: &crate::auth::Users| keeps_admin(app, &credentials, users);
		users::add_token(
			app,
			&user,
			&check,
			&name,
			&body.name,
			body.scope,
			body.expires,
		)
	})
	.await?;
	Ok(Json(json!({ "token": secret, "entry": entry.id })))
}

async fn revoke_token(
	State(app): AppState,
	Extension(user): Extension<User>,
	Extension(credentials): Extension<Credentials>,
	Path((name, token)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>> {
	admin(&user)?;
	let entry = blocking(&app, move |app| {
		let check = |users: &crate::auth::Users| keeps_admin(app, &credentials, users);
		users::revoke_token(app, &user, &check, &name, &token)
	})
	.await?;
	Ok(Json(json!({ "entry": entry.id })))
}

async fn areas(State(app): AppState, Extension(user): Extension<User>) -> Result<Json<Areas>> {
	admin(&user)?;
	Ok(Json(blocking(&app, users::areas).await?))
}

async fn set_areas(
	State(app): AppState,
	Extension(user): Extension<User>,
	JsonBody(body): JsonBody<Areas>,
) -> Result<Json<serde_json::Value>> {
	admin(&user)?;
	let entry = blocking(&app, move |app| users::set_areas(app, &user, body)).await?;
	Ok(Json(json!({ "entry": entry.id })))
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
