//! `/api/auth/*`: what the login page needs, password and passkey sign-in, setup and logout;
//! `/api/account/*`: one's own password, passkeys and sessions.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use axum::extract::{ConnectInfo, Extension, FromRequestParts, Path, Request, State};
use axum::http::header::{SET_COOKIE, USER_AGENT};
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use time::OffsetDateTime;
use violet_core::Session;
use webauthn_rs::prelude::{PublicKeyCredential, RegisterPublicKeyCredential};

use crate::app::blocking;
use crate::auth::{
	LoginMode, Role, Scope, StoredPasskey, User, Via, passkey_credential, password_credential,
	session_cookie, valid_label,
};
use crate::http::{AppState, JsonBody, csrf, from_a_page, writes};
use crate::login::{self, Ceremony, WRONG_PASSKEY, WRONG_PASSWORD, check_password};
use crate::users::{self, NO_CHECK, PasskeyView};
use crate::{App, Error, Result};

const SESSION_ID: usize = 16;

pub fn router(app: Arc<App>) -> Router<Arc<App>> {
	let auth = Router::new()
		.route("/auth", get(state))
		.route("/auth/setup/start", post(setup_start))
		.route("/auth/setup/finish", post(setup_finish))
		.route("/auth/login", post(sign_in))
		.route("/auth/passkey/start", post(passkey_start))
		.route("/auth/passkey/finish", post(passkey_finish))
		.route("/auth/logout", post(logout))
		.layer(middleware::from_fn_with_state(app.clone(), maybe_identify));
	let account = Router::new()
		.route("/account/password", put(change_password))
		.route("/account/passkeys", get(passkeys))
		.route("/account/passkeys/start", post(register_start))
		.route("/account/passkeys/finish", post(register_finish))
		.route(
			"/account/passkeys/{name}",
			axum::routing::patch(rename_passkey).delete(remove_passkey),
		)
		.route("/account/sessions", get(sessions))
		.route("/account/sessions/{id}", delete(end_session))
		.layer(middleware::from_fn_with_state(app, signed_in));
	auth.merge(account)
}

/// The address of the peer, when the server knows it.
struct Peer(Option<IpAddr>);

impl<S: Send + Sync> FromRequestParts<S> for Peer {
	type Rejection = std::convert::Infallible;

	async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
		Ok(Self(
			parts
				.extensions
				.get::<ConnectInfo<SocketAddr>>()
				.map(|ConnectInfo(address)| address.ip()),
		))
	}
}

/// The user of a request that may have none.
async fn maybe_identify(State(app): AppState, mut request: Request, next: Next) -> Response {
	if writes(request.method())
		&& let Err(error) = enabled(&app)
	{
		return error.into_response();
	}
	let user = match app.auth.resolve(request.headers()) {
		Ok(user) => Some(user),
		Err(Error::Unauthorized(_)) => None,
		Err(error) => return error.into_response(),
	};
	request.extensions_mut().insert(user);
	next.run(request).await
}

/// Lets through only requests signed in with a session, and checks their CSRF token.
async fn signed_in(State(app): AppState, mut request: Request, next: Next) -> Response {
	if app.auth.mode() == LoginMode::Off {
		return Error::NotFound("no such endpoint".into()).into_response();
	}
	let user = match app.auth.resolve(request.headers()) {
		Ok(user) => user,
		Err(error) => return error.into_response(),
	};
	if user.via != Via::Session {
		return Error::Forbidden("sign in first".into()).into_response();
	}
	if writes(request.method())
		&& let Err(error) = csrf(request.headers(), &user)
	{
		return error.into_response();
	}
	request.extensions_mut().insert(user);
	next.run(request).await
}

fn enabled(app: &App) -> Result<()> {
	match app.auth.mode() {
		LoginMode::Off => Err(Error::NotFound("no such endpoint".into())),
		_ => Ok(()),
	}
}

fn name_of(user: &User) -> Result<&str> {
	user.name
		.as_deref()
		.ok_or_else(|| Error::Forbidden("sign in first".into()))
}

#[derive(Serialize)]
pub struct Me {
	/// The CSRF token of the session the request was signed in with.
	csrf: Option<String>,
	name: Option<String>,
	role: Role,
	scope: Scope,
	via: Via,
	/// The areas of the documents this user can see.
	areas: Vec<String>,
	/// Whether the library has a users file.
	users: bool,
	password: bool,
	passkeys: Vec<String>,
}

