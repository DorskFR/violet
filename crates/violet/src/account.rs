//! `/api/auth/*`: what the login page needs, password and passkey sign-in, setup and logout;
//! `/api/account/*`: one's own password, passkeys and sessions.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use axum::extract::{ConnectInfo, Extension, FromRequestParts, Path, Request, State};
use axum::http::header::{SET_COOKIE, USER_AGENT};
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use kagi::{
	PasskeyRole, Proof, PublicKeyCredential, RegisterPublicKeyCredential, RegistrationChallenge,
	SignInChallenge,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use time::OffsetDateTime;
use violet_core::Session;

use crate::app::blocking;
use crate::auth::{
	LoginMode, Role, Scope, StoredPasskey, User, Via, passkey_credential, password_credential,
	session_cookie, valid_label,
};
use crate::http::{AppState, JsonBody, csrf, from_a_page, writes};
use crate::login::{self, WRONG_PASSKEY, WRONG_PASSWORD, check_password};
use crate::passkeys::{Store, refused, sign_in_refused};
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
		.route("/auth/passkey/autofill", post(passkey_autofill))
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
			signed_in: session.signed_in,
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
			login::open_session(app.auth.sessions()?, &user, credential, label, None, None)?;
		signed_in_as(app, &token, &session)
	})
	.await
}

fn client(app: &App, headers: &HeaderMap, peer: Option<IpAddr>) -> Option<IpAddr> {
	login::client(headers, peer, &app.auth.config().trusted_proxies)
}

