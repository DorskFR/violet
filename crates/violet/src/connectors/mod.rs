//! Brings documents from websites into `inbox/` on a schedule. A kind (one site) signs in,
//! lists what the site holds and fetches a document; everything else is here: config,
//! schedules, what is kept between runs, which documents were already taken, the inbox and
//! where each file came from. Off unless `VIOLET_CONNECTORS_CONFIG` names a config file.

pub mod config;
pub mod kinds;
pub mod schedule;
pub mod session;

use std::collections::HashSet;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use anyhow::anyhow;
use serde::Serialize;
use time::{OffsetDateTime, UtcOffset};
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use uuid::Uuid;
use violet_core::{
	ConnectorHint, ConnectorItem, ConnectorItemRecord, ConnectorItemStatus, ConnectorSource,
	ConnectorStore, hash,
};

pub use config::{Config, ConnectorConfig, Hints};
pub use schedule::Schedule;

use crate::app::blocking;
use crate::{App, Error, Result};

pub const CONFIG_ENV: &str = "VIOLET_CONNECTORS_CONFIG";
const MAX_DOCUMENT_BYTES: usize = 100 * 1024 * 1024;
const CODE_WAIT: Duration = Duration::from_secs(15 * 60);
const RECHECK: Duration = Duration::from_secs(10 * 60);
const ITEMS_SHOWN: usize = 50;
const MAX_CODE_CHARS: usize = 64;
const NEEDS_LOGIN: &str = "the site asks for a code; connect from the settings";
const NEEDS_CODE: &str = "a sign-in is waiting for its code";

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// What to sign in with, read from the environment for each login.
#[derive(Clone)]
pub struct Credentials {
	pub username: String,
	pub password: String,
}

impl std::fmt::Debug for Credentials {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.write_str("Credentials")
	}
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Login {
	Ready,
	/// The site sent the user a code; `prompt` tells them where to look for it.
	NeedsCode {
		prompt: String,
	},
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteDoc {
	/// What names the document on the site for good; a document is fetched once per id.
	pub remote_id: String,
	/// The name its file gets in the inbox, with its extension.
	pub file_name: String,
	pub title: Option<String>,
	/// `YYYY-MM-DD` when the site tells.
	pub date: Option<String>,
}

/// One site. A value serves one login and the run, or the code entry, that follows it. Its
/// errors are shown to admins and logged, so they must hold no credential, cookie or token.
pub trait Connector: Send {
	/// Signs in. `saved` is what [`Connector::state`] last returned for this connector.
	fn login<'a>(
		&'a mut self,
		credentials: &'a Credentials,
		saved: Option<serde_json::Value>,
	) -> BoxFuture<'a, anyhow::Result<Login>>;

	/// Finishes a login that answered [`Login::NeedsCode`].
	fn submit_code<'a>(&'a mut self, code: &'a str) -> BoxFuture<'a, anyhow::Result<()>>;

	/// What to keep for the next login, such as a trusted-device cookie. Stored after each
	/// login and run, never shown or logged; none keeps what is stored.
	fn state(&self) -> Option<serde_json::Value>;

	/// Every document the site holds now, signed in.
	fn list(&mut self) -> BoxFuture<'_, anyhow::Result<Vec<RemoteDoc>>>;

	fn fetch<'a>(&'a mut self, remote_id: &'a str) -> BoxFuture<'a, anyhow::Result<Vec<u8>>>;
}

/// Makes the connector of a kind from its config, refusing options it does not know.
pub type Factory =
	Arc<dyn Fn(&ConnectorConfig) -> anyhow::Result<Box<dyn Connector>> + Send + Sync>;

/// The kinds a config may name.
#[derive(Clone, Default)]
pub struct Kinds(Vec<(String, Factory)>);

impl Kinds {
	pub fn new() -> Self {
		Self::default()
	}

	pub fn with(
		mut self,
		kind: &str,
		factory: impl Fn(&ConnectorConfig) -> anyhow::Result<Box<dyn Connector>> + Send + Sync + 'static,
	) -> Self {
		self.0.push((kind.into(), Arc::new(factory)));
		self
	}

