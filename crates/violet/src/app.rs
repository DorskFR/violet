use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use serde::Serialize;
use tokio::sync::{Notify, Semaphore};
use violet_core::{Index, Journal, Library};
use violet_model::{Client, ModelConfig};

use crate::cache::PageCache;
use crate::{Error, Result};

const PAGE_CACHE_BYTES: usize = 64 * 1024 * 1024;
const CONCURRENT_RENDERS: usize = 2;
const MODEL_STATUS_TTL: Duration = Duration::from_secs(15);
pub const DEFAULT_JOB_DEADLINE: Duration = Duration::from_secs(4 * 60 * 60);

/// The endpoints to use; `text` analyses, and falls back to `vision` when not set.
#[derive(Debug, Clone, Default)]
pub struct Models {
	pub vision: Option<ModelConfig>,
	pub text: Option<ModelConfig>,
	/// How long reading and analysing one inbox file may take before its job fails;
	/// [`DEFAULT_JOB_DEADLINE`] when not set.
	pub job_deadline: Option<Duration>,
}

/// What [`App::reindex`] did.
#[derive(Debug)]
pub struct Reindexed {
	pub documents: usize,
	/// Paths under `library/` and what is wrong with them.
	pub problems: Vec<(PathBuf, String)>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct ModelStatus {
	pub vision: bool,
	pub text: bool,
}

/// One open library, shared by the worker, the HTTP handlers and the CLI. The index is
/// behind a mutex that every change to the library holds from start to end, so changes are
/// serialised; never hold it across an await.
pub struct App {
	pub library: Library,
	index: Mutex<Index>,
	pub vision: Option<Client>,
	pub text: Option<Client>,
	pub job_deadline: Duration,
	pub pages: PageCache,
	pub renders: Semaphore,
	pub wake_worker: Notify,
	pub wake_watcher: Notify,
	model_status: Mutex<Option<(Instant, ModelStatus)>>,
	lock: Option<std::fs::File>,
	mail: OnceLock<Arc<crate::mail::Mail>>,
	connectors: OnceLock<Arc<crate::connectors::Connectors>>,
	pub auth: crate::auth::Auth,
}

impl App {
	/// Opens the library to change it: takes its lock for as long as the app lives, refused
	/// while another process holds it, and rolls back a change a dead process left unfinished.
	/// An index that did not exist is rebuilt from the sidecars.
	pub fn open(root: impl Into<PathBuf>, models: Models) -> Result<Arc<Self>> {
		let library = Library::open(root)?;
		let lock = library.lock().map_err(|error| match error {
			violet_core::Error::Locked(root) => Error::Conflict(format!(
				"the library at {root} is in use by another violet process; stop it or make \
				 the change through its web UI"
			)),
			other => other.into(),
		})?;
		let app = Self::start(library, models, Some(lock))?;
		if let Err(error) = crate::ops::recover(&app) {
			tracing::error!(%error, "a change that did not finish could not be rolled back");
		}
		Ok(app)
	}

	/// Opens the library only to read it, without the lock.
	pub fn open_read_only(root: impl Into<PathBuf>, models: Models) -> Result<Arc<Self>> {
		Self::start(Library::open(root)?, models, None)
	}

	fn start(library: Library, models: Models, lock: Option<std::fs::File>) -> Result<Arc<Self>> {
		let fresh = !library.index_path().exists();
		let index = Index::open(&library.index_path())?;
		let client = |config: Option<ModelConfig>| config.map(Client::new).transpose();
		let auth = crate::auth::Auth::new(library.root());
		let app = Arc::new(Self {
			library,
			index: Mutex::new(index),
			vision: client(models.vision)?,
			text: client(models.text)?,
			job_deadline: models.job_deadline.unwrap_or(DEFAULT_JOB_DEADLINE),
			pages: PageCache::new(PAGE_CACHE_BYTES),
			renders: Semaphore::new(CONCURRENT_RENDERS),
			wake_worker: Notify::new(),
			wake_watcher: Notify::new(),
			model_status: Mutex::new(None),
			lock,
			mail: OnceLock::new(),
			connectors: OnceLock::new(),
			auth,
		});
		if fresh {
			app.reindex()?;
		}
		Ok(app)
	}

	/// The mail sync, when it is configured.
	pub fn mail(&self) -> Option<&Arc<crate::mail::Mail>> {
		self.mail.get()
	}

	pub fn set_mail(&self, mail: Arc<crate::mail::Mail>) -> Result<(), Arc<crate::mail::Mail>> {
		self.mail.set(mail)
	}

	/// The connectors, when they are configured.
	pub fn connectors(&self) -> Option<&Arc<crate::connectors::Connectors>> {
		self.connectors.get()
	}

	pub fn set_connectors(
		&self,
		connectors: Arc<crate::connectors::Connectors>,
	) -> Result<(), Arc<crate::connectors::Connectors>> {
		self.connectors.set(connectors)
	}

	pub fn index(&self) -> MutexGuard<'_, Index> {
		self.index.lock().unwrap_or_else(PoisonError::into_inner)
	}

	pub fn holds_lock(&self) -> bool {
		self.lock.is_some()
	}

	pub fn journal(&self) -> Journal {
		Journal::open(&self.library.journal_path())
	}

	/// The client that analyses documents.
	pub fn analyst(&self) -> Option<&Client> {
		self.text.as_ref().or(self.vision.as_ref())
	}

	/// Rebuilds the index from the sidecars. Changes no file; what does not add up in the
	/// library is logged and returned.
	pub fn reindex(&self) -> Result<Reindexed> {
		let mut index = self.index();
		let problems = self.library.scan()?.invalid;
		for (path, reason) in &problems {
			tracing::warn!(path = %path.display(), reason, "library drift");
		}
		Ok(Reindexed {
			documents: index.rebuild(&self.library)?,
			problems,
		})
	}

	/// Whether the endpoints answer, asked at most every few seconds.
	pub async fn model_status(&self) -> ModelStatus {
		let cached = *self.status_cache();
		if let Some((at, status)) = cached
			&& at.elapsed() < MODEL_STATUS_TTL
		{
			return status;
		}
		let ready = async |client: Option<&Client>| match client {
			Some(client) => client.ready().await,
			None => false,
		};
		let (vision, text) = tokio::join!(ready(self.vision.as_ref()), ready(self.text.as_ref()));
		let status = ModelStatus {
			vision,
			text: if self.text.is_some() { text } else { vision },
		};
		*self.status_cache() = Some((Instant::now(), status));
		status
	}

	fn status_cache(&self) -> MutexGuard<'_, Option<(Instant, ModelStatus)>> {
		self.model_status
			.lock()
			.unwrap_or_else(PoisonError::into_inner)
	}
}

/// Runs a synchronous piece of library work off the async threads.
pub async fn blocking<T, F>(app: &Arc<App>, work: F) -> Result<T>
where
	T: Send + 'static,
	F: FnOnce(&App) -> Result<T> + Send + 'static,
{
	let app = app.clone();
	tokio::task::spawn_blocking(move || work(&app))
		.await
		.map_err(Error::from)?
}
