//! Brings documents from elsewhere into `inbox/` on a schedule. A kind is either one site, a
//! [`Connector`] that signs in, lists what the site holds and fetches a document, or a
//! [`Runner`] that does a whole run its own way, as mailboxes do. Everything else is here: the
//! connectors users add in the settings, their owners, their schedules and last runs, and for
//! sites what is kept between runs, which documents were already taken, the inbox and where
//! each file came from.

pub mod config;
pub mod kinds;
pub mod moneyforward;
pub mod schedule;
pub mod session;

use std::collections::HashSet;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use anyhow::anyhow;
use serde::Serialize;
use time::OffsetDateTime;
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use uuid::Uuid;
use violet_core::{
	ConnectorAdded, ConnectorHint, ConnectorItem, ConnectorItemRecord, ConnectorItemStatus,
	ConnectorSource, ConnectorStore, hash,
};

pub use config::{ConnectorConfig, ConnectorInput};
pub use schedule::Schedule;

use crate::app::blocking;
use crate::auth::User;
use crate::{App, Error, Result};

const RETIRED_CONFIG_ENV: &str = "VIOLET_CONNECTORS_CONFIG";
const DEFAULT_SCHEDULE: &str = "daily 09:00";
const MAX_DOCUMENT_BYTES: usize = 100 * 1024 * 1024;
const CODE_WAIT: Duration = Duration::from_secs(15 * 60);
const RECHECK: Duration = Duration::from_secs(10 * 60);
const ITEMS_SHOWN: usize = 50;
const MAX_CODE_CHARS: usize = 64;
const NEEDS_LOGIN: &str = "the site asks for a code; connect from the settings";
const NEEDS_CODE: &str = "a sign-in is waiting for its code";
const CHANGED: &str = "the connector was changed meanwhile";
const SIGNS_IN_BY_ITSELF: &str =
	"a connector of this kind signs in by itself at each run; there is nothing to connect";

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// What to sign in with.
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
/// errors are shown to the connector's owner and logged, so they must hold no credential,
/// cookie or token.
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

/// One run of a connector whose kind is a [`Runner`].
pub struct Run<'a> {
	pub app: &'a Arc<App>,
	pub config: &'a ConnectorConfig,
	/// The connectors of this kind and this owner, this one included.
	pub siblings: Vec<String>,
	/// False once the connector was edited or removed; the run should stop then.
	pub still: &'a (dyn Fn() -> bool + Send + Sync),
}

/// A kind that runs its own way instead of listing and fetching documents: it signs in at
/// each run with the connector's username and password and never asks for a code, and it keeps
/// its own records of what it took, under the connector's name. Its errors are shown to the
/// connector's owner and logged, so they must hold no credential.
pub trait Runner: Send + Sync {
	/// Refuses options the kind does not know.
	fn check(&self, config: &ConnectorConfig) -> anyhow::Result<()>;

	fn run<'a>(&'a self, run: Run<'a>) -> BoxFuture<'a, anyhow::Result<Report>>;

	/// How many things the connector of that name handled, by how they ended.
	fn counts(&self, index: &violet_core::Index, name: &str) -> Result<Counts>;

	/// Whether records are kept under that name.
	fn kept(&self, index: &violet_core::Index, name: &str) -> Result<bool>;

	/// Forgets the records kept under that name.
	fn forget(&self, index: &mut violet_core::Index, name: &str) -> Result<()>;
}

