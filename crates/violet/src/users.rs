//! Changes to the users file and the private areas. Each one is a journaled change of a
//! settings file, so it can be undone.

use std::io::ErrorKind;

use serde::Serialize;
use time::OffsetDateTime;
use violet_core::Entry;

use crate::auth::{
	AREAS_FILE, Account, Areas, Role, Scope, Token, USERS_FILE, User, Users, new_token, valid_name,
};
use crate::{App, Error, Result, ops};

/// A check on the users file a change would write, refusing it with an error.
pub type Check<'a> = &'a dyn Fn(&Users) -> Result<()>;

pub const NO_CHECK: Check<'static> = &|_| Ok(());

#[derive(Debug, Clone, Serialize)]
pub struct TokenView {
	pub name: String,
	pub scope: Scope,
	#[serde(with = "time::serde::rfc3339::option")]
	pub expires: Option<OffsetDateTime>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UserView {
	pub name: String,
	pub role: Role,
	pub identities: Vec<String>,
	pub tokens: Vec<TokenView>,
}

impl From<&Account> for UserView {
	fn from(account: &Account) -> Self {
		Self {
			name: account.name.clone(),
			role: account.role,
			identities: account.identities.clone(),
			tokens: account
				.tokens
				.iter()
				.map(|token| TokenView {
					name: token.name.clone(),
					scope: token.scope,
					expires: token.expires,
				})
				.collect(),
		}
	}
}

/// The users, without token hashes; none without a users file.
pub fn list(app: &App) -> Result<Option<Vec<UserView>>> {
	Ok(app
		.auth
		.users()?
		.map(|users| users.users.iter().map(UserView::from).collect()))
}

fn read(app: &App, file: &str) -> Result<Option<Vec<u8>>> {
	match std::fs::read(app.library.root().join(file)) {
		Ok(bytes) => Ok(Some(bytes)),
		Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
		Err(error) => Err(error.into()),
	}
}

fn json(value: &impl Serialize) -> Result<Vec<u8>> {
	let mut bytes = serde_json::to_vec_pretty(value)?;
	bytes.push(b'\n');
	Ok(bytes)
}

/// Reads the users file again under the index lock, edits it and writes it as one change.
fn edit_users(
	app: &App,
	by: &User,
	check: Check,
	summary: String,
	edit: impl FnOnce(&mut Users) -> Result<()>,
) -> Result<Entry> {
	let mut index = app.index();
	let mut users = match read(app, USERS_FILE)? {
		Some(bytes) => Users::parse(&bytes)?,
		None => Users::default(),
	};
	edit(&mut users)?;
	users.validate()?;
	check(&users)?;
	ops::write_settings(
		app,
		&mut index,
		&by.actor(),
		USERS_FILE,
		Some(&json(&users)?),
		summary,
	)
}

fn account<'a>(users: &'a mut Users, name: &str) -> Result<&'a mut Account> {
	users
		.get_mut(name)
		.ok_or_else(|| Error::NotFound(format!("user {name}")))
}

fn clean_identities(identities: Vec<String>) -> Vec<String> {
	let mut clean: Vec<String> = Vec::new();
	for identity in identities {
		let identity = identity.trim().to_string();
		if !identity.is_empty() && !clean.contains(&identity) {
			clean.push(identity);
		}
	}
	clean
}

pub fn add(
	app: &App,
	by: &User,
	check: Check,
	name: &str,
	role: Role,
	identities: Vec<String>,
) -> Result<Entry> {
	valid_name("user", name)?;
	let summary = format!("Added user {name} ({})", role_name(role));
	edit_users(app, by, check, summary, |users| {
		if users.get(name).is_some() {
			return Err(Error::Conflict(format!("user {name} exists")));
		}
		users.users.push(Account {
			name: name.to_string(),
			role,
			identities: clean_identities(identities),
			tokens: Vec::new(),
		});
		Ok(())
	})
}

