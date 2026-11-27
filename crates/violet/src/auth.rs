//! Who a request runs as. Users, their credentials and tokens live in `.users.json`, private
//! areas in `.areas.json`, both at the library root and reloaded whenever they change. Without a
//! users file every request runs as one implicit admin, as a single-user library always has.

use std::fmt;
use std::io::ErrorKind;
use std::net::IpAddr;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::SystemTime;

use axum::http::HeaderMap;
use axum::http::header::{AUTHORIZATION, COOKIE};
use kagi::{Kagi, Passkey, Settings, Url};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use violet_core::{Entry, Sessions, Viewer, hash, naming};

use crate::login::{self, Login};
use crate::{Error, Result};

pub const USERS_FILE: &str = ".users.json";
pub const AREAS_FILE: &str = ".areas.json";
pub const SESSIONS_FILE: &str = ".sessions.sqlite";
/// The key decoy passkeys are derived from, so a name without passkeys answers the same across
/// restarts.
pub const DECOY_KEY_FILE: &str = ".passkeys.key";
/// The token that creates the first admin, when `VIOLET_SETUP_TOKEN` is not set.
pub const SETUP_TOKEN_FILE: &str = ".setup-token";
pub const RP_NAME: &str = "violet";
pub const TOKEN_PREFIX: &str = "vt_";
pub const AGENT: &str = "agent";
const USER_ACTOR: &str = "user";
const MAX_NAME: usize = 64;
const PASSWORD_PREFIX: &str = "password:";
const PASSKEY_PREFIX: &str = "passkey:";

/// How far sign-in is turned on; see docs/design/multi-user.md.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LoginMode {
	#[default]
	Off,
	Optional,
	Required,
}

impl FromStr for LoginMode {
	type Err = String;
	fn from_str(value: &str) -> Result<Self, String> {
		match value.trim() {
			"off" => Ok(Self::Off),
			"optional" => Ok(Self::Optional),
			"required" => Ok(Self::Required),
			other => Err(format!(
				"{other} is not a login mode (off, optional, required)"
			)),
		}
	}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
	Member,
	Admin,
}

/// What a request may do: read, change documents, or administer users and settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
	Read,
	Write,
	Admin,
}

impl Role {
	pub fn scope(self) -> Scope {
		match self {
			Self::Member => Scope::Write,
			Self::Admin => Scope::Admin,
		}
	}
}

impl FromStr for Role {
	type Err = String;
	fn from_str(value: &str) -> Result<Self, String> {
		match value {
			"member" => Ok(Self::Member),
			"admin" => Ok(Self::Admin),
			other => Err(format!("{other} is not a role (member, admin)")),
		}
	}
}