fn wait(app: &App, name: Option<&str>, address: Option<IpAddr>) -> Result<()> {
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
	wait(&app, Some(&body.name), address)?;
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
			app.auth.login.failed(Some(&body.name), address);
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetUp {
	name: String,
	token: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetUpFinish {
	ceremony: String,
	credential: Value,
	token: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AddFinish {
	ceremony: String,
	credential: Value,
}

fn creation(credential: Value) -> Result<RegisterPublicKeyCredential> {
	serde_json::from_value(credential)
		.map_err(|_| Error::Invalid("the passkey answer is not a credential".into()))
}

/// Only the implicit admin, while no account can sign in, creates the first admin.
fn may_set_up(app: &App, user: Option<User>) -> Result<User> {
	let Some(user) = user.filter(|user| user.via == Via::Implicit) else {
		return Err(Error::Forbidden(
			"only the implicit admin can set the library up".into(),
		));
	};
	if !app.auth.needs_setup()? {
		return Err(Error::Conflict("the library is set up already".into()));
	}
	Ok(user)
}

/// The setup token a request carries, once it is checked; a wrong one counts as a failed
/// sign-in of its address.
fn setup_token(app: &App, token: &str, address: Option<IpAddr>) -> Result<String> {
	wait(app, None, address)?;
	let token = token.trim();
	if token.is_empty() || !app.auth.is_setup_token(token)? {
		app.auth.login.failed(None, address);
		return Err(Error::Forbidden("the setup token is wrong".into()));
	}
	Ok(token.to_string())
}

async fn setup_start(
	State(app): AppState,
	Extension(user): Extension<Option<User>>,
	Peer(peer): Peer,
	headers: HeaderMap,
	JsonBody(body): JsonBody<SetUp>,
) -> Result<Json<RegistrationChallenge>> {
	enabled(&app)?;
	from_a_page(&headers)?;
	let kagi = app.auth.passkeys()?;
	let address = client(&app, &headers, peer);
	may_set_up(&app, user)?;
	let token = setup_token(&app, &body.token, address)?;
	let name = body.name.trim().to_string();
	crate::auth::valid_name("user", &name)?;
	let name = app
		.auth
		.users()?
		.and_then(|users| users.get(&name).map(|account| account.name.clone()))
		.unwrap_or(name);
	let account = kagi::Account {
		user: name.clone(),
		name: &name,
		display_name: &name,
	};
	let proof = Proof::Grant {
		grant: "setup".into(),
	};
	let store = Store::new(&app);
	Ok(Json(
		kagi.start_registration(&store, account, proof, &token, address)
			.await
			.map_err(refused)?,
	))
}

async fn setup_finish(
	State(app): AppState,
	Extension(user): Extension<Option<User>>,
	Peer(peer): Peer,
	headers: HeaderMap,
	JsonBody(body): JsonBody<SetUpFinish>,
) -> Result<Response> {
	enabled(&app)?;
	from_a_page(&headers)?;
	let kagi = app.auth.passkeys()?;
	let address = client(&app, &headers, peer);
	let user = may_set_up(&app, user)?;
	let token = setup_token(&app, &body.token, address)?;
	let credential = creation(body.credential)?;
	let enrolment = kagi
		.finish_enrolment(&Store::new(&app), &body.ceremony, &token, &credential)
		.await
		.map_err(|error| {
			tracing::info!(%error, "first passkey refused");
			refused(error)
		})?;
	let session = passkey_credential(enrolment.passkey.cred_id());
	let stored = StoredPasskey {
		name: login::free_label(&label(&headers), |_| false),
		created: now(),
		credential: enrolment.passkey,
	};
	let owner = enrolment.user;
	let account = owner.clone();
	blocking(&app, move |app| {
		users::set_up(app, &user, &account, stored)?;
		app.auth.setup_done();
		Ok(())
	})
	.await?;
	open(&app, &headers, owner, session).await
}

fn now() -> OffsetDateTime {
	let now = OffsetDateTime::now_utc();
	now.replace_nanosecond(0).unwrap_or(now)
}

async fn passkey_start(
	State(app): AppState,
	Peer(peer): Peer,
	headers: HeaderMap,
	JsonBody(body): JsonBody<MaybeNamed>,
) -> Result<Json<SignInChallenge>> {
	enabled(&app)?;
	from_a_page(&headers)?;
	let kagi = app.auth.passkeys()?;
	let address = client(&app, &headers, peer);
	let name = body.name();
	wait(&app, name, address)?;
	let challenge = match name {
		None => kagi.start_discoverable(address, false).await,
		Some(name) => {
			let user = app
				.auth
				.users()?
				.and_then(|users| users.get(name).map(|account| account.name.clone()));
			kagi.start_sign_in(&Store::new(&app), name, user.as_ref(), address)
				.await
		}
	};
	Ok(Json(challenge.map_err(refused)?))
}

/// A sign-in offered in the name field's autofill, finished by `passkey_finish`.
async fn passkey_autofill(
	State(app): AppState,
	Peer(peer): Peer,
	headers: HeaderMap,
) -> Result<Json<SignInChallenge>> {
	enabled(&app)?;
	from_a_page(&headers)?;
	let kagi = app.auth.passkeys()?;
	let address = client(&app, &headers, peer);
	wait(&app, None, address)?;
	Ok(Json(
		kagi.start_discoverable(address, true)
			.await
			.map_err(refused)?,
	))
}

async fn passkey_finish(
	State(app): AppState,
	Peer(peer): Peer,
	headers: HeaderMap,
	JsonBody(body): JsonBody<Finish>,
) -> Result<Response> {
	enabled(&app)?;
	from_a_page(&headers)?;
	let kagi = app.auth.passkeys()?;
	let address = client(&app, &headers, peer);
	wait(&app, None, address)?;
	let credential: PublicKeyCredential = serde_json::from_value(body.credential)
		.map_err(|_| Error::Unauthorized(WRONG_PASSKEY.into()))?;
	let signed_in = kagi
		.finish_sign_in(&Store::new(&app), &body.ceremony, &credential)
		.await
		.map_err(|error| sign_in_refused(&app.auth.login, error, address))?;
	let session = passkey_credential(&signed_in.passkey);
	open(&app, &headers, signed_in.user, session).await
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
	let proved = account.password.is_some();
	if let Some(phc) = &account.password {
		let address = client(&app, &headers, peer);
		wait(&app, Some(&name), address)?;
		let Some(current) = body.current else {
			return Err(Error::Invalid("give the current password".into()));
		};
		if !app.auth.login.verify(Some(phc.clone()), current).await? {
			app.auth.login.failed(Some(&name), address);
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
		let signed_in = (!proved).then_some(session.signed_in);
		let (token, _) = login::open_session(sessions, &name, credential, label, csrf, signed_in)?;
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

/// When the session of `user` last signed in, which adding a passkey needs to be recent.
fn recent_sign_in(user: &User) -> Proof {
	let at = user.session.as_ref().map_or(0, |session| session.signed_in);
	Proof::RecentSignIn {
		at: SystemTime::UNIX_EPOCH + Duration::from_secs(u64::try_from(at).unwrap_or(0)),
		human: true,
	}
}

fn session_hash(user: &User) -> Result<&str> {
	user.session
		.as_ref()
		.map(|session| session.hash.as_str())
		.ok_or_else(|| Error::Forbidden("sign in first".into()))
}

/// The name a new passkey gets: the one asked for, else one after this browser.
fn new_label(
	account: &crate::auth::Account,
	asked: Option<&str>,
	headers: &HeaderMap,
) -> Result<String> {
	match asked {
		Some(label) => {
			valid_label("passkey", label)?;
			if account.passkey(label).is_some() {
				return Err(Error::Conflict(format!("you have a passkey named {label}")));
			}
			Ok(label.to_string())
		}
		None => Ok(login::free_label(&label(headers), |taken| {
			account.passkey(taken).is_some()
		})),
	}
}

fn own_account(app: &App, user: &User) -> Result<crate::auth::Account> {
	let name = name_of(user)?;
	app.auth
		.users()?
		.and_then(|users| users.get(name).cloned())
		.ok_or_else(|| Error::Forbidden("sign in first".into()))
}

async fn register_start(
	State(app): AppState,
	Extension(user): Extension<User>,
	Peer(peer): Peer,
	headers: HeaderMap,
	JsonBody(body): JsonBody<MaybeNamed>,
) -> Result<Json<RegistrationChallenge>> {
	let kagi = app.auth.passkeys()?;
	let account = own_account(&app, &user)?;
	let label = new_label(&account, body.name(), &headers)?;
	let address = client(&app, &headers, peer);
	let passkey_account = kagi::Account {
		user: account.name.clone(),
		name: &account.name,
		display_name: &account.name,
	};
	let store = Store::by(&app, &user);
	Ok(Json(
		kagi.start_registration_with(
			&store,
			passkey_account,
			recent_sign_in(&user),
			session_hash(&user)?,
			address,
			&label,
		)
		.await
		.map_err(refused)?,
	))
}

async fn register_finish(
	State(app): AppState,
	Extension(user): Extension<User>,
	JsonBody(body): JsonBody<AddFinish>,
) -> Result<Json<Value>> {
	let kagi = app.auth.passkeys()?;
	let credential = creation(body.credential)?;
	let store = Store::by(&app, &user);
	let registered = kagi
		.finish_carried_registration(&store, &body.ceremony, session_hash(&user)?, &credential)
		.await
		.map_err(|error| {
			tracing::info!(%error, "passkey registration refused");
			refused(error)
		})?;
	let entry = store
		.entry()
		.ok_or_else(|| Error::Internal("the passkey was not recorded".into()))?;
	let view = app
		.auth
		.users()?
		.and_then(|users| {
			users
				.get(&registered.user)?
				.passkeys
				.iter()
				.find(|passkey| {
					passkey.credential.cred_id().as_slice() == registered.passkey.as_slice()
				})
				.map(|passkey| PasskeyView {
					name: passkey.name.clone(),
					created: passkey.created,
				})
		})
		.ok_or_else(|| Error::Internal("the passkey was not recorded".into()))?;
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

/// Removes one's passkey and ends the sessions it opened.
async fn remove_passkey(
	State(app): AppState,
	Extension(user): Extension<User>,
	Path(name): Path<String>,
) -> Result<Json<Value>> {
	let kagi = app.auth.passkeys()?;
	let account = own_account(&app, &user)?;
	let id = account
		.passkey(&name)
		.map(|passkey| passkey.credential.cred_id().to_vec())
		.ok_or_else(|| Error::NotFound(format!("passkey {name}")))?;
	let store = Store::by(&app, &user);
	let removed = kagi
		.remove(
			&store,
			&account.name,
			&id,
			PasskeyRole::Alternative,
			account.password.is_some(),
		)
		.await
		.map_err(refused)?;
	let entry = store
		.entry()
		.ok_or_else(|| Error::Internal("the removal was not recorded".into()))?;
	let opened = passkey_credential(&removed.passkey);
	let owner = account.name;
	blocking(&app, move |app| {
		let sessions = app.auth.sessions()?;
		for session in sessions.list(&owner)? {
			if session.credential == opened {
				sessions.delete(&session.hash)?;
			}
		}
		Ok(())
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
