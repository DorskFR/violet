//! Finds files in `inbox/`, queues them once they stop changing, and works the queue one job
//! at a time.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use notify::Watcher;
use serde_json::json;
use violet_core::{Job, JobKind, JobQueue, Library, hash};

use crate::app::blocking;
use crate::pipeline::{self, Outcome};
use crate::{App, Error, Result};

const TEMP_EXTENSIONS: [&str; 7] = [
	"part",
	"partial",
	"tmp",
	"temp",
	"crdownload",
	"download",
	"swp",
];
const DEBOUNCE: Duration = Duration::from_millis(100);
const IDLE_POLL: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy)]
pub struct Settings {
	/// How long a file's size and modification time must stay the same before it is queued.
	pub settle: Duration,
	/// How often the inbox is listed even without a file event.
	pub rescan: Duration,
}

impl Default for Settings {
	fn default() -> Self {
		Self {
			settle: Duration::from_secs(2),
			rescan: Duration::from_secs(10),
		}
	}
}

fn ignored(name: &str) -> bool {
	let lower = name.to_lowercase();
	name.starts_with('.')
		|| name.ends_with('~')
		|| lower
			.rsplit_once('.')
			.is_some_and(|(_, ext)| TEMP_EXTENSIONS.contains(&ext))
}

/// Files in `inbox/` and its folders, as paths relative to it, without hidden and temporary
/// ones.
pub fn inbox_files(library: &Library) -> Vec<(PathBuf, fs::Metadata)> {
	let root = library.inbox();
	let mut files = Vec::new();
	let mut pending = vec![root.clone()];
	while let Some(dir) = pending.pop() {
		let Ok(entries) = fs::read_dir(&dir) else {
			continue;
		};
		for entry in entries.flatten() {
			if ignored(&entry.file_name().to_string_lossy()) {
				continue;
			}
			let Ok(metadata) = entry.metadata() else {
				continue;
			};
			let path = entry.path();
			if metadata.is_dir() {
				pending.push(path);
			} else if metadata.is_file()
				&& let Ok(relative) = path.strip_prefix(&root)
			{
				files.push((relative.to_path_buf(), metadata));
			}
		}
	}
	files.sort_by(|a, b| a.0.cmp(&b.0));
	files
}

/// Queues an ingest of an inbox file, keyed by its content. Returns false when that content
/// is already queued or running.
pub fn enqueue(app: &App, relative: &Path) -> Result<bool> {
	let sha256 = hash::sha256_file(&app.library.inbox().join(relative))?;
	let payload = json!({ "path": relative });
	let index = app.index();
	let queue = JobQueue::new(index.connection())?;
	Ok(queue.enqueue(JobKind::Ingest, &sha256, &payload)?
		|| queue.reopen(JobKind::Ingest, &sha256, &payload)?)
}