#[derive(Clone)]
enum How {
	Documents(Factory),
	Runs(Arc<dyn Runner>),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Choice {
	pub value: String,
	pub label: String,
}

/// How the settings ask for an option of a kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OptionInput {
	/// A line of text, sent as a string, left out when empty unless `required`.
	Text {
		default: Option<String>,
		required: bool,
		placeholder: Option<String>,
	},
	/// A whole number, sent as a number or left out.
	Number {
		default: Option<i64>,
		min: Option<i64>,
		max: Option<i64>,
	},
	/// Sent as true or false.
	Boolean { default: bool },
	/// One of `choices`, sent as its value.
	Choice {
		choices: Vec<Choice>,
		default: String,
	},
	/// Any of `choices`, sent as an array of their values; `default` when the option is left out.
	Choices {
		choices: Vec<Choice>,
		default: Vec<String>,
	},
	/// A day, sent as `YYYY-MM-DD` or left out.
	Date,
	/// Lines of text, sent as an array of strings.
	TextList,
	/// A group of `fields` filled in at least `min` times, sent as an array of objects with a
	/// key per field; `item` is what one of them is called.
	Group {
		item: String,
		fields: Vec<OptionSpec>,
		min: u32,
	},
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OptionSpec {
	pub name: String,
	pub label: String,
	pub help: Option<String>,
	#[serde(flatten)]
	pub input: OptionInput,
}

/// A kind as the settings show it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KindView {
	pub kind: String,
	pub label: String,
	/// Whether a connector of this kind is connected from the settings before it runs, with a
	/// code when the site asks for one; false when it signs in by itself at each run.
	pub signs_in: bool,
	/// The schedule the form starts from.
	pub schedule: String,
	pub options: Vec<OptionSpec>,
}

impl From<&str> for KindView {
	fn from(kind: &str) -> Self {
		Self {
			kind: kind.into(),
			label: kind.into(),
			signs_in: true,
			schedule: DEFAULT_SCHEDULE.into(),
			options: Vec::new(),
		}
	}
}

/// The kinds a connector may be of.
#[derive(Clone, Default)]
pub struct Kinds(Vec<(KindView, How)>);

impl Kinds {
	pub fn new() -> Self {
		Self::default()
	}

	pub fn with(
		mut self,
		kind: impl Into<KindView>,
		factory: impl Fn(&ConnectorConfig) -> anyhow::Result<Box<dyn Connector>> + Send + Sync + 'static,
	) -> Self {
		let mut kind = kind.into();
		kind.signs_in = true;
		self.0.push((kind, How::Documents(Arc::new(factory))));
		self
	}

	pub fn with_runner(mut self, kind: impl Into<KindView>, runner: impl Runner + 'static) -> Self {
		let mut kind = kind.into();
		kind.signs_in = false;
		self.0.push((kind, How::Runs(Arc::new(runner))));
		self
	}

	fn get(&self, kind: &str) -> anyhow::Result<&How> {
		self.0
			.iter()
			.find(|(view, _)| view.kind == kind)
			.map(|(_, how)| how)
			.ok_or_else(|| anyhow!("unknown kind \"{kind}\" (known: {})", self.names()))
	}

	fn runner(&self, kind: &str) -> Option<Arc<dyn Runner>> {
		match self.get(kind) {
			Ok(How::Runs(runner)) => Some(runner.clone()),
			_ => None,
		}
	}

	fn runners(&self) -> impl Iterator<Item = &Arc<dyn Runner>> {
		self.0.iter().filter_map(|(_, how)| match how {
			How::Runs(runner) => Some(runner),
			How::Documents(_) => None,
		})
	}

	fn check(&self, config: &ConnectorConfig) -> anyhow::Result<()> {
		match self.get(&config.kind)? {
			How::Documents(factory) => factory(config).map(drop),
			How::Runs(runner) => runner.check(config),
		}
	}

	fn names(&self) -> String {
		let names: Vec<&str> = self.0.iter().map(|(view, _)| view.kind.as_str()).collect();
		if names.is_empty() {
			"none".into()
		} else {
			names.join(", ")
		}
	}