pub fn me(app: &App, user: User) -> Result<Me> {
	let areas = app
		.index()
		.facets(&user.viewer)?
		.areas
		.into_iter()
		.map(|facet| facet.value)
		.collect();
	let users = app.auth.users()?;
	let account = users
		.as_deref()
		.zip(user.name.as_deref())
		.and_then(|(users, name)| users.get(name));
	Ok(Me {
		csrf: user.session.as_ref().map(|session| session.csrf.clone()),
		users: users.is_some(),
		password: account.is_some_and(|account| account.password.is_some()),
		passkeys: account
			.map(|account| {
				account
					.passkeys
					.iter()
					.map(|passkey| passkey.name.clone())
					.collect()
			})
			.unwrap_or_default(),
		name: user.name,
		role: user.role,
		scope: user.scope,
		via: user.via,
		areas,
	})
}

#[derive(Serialize)]
struct AuthState {
	mode: LoginMode,
	/// Whether no account has a credential yet, so the first admin can be created.
	setup: bool,
	user: Option<Me>,
}

async fn state(
	State(app): AppState,
	Extension(user): Extension<Option<User>>,
) -> Result<Json<AuthState>> {
	blocking(&app, move |app| {
		let mode = app.auth.mode();
		let set_up = app
			.auth
			.users()?
			.is_some_and(|users| users.any_credential());
		Ok(Json(AuthState {
			mode,
			setup: mode != LoginMode::Off && !set_up,
			user: user.map(|user| me(app, user)).transpose()?,
		}))
	})
	.await
}

/// The answer of a sign-in: who it is, with the cookie of the new session.
fn signed_in_as(app: &App, token: &str, session: &Session) -> Result<Response> {
	let users = app.auth.users()?;
	let account = users
		.as_deref()
		.and_then(|users| users.get(&session.user))
		.ok_or_else(|| Error::Unauthorized("sign in first".into()))?;
	let user = User {
		name: Some(account.name.clone()),
		role: account.role,
		scope: account.role.scope(),
		viewer: app.auth.viewer(Some(&account.name))?,
		via: Via::Session,
		session: Some(crate::auth::SessionRef {
			hash: session.hash.clone(),
			csrf: session.csrf.clone(),
			credential: session.credential.clone(),
		}),
	};
	let mut response = Json(me(app, user)?).into_response();
	response
		.headers_mut()
		.insert(SET_COOKIE, login::set_cookie(token));
	Ok(response)
}

fn label(headers: &HeaderMap) -> String {
	login::label(
		headers
			.get(USER_AGENT)
			.and_then(|value| value.to_str().ok()),
	)
}

async fn open(
	app: &Arc<App>,
	headers: &HeaderMap,
	user: String,
	credential: String,
) -> Result<Response> {
	let label = label(headers);
	blocking(app, move |app| {
		let (token, session) =
			login::open_session(app.auth.sessions()?, &user, credential, label, None)?;
		signed_in_as(app, &token, &session)
	})
	.await
}

fn client(app: &App, headers: &HeaderMap, peer: Option<IpAddr>) -> Option<IpAddr> {
	login::client(headers, peer, &app.auth.config().trusted_proxies)
}

