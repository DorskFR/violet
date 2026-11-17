use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
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

/// The endpoints to use; `text` analyses, and falls back to `vision` when not set.
#[derive(Debug, Clone, Default)]
pub struct Models {
	pub vision: Option<ModelConfig>,
	pub text: Option<ModelConfig>,
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
	pub pages: PageCache,
	pub renders: Semaphore,
	pub wake_worker: Notify,
	pub wake_watcher: Notify,
	model_status: Mutex<Option<(Instant, ModelStatus)>>,
}

impl App {
	/// Opens the library and its index; an index that did not exist is rebuilt from the
	/// sidecars.
	pub fn open(root: impl Into<PathBuf>, models: Models) -> Result<Arc<Self>> {
		let library = Library::open(root)?;
		let fresh = !library.index_path().exists();
		let index = Index::open(&library.index_path())?;
		let client = |config: Option<ModelConfig>| config.map(Client::new).transpose();
		let app = Arc::new(Self {
			library,
			index: Mutex::new(index),
			vision: client(models.vision)?,
			text: client(models.text)?,
			pages: PageCache::new(PAGE_CACHE_BYTES),
			renders: Semaphore::new(CONCURRENT_RENDERS),
			wake_worker: Notify::new(),
			wake_watcher: Notify::new(),
			model_status: Mutex::new(None),
		});
		if fresh {
			app.reindex()?;
		}
		Ok(app)
	}

	pub fn index(&self) -> MutexGuard<'_, Index> {
		self.index.lock().unwrap_or_else(PoisonError::into_inner)
	}

	pub fn journal(&self) -> Journal {
		Journal::open(&self.library.journal_path())
	}

	/// The client that analyses documents.
	pub fn analyst(&self) -> Option<&Client> {
		self.text.as_ref().or(self.vision.as_ref())
	}

	/// Rebuilds the index from the sidecars and returns the number of documents.
	pub fn reindex(&self) -> Result<usize> {
		let mut index = self.index();
		for (path, reason) in self.library.scan()?.invalid {
			tracing::warn!(path = %path.display(), reason, "sidecar skipped");
		}
		Ok(index.rebuild(&self.library)?)
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
