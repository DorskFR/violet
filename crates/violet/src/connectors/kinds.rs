//! What violet can take documents from. A site is a module here with a function that makes
//! its [`Connector`](super::Connector) from a [`ConnectorConfig`](super::ConnectorConfig) and
//! one that describes it and its options to the settings; mailboxes are a
//! [`Runner`](super::Runner) in [`crate::mail::kind`]. All are listed in [`builtin`].

use super::{Kinds, moneyforward};
use crate::mail;

pub fn builtin() -> Kinds {
	Kinds::new()
		.with(moneyforward::kind(), moneyforward::connector)
		.with_runner(mail::kind::kind(), mail::kind::Imap)
}