fn wait(app: &App, name: &str, address: Option<IpAddr>) -> Result<()> {
	match app.auth.login.limited(name, address) {
		Some(wait) => Err(Error::TooMany(wait.as_secs_f64().ceil() as u64)),
		None => Ok(()),
	}
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Credentials {
	name: String,
	password: String,
}

async fn sign_in(
	State(app): AppState,
	Peer(peer): Peer,
	headers: HeaderMap,
	JsonBody(body): JsonBody<Credentials>,
) -> Result<Response> {
	enabled(&app)?;
	from_a_page(&headers)?;
	let address = client(&app, &headers, peer);
	wait(&app, &body.name, address)?;
	let account = app
		.auth
		.users()?
		.and_then(|users| users.get(&body.name).cloned());
	let phc = account
		.as_ref()
		.and_then(|account| account.password.clone());
	let right = app.auth.login.verify(phc.clone(), body.password).await?;
	match (account, phc) {
		(Some(account), Some(phc)) if right => {
			open(&app, &headers, account.name, password_credential(&phc)).await
		}
		_ => {
			app.auth.login.failed(&body.name, address);
			Err(Error::Unauthorized(WRONG_PASSWORD.into()))
		}
	}
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Named {
	name: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MaybeNamed {
	#[serde(default)]
	name: Option<String>,
}

impl MaybeNamed {
	fn name(&self) -> Option<&str> {
		self.name
			.as_deref()
			.map(str::trim)
			.filter(|name| !name.is_empty())
	}
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Finish {
	ceremony: String,
	credential: Value,
}

fn webauthn(app: &App) -> Result<&webauthn_rs::Webauthn> {
	app.auth
		.webauthn()
		.ok_or_else(|| Error::NotFound("passkeys are off".into()))
}

/// Only the implicit admin, while no account can sign in, creates the first admin.
fn may_set_up(app: &App, user: Option<User>) -> Result<User> {
	let Some(user) = user.filter(|user| user.via == Via::Implicit) else {
		return Err(Error::Forbidden(
			"only the implicit admin can set the library up".into(),
		));
	};
	if app
		.auth
		.users()?
		.is_some_and(|users| users.any_credential())
	{
		return Err(Error::Conflict("the library is set up already".into()));
	}
	Ok(user)
}

async fn setup_start(
	State(app): AppState,
	Extension(user): Extension<Option<User>>,
	headers: HeaderMap,
	JsonBody(body): JsonBody<Named>,
) -> Result<Json<Value>> {
	enabled(&app)?;
	from_a_page(&headers)?;
	let webauthn = webauthn(&app)?;
	may_set_up(&app, user)?;
	let name = body.name.trim().to_string();
	crate::auth::valid_name("user", &name)?;
	let (options, state) = webauthn
		.start_passkey_registration(login::user_handle(&name), &name, &name, None)
		.map_err(|error| Error::Internal(format!("passkey: {error}")))?;
	let ceremony = app.auth.login.start(Ceremony::SetUp {
		user: name,
		name: login::free_label(&label(&headers), |_| false),
		state,
	})?;
	Ok(Json(json!({
		"ceremony": ceremony,
		"options": login::discoverable(serde_json::to_value(options)?),
	})))
}

async fn setup_finish(
	State(app): AppState,
	Extension(user): Extension<Option<User>>,
	headers: HeaderMap,
	JsonBody(body): JsonBody<Finish>,
) -> Result<Response> {
	enabled(&app)?;
	from_a_page(&headers)?;
	let webauthn = webauthn(&app)?;
	let user = may_set_up(&app, user)?;
	let Some(Ceremony::SetUp {
		user: owner,
		name,
		state,
	}) = app.auth.login.finish(&body.ceremony)
	else {
		return Err(Error::Invalid(
			"the passkey request expired: start again".into(),
		));
	};
	let credential: RegisterPublicKeyCredential = serde_json::from_value(body.credential)
		.map_err(|_| Error::Invalid("the passkey answer is not a credential".into()))?;
	let passkey = webauthn
		.finish_passkey_registration(&credential, &state)
		.map_err(|error| {
			tracing::info!(%error, "passkey registration refused");
			Error::Invalid("the passkey could not be registered".into())
		})?;
	let session = passkey_credential(passkey.cred_id());
	let stored = StoredPasskey {
		name,
		created: now(),
		credential: passkey,
	};
	let account = owner.clone();
	blocking(&app, move |app| users::set_up(app, &user, &account, stored)).await?;
	open(&app, &headers, owner, session).await
}

fn now() -> OffsetDateTime {
	let now = OffsetDateTime::now_utc();
	now.replace_nanosecond(0).unwrap_or(now)
}

async fn passkey_start(
	State(app): AppState,
	headers: HeaderMap,
	JsonBody(body): JsonBody<MaybeNamed>,
) -> Result<Json<Value>> {
	enabled(&app)?;
	from_a_page(&headers)?;
	let webauthn = webauthn(&app)?;
	let Some(name) = body.name() else {
		let (options, state) = webauthn
			.start_discoverable_authentication()
			.map_err(|error| Error::Internal(format!("passkey: {error}")))?;
		let ceremony = app.auth.login.start(Ceremony::Discover { state })?;
		return Ok(Json(json!({
			"ceremony": ceremony,
			"options": login::modal(serde_json::to_value(options)?),
		})));
	};
	let account = app
		.auth
		.users()?
		.and_then(|users| users.get(name).cloned())
		.filter(|account| !account.passkeys.is_empty())
		.ok_or_else(|| Error::Unauthorized(WRONG_PASSKEY.into()))?;
	let passkeys: Vec<_> = account
		.passkeys
		.iter()
		.map(|passkey| passkey.credential.clone())
		.collect();
	let (options, state) = webauthn
		.start_passkey_authentication(&passkeys)
		.map_err(|error| Error::Internal(format!("passkey: {error}")))?;
	let ceremony = app.auth.login.start(Ceremony::SignIn {
		user: account.name,
		state,
	})?;
	Ok(Json(json!({ "ceremony": ceremony, "options": options })))
}

async fn passkey_finish(
	State(app): AppState,
	headers: HeaderMap,
	JsonBody(body): JsonBody<Finish>,
) -> Result<Response> {
	enabled(&app)?;
	from_a_page(&headers)?;
	let webauthn = webauthn(&app)?;
	let wrong = || Error::Unauthorized(WRONG_PASSKEY.into());
	let refused = |error: webauthn_rs::prelude::WebauthnError| {
		tracing::info!(%error, "passkey sign-in refused");
		wrong()
	};
	let credential: Result<PublicKeyCredential> =
		serde_json::from_value(body.credential).map_err(|_| wrong());
	let (user, result) = match app.auth.login.finish(&body.ceremony) {
		Some(Ceremony::SignIn { user, state }) => {
			let result = webauthn
				.finish_passkey_authentication(&credential?, &state)
				.map_err(refused)?;
			(user, result)
		}
		Some(Ceremony::Discover { state }) => {
			let credential = credential?;
			let (handle, id) = webauthn
				.identify_discoverable_authentication(&credential)
				.map_err(refused)?;
			let users = app.auth.users()?.ok_or_else(wrong)?;
			let (account, passkey) = users
				.users
				.iter()
				.filter(|account| login::user_handle(&account.name) == handle)
				.find_map(|account| {
					let passkey = account
						.passkeys
						.iter()
						.find(|passkey| passkey.credential.cred_id().as_slice() == id)?;
					Some((account.name.clone(), passkey.credential.clone()))
				})
				.ok_or_else(wrong)?;
			let result = webauthn
				.finish_discoverable_authentication_v2(&credential, state, &passkey)
				.map_err(refused)?;
			(account, result)
		}
		_ => return Err(wrong()),
	};
	let account = app
		.auth
		.users()?
		.and_then(|users| users.get(&user).cloned())
		.filter(|account| {
			account
				.passkeys
				.iter()
				.any(|passkey| passkey.credential.cred_id() == result.cred_id())
		})
		.ok_or_else(wrong)?;
	let credential = passkey_credential(result.cred_id());
	open(&app, &headers, account.name, credential).await
}

async fn logout(
	State(app): AppState,
	Extension(user): Extension<Option<User>>,
	headers: HeaderMap,
) -> Result<Response> {
	enabled(&app)?;
	if let Some(user) = user.as_ref().filter(|user| user.via == Via::Session) {
		csrf(&headers, user)?;
	}
	if let Some(token) = session_cookie(&headers) {
		let hash = violet_core::hash::sha256(token.as_bytes());
		blocking(&app, move |app| {
			app.auth.sessions()?.delete(&hash)?;
			Ok(())
		})
		.await?;
	}
	Ok((
		StatusCode::NO_CONTENT,
		[(SET_COOKIE, login::clear_cookie())],
	)
		.into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PasswordChange {
	#[serde(default)]
	current: Option<String>,
	password: String,
}

async fn change_password(
	State(app): AppState,
	Extension(user): Extension<User>,
	Peer(peer): Peer,
	headers: HeaderMap,
	JsonBody(body): JsonBody<PasswordChange>,
) -> Result<Response> {
	let name = name_of(&user)?.to_string();
	let account = app
		.auth
		.users()?
		.and_then(|users| users.get(&name).cloned())
		.ok_or_else(|| Error::Forbidden("sign in first".into()))?;
	check_password(&name, &body.password)?;
	if let Some(phc) = &account.password {
		let address = client(&app, &headers, peer);
		wait(&app, &name, address)?;
		let Some(current) = body.current else {
			return Err(Error::Invalid("give the current password".into()));
		};
		if !app.auth.login.verify(Some(phc.clone()), current).await? {
			app.auth.login.failed(&name, address);
			return Err(Error::Forbidden("the current password is wrong".into()));
		}
	}
	let phc = app.auth.login.hash(body.password).await?;
	let label = label(&headers);
	let (entry, token) = blocking(&app, move |app| {
		let credential = password_credential(&phc);
		let entry = users::set_password(
			app,
			&user,
			NO_CHECK,
			&name,
			phc,
			account.password.as_deref(),
		)?;
		let Some(session) = &user.session else {
			return Ok((entry, None));
		};
		let sessions = app.auth.sessions()?;
		sessions.delete(&session.hash)?;
		let csrf = Some(session.csrf.clone());
		let (token, _) = login::open_session(sessions, &name, credential, label, csrf)?;
		Ok((entry, Some(token)))
	})
	.await?;
	let mut response = Json(json!({ "entry": entry.id })).into_response();
	if let Some(token) = token {
		response
			.headers_mut()
			.insert(SET_COOKIE, login::set_cookie(&token));
	}
	Ok(response)
}

async fn passkeys(
	State(app): AppState,
	Extension(user): Extension<User>,
) -> Result<Json<Vec<PasskeyView>>> {
	let name = name_of(&user)?;
	Ok(Json(
		app.auth
			.users()?
			.and_then(|users| {
				users
					.get(name)
					.map(|account| account.passkeys.iter().map(PasskeyView::from).collect())
			})
			.unwrap_or_default(),
	))
}

async fn register_start(
	State(app): AppState,
	Extension(user): Extension<User>,
	headers: HeaderMap,
	JsonBody(body): JsonBody<MaybeNamed>,
) -> Result<Json<Value>> {
	let webauthn = webauthn(&app)?;
	let name = name_of(&user)?;
	let account = app
		.auth
		.users()?
		.and_then(|users| users.get(name).cloned())
		.ok_or_else(|| Error::Forbidden("sign in first".into()))?;
	let label = match body.name() {
		Some(label) => {
			valid_label("passkey", label)?;
			if account.passkey(label).is_some() {
				return Err(Error::Conflict(format!("you have a passkey named {label}")));
			}
			label.to_string()
		}
		None => login::free_label(&label(&headers), |taken| account.passkey(taken).is_some()),
	};
	let exclude = account
		.passkeys
		.iter()
		.map(|passkey| passkey.credential.cred_id().clone())
		.collect();
	let (options, state) = webauthn
		.start_passkey_registration(
			login::user_handle(&account.name),
			&account.name,
			&account.name,
			Some(exclude),
		)
		.map_err(|error| Error::Internal(format!("passkey: {error}")))?;
	let session = user
		.session
		.as_ref()
		.map(|session| session.hash.clone())
		.unwrap_or_default();
	let ceremony = app.auth.login.start(Ceremony::Register {
		user: account.name,
		session,
		name: label,
		state,
	})?;
	Ok(Json(json!({
		"ceremony": ceremony,
		"options": login::discoverable(serde_json::to_value(options)?),
	})))
}

async fn register_finish(
	State(app): AppState,
	Extension(user): Extension<User>,
	JsonBody(body): JsonBody<Finish>,
) -> Result<Json<Value>> {
	let webauthn = webauthn(&app)?;
	let me = name_of(&user)?.to_string();
	let mine = user
		.session
		.as_ref()
		.map(|session| session.hash.clone())
		.unwrap_or_default();
	let expired = || Error::Invalid("the passkey request expired: start again".into());
	let Some(Ceremony::Register {
		user: owner,
		session,
		name,
		state,
	}) = app.auth.login.finish(&body.ceremony)
	else {
		return Err(expired());
	};
	if !owner.eq_ignore_ascii_case(&me) || session != mine {
		return Err(expired());
	}
	let credential: RegisterPublicKeyCredential = serde_json::from_value(body.credential)
		.map_err(|_| Error::Invalid("the passkey answer is not a credential".into()))?;
	let passkey = webauthn
		.finish_passkey_registration(&credential, &state)
		.map_err(|error| {
			tracing::info!(%error, "passkey registration refused");
			Error::Invalid("the passkey could not be registered".into())
		})?;
	let stored = StoredPasskey {
		name,
		created: now(),
		credential: passkey,
	};
	let view = PasskeyView::from(&stored);
	let entry = blocking(&app, move |app| {
		users::add_passkey(app, &user, &owner, stored)
	})
	.await?;
	Ok(Json(json!({ "entry": entry.id, "passkey": view })))
}

async fn rename_passkey(
	State(app): AppState,
	Extension(user): Extension<User>,
	Path(name): Path<String>,
	JsonBody(body): JsonBody<Named>,
) -> Result<Json<Value>> {
	let owner = name_of(&user)?.to_string();
	let entry = blocking(&app, move |app| {
		users::rename_passkey(app, &user, &owner, &name, &body.name)
	})
	.await?;
	Ok(Json(json!({ "entry": entry.id })))
}

async fn remove_passkey(
	State(app): AppState,
	Extension(user): Extension<User>,
	Path(name): Path<String>,
) -> Result<Json<Value>> {
	let owner = name_of(&user)?.to_string();
	let entry = blocking(&app, move |app| {
		users::remove_passkey(app, &user, &owner, &name)
	})
	.await?;
	Ok(Json(json!({ "entry": entry.id })))
}

#[derive(Serialize)]
struct SessionView {
	id: String,
	label: String,
	#[serde(with = "time::serde::rfc3339")]
	created: OffsetDateTime,
	#[serde(with = "time::serde::rfc3339")]
	last_seen: OffsetDateTime,
	/// When it ends if it stays idle.
	#[serde(with = "time::serde::rfc3339")]
	expires: OffsetDateTime,
	credential: &'static str,
	current: bool,
}

fn at(seconds: i64) -> OffsetDateTime {
	OffsetDateTime::from_unix_timestamp(seconds).unwrap_or(OffsetDateTime::UNIX_EPOCH)
}

/// The sessions of `user` that can still be used.
fn live_sessions(app: &App, user: &User) -> Result<Vec<Session>> {
	let name = name_of(user)?;
	let users = app.auth.users()?;
	let Some(account) = users.as_deref().and_then(|users| users.get(name)) else {
		return Ok(Vec::new());
	};
	let now = OffsetDateTime::now_utc().unix_timestamp();
	Ok(app
		.auth
		.sessions()?
		.list(name)?
		.into_iter()
		.filter(|session| login::session_alive(session, now) && account.holds(&session.credential))
		.collect())
}

async fn sessions(
	State(app): AppState,
	Extension(user): Extension<User>,
) -> Result<Json<Vec<SessionView>>> {
	blocking(&app, move |app| {
		let current = user.session.as_ref().map(|session| session.hash.clone());
		Ok(Json(
			live_sessions(app, &user)?
				.into_iter()
				.map(|session| SessionView {
					id: session.hash[..SESSION_ID].to_string(),
					current: current.as_deref() == Some(session.hash.as_str()),
					credential: if session.credential.starts_with("passkey:") {
						"passkey"
					} else {
						"password"
					},
					expires: at(session.expires.min(session.last_seen + login::IDLE)),
					label: session.label,
					created: at(session.created),
					last_seen: at(session.last_seen),
				})
				.collect(),
		))
	})
	.await
}

async fn end_session(
	State(app): AppState,
	Extension(user): Extension<User>,
	Path(id): Path<String>,
) -> Result<Response> {
	let current = user.session.as_ref().map(|session| session.hash.clone());
	let ended = blocking(&app, move |app| {
		let found = live_sessions(app, &user)?
			.into_iter()
			.find(|session| id.len() == SESSION_ID && session.hash.starts_with(&id));
		let Some(session) = found else {
			return Err(Error::NotFound(format!("session {id}")));
		};
		app.auth.sessions()?.delete(&session.hash)?;
		Ok(session.hash)
	})
	.await?;
	let mut response = StatusCode::NO_CONTENT.into_response();
	if current.as_deref() == Some(ended.as_str()) {
		response
			.headers_mut()
			.insert(SET_COOKIE, login::clear_cookie());
	}
	Ok(response)
}
