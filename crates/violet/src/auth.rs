//! Who a request runs as. Users and their tokens live in `.users.json`, private areas in
//! `.areas.json`, both at the library root and reloaded whenever they change. Without a users
//! file every request runs as one implicit admin, as a single-user library always has.

use std::fmt;
use std::io::ErrorKind;
use std::net::IpAddr;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::SystemTime;

use axum::http::HeaderMap;
use axum::http::header::AUTHORIZATION;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use violet_core::{Entry, Viewer, hash, naming};

use crate::{Error, Result};

pub const USERS_FILE: &str = ".users.json";
pub const AREAS_FILE: &str = ".areas.json";
pub const TOKEN_PREFIX: &str = "vt_";
pub const AGENT: &str = "agent";
const USER_ACTOR: &str = "user";
const MAX_NAME: usize = 64;

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
	fn scope(self) -> Scope {
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Account {
	pub name: String,
	pub role: Role,
	/// Client certificate subjects, CNs (`CN=…`), emails or URIs that sign in as this user.
	#[serde(default)]
	pub identities: Vec<String>,
	#[serde(default)]
	pub tokens: Vec<Token>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
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
		let mut identities: Vec<&str> = Vec::new();
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
			for identity in &account.identities {
				if identity.trim().is_empty() {
					return Err(Error::Invalid(format!(
						"user {} has an empty identity",
						account.name
					)));
				}
				if identities.iter().any(|known| same(known, identity)) {
					return Err(Error::Invalid(format!(
						"identity {identity} belongs to more than one user"
					)));
				}
				identities.push(identity);
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

	pub fn get(&self, name: &str) -> Option<&Account> {
		self.users.iter().find(|account| same(&account.name, name))
	}

	pub fn get_mut(&mut self, name: &str) -> Option<&mut Account> {
		self.users
			.iter_mut()
			.find(|account| same(&account.name, name))
	}

	fn by_identity(&self, identities: &[String]) -> Option<&Account> {
		self.users.iter().find(|account| {
			account
				.identities
				.iter()
				.any(|known| identities.iter().any(|seen| same(known, seen)))
		})
	}

	fn by_token(&self, secret: &str) -> Option<(&Account, &Token)> {
		let sha256 = hash::sha256(secret.as_bytes());
		self.users.iter().find_map(|account| {
			let token = account.tokens.iter().find(|token| token.sha256 == sha256)?;
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

/// Splits on `separator` outside double quotes.
fn split_unquoted(value: &str, separator: char) -> Vec<&str> {
	let mut parts = Vec::new();
	let (mut start, mut quoted, mut escaped) = (0, false, false);
	for (at, c) in value.char_indices() {
		match c {
			_ if escaped => escaped = false,
			'\\' => escaped = true,
			'"' => quoted = !quoted,
			c if c == separator && !quoted => {
				parts.push(&value[start..at]);
				start = at + c.len_utf8();
			}
			_ => {}
		}
	}
	parts.push(&value[start..]);
	parts
}

fn unquote(value: &str) -> String {
	let value = value.trim();
	let Some(inner) = value
		.strip_prefix('"')
		.and_then(|value| value.strip_suffix('"'))
	else {
		return value.to_string();
	};
	let mut out = String::with_capacity(inner.len());
	let mut chars = inner.chars();
	while let Some(c) = chars.next() {
		match (c, chars.clone().next()) {
			('\\', Some(next @ ('"' | '\\'))) => {
				out.push(next);
				chars.next();
			}
			_ => out.push(c),
		}
	}
	out
}

/// The full subject, each `KEY=value` part of it, and its email.
fn subject_identities(subject: &str, into: &mut Vec<String>) {
	let subject = subject.trim();
	if subject.is_empty() {
		return;
	}
	into.push(subject.to_string());
	let mut part = String::new();
	let mut parts = Vec::new();
	let mut chars = subject.chars();
	while let Some(c) = chars.next() {
		match c {
			'\\' => part.extend(chars.next()),
			',' => parts.push(std::mem::take(&mut part)),
			c => part.push(c),
		}
	}
	parts.push(part);
	for part in parts {
		let part = part.trim();
		let Some((key, value)) = part.split_once('=') else {
			continue;
		};
		into.push(format!("{}={}", key.trim(), value.trim()));
		if key.trim().eq_ignore_ascii_case("emailAddress") || key.trim().eq_ignore_ascii_case("E") {
			into.push(value.trim().to_string());
		}
	}
}

/// The identities a client certificate header names: Envoy's `x-forwarded-client-cert`
/// (`By=…;Hash=…;Subject="CN=…";URI=…`, the element the nearest proxy added is the last), or
/// a plain subject.
pub fn client_identities(value: &str) -> Vec<String> {
	let mut identities = Vec::new();
	let xfcc = ["subject=", "uri=", "hash=", "by=", "dns="]
		.iter()
		.any(|key| value.to_ascii_lowercase().contains(key));
	if !xfcc {
		subject_identities(value, &mut identities);
		return identities;
	}
	let Some(element) = split_unquoted(value, ',')
		.into_iter()
		.rfind(|element| !element.trim().is_empty())
	else {
		return identities;
	};
	for pair in split_unquoted(element, ';') {
		let Some((key, value)) = pair.split_once('=') else {
			continue;
		};
		let value = unquote(value);
		match key.trim().to_ascii_lowercase().as_str() {
			"subject" => subject_identities(&value, &mut identities),
			"uri" | "dns" if !value.trim().is_empty() => identities.push(value.trim().to_string()),
			_ => {}
		}
	}
	identities
}

/// How a request's user was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Via {
	/// No users file: the one implicit user of a single-user library.
	Implicit,
	Certificate,
	Token,
	/// `VIOLET_DEFAULT_USER`.
	Default,
	/// The command line, trusted because it can read the library anyway.
	Local,
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

/// Who an entry belongs to: `user:<name>`, or the agent's `on_behalf_of`; the default user
/// (`None` here) for `user` and an agent entry without one.
pub fn entry_owner(entry: &Entry) -> Option<&str> {
	match entry.actor.strip_prefix(USER_ACTOR) {
		Some(rest) => rest.strip_prefix(':').filter(|name| !name.is_empty()),
		None => entry.on_behalf_of.as_deref(),
	}
}

/// Where identities come from, from the environment.
#[derive(Debug, Clone, Default)]
pub struct AuthConfig {
	/// The header a trusted proxy puts the verified client certificate in.
	pub identity_header: Option<String>,
	pub trusted_proxies: Vec<Network>,
	/// Who a request without a certificate or a token runs as.
	pub default_user: Option<String>,
	/// Who the command line runs as.
	pub local_user: Option<String>,
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
	config: OnceLock<AuthConfig>,
	users: Cached<Users>,
	areas: Cached<Areas>,
}

impl Auth {
	pub fn new(root: &Path) -> Self {
		Self {
			config: OnceLock::new(),
			users: Cached::new(root.join(USERS_FILE), Users::parse),
			areas: Cached::new(root.join(AREAS_FILE), Areas::parse),
		}
	}

	/// Sets where identities come from; once, before serving.
	pub fn configure(&self, config: AuthConfig) -> Result<(), AuthConfig> {
		self.config.set(config)
	}

	pub fn config(&self) -> &AuthConfig {
		self.config.get_or_init(AuthConfig::default)
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

	fn account(&self, account: &Account, scope: Scope, via: Via) -> Result<User> {
		Ok(User {
			name: Some(account.name.clone()),
			role: account.role,
			scope: scope.min(account.role.scope()),
			viewer: self.viewer(Some(&account.name))?,
			via,
		})
	}

	/// The user a request runs as: a client certificate passed on by a trusted proxy, a bearer
	/// token, else the default user. Without a users file, the implicit user (or the default
	/// user, as an admin).
	pub fn resolve(&self, headers: &HeaderMap, peer: Option<IpAddr>) -> Result<User> {
		let config = self.config();
		let Some(users) = self.users()? else {
			return Ok(match &config.default_user {
				Some(name) => User {
					name: Some(name.clone()),
					viewer: self.viewer(Some(name))?,
					via: Via::Default,
					..User::implicit()
				},
				None => User::implicit(),
			});
		};
		self.resolve_in(&users, headers, peer)
	}

	/// `resolve` against a users file that is not written yet.
	pub fn resolve_in(
		&self,
		users: &Users,
		headers: &HeaderMap,
		peer: Option<IpAddr>,
	) -> Result<User> {
		let config = self.config();
		if let Some(header) = &config.identity_header
			&& let Some(peer) = peer
			&& config
				.trusted_proxies
				.iter()
				.any(|network| network.contains(peer))
			&& let Some(value) = headers.get(header.as_str())
		{
			let identities = client_identities(&String::from_utf8_lossy(value.as_bytes()));
			if let Some(account) = users.by_identity(&identities) {
				return self.account(account, Scope::Admin, Via::Certificate);
			}
			tracing::debug!(?identities, "client certificate of no user");
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
				return self.account(account, token.scope, Via::Token);
			}
		}
		if let Some(name) = &config.default_user {
			let Some(account) = users.get(name) else {
				tracing::warn!(user = name, "VIOLET_DEFAULT_USER is not in the users file");
				return Err(Error::Unauthorized(format!(
					"the default user {name} is not in {USERS_FILE}"
				)));
			};
			return self.account(account, Scope::Admin, Via::Default);
		}
		Err(Error::Unauthorized(
			"sign in with a client certificate or a token".into(),
		))
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
	/// the default user, else nobody in particular (the implicit user without a users file).
	pub fn uploader(&self, uploader: Option<&str>) -> Result<(Option<String>, Viewer)> {
		let name = uploader
			.map(str::to_string)
			.or_else(|| self.config().default_user.clone());
		if name.is_none() && self.users()?.is_none() {
			return Ok((None, Viewer::everything()));
		}
		let viewer = self.viewer(name.as_deref())?;
		Ok((name, viewer))
	}
}

pub fn new_token() -> (String, String) {
	let secret = format!(
		"{TOKEN_PREFIX}{}{}",
		uuid::Uuid::new_v4().simple(),
		uuid::Uuid::new_v4().simple()
	);
	let sha256 = hash::sha256(secret.as_bytes());
	(secret, sha256)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn xfcc_names_the_subject_parts_email_and_uri_of_the_last_element() {
		let header = r#"By=spiffe://gw;Hash=aa;Subject="CN=mallory",By=spiffe://violet;Hash=bb;Subject="CN=alex-phone,O=Home\, Inc,emailAddress=alex@example.com";URI=spiffe://home/alex"#;
		let ids = client_identities(header);
		assert!(ids.contains(&"CN=alex-phone".to_string()), "{ids:?}");
		assert!(ids.contains(&"alex@example.com".to_string()), "{ids:?}");
		assert!(ids.contains(&"spiffe://home/alex".to_string()), "{ids:?}");
		assert!(ids.contains(&"O=Home, Inc".to_string()), "{ids:?}");
		assert!(!ids.iter().any(|id| id.contains("mallory")), "{ids:?}");

		let plain = client_identities("CN=sam-laptop,O=Home");
		assert_eq!(plain, ["CN=sam-laptop,O=Home", "CN=sam-laptop", "O=Home"]);
		assert!(client_identities("").is_empty());
	}

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

	#[test]
	fn users_files_are_validated_strictly() {
		let good = br#"{"users": [{"name": "alex", "role": "admin", "identities": ["CN=alex"],
			"tokens": [{"name": "scanner", "sha256": "0000000000000000000000000000000000000000000000000000000000000000", "scope": "write", "expires": null}]},
			{"name": "sam", "role": "member"}]}"#;
		let users = Users::parse(good).unwrap();
		assert_eq!(users.get("SAM").unwrap().role, Role::Member);
		for bad in [
			&br#"{"users": [{"name": "alex", "role": "owner"}]}"#[..],
			br#"{"users": [{"name": "a b", "role": "admin"}]}"#,
			br#"{"users": [{"name": "a", "role": "admin"}, {"name": "A", "role": "member"}]}"#,
			br#"{"users": [{"name": "a", "role": "admin", "identities": ["x"]}, {"name": "b", "role": "member", "identities": ["X"]}]}"#,
			br#"{"users": [{"name": "a", "role": "admin", "tokens": [{"name": "t", "sha256": "abc", "scope": "read"}]}]}"#,
			br#"{"users": [], "extra": 1}"#,
			b"not json",
		] {
			assert!(Users::parse(bad).is_err(), "{}", String::from_utf8_lossy(bad));
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
