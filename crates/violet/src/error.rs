use std::fmt;

/// What went wrong, in the terms the HTTP API answers with.
#[derive(Debug)]
pub enum Error {
	NotFound(String),
	Invalid(String),
	/// The request is valid but the library is no longer in the state it needs.
	Conflict(String),
	/// No user could be found for the request.
	Unauthorized(String),
	/// The user may not do this.
	Forbidden(String),
	Internal(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

impl fmt::Display for Error {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::NotFound(what) => write!(f, "not found: {what}"),
			Self::Invalid(message)
			| Self::Conflict(message)
			| Self::Unauthorized(message)
			| Self::Forbidden(message)
			| Self::Internal(message) => f.write_str(message),
		}
	}
}

impl std::error::Error for Error {}

impl From<violet_core::Error> for Error {
	fn from(error: violet_core::Error) -> Self {
		match error {
			violet_core::Error::NotFound(what) => Self::NotFound(what),
			violet_core::Error::Invalid(message) => Self::Invalid(message),
			other => Self::Internal(other.to_string()),
		}
	}
}

impl From<violet_pdf::Error> for Error {
	fn from(error: violet_pdf::Error) -> Self {
		Self::Invalid(error.to_string())
	}
}

impl From<violet_model::Error> for Error {
	fn from(error: violet_model::Error) -> Self {
		Self::Internal(format!("model: {error}"))
	}
}

impl From<std::io::Error> for Error {
	fn from(error: std::io::Error) -> Self {
		Self::Internal(format!("io: {error}"))
	}
}

impl From<serde_json::Error> for Error {
	fn from(error: serde_json::Error) -> Self {
		Self::Internal(format!("json: {error}"))
	}
}

impl From<tokio::task::JoinError> for Error {
	fn from(error: tokio::task::JoinError) -> Self {
		Self::Internal(format!("task: {error}"))
	}
}