impl FromStr for Scope {
	type Err = String;
	fn from_str(value: &str) -> Result<Self, String> {
		match value {
			"read" => Ok(Self::Read),
			"write" => Ok(Self::Write),
			"admin" => Ok(Self::Admin),
			other => Err(format!("{other} is not a scope (read, write, admin)")),
		}
	}
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Token {
	pub name: String,
	/// Hex SHA-256 of the whole secret, `vt_` included.
	pub sha256: String,
	pub scope: Scope,
	#[serde(default, with = "time::serde::rfc3339::option")]
	pub expires: Option<OffsetDateTime>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredPasskey {
	pub name: String,
	#[serde(with = "time::serde::rfc3339")]
	pub created: OffsetDateTime,
	pub credential: Passkey,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Account {
	pub name: String,
	pub role: Role,
	/// An argon2id PHC string.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub password: Option<String>,
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub passkeys: Vec<StoredPasskey>,
	#[serde(default)]
	pub tokens: Vec<Token>,
}

impl Account {
	pub fn has_credential(&self) -> bool {
		self.password.is_some() || !self.passkeys.is_empty()
	}

	pub fn passkey(&self, name: &str) -> Option<&StoredPasskey> {
		self.passkeys
			.iter()
			.find(|passkey| same(&passkey.name, name))
	}

	/// Whether a session opened with `credential` may still be used.
	pub fn holds(&self, credential: &str) -> bool {
		if let Some(hash) = credential.strip_prefix(PASSWORD_PREFIX) {
			return self.password.as_deref().is_some_and(|phc| {
				login::equal(hash::sha256(phc.as_bytes()).as_bytes(), hash.as_bytes())
			});
		}
		credential.strip_prefix(PASSKEY_PREFIX).is_some_and(|id| {
			self.passkeys
				.iter()
				.any(|passkey| login::hex(passkey.credential.cred_id()) == id)
		})
	}
}

/// What a session opened with this password names.
pub fn password_credential(phc: &str) -> String {
	format!("{PASSWORD_PREFIX}{}", hash::sha256(phc.as_bytes()))
}

pub fn passkey_credential(id: &[u8]) -> String {
	format!("{PASSKEY_PREFIX}{}", login::hex(id))
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Users {
	pub users: Vec<Account>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrivateArea {
	pub area: String,
	pub members: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Areas {
	#[serde(default)]
	pub private: Vec<PrivateArea>,
}

fn same(a: &str, b: &str) -> bool {
	a.trim().to_lowercase() == b.trim().to_lowercase()
}

/// A name people choose for a passkey: 1 to 64 characters, no control characters, no space at
/// either end.
pub fn valid_label(what: &str, name: &str) -> Result<()> {
	let ok = !name.is_empty()
		&& name.chars().count() <= MAX_NAME
		&& name.trim() == name
		&& !name.chars().any(char::is_control);
	if ok {
		Ok(())
	} else {
		Err(Error::Invalid(format!(
			"a {what} name must be 1 to {MAX_NAME} characters without spaces at either end"
		)))
	}
}

pub fn valid_name(what: &str, name: &str) -> Result<()> {
	let ok = !name.is_empty()
		&& name.len() <= MAX_NAME
		&& name
			.chars()
			.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
	if ok {
		Ok(())
	} else {
		Err(Error::Invalid(format!(
			"{what} \"{name}\" must be 1 to {MAX_NAME} letters, digits, '.', '_' or '-'"
		)))
	}
}

impl Users {
	pub fn parse(bytes: &[u8]) -> Result<Self> {
		let users: Self = serde_json::from_slice(bytes)
			.map_err(|error| Error::Invalid(format!("{USERS_FILE}: {error}")))?;
		users.validate()?;
		Ok(users)
	}

	pub fn validate(&self) -> Result<()> {
		let mut credentials: Vec<&[u8]> = Vec::new();
		for (position, account) in self.users.iter().enumerate() {
			valid_name("user", &account.name)?;
			if self.users[..position]
				.iter()
				.any(|other| same(&other.name, &account.name))
			{
				return Err(Error::Invalid(format!(
					"user {} is listed twice",
					account.name
				)));
			}
			if let Some(phc) = &account.password
				&& !login::is_argon2id(phc)
			{
				return Err(Error::Invalid(format!(
					"the password of {} is not an argon2id PHC string",
					account.name
				)));
			}
			for (at, passkey) in account.passkeys.iter().enumerate() {
				valid_label("passkey", &passkey.name)?;
				if account.passkeys[..at]
					.iter()
					.any(|other| same(&other.name, &passkey.name))
				{
					return Err(Error::Invalid(format!(
						"user {} has two passkeys named {}",
						account.name, passkey.name
					)));
				}
				let id = passkey.credential.cred_id().as_slice();
				if credentials.contains(&id) {
					return Err(Error::Invalid(format!(
						"passkey {} of {} is registered twice",
						passkey.name, account.name
					)));
				}
				credentials.push(id);
			}
			for (at, token) in account.tokens.iter().enumerate() {
				valid_name("token", &token.name)?;
				if account.tokens[..at]
					.iter()
					.any(|other| other.name == token.name)
				{
					return Err(Error::Invalid(format!(
						"user {} has two tokens named {}",
						account.name, token.name
					)));
				}
				let hex = token.sha256.len() == 64
					&& token
						.sha256
						.chars()
						.all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c));
				if !hex {
					return Err(Error::Invalid(format!(
						"token {} of {} has no valid sha256",
						token.name, account.name
					)));
				}
			}
		}
		Ok(())
	}

	/// Whether some account can sign in with a password or a passkey.
	pub fn any_credential(&self) -> bool {
		self.users.iter().any(Account::has_credential)
	}

	pub fn get(&self, name: &str) -> Option<&Account> {
		self.users.iter().find(|account| same(&account.name, name))
	}

	pub fn get_mut(&mut self, name: &str) -> Option<&mut Account> {
		self.users
			.iter_mut()
			.find(|account| same(&account.name, name))
	}

	fn by_token(&self, secret: &str) -> Option<(&Account, &Token)> {
		let sha256 = hash::sha256(secret.as_bytes());
		self.users.iter().find_map(|account| {
			let token = account
				.tokens
				.iter()
				.find(|token| login::equal(token.sha256.as_bytes(), sha256.as_bytes()))?;
			Some((account, token))
		})
	}
}

impl Areas {
	pub fn parse(bytes: &[u8]) -> Result<Self> {
		let areas: Self = serde_json::from_slice(bytes)
			.map_err(|error| Error::Invalid(format!("{AREAS_FILE}: {error}")))?;
		areas.validate()?;
		Ok(areas)
	}

	pub fn validate(&self) -> Result<()> {
		for (position, private) in self.private.iter().enumerate() {
			let area = private.area.as_str();
			if area.trim().is_empty() || naming::sanitize(area) != area {
				return Err(Error::Invalid(format!("\"{area}\" cannot be an area")));
			}
			if self.private[..position]
				.iter()
				.any(|other| same(&other.area, area))
			{
				return Err(Error::Invalid(format!("area {area} is listed twice")));
			}
			for member in &private.members {
				valid_name("member", member)?;
			}
		}
		Ok(())
	}

	/// The members of `area` when it is private.
	pub fn members(&self, area: &str) -> Option<&[String]> {
		self.private
			.iter()
			.find(|private| same(&private.area, area))
			.map(|private| private.members.as_slice())
	}

	/// What `name` cannot see; `None` is nobody in particular, who sees no private area.
	fn viewer(&self, name: Option<&str>) -> Viewer {
		Viewer::hiding(
			self.private
				.iter()
				.filter(|private| {
					!name
						.is_some_and(|name| private.members.iter().any(|member| same(member, name)))
				})
				.map(|private| private.area.clone()),
		)
	}
}

/// An address or a CIDR block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Network {
	addr: IpAddr,
	prefix: u8,
}

impl FromStr for Network {
	type Err = String;
	fn from_str(value: &str) -> Result<Self, String> {
		let value = value.trim();
		let (addr, prefix) = match value.split_once('/') {
			Some((addr, prefix)) => (addr, Some(prefix)),
			None => (value, None),
		};
		let addr: IpAddr = addr
			.parse()
			.map_err(|_| format!("{value} is not an address or a CIDR block"))?;
		let addr = addr.to_canonical();
		let max = if addr.is_ipv4() { 32 } else { 128 };
		let prefix = match prefix {
			Some(prefix) => prefix
				.parse::<u8>()
				.ok()
				.filter(|prefix| *prefix <= max)
				.ok_or_else(|| format!("{value} has no valid prefix length"))?,
			None => max,
		};
		Ok(Self { addr, prefix })
	}
}

impl Network {
	pub fn contains(&self, ip: IpAddr) -> bool {
		let bits = |ip: IpAddr| -> (u128, u8) {
			match ip.to_canonical() {
				IpAddr::V4(v4) => (u128::from(u32::from(v4)) << 96, 32),
				IpAddr::V6(v6) => (u128::from(v6), 128),
			}
		};
		let ((want, width), (have, other)) = (bits(self.addr), bits(ip));
		if width != other {
			return false;
		}
		let mask = match self.prefix {
			0 => 0,
			prefix => u128::MAX << (128 - u32::from(prefix)),
		};
		want & mask == have & mask
	}
}

/// How a request's user was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Via {
	/// Nobody signed in, and the login mode lets that run as the one implicit admin.
	Implicit,
	Session,
	Token,
	/// The command line, trusted because it can read the library anyway.
	Local,
}

/// The session a request was signed in with.
#[derive(Debug, Clone)]
pub struct SessionRef {
	/// SHA-256 of the session token.
	pub hash: String,
	pub csrf: String,
	pub credential: String,
	/// Unix seconds of its user's last sign-in.
	pub signed_in: i64,
}

/// The user a request or a command runs as.
#[derive(Debug, Clone)]
pub struct User {
	/// None for the implicit user, and for the command line without `VIOLET_USER` or `USER`.
	pub name: Option<String>,
	pub role: Role,
	pub scope: Scope,
	pub viewer: Viewer,
	pub via: Via,
	pub session: Option<SessionRef>,
}

/// Who an entry is recorded as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Actor {
	pub actor: String,
	pub on_behalf_of: Option<String>,
}

impl Actor {
	/// An automatic step for `user`.
	pub fn agent(user: Option<&str>) -> Self {
		Self {
			actor: AGENT.into(),
			on_behalf_of: user.map(str::to_string),
		}
	}
}

impl From<&str> for Actor {
	fn from(actor: &str) -> Self {
		Self {
			actor: actor.to_string(),
			on_behalf_of: None,
		}
	}
}

impl From<&Actor> for Actor {
	fn from(actor: &Actor) -> Self {
		actor.clone()
	}
}

impl fmt::Display for Actor {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.write_str(&self.actor)
	}
}