	fn connector(&self, config: &ConnectorConfig) -> anyhow::Result<Box<dyn Connector>> {
		match self.get(&config.kind)? {
			How::Documents(factory) => factory(config),
			How::Runs(_) => Err(anyhow!(SIGNS_IN_BY_ITSELF)),
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
	config: Arc<ConnectorConfig>,
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
	name: String,
	/// None once the connector is removed. Held while anything about it is written to the
	/// store, so that a run or a login that began before an edit writes nothing after it.
	config: Mutex<Option<Arc<ConnectorConfig>>>,
	wake: Notify,
	changed: Notify,
	busy: tokio::sync::Mutex<()>,
	live: Mutex<Live>,
}

impl Slot {
	fn new(config: ConnectorConfig) -> Arc<Self> {
		Arc::new(Self {
			name: config.name.clone(),
			config: Mutex::new(Some(Arc::new(config))),
			wake: Notify::new(),
			changed: Notify::new(),
			busy: tokio::sync::Mutex::new(()),
			live: Mutex::new(Live::default()),
		})
	}

	fn current(&self) -> MutexGuard<'_, Option<Arc<ConnectorConfig>>> {
		self.config.lock().unwrap_or_else(PoisonError::into_inner)
	}

	fn config(&self) -> Option<Arc<ConnectorConfig>> {
		self.current().clone()
	}

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

	/// Does `work` unless the connector was edited or removed since `config` was read.
	fn while_still<T>(
		&self,
		config: &Arc<ConnectorConfig>,
		work: impl FnOnce() -> Result<T>,
	) -> Result<T> {
		let current = self.current();
		if current
			.as_ref()
			.is_some_and(|current| Arc::ptr_eq(current, config))
		{
			work()
		} else {
			Err(Error::Conflict(CHANGED.into()))
		}
	}

	fn wait_for_code(&self, pending: Pending) -> Result<()> {
		let config = pending.config.clone();
		self.while_still(&config, || {
			self.live().pending = Some(pending);
			Ok(())
		})
	}
}

/// The connectors of a running server.
pub struct Connectors {
	store: ConnectorStore,
	kinds: Kinds,
	slots: Mutex<Vec<Arc<Slot>>>,
	looping: AtomicBool,
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
	pub owner: Option<String>,
	pub username: String,
	pub password_set: bool,
	pub schedule: String,
	/// The offset the clock times of the schedule are in, as `+09:00`.
	pub utc_offset: String,
	pub area: Option<String>,
	pub tags: Vec<String>,
	pub options: serde_json::Value,
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

fn missing(name: &str) -> Error {
	Error::NotFound(format!("connector {name}"))
}

/// Admins see every connector, anyone else their own.
fn visible(user: &User, config: &ConnectorConfig) -> bool {
	user.is_admin() || (user.name.is_some() && user.name == config.owner)
}

/// Who a connector is for: `kept` unless the request asks for someone else, which only an
/// admin may, and only for a user there is.
fn owner(
	app: &App,
	asked: Option<&str>,
	user: &User,
	kept: Option<String>,
) -> Result<Option<String>> {
	let Some(asked) = asked.map(str::trim).filter(|asked| !asked.is_empty()) else {
		return Ok(kept);
	};
	if kept.as_deref() == Some(asked) {
		return Ok(kept);
	}
	if !user.is_admin() {
		return Err(Error::Forbidden(
			"only an admin chooses the owner of a connector".into(),
		));
	}
	let known = app
		.auth
		.users()?
		.is_some_and(|users| users.get(asked).is_some());
	if !known {
		return Err(Error::Invalid(format!("there is no user named {asked}")));
	}
	Ok(Some(asked.to_string()))
}

impl Connectors {
	/// Opens the connectors kept at `store`. None runs before [`spawn`].
	pub fn open(kinds: Kinds, store: &Path) -> anyhow::Result<Arc<Self>> {
		let store = ConnectorStore::open(store)
			.map_err(|error| anyhow!("cannot open {}: {error}", store.display()))?;
		let mut slots = Vec::new();
		for definition in store.definitions()? {
			let name = definition.name.clone();
			match ConnectorConfig::stored(definition) {
				Ok(config) => slots.push(Slot::new(config)),
				Err(error) => {
					tracing::error!(connector = name, error, "a stored connector is left out");
				}
			}
		}
		Ok(Arc::new(Self {
			store,
			kinds,
			slots: Mutex::new(slots),
			looping: AtomicBool::new(false),
		}))
	}

	fn slots(&self) -> MutexGuard<'_, Vec<Arc<Slot>>> {
		self.slots.lock().unwrap_or_else(PoisonError::into_inner)
	}