	fn get(&self, kind: &str) -> Option<&Factory> {
		self.0
			.iter()
			.find(|(name, _)| name == kind)
			.map(|(_, factory)| factory)
	}

	fn names(&self) -> String {
		let names: Vec<&str> = self.0.iter().map(|(name, _)| name.as_str()).collect();
		if names.is_empty() {
			"none".into()
		} else {
			names.join(", ")
		}
	}
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct Report {
	/// Documents the site listed.
	pub listed: u32,
	/// Files written to the inbox.
	pub ingested: u32,
	pub skipped: u32,
	pub failed: u32,
	pub errors: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Auth {
	Ready,
	/// A login started from the settings waits for the code the site sent.
	NeedsCode,
	/// Nothing is saved to sign in with, or a run met a code prompt; runs wait for a login.
	NeedsLogin,
}

struct Pending {
	connector: Box<dyn Connector>,
	prompt: String,
	started: Instant,
}

#[derive(Default)]
struct Live {
	running: bool,
	failures: u32,
	retry_at: Option<OffsetDateTime>,
	pending: Option<Pending>,
}

struct Slot {
	factory: Factory,
	wake: Notify,
	busy: tokio::sync::Mutex<()>,
	live: Mutex<Live>,
}

impl Slot {
	fn live(&self) -> MutexGuard<'_, Live> {
		let mut live = self.live.lock().unwrap_or_else(PoisonError::into_inner);
		if live
			.pending
			.as_ref()
			.is_some_and(|pending| pending.started.elapsed() >= CODE_WAIT)
		{
			live.pending = None;
		}
		live
	}
}

/// The connectors of a running server.
pub struct Connectors {
	pub config: Config,
	store: ConnectorStore,
	slots: Vec<(String, Slot)>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Counts {
	pub ingested: u64,
	pub skipped: u64,
	pub failed: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConnectorView {
	pub name: String,
	pub kind: String,
	pub schedule: String,
	/// None while runs wait for a login.
	#[serde(with = "time::serde::rfc3339::option")]
	pub next_run: Option<OffsetDateTime>,
	pub running: bool,
	#[serde(with = "time::serde::rfc3339::option")]
	pub last_run: Option<OffsetDateTime>,
	#[serde(with = "time::serde::rfc3339::option")]
	pub last_success: Option<OffsetDateTime>,
	pub last_error: Option<String>,
	pub last_report: Option<Report>,
	pub auth: Auth,
	/// What the site said about the code it waits for.
	pub prompt: Option<String>,
	pub counts: Counts,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConnectorsView {
	pub enabled: bool,
	/// The offset the schedules are in, as `+09:00`.
	pub utc_offset: Option<String>,
	pub connectors: Vec<ConnectorView>,
	/// The newest handled remote documents.
	pub items: Vec<ConnectorItem>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LoginView {
	pub name: String,
	pub auth: Auth,
	pub prompt: Option<String>,
}

fn timestamp(seconds: Option<i64>) -> Option<OffsetDateTime> {
	seconds.and_then(|seconds| OffsetDateTime::from_unix_timestamp(seconds).ok())
}

fn offset_text(offset: UtcOffset) -> String {
	let (hours, minutes, _) = offset.as_hms();
	let sign = if offset.is_negative() { '-' } else { '+' };
	format!("{sign}{:02}:{:02}", hours.abs(), minutes.abs())
}

impl Connectors {
	/// Opens what the connectors keep at `store` and checks every connector against its kind.
	pub fn new(config: Config, kinds: &Kinds, store: &Path) -> anyhow::Result<Arc<Self>> {
		let mut slots = Vec::new();
		for connector in &config.connectors {
			let name = &connector.name;
			let factory = kinds.get(&connector.kind).ok_or_else(|| {
				anyhow!(
					"connector {name}: unknown kind \"{}\" (known: {})",
					connector.kind,
					kinds.names()
				)
			})?;
			factory(connector).map_err(|error| anyhow!("connector {name}: {error:#}"))?;
			slots.push((
				name.clone(),
				Slot {
					factory: factory.clone(),
					wake: Notify::new(),
					busy: tokio::sync::Mutex::new(()),
					live: Mutex::new(Live::default()),
				},
			));
		}
		let store = ConnectorStore::open(store)
			.map_err(|error| anyhow!("cannot open {}: {error}", store.display()))?;
		Ok(Arc::new(Self {
			config,
			store,
			slots,
		}))
	}

	fn slot(&self, name: &str) -> Option<&Slot> {
		self.slots
			.iter()
			.find(|(known, _)| known == name)
			.map(|(_, slot)| slot)
	}

	fn known(&self, name: &str) -> Result<(&ConnectorConfig, &Slot)> {
		self.config
			.connector(name)
			.zip(self.slot(name))
			.ok_or_else(|| Error::NotFound(format!("connector {name}")))
	}

	/// Asks a connector's loop to run now; false when there is no such connector.
	pub fn wake(&self, name: &str) -> bool {
		match self.slot(name) {
			Some(slot) => {
				slot.wake.notify_one();
				true
			}
			None => false,
		}
	}

	/// When the connector runs next by itself; none while it waits for a login or a code.
	pub fn next_run(&self, name: &str, now: OffsetDateTime) -> Result<Option<OffsetDateTime>> {
		let (config, slot) = self.known(name)?;
		let (retry_at, waits_for_code) = {
			let live = slot.live();
			(live.retry_at, live.pending.is_some())
		};
		let run = self.store.run(name)?;
		if waits_for_code || run.needs_login {
			return Ok(None);
		}
		Ok(Some(schedule::next_run(
			&config.schedule,
			self.config.utc_offset,
			timestamp(run.last_success),
			retry_at,
			now,
		)))
	}

	fn saved(&self, name: &str) -> Result<Option<serde_json::Value>> {
		Ok(self
			.store
			.state(name)?
			.and_then(|state| serde_json::from_str(&state).ok()))
	}

	fn save(&self, name: &str, connector: &dyn Connector) -> Result<()> {
		if let Some(state) = connector.state() {
			self.store.set_state(name, Some(&state.to_string()))?;
		}
		Ok(())
	}

	fn finish(&self, name: &str, slot: &Slot, outcome: &anyhow::Result<Report>) {
		let now = OffsetDateTime::now_utc();
		let (error, report) = match outcome {
			Ok(report) if report.errors.is_empty() => (None, Some(report)),
			Ok(report) => (Some(report.errors.join("; ")), Some(report)),
			Err(error) => (Some(format!("{error:#}")), None),
		};
		let waits_for_login = self.store.run(name).is_ok_and(|run| run.needs_login);
		{
			let mut live = slot.live();
			live.running = false;
			if error.is_none() || waits_for_login {
				live.failures = 0;
				live.retry_at = None;
			} else {
				live.failures = live.failures.saturating_add(1);
				live.retry_at = Some(now + schedule::backoff(live.failures));
			}
		}
		let report = report.and_then(|report| serde_json::to_string(report).ok());
		if let Err(error) = self.store.finish_run(
			name,
			now.unix_timestamp(),
			error.as_deref(),
			report.as_deref(),
		) {
			tracing::error!(connector = name, %error, "the run could not be recorded");
		}
	}
}

fn credentials(config: &ConnectorConfig) -> anyhow::Result<Credentials> {
	let read = |variable: &str| {
		std::env::var(variable)
			.map_err(|_| anyhow!("the environment variable {variable} is not set"))
	};
	Ok(Credentials {
		username: read(&config.username_env)?,
		password: read(&config.password_env)?,
	})
}

/// Reads the config `VIOLET_CONNECTORS_CONFIG` names, if it is set, and starts one loop per
/// connector.
pub fn start(app: &Arc<App>) -> anyhow::Result<Vec<JoinHandle<()>>> {
	let Some(path) = std::env::var_os(CONFIG_ENV).filter(|path| !path.is_empty()) else {
		return Ok(Vec::new());
	};
	let config = Config::load(Path::new(&path))?;
	let connectors = Connectors::new(config, &kinds::builtin(), &app.library.connectors_path())?;
	app.set_connectors(connectors.clone())
		.map_err(|_| anyhow!("connectors are already set up"))?;
	tracing::info!(
		connectors = connectors.config.connectors.len(),
		"connectors on"
	);
	Ok(spawn(app, &connectors))
}

/// One loop per connector, each running it when it is due or woken.
pub fn spawn(app: &Arc<App>, connectors: &Arc<Connectors>) -> Vec<JoinHandle<()>> {
	connectors
		.config
		.connectors
		.iter()
		.map(|connector| {
			tokio::spawn(run_loop(
				app.clone(),
				connectors.clone(),
				connector.name.clone(),
			))
		})
		.collect()
}

async fn run_loop(app: Arc<App>, connectors: Arc<Connectors>, name: String) {
	let Some(slot) = connectors.slot(&name) else {
		return;
	};
	loop {
		let now = OffsetDateTime::now_utc();
		let wait = match connectors.next_run(&name, now) {
			Ok(Some(due)) if due <= now => None,
			Ok(Some(due)) => Some(
				Duration::try_from(due - now)
					.unwrap_or(RECHECK)
					.min(RECHECK),
			),
			Ok(None) | Err(_) => Some(RECHECK),
		};
		if let Some(wait) = wait {
			tokio::select! {
				() = slot.wake.notified() => {}
				() = tokio::time::sleep(wait) => continue,
			}
		}
		let _ = run(&app, &connectors, &name).await;
	}
}

/// Runs one connector now, after any run or login of it already going on: signs in with what
/// is saved, fetches the documents not taken yet into the inbox. Never waits for a code.
pub async fn run(app: &Arc<App>, connectors: &Connectors, name: &str) -> anyhow::Result<Report> {
	let (config, slot) = connectors
		.known(name)
		.map_err(|_| anyhow!("no connector named {name}"))?;
	let _busy = slot.busy.lock().await;
	{
		let mut live = slot.live();
		if live.pending.is_some() {
			return Err(anyhow!(NEEDS_CODE));
		}
		live.running = true;
	}
	let outcome = run_connector(app, connectors, config, slot).await;
	connectors.finish(name, slot, &outcome);
	match &outcome {
		Ok(report) if report.errors.is_empty() => {
			if report.ingested > 0 {
				tracing::info!(connector = name, ?report, "connector ran");
			}
		}
		Ok(report) => {
			tracing::warn!(connector = name, errors = ?report.errors, "connector ran with errors");
		}
		Err(error) => {
			tracing::error!(
				connector = name,
				error = format!("{error:#}"),
				"connector failed"
			);
		}
	}
	outcome
}

async fn run_connector(
	app: &Arc<App>,
	connectors: &Connectors,
	config: &ConnectorConfig,
	slot: &Slot,
) -> anyhow::Result<Report> {
	let name = &config.name;
	let credentials = credentials(config)?;
	let mut connector = (slot.factory)(config)?;
	let saved = connectors.saved(name)?;
	match connector.login(&credentials, saved).await? {
		Login::Ready => {}
		Login::NeedsCode { .. } => {
			connectors.store.set_needs_login(name, true)?;
			return Err(anyhow!(NEEDS_LOGIN));
		}
	}
	connectors.save(name, connector.as_ref())?;
	connectors.store.set_needs_login(name, false)?;

	let outcome = fetch_new(app, config, connector.as_mut()).await;
	connectors.save(name, connector.as_ref())?;
	outcome
}

async fn fetch_new(
	app: &Arc<App>,
	config: &ConnectorConfig,
	connector: &mut dyn Connector,
) -> anyhow::Result<Report> {
	let mut report = Report::default();
	let listed = connector.list().await?;
	report.listed = listed.len() as u32;
	let handled = {
		let name = config.name.clone();
		blocking(app, move |app| Ok(app.index().connector_handled(&name)?)).await?
	};
	let mut seen = HashSet::new();
	for doc in listed {
		if handled.contains(&doc.remote_id) || !seen.insert(doc.remote_id.clone()) {
			continue;
		}
		let mut row = ConnectorItemRecord {
			connector: config.name.clone(),
			remote_id: doc.remote_id.clone(),
			sha256: None,
			title: doc.title.clone(),
			date: doc.date.clone(),
			status: ConnectorItemStatus::Failed,
			detail: None,
			error: None,
		};
		let delivered = match connector.fetch(&doc.remote_id).await {
			Ok(bytes) if bytes.is_empty() => {
				row.status = ConnectorItemStatus::Skipped;
				row.detail = Some("empty file".into());
				Ok(None)
			}
			Ok(bytes) if bytes.len() > MAX_DOCUMENT_BYTES => {
				row.status = ConnectorItemStatus::Skipped;
				row.detail = Some(format!(
					"larger than {} MB",
					MAX_DOCUMENT_BYTES / 1024 / 1024
				));
				Ok(None)
			}
			Ok(bytes) => Ok(Some(bytes)),
			Err(error) => Err(format!("{error:#}")),
		};
		let outcome = match delivered {
			Ok(None) => Ok(()),
			Ok(Some(bytes)) => {
				row.status = ConnectorItemStatus::Ingested;
				row.sha256 = Some(hash::sha256(&bytes));
				let hint = ConnectorHint {
					connector: ConnectorSource {
						name: config.name.clone(),
						kind: config.kind.clone(),
						remote_id: doc.remote_id.clone(),
						title: doc.title.clone(),
						date: doc.date.clone(),
					},
					area: config.hints.area.clone(),
					tags: config.hints.tags.clone(),
				};
				let (delivered, owner) = (row.clone(), config.hints.owner.clone());
				let file_name = inbox_name(&doc.file_name);
				blocking(app, move |app| {
					deliver(app, &delivered, &file_name, &bytes, &hint, owner.as_deref())
				})
				.await
				.map_err(|error| error.to_string())
			}
			Err(error) => Err(error),
		};
		match outcome {
			Ok(()) if row.status == ConnectorItemStatus::Ingested => {
				report.ingested += 1;
				app.wake_watcher.notify_one();
				continue;
			}
			Ok(()) => report.skipped += 1,
			Err(error) => {
				row.status = ConnectorItemStatus::Failed;
				row.sha256 = None;
				report.failed += 1;
				report.errors.push(format!(
					"{}: {error}",
					doc.title.as_deref().unwrap_or(&doc.file_name)
				));
				row.error = Some(error);
			}
		}
		blocking(app, move |app| {
			Ok(app.index().connector_record(&row, None)?)
		})
		.await?;
	}
	Ok(report)
}

/// The last component of the name a site gave, without what a file name cannot hold.
fn inbox_name(given: &str) -> String {
	let name: String = given
		.rsplit(['/', '\\'])
		.next()
		.unwrap_or_default()
		.chars()
		.filter(|c| !c.is_control())
		.collect();
	let name = name.trim().trim_start_matches('.');
	if name.is_empty() {
		"document".into()
	} else {
		name.into()
	}
}

/// Writes the file into the inbox (through a hidden temporary file and a rename) and records
/// the remote document with its provenance.
fn deliver(
	app: &App,
	row: &ConnectorItemRecord,
	file_name: &str,
	bytes: &[u8],
	hint: &ConnectorHint,
	owner: Option<&str>,
) -> Result<()> {
	let temp = app
		.library
		.inbox()
		.join(format!(".connector-{}.part", Uuid::new_v4().simple()));
	let written = std::fs::write(&temp, bytes)
		.and_then(|()| std::fs::File::open(&temp)?.sync_all())
		.map_err(violet_core::Error::from)
		.and_then(|()| app.library.into_inbox(&temp, Path::new(file_name)));
	if let Err(error) = written {
		let _ = std::fs::remove_file(&temp);
		return Err(Error::Internal(format!(
			"cannot write {file_name} to the inbox: {error}"
		)));
	}
	let mut index = app.index();
	index.connector_record(row, Some(hint))?;
	if let (Some(owner), Some(sha256)) = (owner, &row.sha256) {
		index.set_uploader(sha256, owner)?;
	}
	Ok(())
}

fn signed_in(connectors: &Connectors, name: &str, connector: &dyn Connector) -> Result<LoginView> {
	connectors.save(name, connector)?;
	connectors.store.set_needs_login(name, false)?;
	connectors.wake(name);
	Ok(LoginView {
		name: name.into(),
		auth: Auth::Ready,
		prompt: None,
	})
}

/// Signs in from the settings. When the site sends a code, the half-done login is kept for
/// [`submit_code`], for fifteen minutes.
pub async fn login(connectors: &Connectors, name: &str) -> Result<LoginView> {
	let (config, slot) = connectors.known(name)?;
	let _busy = slot.busy.lock().await;
	slot.live().pending = None;
	let failed = |error: anyhow::Error| Error::Conflict(format!("could not sign in: {error:#}"));
	let credentials = credentials(config).map_err(failed)?;
	let mut connector = (slot.factory)(config).map_err(failed)?;
	let saved = connectors.saved(name)?;
	match connector.login(&credentials, saved).await.map_err(failed)? {
		Login::Ready => signed_in(connectors, name, connector.as_ref()),
		Login::NeedsCode { prompt } => {
			slot.live().pending = Some(Pending {
				connector,
				prompt: prompt.clone(),
				started: Instant::now(),
			});
			Ok(LoginView {
				name: name.into(),
				auth: Auth::NeedsCode,
				prompt: Some(prompt),
			})
		}
	}
}

/// Gives the code to the login that waits for it. A refused code can be tried again.
pub async fn submit_code(connectors: &Connectors, name: &str, code: &str) -> Result<LoginView> {
	let (_, slot) = connectors.known(name)?;
	let code = code.trim();
	if code.is_empty() || code.chars().count() > MAX_CODE_CHARS {
		return Err(Error::Invalid("enter the code the site sent".into()));
	}
	let _busy = slot.busy.lock().await;
	let Some(mut pending) = slot.live().pending.take() else {
		return Err(Error::Conflict(
			"no sign-in is waiting for a code; connect again".into(),
		));
	};
	match pending.connector.submit_code(code).await {
		Ok(()) => signed_in(connectors, name, pending.connector.as_ref()),
		Err(error) => {
			slot.live().pending = Some(pending);
			Err(Error::Invalid(format!(
				"the code was not accepted: {error:#}"
			)))
		}
	}
}

/// Connectors, their schedules and last runs, and the newest remote documents handled.
pub fn view(app: &App) -> Result<ConnectorsView> {
	let index = app.index();
	let items = index.connector_items(None, ITEMS_SHOWN)?;
	let Some(connectors) = app.connectors() else {
		return Ok(ConnectorsView {
			enabled: false,
			utc_offset: None,
			connectors: Vec::new(),
			items,
		});
	};
	let now = OffsetDateTime::now_utc();
	let mut views = Vec::new();
	for (config, (_, slot)) in connectors.config.connectors.iter().zip(&connectors.slots) {
		let name = &config.name;
		let mut counts = Counts::default();
		for (status, count) in index.connector_counts(name)? {
			match status {
				ConnectorItemStatus::Ingested => counts.ingested = count,
				ConnectorItemStatus::Skipped => counts.skipped = count,
				ConnectorItemStatus::Failed => counts.failed = count,
			}
		}
		let run = connectors.store.run(name)?;
		let (running, prompt) = {
			let live = slot.live();
			(
				live.running,
				live.pending.as_ref().map(|pending| pending.prompt.clone()),
			)
		};
		let never_signed_in = run.last_success.is_none() && connectors.store.state(name)?.is_none();
		let auth = if prompt.is_some() {
			Auth::NeedsCode
		} else if run.needs_login || never_signed_in {
			Auth::NeedsLogin
		} else {
			Auth::Ready
		};
		views.push(ConnectorView {
			name: name.clone(),
			kind: config.kind.clone(),
			schedule: config.schedule.to_string(),
			next_run: connectors.next_run(name, now)?,
			running,
			last_run: timestamp(run.last_run),
			last_success: timestamp(run.last_success),
			last_error: run.last_error,
			last_report: run
				.last_report
				.and_then(|report| serde_json::from_str(&report).ok()),
			auth,
			prompt,
			counts,
		});
	}
	Ok(ConnectorsView {
		enabled: true,
		utc_offset: Some(offset_text(connectors.config.utc_offset)),
		connectors: views,
		items,
	})
}

/// Forgets a handled remote document and runs its connector, which fetches it again if the
/// site still lists it.
pub fn refetch(app: &App, id: i64) -> Result<ConnectorItem> {
	let item = app
		.index()
		.connector_forget(id)?
		.ok_or_else(|| Error::NotFound(format!("connector item {id}")))?;
	if let Some(connectors) = app.connectors() {
		connectors.wake(&item.connector);
	}
	Ok(item)
}

#[cfg(test)]
mod tests {
	use super::*;

	struct Asks;

	impl Connector for Asks {
		fn login<'a>(
			&'a mut self,
			_: &'a Credentials,
			_: Option<serde_json::Value>,
		) -> BoxFuture<'a, anyhow::Result<Login>> {
			Box::pin(async {
				Ok(Login::NeedsCode {
					prompt: "check your mail".into(),
				})
			})
		}

		fn submit_code<'a>(&'a mut self, _: &'a str) -> BoxFuture<'a, anyhow::Result<()>> {
			Box::pin(async { Ok(()) })
		}

		fn state(&self) -> Option<serde_json::Value> {
			None
		}

		fn list(&mut self) -> BoxFuture<'_, anyhow::Result<Vec<RemoteDoc>>> {
			Box::pin(async { Ok(Vec::new()) })
		}

		fn fetch<'a>(&'a mut self, _: &'a str) -> BoxFuture<'a, anyhow::Result<Vec<u8>>> {
			Box::pin(async { Ok(Vec::new()) })
		}
	}

	#[tokio::test]
	async fn a_login_left_waiting_for_its_code_is_dropped_after_a_while() {
		let dir = tempfile::tempdir().unwrap();
		let config = Config::parse(
			r#"{"connectors": [{"name": "a", "kind": "asks", "username_env": "PATH",
				"password_env": "PATH", "schedule": "every 1d"}]}"#,
		)
		.unwrap();
		let kinds = Kinds::new().with("asks", |_| Ok(Box::new(Asks)));
		let connectors =
			Connectors::new(config, &kinds, &dir.path().join(".connectors.sqlite")).unwrap();
		let now = OffsetDateTime::now_utc();
		assert_eq!(login(&connectors, "a").await.unwrap().auth, Auth::NeedsCode);
		assert_eq!(connectors.next_run("a", now).unwrap(), None);