impl User {
	pub fn implicit() -> Self {
		Self {
			name: None,
			role: Role::Admin,
			scope: Scope::Admin,
			viewer: Viewer::everything(),
			via: Via::Implicit,
			session: None,
		}
	}

	pub fn actor(&self) -> Actor {
		Actor {
			actor: match &self.name {
				Some(name) => format!("{USER_ACTOR}:{name}"),
				None => USER_ACTOR.into(),
			},
			on_behalf_of: None,
		}
	}

	/// An automatic step taken for this user, like reading a document again.
	pub fn agent(&self) -> Actor {
		Actor::agent(self.name.as_deref())
	}

	pub fn is_admin(&self) -> bool {
		self.role == Role::Admin && self.scope >= Scope::Admin
	}

	pub fn can_write(&self) -> bool {
		self.scope >= Scope::Write
	}
}

/// Who an entry belongs to: `user:<name>`, or the agent's `on_behalf_of`; nobody in particular
/// (`None`) for `user` and an agent entry without one.
pub fn entry_owner(entry: &Entry) -> Option<&str> {
	match entry.actor.strip_prefix(USER_ACTOR) {
		Some(rest) => rest.strip_prefix(':').filter(|name| !name.is_empty()),
		None => entry.on_behalf_of.as_deref(),
	}
}

