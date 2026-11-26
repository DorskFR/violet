//! Changes to the users file and the private areas. Each one is a journaled change of a
//! settings file, so it can be undone.

use std::io::ErrorKind;

use serde::Serialize;
use time::OffsetDateTime;
use violet_core::Entry;

use crate::auth::{
	AREAS_FILE, Account, Actor, Areas, Role, Scope, StoredPasskey, Token, USERS_FILE, User, Users,
	new_token, valid_label, valid_name,
};
use crate::{App, Error, Result, ops};

/// A check on the users file a change would write, refusing it with an error.
pub type Check<'a> = &'a dyn Fn(&Users) -> Result<()>;

pub const NO_CHECK: Check<'static> = &|_| Ok(());

pub const LAST_WAY_IN: &str =
	"this is your last way to sign in: set a password or add another passkey first";

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
	/// Whether the user has a password.
	pub password: bool,
	pub passkeys: Vec<String>,
	pub tokens: Vec<TokenView>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PasskeyView {
	pub name: String,
	#[serde(with = "time::serde::rfc3339")]
	pub created: OffsetDateTime,
}

impl From<&StoredPasskey> for PasskeyView {
	fn from(passkey: &StoredPasskey) -> Self {
		Self {
			name: passkey.name.clone(),
			created: passkey.created,
		}
	}
}