		let slot = connectors.slot("a").unwrap();
		let Some(long_ago) = Instant::now().checked_sub(CODE_WAIT) else {
			return;
		};
		slot.live.lock().unwrap().pending.as_mut().unwrap().started = long_ago;
		assert!(slot.live().pending.is_none());
		assert_eq!(connectors.next_run("a", now).unwrap(), Some(now));
		assert!(matches!(
			submit_code(&connectors, "a", "123456").await,
			Err(Error::Conflict(_))
		));
	}

	#[test]
	fn names_from_a_site_stay_inside_the_inbox() {
		for (given, expected) in [
			("2026-10.pdf", "2026-10.pdf"),
			("../../etc/passwd", "passwd"),
			("a\\b\\給与明細.pdf", "給与明細.pdf"),
			(" .hidden\n.pdf ", "hidden.pdf"),
			("dir/", "document"),
			("..", "document"),
			("", "document"),
		] {
			assert_eq!(inbox_name(given), expected, "{given:?}");
		}
	}

	#[test]
	fn offsets_are_written_as_configured() {
		assert_eq!(offset_text(UtcOffset::UTC), "+00:00");
		assert_eq!(offset_text(time::macros::offset!(+9)), "+09:00");
		assert_eq!(offset_text(time::macros::offset!(-3:30)), "-03:30");
	}
}
