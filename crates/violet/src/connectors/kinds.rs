//! The sites violet can sign in to. A kind is a module here with a function that makes its
//! [`Connector`](super::Connector) from a [`ConnectorConfig`](super::ConnectorConfig), listed
//! in [`builtin`].

use super::{Kinds, moneyforward};

pub fn builtin() -> Kinds {
	Kinds::new().with("moneyforward", moneyforward::connector)
}