	fn slot(&self, name: &str) -> Option<Arc<Slot>> {
		self.slots().iter().find(|slot| slot.name == name).cloned()
	}

	fn known(&self, name: &str) -> Result<(Arc<ConnectorConfig>, Arc<Slot>)> {
		let slot = self.slot(name).ok_or_else(|| missing(name))?;
		let config = slot.config().ok_or_else(|| missing(name))?;
		Ok((config, slot))
	}

	/// Refuses a connector `user` may not see as one that is not there.
	pub fn authorize(&self, name: &str, user: &User) -> Result<()> {
		let (config, _) = self.known(name)?;
		if visible(user, &config) {
			Ok(())
		} else {
			Err(missing(name))
		}
	}

	pub fn kinds(&self) -> Vec<KindView> {
		self.kinds.0.iter().map(|(view, _)| view.clone()).collect()
	}

	fn checked(&self, config: &ConnectorConfig) -> Result<()> {
		self.kinds
			.check(config)
			.map_err(|error| Error::Invalid(format!("{error:#}")))
	}

	/// The connectors of one kind that `user` sees, by name.
	pub fn names_of(&self, kind: &str, user: &User) -> Vec<String> {
		self.slots()
			.iter()
			.filter_map(|slot| slot.config())
			.filter(|config| config.kind == kind && visible(user, config))
			.map(|config| config.name.clone())
			.collect()
	}

	fn siblings(&self, of: &ConnectorConfig) -> Vec<String> {
		self.slots()
			.iter()
			.filter_map(|slot| slot.config())
			.filter(|config| config.kind == of.kind && config.owner == of.owner)
			.map(|config| config.name.clone())
			.collect()
	}

	/// Adds a connector. Under the name of one somebody else removed it starts from nothing:
	/// what that one handled is forgotten first, so none of it is shown or held back. A name
	/// that holds records from before it was a connector, as a mail account of the retired
	/// config file does, is for an admin to take.
	fn add(&self, app: &App, input: ConnectorInput, user: &User) -> Result<(Arc<Slot>, bool)> {
		let owner = owner(app, input.owner.as_deref(), user, user.name.clone())?;
		let config = input.added(owner).map_err(Error::Invalid)?;
		self.checked(&config)?;
		let mut slots = self.slots();
		match self.store.removed_by(&config.name)? {
			Some(removed_by) if removed_by != config.owner => {
				let mut index = app.index();
				index.connector_forget_all(&config.name)?;
				for runner in self.kinds.runners() {
					runner.forget(&mut index, &config.name)?;
				}
			}
			Some(_) => {}
			None if user.is_admin() => {}
			None => {
				let index = app.index();
				for runner in self.kinds.runners() {
					if runner.kept(&index, &config.name)? {
						return Err(Error::Conflict(format!(
							"the name {} holds earlier records; an admin can add this connector",
							config.name
						)));
					}
				}
			}
		}
		if self.store.create(&config.definition())? == ConnectorAdded::Taken {
			return Err(Error::Conflict(format!(
				"a connector named {} exists already",
				config.name
			)));
		}
		if self.kinds.runner(&config.kind).is_some() {
			self.store.set_needs_login(&config.name, false)?;
		}
		let slot = Slot::new(config);
		slots.push(slot.clone());
		Ok((slot, self.looping.load(Ordering::SeqCst)))
	}

	/// Replaces what a connector is. A new username or password drops what it saved to sign
	/// in, so it needs a login again. An admin may give it to another user, who then sees what
	/// it handled.
	pub fn replace(&self, app: &App, name: &str, input: ConnectorInput, user: &User) -> Result<()> {
		let slot = self.slot(name).ok_or_else(|| missing(name))?;
		{
			let mut current = slot.current();
			let old = current
				.clone()
				.filter(|config| visible(user, config))
				.ok_or_else(|| missing(name))?;
			let owner = owner(app, input.owner.as_deref(), user, old.owner.clone())?;
			let new = input.replacing(&old, owner).map_err(Error::Invalid)?;
			self.checked(&new)?;
			let sign_out = !new.signs_in_like(&old) && self.kinds.runner(&new.kind).is_none();
			if !self.store.update(&new.definition(), sign_out)? {
				return Err(missing(name));
			}
			*current = Some(Arc::new(new));
		}
		{
			let mut live = slot.live();
			live.pending = None;
			live.failures = 0;
			live.retry_at = None;
		}
		slot.changed.notify_one();
		Ok(())
	}