impl From<&Account> for UserView {
	fn from(account: &Account) -> Self {
		Self {
			name: account.name.clone(),
			role: account.role,
			password: account.password.is_some(),
			passkeys: account
				.passkeys
				.iter()
				.map(|passkey| passkey.name.clone())
				.collect(),
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
	by: &Actor,
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
		by,
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

/// Adds a user; `password` is a PHC string already hashed.
pub fn add(
	app: &App,
	by: &User,
	check: Check,
	name: &str,
	role: Role,
	password: Option<String>,
) -> Result<Entry> {
	valid_name("user", name)?;
	let summary = format!("Added user {name} ({})", role_name(role));
	edit_users(app, &by.actor(), check, summary, |users| {
		if users.get(name).is_some() {
			return Err(Error::Conflict(format!("user {name} exists")));
		}
		users.users.push(Account {
			name: name.to_string(),
			role,
			password,
			passkeys: Vec::new(),
			tokens: Vec::new(),
		});
		Ok(())
	})
}

/// The first admin, with a passkey, while no account has a credential; an account without one
/// of that name is claimed.
pub fn set_up(app: &App, by: &User, name: &str, passkey: StoredPasskey) -> Result<Entry> {
	valid_name("user", name)?;
	let summary = format!("Set up the admin {name}");
	edit_users(app, &by.actor(), NO_CHECK, summary, |users| {
		if users.any_credential() {
			return Err(Error::Conflict("the library is set up already".into()));
		}
		match users.get_mut(name) {
			Some(account) => {
				account.role = Role::Admin;
				account.passkeys.push(passkey);
			}
			None => users.users.push(Account {
				name: name.to_string(),
				role: Role::Admin,
				password: None,
				passkeys: vec![passkey],
				tokens: Vec::new(),
			}),
		}
		Ok(())
	})
}

pub fn update(app: &App, by: &User, check: Check, name: &str, role: Role) -> Result<Entry> {
	edit_users(
		app,
		&by.actor(),
		check,
		format!("Changed user {name}"),
		|users| {
			let account = account(users, name)?;
			if account.role == role {
				return Err(Error::Invalid(format!("nothing to change for {name}")));
			}
			account.role = role;
			Ok(())
		},
	)
}

pub fn remove(app: &App, by: &User, check: Check, name: &str) -> Result<Entry> {
	edit_users(
		app,
		&by.actor(),
		check,
		format!("Removed user {name}"),
		|users| {
			account(users, name)?;
			users
				.users
				.retain(|account| !account.name.eq_ignore_ascii_case(name));
			Ok(())
		},
	)
}

/// Sets the password of `user` to `phc`, a hash; `current` is what the password was when the
/// sender checked it, so that two changes at once cannot both pass.
pub fn set_password(
	app: &App,
	by: &User,
	check: Check,
	user: &str,
	phc: String,
	current: Option<&str>,
) -> Result<Entry> {
	let summary = format!("Changed the password of {user}");
	edit_users(app, &by.actor(), check, summary, |users| {
		let account = account(users, user)?;
		if let Some(current) = current
			&& account.password.as_deref() != Some(current)
		{
			return Err(Error::Conflict("the password changed meanwhile".into()));
		}
		account.password = Some(phc);
		Ok(())
	})
}

pub fn add_passkey(app: &App, by: &User, user: &str, passkey: StoredPasskey) -> Result<Entry> {
	valid_label("passkey", &passkey.name)?;
	let summary = format!("Added passkey {} of {user}", passkey.name);
	edit_users(app, &by.actor(), NO_CHECK, summary, |users| {
		let account = account(users, user)?;
		if account.passkey(&passkey.name).is_some() {
			return Err(Error::Conflict(format!(
				"{user} has a passkey named {}",
				passkey.name
			)));
		}
		account.passkeys.push(passkey);
		Ok(())
	})
}

pub fn rename_passkey(app: &App, by: &User, user: &str, name: &str, to: &str) -> Result<Entry> {
	valid_label("passkey", to)?;
	let summary = format!("Renamed passkey {name} of {user} to {to}");
	edit_users(app, &by.actor(), NO_CHECK, summary, |users| {
		let account = account(users, user)?;
		if account.passkey(name).is_none() {
			return Err(Error::NotFound(format!("passkey {name}")));
		}
		if name == to {
			return Err(Error::Invalid(format!("the passkey is named {to} already")));
		}
		if !name.eq_ignore_ascii_case(to) && account.passkey(to).is_some() {
			return Err(Error::Conflict(format!("{user} has a passkey named {to}")));
		}
		for passkey in &mut account.passkeys {
			if passkey.name.eq_ignore_ascii_case(name) {
				passkey.name = to.to_string();
			}
		}
		Ok(())
	})
}

/// Removes the passkey with credential id `id`; with `keep_last`, not the only one of `user`.
/// Answers how many passkeys `user` has left.
pub fn remove_passkey(
	app: &App,
	by: &User,
	user: &str,
	id: &[u8],
	keep_last: bool,
) -> Result<(Entry, usize)> {
	let label = app
		.auth
		.users()?
		.and_then(|users| {
			users
				.get(user)?
				.passkeys
				.iter()
				.find(|passkey| passkey.credential.cred_id().as_slice() == id)
				.map(|passkey| passkey.name.clone())
		})
		.unwrap_or_else(|| "?".into());
	let summary = format!("Removed passkey {label} of {user}");
	let mut remaining = 0;
	let entry = edit_users(app, &by.actor(), NO_CHECK, summary, |users| {
		let account = account(users, user)?;
		let before = account.passkeys.len();
		account
			.passkeys
			.retain(|passkey| passkey.credential.cred_id().as_slice() != id);
		if account.passkeys.len() == before {
			return Err(Error::NotFound("no such passkey".into()));
		}
		if keep_last && account.passkeys.is_empty() {
			return Err(Error::Conflict(LAST_WAY_IN.into()));
		}
		remaining = account.passkeys.len();
		Ok(())
	})?;
	Ok((entry, remaining))
}

/// Writes back what a sign-in with the passkey `id` changed in it: its counter and backup
/// flags. Recorded as an automatic step for its owner.
pub fn update_passkey(app: &App, id: &[u8], passkey: &kagi::Passkey) -> Result<Entry> {
	let owner = app
		.auth
		.users()?
		.and_then(|users| {
			users.users.iter().find_map(|account| {
				let stored = account
					.passkeys
					.iter()
					.find(|stored| stored.credential.cred_id().as_slice() == id)?;
				Some((account.name.clone(), stored.name.clone()))
			})
		})
		.ok_or_else(|| Error::NotFound("no such passkey".into()))?;
	let (user, label) = owner;
	let summary = format!("Updated the sign-in counter of passkey {label} of {user}");
	edit_users(
		app,
		&Actor::agent(Some(&user)),
		NO_CHECK,
		summary,
		|users| {
			let stored = account(users, &user)?
				.passkeys
				.iter_mut()
				.find(|stored| stored.credential.cred_id().as_slice() == id)
				.ok_or_else(|| Error::NotFound("no such passkey".into()))?;
			stored.credential = passkey.clone();
			Ok(())
		},
	)
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
	let entry = edit_users(app, &by.actor(), check, summary, |users| {
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
	edit_users(app, &by.actor(), check, summary, |users| {
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