/// How requests sign in, from the environment.
#[derive(Debug, Clone, Default)]
pub struct AuthConfig {
	pub mode: LoginMode,
	/// The address people reach violet at; the WebAuthn relying party. Set unless the mode is
	/// off.
	pub public_url: Option<Url>,
	/// Proxies whose `X-Forwarded-For` names the client, for rate limiting.
	pub trusted_proxies: Vec<Network>,
	/// Who the command line runs as.
	pub local_user: Option<String>,
	/// What creates the first admin; generated into `.setup-token` when not set.
	pub setup_token: Option<String>,
}

impl AuthConfig {
	/// Refuses a mode without the public URL it needs, and a URL that cannot be a relying
	/// party.
	pub fn validate(&self) -> Result<()> {
		match (&self.public_url, self.mode) {
			(None, LoginMode::Off) => Ok(()),
			(None, _) => Err(Error::Invalid(
				"VIOLET_PUBLIC_URL must be set when VIOLET_LOGIN is not off".into(),
			)),
			(Some(url), _) => kagi::relying_party(url, None, RP_NAME)
				.map(|_| ())
				.map_err(|error| Error::Invalid(format!("VIOLET_PUBLIC_URL: {error}"))),
		}?;
		if self
			.setup_token
			.as_deref()
			.is_some_and(|token| token.len() < 16)
		{
			return Err(Error::Invalid(
				"VIOLET_SETUP_TOKEN must be at least 16 characters".into(),
			));
		}
		Ok(())
	}
}

/// Writes a secret file only its owner can read, unless it exists.
fn create_secret(path: &Path, contents: &str) -> std::io::Result<()> {
	use std::io::Write;
	use std::os::unix::fs::OpenOptionsExt;
	let mut file = std::fs::OpenOptions::new()
		.write(true)
		.create_new(true)
		.mode(0o600)
		.open(path)?;
	file.write_all(contents.as_bytes())?;
	file.sync_all()
}

fn read_secret(path: &Path) -> std::io::Result<Option<String>> {
	match std::fs::read_to_string(path) {
		Ok(text) if login::is_token(text.trim()) => Ok(Some(text.trim().to_string())),
		Ok(_) => Err(std::io::Error::new(
			ErrorKind::InvalidData,
			format!("{} is not 64 hex digits", path.display()),
		)),
		Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
		Err(error) => Err(error),
	}
}