struct Seen {
	size: u64,
	modified: Option<SystemTime>,
	since: Instant,
	queued: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Scanned {
	pub queued: usize,
	/// Files still changing, or not yet unchanged for long enough.
	pub unsettled: usize,
}

/// Remembers what the inbox looked like, so a file is queued once per version of it: a
/// failed job is not retried until the file changes or the process restarts.
#[derive(Default)]
pub struct Scanner {
	seen: HashMap<PathBuf, Seen>,
}

impl Scanner {
	pub fn scan(&mut self, app: &App, settle: Duration) -> Scanned {
		let files = inbox_files(&app.library);
		let now = Instant::now();
		let mut scanned = Scanned::default();
		self.seen
			.retain(|path, _| files.iter().any(|(present, _)| present == path));
		for (path, metadata) in files {
			let (size, modified) = (metadata.len(), metadata.modified().ok());
			let seen = self.seen.entry(path.clone()).or_insert(Seen {
				size,
				modified,
				since: now,
				queued: false,
			});
			if seen.size != size || seen.modified != modified {
				*seen = Seen {
					size,
					modified,
					since: now,
					queued: false,
				};
			}
			if seen.queued {
				continue;
			}
			if size == 0 || now.duration_since(seen.since) < settle {
				scanned.unsettled += 1;
				continue;
			}
			match enqueue(app, &path) {
				Ok(queued) => {
					seen.queued = true;
					if queued {
						tracing::info!(file = %path.display(), "queued");
						scanned.queued += 1;
					}
				}
				Err(error) => {
					tracing::warn!(file = %path.display(), %error, "cannot queue");
					scanned.unsettled += 1;
				}
			}
		}
		scanned
	}
}

/// Watches `inbox/` until the task is dropped: file events wake it at once, and it lists
/// the folder every `rescan` anyway, which also covers files present at startup and
/// filesystems without events.
pub async fn watch(app: Arc<App>, settings: Settings) {
	let waker = app.clone();
	let mut watcher = notify::recommended_watcher(move |_| waker.wake_watcher.notify_one())
		.inspect_err(|error| tracing::warn!(%error, "no file events, relying on rescans"))
		.ok();
	if let Some(watcher) = &mut watcher
		&& let Err(error) = watcher.watch(&app.library.inbox(), notify::RecursiveMode::Recursive)
	{
		tracing::warn!(%error, "no file events, relying on rescans");
	}
	let mut scanner = Scanner::default();
	let poll = (settings.settle / 4).clamp(Duration::from_millis(20), Duration::from_millis(500));
	loop {
		let scanning = app.clone();
		let scan = tokio::task::spawn_blocking(move || {
			let scanned = scanner.scan(&scanning, settings.settle);
			(scanner, scanned)
		})
		.await;
		let scanned = match scan {
			Ok((returned, scanned)) => {
				scanner = returned;
				scanned
			}
			Err(error) => {
				tracing::error!(%error, "inbox scan failed");
				scanner = Scanner::default();
				Scanned::default()
			}
		};
		if scanned.queued > 0 {
			app.wake_worker.notify_one();
		}
		let wait = if scanned.unsettled > 0 {
			poll
		} else {
			settings.rescan
		};
		tokio::select! {
			() = app.wake_watcher.notified() => tokio::time::sleep(DEBOUNCE.min(poll)).await,
			() = tokio::time::sleep(wait) => {}
		}
	}
}

/// Puts jobs that a dead process left running back in the queue.
pub fn recover(app: &App) -> Result<usize> {
	let index = app.index();
	Ok(JobQueue::new(index.connection())?.recover()?)
}

fn claim(app: &App) -> Result<Option<Job>> {
	let index = app.index();
	Ok(JobQueue::new(index.connection())?.claim()?)
}

fn finish(app: &App, job: &Job, result: &Result<Outcome>) -> Result<()> {
	let index = app.index();
	let queue = JobQueue::new(index.connection())?;
	match result {
		Ok(_) => queue.complete(job.id)?,
		Err(error) => queue.fail(job.id, &error.to_string())?,
	}
	Ok(())
}

/// Claims the oldest pending job and runs it to the end. `None` when the queue is empty.
pub async fn run_next(app: &Arc<App>) -> Result<Option<(Job, Result<Outcome>)>> {
	let Some(job) = blocking(app, claim).await? else {
		return Ok(None);
	};
	let result = match job.payload["path"].as_str() {
		Some(path) => pipeline::ingest(app, Path::new(path)).await,
		None => Err(Error::Invalid("job without a path".into())),
	};
	match &result {
		Ok(outcome) => tracing::info!(job = job.id, ?outcome, "job done"),
		Err(error) => tracing::error!(job = job.id, key = job.key, %error, "job failed"),
	}
	let finished = job.clone();
	let recorded = match &result {
		Ok(outcome) => Ok(outcome.clone()),
		Err(error) => Err(Error::Internal(error.to_string())),
	};
	blocking(app, move |app| finish(app, &finished, &recorded)).await?;
	Ok(Some((job, result)))
}

/// Runs jobs until none is pending.
pub async fn drain(app: &Arc<App>) -> Result<Vec<(Job, Result<Outcome>)>> {
	let mut done = Vec::new();
	while let Some(finished) = run_next(app).await? {
		done.push(finished);
	}
	Ok(done)
}

/// Works the queue until the task is dropped, one job at a time.
pub async fn work(app: Arc<App>) {
	loop {
		match run_next(&app).await {
			Ok(Some(_)) => continue,
			Ok(None) => {}
			Err(error) => tracing::error!(%error, "worker"),
		}
		tokio::select! {
			() = app.wake_worker.notified() => {}
			() = tokio::time::sleep(IDLE_POLL) => {}
		}
	}
}
