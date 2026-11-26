//! Passkeys through kagi: its credential store over the users file, where every change is a
//! journaled settings change, and how its refusals answer.

use std::sync::{Arc, Mutex, PoisonError};

use kagi::{CredentialStore, Passkey, Removal, Stored, StoredPasskey as KagiPasskey, Uuid};
use time::OffsetDateTime;
use violet_core::Entry;

use crate::app::blocking;
use crate::auth::{StoredPasskey, User};
use crate::login::{self, WRONG_PASSKEY};
use crate::users::{self, LAST_WAY_IN};
use crate::{App, Error, Result};

/// The passkeys of the users file. `by` is who adds and removes them; the entry of the last
/// such change is kept for the answer.
pub struct Store {
	app: Arc<App>,
	by: Option<User>,
	entry: Mutex<Option<Entry>>,
}

impl Store {
	/// For sign-ins, which change nothing but counters.
	pub fn new(app: &Arc<App>) -> Self {
		Self {
			app: app.clone(),
			by: None,
			entry: Mutex::new(None),
		}
	}

	pub fn by(app: &Arc<App>, user: &User) -> Self {
		Self {
			by: Some(user.clone()),
			..Self::new(app)
		}
	}

	pub fn entry(&self) -> Option<Entry> {
		self.entry
			.lock()
			.unwrap_or_else(PoisonError::into_inner)
			.clone()
	}

	fn keep(&self, entry: Entry) {
		*self.entry.lock().unwrap_or_else(PoisonError::into_inner) = Some(entry);
	}

	fn changer(&self) -> Result<User> {
		self.by
			.clone()
			.ok_or_else(|| Error::Forbidden("sign in first".into()))
	}

	fn all(&self) -> Result<Vec<Stored<Self>>> {
		Ok(self
			.app
			.auth
			.users()?
			.map(|users| {
				users
					.users
					.iter()
					.flat_map(|account| {
						account.passkeys.iter().map(|passkey| KagiPasskey {
							id: passkey.credential.cred_id().to_vec(),
							user: account.name.clone(),
							passkey: passkey.credential.clone(),
						})
					})
					.collect()
			})
			.unwrap_or_default())
	}
}

fn now() -> OffsetDateTime {
	let now = OffsetDateTime::now_utc();
	now.replace_nanosecond(0).unwrap_or(now)
}

impl CredentialStore for Store {
	type User = String;
	type PasskeyId = Vec<u8>;
	type Error = Error;

	fn user_handle(&self, user: &String) -> Uuid {
		login::user_handle(user)
	}

	async fn passkeys(&self, user: &String) -> Result<Vec<Stored<Self>>> {
		Ok(self
			.all()?
			.into_iter()
			.filter(|stored| stored.user.eq_ignore_ascii_case(user))
			.collect())
	}

	async fn find(&self, credential_id: &[u8]) -> Result<Option<Stored<Self>>> {
		Ok(self
			.all()?
			.into_iter()
			.find(|stored| stored.id == credential_id))
	}

	async fn add(&self, user: &String, passkey: &Passkey, label: &str) -> Result<Vec<u8>> {
		let by = self.changer()?;
		let (user, stored) = (
			user.clone(),
			StoredPasskey {
				name: label.to_string(),
				created: now(),
				credential: passkey.clone(),
			},
		);
		let id = passkey.cred_id().to_vec();
		let entry = blocking(&self.app, move |app| {
			users::add_passkey(app, &by, &user, stored)
		})
		.await?;
		self.keep(entry);
		Ok(id)
	}

	async fn update(&self, id: &Vec<u8>, passkey: &Passkey) -> Result<()> {
		let (id, passkey) = (id.clone(), passkey.clone());
		let entry = blocking(&self.app, move |app| {
			users::update_passkey(app, &id, &passkey)
		})
		.await?;
		self.keep(entry);
		Ok(())
	}

	async fn remove(&self, user: &String, id: &Vec<u8>, keep_last: bool) -> Result<Removal> {
		let by = self.changer()?;
		let (user, id) = (user.clone(), id.clone());
		let removed = blocking(&self.app, move |app| {
			users::remove_passkey(app, &by, &user, &id, keep_last)
		})
		.await;
		match removed {
			Ok((entry, remaining)) => {
				self.keep(entry);
				Ok(Removal::Removed { remaining })
			}
			Err(Error::NotFound(_)) => Ok(Removal::NotFound),
			Err(Error::Conflict(message)) if message == LAST_WAY_IN => Ok(Removal::Last),
			Err(error) => Err(error),
		}
	}
}

/// What a step of a passkey ceremony answers when kagi refuses it. A refused sign-in answers
/// as an unknown name would.
pub fn refused(error: kagi::Error) -> Error {
	match error {
		kagi::Error::TooMany { retry_after } => Error::TooMany(retry_after.as_secs().max(1)),
		kagi::Error::Busy => Error::TooMany(60),
		kagi::Error::Ceremony | kagi::Error::Purpose | kagi::Error::Binding => {
			Error::Invalid("the passkey request expired: start again".into())
		}
		kagi::Error::Rejected { name: None } => {
			Error::Invalid("the passkey could not be registered".into())
		}
		kagi::Error::Rejected { name: Some(_) } => Error::Unauthorized(WRONG_PASSKEY.into()),
		kagi::Error::StepUp => Error::Forbidden(
			"sign in again before adding a passkey: it is more than 10 minutes since you did"
				.into(),
		),
		kagi::Error::Duplicate => Error::Conflict("this passkey is already registered".into()),
		kagi::Error::LastCredential => Error::Conflict(LAST_WAY_IN.into()),
		kagi::Error::NotFound => Error::NotFound("no such passkey".into()),
		kagi::Error::Store(error) => match error.downcast::<Error>() {
			Ok(error) => *error,
			Err(error) => Error::Internal(format!("passkeys: {error}")),
		},
		kagi::Error::Config(message) => Error::Invalid(message),
		error @ (kagi::Error::Internal(_) | kagi::Error::Webauthn(_)) => {
			Error::Internal(format!("passkeys: {error}"))
		}
	}
}

#[cfg(test)]
mod tests {
	use std::time::Duration;

	use super::*;

	#[test]
	fn refusals_answer_without_saying_why_a_sign_in_failed() {
		let signed_in = refused(kagi::Error::Rejected {
			name: Some("sam".into()),
		});
		assert!(matches!(signed_in, Error::Unauthorized(ref m) if m == WRONG_PASSKEY));
		assert!(matches!(
			refused(kagi::Error::TooMany {
				retry_after: Duration::from_millis(10)
			}),
			Error::TooMany(1)
		));
		for expired in [
			kagi::Error::Ceremony,
			kagi::Error::Purpose,
			kagi::Error::Binding,
		] {
			assert!(matches!(refused(expired), Error::Invalid(_)));
		}
		let store = kagi::Error::Store(Box::new(Error::Conflict("taken".into())));
		assert!(matches!(refused(store), Error::Conflict(ref m) if m == "taken"));
		assert!(matches!(
			refused(kagi::Error::LastCredential),
			Error::Conflict(ref m) if m == LAST_WAY_IN
		));
	}
}