/// The random secret in `path`, created when there is none.
fn secret_file(path: &Path) -> std::io::Result<String> {
	if let Some(secret) = read_secret(path)? {
		return Ok(secret);
	}
	let secret = login::random_hex();
	match create_secret(path, &format!("{secret}\n")) {
		Ok(()) => Ok(secret),
		Err(error) if error.kind() == ErrorKind::AlreadyExists => read_secret(path)?
			.ok_or_else(|| std::io::Error::new(ErrorKind::NotFound, "the secret vanished")),
		Err(error) => Err(error),
	}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stamp {
	modified: Option<SystemTime>,
	len: u64,
	inode: u64,
}

type Loaded<T> = Result<Option<Arc<T>>, String>;

/// A settings file, parsed again when its size, time or inode changes.
struct Cached<T> {
	path: PathBuf,
	parse: fn(&[u8]) -> Result<T>,
	state: Mutex<Option<(Option<Stamp>, Loaded<T>)>>,
}

impl<T> Cached<T> {
	fn new(path: PathBuf, parse: fn(&[u8]) -> Result<T>) -> Self {
		Self {
			path,
			parse,
			state: Mutex::new(None),
		}
	}

	fn get(&self) -> Result<Option<Arc<T>>> {
		let stamp = match std::fs::metadata(&self.path) {
			Ok(metadata) => Some(Stamp {
				modified: metadata.modified().ok(),
				len: metadata.len(),
				inode: metadata.ino(),
			}),
			Err(error) if error.kind() == ErrorKind::NotFound => None,
			Err(error) => return Err(Error::Internal(format!("{}: {error}", self.path.display()))),
		};
		let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
		if let Some((known, loaded)) = state.as_ref()
			&& *known == stamp
		{
			return loaded.clone().map_err(Error::Internal);
		}
		let loaded: Loaded<T> = match stamp {
			None => Ok(None),
			Some(_) => match std::fs::read(&self.path) {
				Ok(bytes) => (self.parse)(&bytes)
					.map(|parsed| Some(Arc::new(parsed)))
					.map_err(|error| error.to_string()),
				Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
				Err(error) => Err(format!("{}: {error}", self.path.display())),
			},
		};
		if let Err(error) = &loaded {
			tracing::error!(file = %self.path.display(), error, "settings file refused");
		}
		*state = Some((stamp, loaded.clone()));
		loaded.map_err(Error::Internal)
	}
}

pub struct Auth {
	root: PathBuf,
	config: OnceLock<AuthConfig>,
	users: Cached<Users>,
	areas: Cached<Areas>,
	sessions: OnceLock<Result<Sessions, String>>,
	passkeys: OnceLock<Result<Kagi<String>, String>>,
	setup_token: Mutex<Option<String>>,
	pub login: Login,
}

/// The session token a request carries in its cookie.
pub fn session_cookie(headers: &HeaderMap) -> Option<&str> {
	headers
		.get_all(COOKIE)
		.iter()
		.filter_map(|value| value.to_str().ok())
		.flat_map(|value| value.split(';'))
		.filter_map(|pair| pair.trim().split_once('='))
		.find(|(name, _)| *name == login::COOKIE)
		.map(|(_, value)| value.trim())
		.filter(|value| login::is_token(value))
}

impl Auth {
	pub fn new(root: &Path) -> Self {
		Self {
			root: root.to_path_buf(),
			config: OnceLock::new(),
			users: Cached::new(root.join(USERS_FILE), Users::parse),
			areas: Cached::new(root.join(AREAS_FILE), Areas::parse),
			sessions: OnceLock::new(),
			passkeys: OnceLock::new(),
			setup_token: Mutex::new(None),
			login: Login::default(),
		}
	}

	/// Sets how requests sign in; once, before serving.
	pub fn configure(&self, config: AuthConfig) -> Result<()> {
		config.validate()?;
		self.config
			.set(config)
			.map_err(|_| Error::Internal("sign-in is configured already".into()))
	}

	pub fn config(&self) -> &AuthConfig {
		self.config.get_or_init(AuthConfig::default)
	}

	pub fn mode(&self) -> LoginMode {
		self.config().mode
	}

	/// The passkey relying party, made on first use; refused when the mode is off.
	pub fn passkeys(&self) -> Result<&Kagi<String>> {
		let config = self.config();
		let Some(url) = config
			.public_url
			.as_ref()
			.filter(|_| config.mode != LoginMode::Off)
		else {
			return Err(Error::NotFound("passkeys are off".into()));
		};
		self.passkeys
			.get_or_init(|| {
				let key = secret_file(&self.root.join(DECOY_KEY_FILE))
					.map_err(|error| format!("{DECOY_KEY_FILE}: {error}"))?;
				Kagi::new(
					Settings::new(url, RP_NAME, key.as_bytes()),
					kagi::MemoryCeremonies::default(),
				)
				.map_err(|error| error.to_string())
			})
			.as_ref()
			.map_err(|error| Error::Internal(format!("passkeys: {error}")))
	}

	/// The token that creates the first admin. Without `VIOLET_SETUP_TOKEN` it lives in
	/// `.setup-token`, written on first use and never logged.
	pub fn setup_token(&self) -> Result<String> {
		if let Some(token) = &self.config().setup_token {
			return Ok(token.clone());
		}
		let mut known = self
			.setup_token
			.lock()
			.unwrap_or_else(PoisonError::into_inner);
		if let Some(token) = known.as_ref() {
			return Ok(token.clone());
		}
		let path = self.root.join(SETUP_TOKEN_FILE);
		let token = secret_file(&path)
			.map_err(|error| Error::Internal(format!("{SETUP_TOKEN_FILE}: {error}")))?;
		tracing::warn!(
			path = %path.display(),
			"no account can sign in yet: create the first admin with the setup token in this file"
		);
		*known = Some(token.clone());
		Ok(token)
	}

	/// Whether `given` is the setup token.
	pub fn is_setup_token(&self, given: &str) -> Result<bool> {
		let expected = self.setup_token()?;
		Ok(login::equal(
			hash::sha256(given.trim().as_bytes()).as_bytes(),
			hash::sha256(expected.as_bytes()).as_bytes(),
		))
	}

	/// Forgets a generated setup token once the first admin exists.
	pub fn setup_done(&self) {
		if self.config().setup_token.is_some() {
			return;
		}
		*self
			.setup_token
			.lock()
			.unwrap_or_else(PoisonError::into_inner) = None;
		let path = self.root.join(SETUP_TOKEN_FILE);
		if let Err(error) = std::fs::remove_file(&path)
			&& error.kind() != ErrorKind::NotFound
		{
			tracing::warn!(path = %path.display(), %error, "the used setup token stays");
		}
	}

	/// Whether the first admin still has to be created.
	pub fn needs_setup(&self) -> Result<bool> {
		Ok(self.mode() != LoginMode::Off
			&& !self.users()?.is_some_and(|users| users.any_credential()))
	}

	/// The session store, opened on first use.
	pub fn sessions(&self) -> Result<&Sessions> {
		self.sessions
			.get_or_init(|| {
				Sessions::open(&self.root.join(SESSIONS_FILE)).map_err(|error| error.to_string())
			})
			.as_ref()
			.map_err(|error| Error::Internal(format!("{SESSIONS_FILE}: {error}")))
	}

	/// The users file, none when there is no such file; an error when it cannot be used, so
	/// that a broken file never opens the library to everyone.
	pub fn users(&self) -> Result<Option<Arc<Users>>> {
		self.users.get()
	}

	pub fn areas(&self) -> Result<Option<Arc<Areas>>> {
		self.areas.get()
	}

	/// What a named user, or nobody in particular, may see.
	pub fn viewer(&self, name: Option<&str>) -> Result<Viewer> {
		Ok(match self.areas()? {
			Some(areas) => areas.viewer(name),
			None => Viewer::everything(),
		})
	}

	fn account(
		&self,
		account: &Account,
		scope: Scope,
		via: Via,
		session: Option<SessionRef>,
	) -> Result<User> {
		Ok(User {
			name: Some(account.name.clone()),
			role: account.role,
			scope: scope.min(account.role.scope()),
			viewer: self.viewer(Some(&account.name))?,
			via,
			session,
		})
	}

	/// The user a request runs as: its session, else its bearer token, else what the login
	/// mode says about nobody signed in.
	pub fn resolve(&self, headers: &HeaderMap) -> Result<User> {
		match self.users()? {
			Some(users) => self.resolve_in(&users, headers, true),
			None => Ok(User::implicit()),
		}
	}

	/// `resolve` against a users file that may not be written yet. `live` lets it end the
	/// session it finds unusable and note its activity.
	pub fn resolve_in(&self, users: &Users, headers: &HeaderMap, live: bool) -> Result<User> {
		let mode = self.mode();
		if mode != LoginMode::Off
			&& let Some(token) = session_cookie(headers)
			&& let Some(user) = self.session_user(users, token, live)?
		{
			return Ok(user);
		}
		if let Some(value) = headers.get(AUTHORIZATION) {
			let value = String::from_utf8_lossy(value.as_bytes());
			if let Some((scheme, secret)) = value.trim().split_once(' ')
				&& scheme.eq_ignore_ascii_case("bearer")
			{
				let secret = secret.trim();
				let Some((account, token)) = secret
					.starts_with(TOKEN_PREFIX)
					.then(|| users.by_token(secret))
					.flatten()
				else {
					return Err(Error::Unauthorized("the token is not valid".into()));
				};
				if token
					.expires
					.is_some_and(|expires| expires <= OffsetDateTime::now_utc())
				{
					return Err(Error::Unauthorized(format!(
						"the token {} expired",
						token.name
					)));
				}
				return self.account(account, token.scope, Via::Token, None);
			}
		}
		match mode {
			LoginMode::Off => Err(Error::Unauthorized("sign in with a token".into())),
			LoginMode::Optional => Ok(User::implicit()),
			LoginMode::Required if !users.any_credential() => Ok(User::implicit()),
			LoginMode::Required => Err(Error::Unauthorized("sign in first".into())),
		}
	}

	fn session_user(&self, users: &Users, token: &str, live: bool) -> Result<Option<User>> {
		let sessions = self.sessions()?;
		let hash = hash::sha256(token.as_bytes());
		let Some(session) = sessions.get(&hash)? else {
			return Ok(None);
		};
		let now = OffsetDateTime::now_utc().unix_timestamp();
		let account = users
			.get(&session.user)
			.filter(|account| account.holds(&session.credential));
		let Some(account) = account.filter(|_| login::session_alive(&session, now)) else {
			if live {
				sessions.delete(&hash)?;
			}
			return Ok(None);
		};
		if live && now - session.last_seen >= login::TOUCH_EVERY {
			sessions.touch(&hash, now)?;
		}
		let session = SessionRef {
			hash,
			csrf: session.csrf,
			credential: session.credential,
			signed_in: session.signed_in,
		};
		self.account(account, Scope::Admin, Via::Session, Some(session))
			.map(Some)
	}

	/// Who the command line runs as: the implicit user without a users file, else
	/// `VIOLET_USER` (or the OS user) as an admin that sees every area.
	pub fn local(&self) -> Result<User> {
		if self.users()?.is_none() {
			return Ok(User::implicit());
		}
		Ok(User {
			name: self.config().local_user.clone(),
			via: Via::Local,
			..User::implicit()
		})
	}

	/// Who a file found in the inbox is filed for, and what they may see: its uploader, else
	/// nobody in particular (the implicit user without a users file).
	pub fn uploader(&self, uploader: Option<&str>) -> Result<(Option<String>, Viewer)> {
		if uploader.is_none() && self.users()?.is_none() {
			return Ok((None, Viewer::everything()));
		}
		Ok((uploader.map(str::to_string), self.viewer(uploader)?))
	}
}

pub fn new_token() -> (String, String) {
	let secret = format!("{TOKEN_PREFIX}{}", login::random_hex());
	let sha256 = hash::sha256(secret.as_bytes());
	(secret, sha256)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn networks_match_addresses_and_blocks() {
		let block: Network = "10.0.0.0/8".parse().unwrap();
		assert!(block.contains("10.1.2.3".parse().unwrap()));
		assert!(block.contains("::ffff:10.1.2.3".parse().unwrap()));
		assert!(!block.contains("11.0.0.1".parse().unwrap()));
		let one: Network = "192.168.1.5".parse().unwrap();
		assert!(one.contains("192.168.1.5".parse().unwrap()));
		assert!(!one.contains("192.168.1.6".parse().unwrap()));
		let v6: Network = "fd00::/8".parse().unwrap();
		assert!(v6.contains("fd12::1".parse().unwrap()));
		assert!(!v6.contains("10.0.0.1".parse().unwrap()));
		let any: Network = "0.0.0.0/0".parse().unwrap();
		assert!(any.contains("8.8.8.8".parse().unwrap()));
		for bad in ["10.0.0.0/33", "nope", "10.0.0.0/x"] {
			assert!(bad.parse::<Network>().is_err(), "{bad}");
		}
	}

	fn passkey(name: &str, id: &str) -> String {
		format!(
			r#"{{"name": "{name}", "created": "2026-11-19T10:00:00Z", "credential": {{"cred": {{
				"cred_id": "{id}",
				"cred": {{"type_": "ES256", "key": {{"EC_EC2": {{"curve": "SECP256R1",
					"x": "axfR8uEsQkf4vOblY6RA8ncDfYEt6zOg9KE5RdiYwpY",
					"y": "T-NC4v4af5uO5-tKfA-eFivOM1drMV7Oy7ZAaDe_UfU"}}}}}},
				"counter": 0, "transports": null, "user_verified": true, "backup_eligible": false,
				"backup_state": false, "registration_policy": "required", "extensions": {{}},
				"attestation": {{"data": "None", "metadata": "None"}},
				"attestation_format": "none"}}}}}}"#
		)
	}

	#[test]
	fn users_files_are_validated_strictly() {
		let phc = login::hash_now("correct horse battery").unwrap();
		let good = format!(
			r#"{{"users": [{{"name": "alex", "role": "admin", "password": "{phc}",
			"passkeys": [{}, {}],
			"tokens": [{{"name": "scanner", "sha256": "0000000000000000000000000000000000000000000000000000000000000000", "scope": "write", "expires": null}}]}},
			{{"name": "sam", "role": "member"}}]}}"#,
			passkey("phone", "AQEB"),
			passkey("Alex's laptop", "AgIC"),
		);
		let users = Users::parse(good.as_bytes()).unwrap();
		assert_eq!(users.get("SAM").unwrap().role, Role::Member);
		let alex = users.get("alex").unwrap();
		assert!(alex.holds(&password_credential(&phc)));
		assert!(!alex.holds(&password_credential("$argon2id$other")));
		assert!(alex.holds(&passkey_credential(&[1, 1, 1])));
		assert!(!alex.holds(&passkey_credential(&[3, 3, 3])));
		assert!(!alex.holds("token:x"));
		assert!(users.any_credential());
		assert!(!users.get("sam").unwrap().has_credential());
		let written = serde_json::to_string(&users).unwrap();
		assert_eq!(Users::parse(written.as_bytes()).unwrap(), users);
		assert!(
			written.contains(r#"{"name":"sam","role":"member","tokens":[]}"#),
			"{written}"
		);

		let with = |account: &str| format!(r#"{{"users": [{account}]}}"#);
		for bad in [
			with(r#"{"name": "alex", "role": "owner"}"#),
			with(r#"{"name": "a b", "role": "admin"}"#),
			r#"{"users": [{"name": "a", "role": "admin"}, {"name": "A", "role": "member"}]}"#
				.to_string(),
			with(r#"{"name": "a", "role": "admin", "identities": ["CN=a"]}"#),
			with(r#"{"name": "a", "role": "admin", "identities": []}"#),
			with(r#"{"name": "a", "role": "admin", "password": "hunter2hunter2"}"#),
			with(
				r#"{"name": "a", "role": "admin", "password": "$argon2i$v=19$m=8,t=1,p=1$c2FsdHNhbHQ$aGFzaA"}"#,
			),
			with(&format!(
				r#"{{"name": "a", "role": "admin", "passkeys": [{}, {}]}}"#,
				passkey("phone", "AQEB"),
				passkey("Phone", "AgIC")
			)),
			with(&format!(
				r#"{{"name": "a", "role": "admin", "passkeys": [{}, {}]}}"#,
				passkey("phone", "AQEB"),
				passkey("laptop", "AQEB")
			)),
			with(&format!(
				r#"{{"name": "a", "role": "admin", "passkeys": [{}]}}"#,
				passkey(" phone", "AQEB")
			)),
			with(
				r#"{"name": "a", "role": "admin", "tokens": [{"name": "t", "sha256": "abc", "scope": "read"}]}"#,
			),
			r#"{"users": [], "extra": 1}"#.to_string(),
			"not json".to_string(),
		] {
			assert!(Users::parse(bad.as_bytes()).is_err(), "{bad}");
		}
		let areas =
			Areas::parse(br#"{"private": [{"area": "Health", "members": ["sam"]}]}"#).unwrap();
		assert_eq!(areas.members("health").unwrap(), ["sam"]);
		assert!(areas.viewer(Some("Sam")).hidden().is_empty());
		assert_eq!(areas.viewer(Some("alex")).hidden(), ["Health"]);
		assert_eq!(areas.viewer(None).hidden(), ["Health"]);
		for bad in [
			&br#"{"private": [{"area": "a/b", "members": []}]}"#[..],
			br#"{"private": [{"area": "X", "members": []}, {"area": "x", "members": []}]}"#,
			br#"{"private": [{"area": "X", "members": ["no one"]}]}"#,
		] {
			assert!(
				Areas::parse(bad).is_err(),
				"{}",
				String::from_utf8_lossy(bad)
			);
		}
	}

	#[test]
	fn owners_of_entries() {
		let entry = |actor: &str, on_behalf_of: Option<&str>| Entry {
			id: uuid::Uuid::new_v4(),
			at: OffsetDateTime::UNIX_EPOCH,
			actor: actor.into(),
			on_behalf_of: on_behalf_of.map(str::to_string),
			action: violet_core::Action::Undo {
				entry: uuid::Uuid::new_v4(),
			},
			summary: String::new(),
		};
		assert_eq!(entry_owner(&entry("user", None)), None);
		assert_eq!(entry_owner(&entry("user:sam", None)), Some("sam"));
		assert_eq!(entry_owner(&entry("agent", Some("sam"))), Some("sam"));
		assert_eq!(entry_owner(&entry("agent", None)), None);
		assert_eq!(User::implicit().actor().actor, "user");
		assert_eq!(User::implicit().agent(), Actor::agent(None));
	}
}