	/// Drops a connector with what it signs in with and what it saved. The remote documents it
	/// handled stay known, so its owner adding it again under its name does not fetch them
	/// again.
	pub fn remove(&self, name: &str, user: &User) -> Result<()> {
		let slot = {
			let mut slots = self.slots();
			let at = slots
				.iter()
				.position(|slot| slot.name == name)
				.ok_or_else(|| missing(name))?;
			let slot = slots[at].clone();
			let mut current = slot.current();
			if !current.as_ref().is_some_and(|config| visible(user, config)) {
				return Err(missing(name));
			}
			self.store.remove(name)?;
			*current = None;
			drop(current);
			slots.remove(at);
			slot
		};
		slot.live().pending = None;
		slot.changed.notify_one();
		Ok(())
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
		self.due(&config, &slot, now)
	}

	fn due(
		&self,
		config: &ConnectorConfig,
		slot: &Slot,
		now: OffsetDateTime,
	) -> Result<Option<OffsetDateTime>> {
		let (retry_at, waits_for_code) = {
			let live = slot.live();
			(live.retry_at, live.pending.is_some())
		};
		let run = self.store.run(&config.name)?;
		if waits_for_code || run.needs_login {
			return Ok(None);
		}
		Ok(Some(schedule::next_run(
			&config.schedule,
			config.utc_offset,
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

	fn save(
		&self,
		slot: &Slot,
		config: &Arc<ConnectorConfig>,
		connector: &dyn Connector,
	) -> Result<()> {
		let Some(state) = connector.state() else {
			return Ok(());
		};
		slot.while_still(config, || {
			Ok(self
				.store
				.set_state(&config.name, Some(&state.to_string()))?)
		})
	}

	fn set_needs_login(
		&self,
		slot: &Slot,
		config: &Arc<ConnectorConfig>,
		needs_login: bool,
	) -> Result<()> {
		slot.while_still(config, || {
			Ok(self.store.set_needs_login(&config.name, needs_login)?)
		})
	}

	fn finish(&self, slot: &Slot, config: &Arc<ConnectorConfig>, outcome: &anyhow::Result<Report>) {
		let name = &config.name;
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
		let recorded = slot.while_still(config, || {
			Ok(self.store.finish_run(
				name,
				now.unix_timestamp(),
				error.as_deref(),
				report.as_deref(),
			)?)
		});
		match recorded {
			Ok(()) | Err(Error::Conflict(_)) => {}
			Err(error) => {
				tracing::error!(connector = name, %error, "the run could not be recorded");
			}
		}
	}
}

/// Opens the connectors of the library and starts one loop per connector.
pub fn start(app: &Arc<App>) -> anyhow::Result<Vec<JoinHandle<()>>> {
	for (variable, instead) in [
		(RETIRED_CONFIG_ENV, "connectors are added in the settings"),
		(
			crate::mail::RETIRED_CONFIG_ENV,
			"mailboxes are connectors of the kind imap, added in the settings",
		),
	] {
		if std::env::var_os(variable).is_some_and(|path| !path.is_empty()) {
			tracing::warn!("{variable} is set and ignored: {instead}");
		}
	}
	let connectors = Connectors::open(kinds::builtin(), &app.library.connectors_path())?;
	app.set_connectors(connectors.clone())
		.map_err(|_| anyhow!("connectors are already set up"))?;
	Ok(spawn(app, &connectors))
}

/// One loop per connector, each running it when it is due or woken, and one more for each
/// connector added from now on.
pub fn spawn(app: &Arc<App>, connectors: &Arc<Connectors>) -> Vec<JoinHandle<()>> {
	let slots = {
		let slots = connectors.slots();
		connectors.looping.store(true, Ordering::SeqCst);
		slots.clone()
	};
	slots
		.into_iter()
		.map(|slot| tokio::spawn(run_loop(app.clone(), connectors.clone(), slot)))
		.collect()
}

async fn run_loop(app: Arc<App>, connectors: Arc<Connectors>, slot: Arc<Slot>) {
	loop {
		let Some(config) = slot.config() else {
			return;
		};
		let now = OffsetDateTime::now_utc();
		let wait = match connectors.due(&config, &slot, now) {
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
				() = slot.changed.notified() => continue,
				() = tokio::time::sleep(wait) => continue,
			}
		}
		let _ = run_slot(&app, &connectors, &slot).await;
	}
}

/// Runs one connector now, after any run or login of it already going on: signs in with what
/// is saved, fetches the documents not taken yet into the inbox, or lets its kind do the run.
/// Never waits for a code.
pub async fn run(app: &Arc<App>, connectors: &Connectors, name: &str) -> anyhow::Result<Report> {
	let slot = connectors
		.slot(name)
		.ok_or_else(|| anyhow!("no connector named {name}"))?;
	run_slot(app, connectors, &slot).await
}

async fn run_slot(app: &Arc<App>, connectors: &Connectors, slot: &Slot) -> anyhow::Result<Report> {
	let _busy = slot.busy.lock().await;
	let name = slot.name.as_str();
	let config = slot
		.config()
		.ok_or_else(|| anyhow!("no connector named {name}"))?;
	{
		let mut live = slot.live();
		if live.pending.is_some() {
			return Err(anyhow!(NEEDS_CODE));
		}
		live.running = true;
	}
	let outcome = run_connector(app, connectors, &config, slot).await;
	connectors.finish(slot, &config, &outcome);
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
	config: &Arc<ConnectorConfig>,
	slot: &Slot,
) -> anyhow::Result<Report> {
	if let Some(runner) = connectors.kinds.runner(&config.kind) {
		let run = Run {
			app,
			config,
			siblings: connectors.siblings(config),
			still: &|| slot.while_still(config, || Ok(())).is_ok(),
		};
		return runner.run(run).await;
	}
	let mut connector = connectors.kinds.connector(config)?;
	let saved = connectors.saved(&config.name)?;
	match connector.login(&config.credentials(), saved).await? {
		Login::Ready => {}
		Login::NeedsCode { .. } => {
			connectors.set_needs_login(slot, config, true)?;
			return Err(anyhow!(NEEDS_LOGIN));
		}
	}
	connectors.save(slot, config, connector.as_ref())?;
	connectors.set_needs_login(slot, config, false)?;

	let outcome = fetch_new(app, config, slot, connector.as_mut()).await;
	connectors.save(slot, config, connector.as_ref())?;
	outcome
}

async fn fetch_new(
	app: &Arc<App>,
	config: &Arc<ConnectorConfig>,
	slot: &Slot,
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
		slot.while_still(config, || Ok(()))?;
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
					area: config.area.clone(),
					tags: config.tags.clone(),
				};
				let (delivered, owner) = (row.clone(), config.owner.clone());
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

fn signed_in(
	connectors: &Connectors,
	slot: &Slot,
	config: &Arc<ConnectorConfig>,
	connector: &dyn Connector,
) -> Result<LoginView> {
	connectors.save(slot, config, connector)?;
	connectors.set_needs_login(slot, config, false)?;
	slot.wake.notify_one();
	Ok(LoginView {
		name: config.name.clone(),
		auth: Auth::Ready,
		prompt: None,
	})
}

/// Signs in from the settings. When the site sends a code, the half-done login is kept for
/// [`submit_code`], for fifteen minutes.
pub async fn login(connectors: &Connectors, name: &str) -> Result<LoginView> {
	let slot = connectors.slot(name).ok_or_else(|| missing(name))?;
	let _busy = slot.busy.lock().await;
	let config = slot.config().ok_or_else(|| missing(name))?;
	if connectors.kinds.runner(&config.kind).is_some() {
		return Err(Error::Invalid(SIGNS_IN_BY_ITSELF.into()));
	}
	slot.live().pending = None;
	let failed = |error: anyhow::Error| Error::Conflict(format!("could not sign in: {error:#}"));
	let mut connector = connectors.kinds.connector(&config).map_err(failed)?;
	let saved = connectors.saved(name)?;
	match connector
		.login(&config.credentials(), saved)
		.await
		.map_err(failed)?
	{
		Login::Ready => signed_in(connectors, &slot, &config, connector.as_ref()),
		Login::NeedsCode { prompt } => {
			slot.wait_for_code(Pending {
				connector,
				config,
				prompt: prompt.clone(),
				started: Instant::now(),
			})?;
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
		Ok(()) => signed_in(
			connectors,
			&slot,
			&pending.config,
			pending.connector.as_ref(),
		),
		Err(error) => {
			slot.wait_for_code(pending)?;
			Err(Error::Invalid(format!(
				"the code was not accepted: {error:#}"
			)))
		}
	}
}

/// Adds the connector `user` describes, theirs unless an admin names another owner, and starts
/// running it.
pub async fn create(
	app: &Arc<App>,
	connectors: &Arc<Connectors>,
	input: ConnectorInput,
	user: &User,
) -> Result<ConnectorView> {
	let (slot, looping) = {
		let (connectors, user) = (connectors.clone(), user.clone());
		blocking(app, move |app| connectors.add(app, input, &user)).await?
	};
	if looping {
		tokio::spawn(run_loop(app.clone(), connectors.clone(), slot.clone()));
	}
	let name = slot.name.clone();
	let connectors = connectors.clone();
	blocking(app, move |app| one(app, &connectors, &name)).await
}

fn connector_view(
	connectors: &Connectors,
	index: &violet_core::Index,
	config: &ConnectorConfig,
	slot: &Slot,
	now: OffsetDateTime,
) -> Result<ConnectorView> {
	let name = &config.name;
	let runner = connectors.kinds.runner(&config.kind);
	let mut counts = Counts::default();
	if let Some(runner) = &runner {
		counts = runner.counts(index, name)?;
	} else {
		for (status, count) in index.connector_counts(name)? {
			match status {
				ConnectorItemStatus::Ingested => counts.ingested = count,
				ConnectorItemStatus::Skipped => counts.skipped = count,
				ConnectorItemStatus::Failed => counts.failed = count,
			}
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
	let auth = if runner.is_some() {
		Auth::Ready
	} else if prompt.is_some() {
		Auth::NeedsCode
	} else if run.needs_login || never_signed_in {
		Auth::NeedsLogin
	} else {
		Auth::Ready
	};
	Ok(ConnectorView {
		name: name.clone(),
		kind: config.kind.clone(),
		owner: config.owner.clone(),
		username: config.username.clone(),
		password_set: true,
		schedule: config.schedule.to_string(),
		utc_offset: schedule::offset_text(config.utc_offset),
		area: config.area.clone(),
		tags: config.tags.clone(),
		options: config.options.clone(),
		next_run: connectors.due(config, slot, now)?,
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
	})
}

/// One connector as the settings show it.
pub fn one(app: &App, connectors: &Connectors, name: &str) -> Result<ConnectorView> {
	let (config, slot) = connectors.known(name)?;
	connector_view(
		connectors,
		&app.index(),
		&config,
		&slot,
		OffsetDateTime::now_utc(),
	)
}

/// The connectors `user` sees, their schedules and last runs, and the newest remote documents
/// they handled.
pub fn view(app: &App, connectors: &Connectors, user: &User) -> Result<ConnectorsView> {
	let slots = connectors.slots().clone();
	let index = app.index();
	let now = OffsetDateTime::now_utc();
	let mut views = Vec::new();
	for slot in &slots {
		if let Some(config) = slot.config().filter(|config| visible(user, config)) {
			views.push(connector_view(connectors, &index, &config, slot, now)?);
		}
	}
	let items = if user.is_admin() {
		index.connector_items(None, ITEMS_SHOWN)?
	} else {
		let mut items = Vec::new();
		for connector in &views {
			items.extend(index.connector_items(Some(&connector.name), ITEMS_SHOWN)?);
		}
		items.sort_by_key(|item| std::cmp::Reverse((item.fetched_at, item.id)));
		items.truncate(ITEMS_SHOWN);
		items
	};
	Ok(ConnectorsView {
		connectors: views,
		items,
	})
}

/// Forgets a handled remote document and runs its connector, which fetches it again if the
/// site still lists it.
pub fn refetch(app: &App, connectors: &Connectors, id: i64, user: &User) -> Result<ConnectorItem> {
	let gone = || Error::NotFound(format!("connector item {id}"));
	let item = app.index().connector_item(id)?.ok_or_else(gone)?;
	if !user.is_admin() {
		connectors
			.authorize(&item.connector, user)
			.map_err(|_| gone())?;
	}
	let item = app.index().connector_forget(id)?.ok_or_else(gone)?;
	connectors.wake(&item.connector);
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

	fn asking(dir: &Path) -> (Arc<App>, Arc<Connectors>) {
		let kinds = Kinds::new().with("asks", |_| Ok(Box::new(Asks)));
		let app = App::open(dir.join("root"), crate::Models::default()).unwrap();
		let connectors = Connectors::open(kinds, &app.library.connectors_path()).unwrap();
		let input = serde_json::from_value(serde_json::json!({
			"name": "a", "kind": "asks", "username": "u", "password": "p", "schedule": "every 1d",
		}))
		.unwrap();
		connectors.add(&app, input, &User::implicit()).unwrap();
		(app, connectors)
	}

	#[tokio::test]
	async fn a_login_left_waiting_for_its_code_is_dropped_after_a_while() {
		let dir = tempfile::tempdir().unwrap();
		let (_app, connectors) = asking(dir.path());
		let now = OffsetDateTime::now_utc();
		assert_eq!(login(&connectors, "a").await.unwrap().auth, Auth::NeedsCode);
		assert_eq!(connectors.next_run("a", now).unwrap(), None);

		let slot = connectors.slot("a").unwrap();
		let Some(long_ago) = Instant::now().checked_sub(CODE_WAIT) else {
			return;
		};
		slot.live.lock().unwrap().pending.as_mut().unwrap().started = long_ago;
		assert!(slot.live().pending.is_none());
		assert_eq!(connectors.next_run("a", now).unwrap(), None);
		assert!(matches!(
			submit_code(&connectors, "a", "123456").await,
			Err(Error::Conflict(_))
		));
	}

	#[tokio::test]
	async fn an_edit_or_a_removal_ends_the_login_that_waits_for_a_code() {
		let dir = tempfile::tempdir().unwrap();
		let (app, connectors) = asking(dir.path());
		let admin = User::implicit();
		let edit = || {
			serde_json::from_value(serde_json::json!({
				"username": "u", "schedule": "every 2d",
			}))
			.unwrap()
		};
		login(&connectors, "a").await.unwrap();
		let before = connectors.known("a").unwrap().0;
		connectors.replace(&app, "a", edit(), &admin).unwrap();
		assert!(matches!(
			submit_code(&connectors, "a", "123456").await,
			Err(Error::Conflict(_))
		));
		let slot = connectors.slot("a").unwrap();
		assert!(matches!(
			connectors.set_needs_login(&slot, &before, false),
			Err(Error::Conflict(_))
		));
		assert!(connectors.store.run("a").unwrap().needs_login);

		login(&connectors, "a").await.unwrap();
		connectors.remove("a", &admin).unwrap();
		assert!(slot.live().pending.is_none() && slot.config().is_none());
		assert!(matches!(
			submit_code(&connectors, "a", "123456").await,
			Err(Error::NotFound(_))
		));
		assert!(matches!(
			connectors.remove("a", &admin),
			Err(Error::NotFound(_))
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
}