pub fn update(
	app: &App,
	by: &User,
	check: Check,
	name: &str,
	role: Option<Role>,
	identities: Option<Vec<String>>,
) -> Result<Entry> {
	edit_users(app, by, check, format!("Changed user {name}"), |users| {
		let account = account(users, name)?;
		let before = account.clone();
		if let Some(role) = role {
			account.role = role;
		}
		if let Some(identities) = identities {
			account.identities = clean_identities(identities);
		}
		if *account == before {
			return Err(Error::Invalid(format!("nothing to change for {name}")));
		}
		Ok(())
	})
}

pub fn remove(app: &App, by: &User, check: Check, name: &str) -> Result<Entry> {
	edit_users(app, by, check, format!("Removed user {name}"), |users| {
		account(users, name)?;
		users
			.users
			.retain(|account| !account.name.eq_ignore_ascii_case(name));
		Ok(())
	})
}

/// Adds a token and returns its secret, which is not kept anywhere.
pub fn add_token(
	app: &App,
	by: &User,
	check: Check,
	user: &str,
	name: &str,
	scope: Scope,
	expires: Option<OffsetDateTime>,
) -> Result<(String, Entry)> {
	valid_name("token", name)?;
	if expires.is_some_and(|expires| expires <= OffsetDateTime::now_utc()) {
		return Err(Error::Invalid("the expiry is in the past".into()));
	}
	let (secret, sha256) = new_token();
	let summary = format!("Added token {name} for {user}");
	let entry = edit_users(app, by, check, summary, |users| {
		let account = account(users, user)?;
		if account.tokens.iter().any(|token| token.name == name) {
			return Err(Error::Conflict(format!("{user} has a token named {name}")));
		}
		account.tokens.push(Token {
			name: name.to_string(),
			sha256,
			scope,
			expires,
		});
		Ok(())
	})?;
	Ok((secret, entry))
}

pub fn revoke_token(app: &App, by: &User, check: Check, user: &str, name: &str) -> Result<Entry> {
	let summary = format!("Revoked token {name} of {user}");
	edit_users(app, by, check, summary, |users| {
		let account = account(users, user)?;
		if !account.tokens.iter().any(|token| token.name == name) {
			return Err(Error::NotFound(format!("token {name} of {user}")));
		}
		account.tokens.retain(|token| token.name != name);
		Ok(())
	})
}

pub fn areas(app: &App) -> Result<Areas> {
	Ok(app
		.auth
		.areas()?
		.map(|areas| (*areas).clone())
		.unwrap_or_default())
}

/// Replaces the list of private areas; members must be users when there is a users file.
pub fn set_areas(app: &App, by: &User, mut areas: Areas) -> Result<Entry> {
	for private in &mut areas.private {
		private.area = private
			.area
			.split_whitespace()
			.collect::<Vec<_>>()
			.join(" ");
		let mut members: Vec<String> = Vec::new();
		for member in &private.members {
			let member = member.trim().to_string();
			if !members
				.iter()
				.any(|known| known.eq_ignore_ascii_case(&member))
			{
				members.push(member);
			}
		}
		private.members = members;
	}
	areas.validate()?;
	let mut index = app.index();
	if let Some(bytes) = read(app, USERS_FILE)? {
		let users = Users::parse(&bytes)?;
		for member in areas.private.iter().flat_map(|private| &private.members) {
			if users.get(member).is_none() {
				return Err(Error::Invalid(format!("{member} is not a user")));
			}
		}
	}
	let current = match read(app, AREAS_FILE)? {
		Some(bytes) => Areas::parse(&bytes)?,
		None => Areas::default(),
	};
	if current == areas {
		return Err(Error::Invalid("the private areas are already so".into()));
	}
	ops::write_settings(
		app,
		&mut index,
		&by.actor(),
		AREAS_FILE,
		Some(&json(&areas)?),
		"Changed the private areas".into(),
	)
}

fn role_name(role: Role) -> &'static str {
	match role {
		Role::Member => "member",
		Role::Admin => "admin",
	}
}
