//! Takes files from IMAP folders into `inbox/`: each rule's matching attachments, or the
//! whole mail as `.eml`, with where they came from for the pipeline to record. Off unless
//! `VIOLET_MAIL_CONFIG` names a config file.

pub mod config;
pub mod imap;
pub mod mime;

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use anyhow::anyhow;
use serde::Serialize;
use time::OffsetDateTime;
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use uuid::Uuid;
use violet_core::{MailHint, MailRecord, MailSource, MailStatus, ProcessedMail, hash};

pub use config::{Account, Config, MailAction, Rule, Scope, Security};
use imap::{Criteria, Session};

use crate::app::blocking;
use crate::{App, Error, Result};

pub const CONFIG_ENV: &str = "VIOLET_MAIL_CONFIG";
const MAX_MAIL_BYTES: u64 = 100 * 1024 * 1024;
const FIRST_BACKOFF: Duration = Duration::from_secs(30);
const PROCESSED_SHOWN: usize = 50;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Report {
	/// New mails looked at.
	pub mails: u32,
	pub ingested: u32,
	/// Files written to the inbox.
	pub files: u32,
	pub skipped: u32,
	pub failed: u32,
	pub errors: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct AccountStatus {
	pub running: bool,
	#[serde(with = "time::serde::rfc3339::option")]
	pub last_run: Option<OffsetDateTime>,
	#[serde(with = "time::serde::rfc3339::option")]
	pub last_success: Option<OffsetDateTime>,
	pub last_error: Option<String>,
	pub last_report: Option<Report>,
}

struct AccountState {
	wake: Notify,
	running: tokio::sync::Mutex<()>,
	status: Mutex<AccountStatus>,
}

/// The mail sync of a running server.
pub struct Mail {
	pub config: Config,
	states: Vec<(String, AccountState)>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Counts {
	pub ingested: u64,
	pub skipped: u64,
	pub failed: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct AccountView {
	pub name: String,
	pub host: String,
	pub folders: Vec<String>,
	pub interval_secs: u64,
	#[serde(flatten)]
	pub status: AccountStatus,
	pub counts: Counts,
}

#[derive(Debug, Clone, Serialize)]
pub struct MailView {
	pub enabled: bool,
	pub accounts: Vec<AccountView>,
	/// The newest handled mails.
	pub processed: Vec<ProcessedMail>,
}

impl Mail {
	pub fn new(config: Config) -> Arc<Self> {
		let states = config
			.accounts
			.iter()
			.map(|account| {
				(
					account.name.clone(),
					AccountState {
						wake: Notify::new(),
						running: tokio::sync::Mutex::new(()),
						status: Mutex::new(AccountStatus::default()),
					},
				)
			})
			.collect();
		Arc::new(Self { config, states })
	}

	fn state(&self, account: &str) -> Option<&AccountState> {
		self.states
			.iter()
			.find(|(name, _)| name == account)
			.map(|(_, state)| state)
	}

	fn status(&self, account: &str) -> Option<MutexGuard<'_, AccountStatus>> {
		self.state(account)
			.map(|state| state.status.lock().unwrap_or_else(PoisonError::into_inner))
	}

	/// Asks an account's loop to sync now; false when there is no such account.
	pub fn wake(&self, account: &str) -> bool {
		match self.state(account) {
			Some(state) => {
				state.wake.notify_one();
				true
			}
			None => false,
		}
	}
}

/// Reads the config `VIOLET_MAIL_CONFIG` names, if it is set, and starts one sync loop per
/// account.
pub fn start(app: &Arc<App>) -> anyhow::Result<Vec<JoinHandle<()>>> {
	let Some(path) = std::env::var_os(CONFIG_ENV).filter(|path| !path.is_empty()) else {
		return Ok(Vec::new());
	};
	let config = Config::load(Path::new(&path))?;
	let mail = Mail::new(config);
	app.set_mail(mail.clone())
		.map_err(|_| anyhow!("mail sync is already set up"))?;
	tracing::info!(
		accounts = mail.config.accounts.len(),
		rules = mail.config.rules.len(),
		"mail sync on"
	);
	Ok(mail
		.config
		.accounts
		.iter()
		.map(|account| tokio::spawn(run(app.clone(), mail.clone(), account.name.clone())))
		.collect())
}

async fn run(app: Arc<App>, mail: Arc<Mail>, account: String) {
	let Some(interval) = mail
		.config
		.account(&account)
		.map(|account| Duration::from_secs(account.interval_secs))
	else {
		return;
	};
	let mut backoff = FIRST_BACKOFF.min(interval);
	loop {
		let wait = match sync(&app, &mail, &account).await {
			Ok(report) if report.errors.is_empty() => {
				backoff = FIRST_BACKOFF.min(interval);
				interval
			}
			_ => {
				let wait = backoff;
				backoff = (backoff * 2).min(interval);
				wait
			}
		};
		let Some(state) = mail.state(&account) else {
			return;
		};
		tokio::select! {
			() = state.wake.notified() => {}
			() = tokio::time::sleep(wait) => {}
		}
	}
}

/// Syncs one account now, after any sync of it already running.
pub async fn sync(app: &Arc<App>, mail: &Mail, account: &str) -> anyhow::Result<Report> {
	let state = mail
		.state(account)
		.ok_or_else(|| anyhow!("no mail account named {account}"))?;
	let config = mail
		.config
		.account(account)
		.ok_or_else(|| anyhow!("no mail account named {account}"))?;
	let _running = state.running.lock().await;
	if let Some(mut status) = mail.status(account) {
		status.running = true;
	}
	let rules: Vec<&Rule> = mail.config.rules_of(account).collect();
	let result = sync_account(app, config, &rules).await;
	let now = OffsetDateTime::now_utc();
	if let Some(mut status) = mail.status(account) {
		status.running = false;
		status.last_run = Some(now);
		match &result {
			Ok(report) => {
				status.last_report = Some(report.clone());
				if report.errors.is_empty() {
					status.last_success = Some(now);
					status.last_error = None;
				} else {
					status.last_error = Some(report.errors.join("; "));
				}
			}
			Err(error) => status.last_error = Some(format!("{error:#}")),
		}
	}
	match &result {
		Ok(report) if report.errors.is_empty() => {
			if report.mails > 0 {
				tracing::info!(account, ?report, "mail synced");
			}
		}
		Ok(report) => tracing::warn!(account, errors = ?report.errors, "mail synced with errors"),
		Err(error) => tracing::error!(account, error = format!("{error:#}"), "mail sync failed"),
	}
	result
}

async fn sync_account(
	app: &Arc<App>,
	account: &Account,
	rules: &[&Rule],
) -> anyhow::Result<Report> {
	let mut report = Report::default();
	if rules.is_empty() {
		return Ok(report);
	}
	let password = std::env::var(&account.password_env).map_err(|_| {
		anyhow!(
			"the environment variable {} is not set",
			account.password_env
		)
	})?;
	let mut session = Session::connect(account, &password).await?;
	for rule in rules {
		if let Err(error) = sync_rule(app, &mut session, account, rule, &mut report).await {
			report.errors.push(format!("{}: {error:#}", rule.folder));
		}
	}
	session.logout().await;
	Ok(report)
}

fn criteria(rule: &Rule) -> Criteria {
	let today = OffsetDateTime::now_utc().date();
	Criteria {
		since: today.checked_sub(time::Duration::days(i64::from(rule.max_age_days))),
		from: rule.from.clone(),
		to: rule.to.clone(),
		subject: rule.subject.clone(),
		without_keyword: match &rule.action {
			MailAction::Keyword(keyword) => Some(keyword.clone()),
			_ => None,
		},
		unseen: rule.action == MailAction::Seen,
		unflagged: rule.action == MailAction::Flag,
	}
}

async fn record(app: &Arc<App>, record: MailRecord) -> anyhow::Result<i64> {
	Ok(blocking(app, move |app| Ok(app.index().mail_record(&record, &[])?)).await?)
}

async fn sync_rule(
	app: &Arc<App>,
	session: &mut Session,
	account: &Account,
	rule: &Rule,
	report: &mut Report,
) -> anyhow::Result<()> {
	let uidvalidity = session.select(&rule.folder).await?;
	let uids = session.search(&criteria(rule)).await?;
	if uids.is_empty() {
		return Ok(());
	}
	let handled = {
		let (name, folder, found) = (account.name.clone(), rule.folder.clone(), uids.clone());
		blocking(app, move |app| {
			Ok(app
				.index()
				.mail_handled(&name, &folder, uidvalidity, &found)?)
		})
		.await?
	};
	let fresh: Vec<u32> = uids
		.into_iter()
		.filter(|uid| !handled.contains(uid))
		.collect();
	if fresh.is_empty() {
		return Ok(());
	}
	for meta in session.fetch_meta(&fresh).await? {
		report.mails += 1;
		let mut row = MailRecord {
			account: account.name.clone(),
			folder: rule.folder.clone(),
			uidvalidity,
			uid: meta.uid,
			message_id: None,
			sender: None,
			subject: None,
			received: meta.received.clone(),
			status: MailStatus::Skipped,
			files: 0,
			detail: None,
			error: None,
		};
		if meta.size > MAX_MAIL_BYTES {
			row.detail = Some(format!("larger than {} MB", MAX_MAIL_BYTES / 1024 / 1024));
			report.skipped += 1;
			record(app, row).await?;
			continue;
		}
		let raw = session.fetch(meta.uid).await?;
		let parsed = tokio::task::spawn_blocking({
			let rule = rule.clone();
			move || mime::pick(&raw, &rule)
		})
		.await?;
		row.message_id = parsed.message_id.clone();
		row.sender = parsed.from.clone();
		row.subject = parsed.subject.clone();
		if let Some(message_id) = parsed.message_id.clone() {
			let seen = blocking(app, move |app| {
				Ok(app.index().mail_message_handled(&message_id)?)
			})
			.await?;
			if seen {
				row.detail = Some("already taken from another folder".into());
				report.skipped += 1;
				record(app, row).await?;
				continue;
			}
		}
		if parsed.files.is_empty() {
			row.detail = Some(if parsed.left_out.is_empty() {
				"no attachment".into()
			} else {
				parsed.left_out.join("; ")
			});
			report.skipped += 1;
			record(app, row).await?;
			continue;
		}
		let hint = MailHint {
			mail: MailSource {
				account: account.name.clone(),
				folder: rule.folder.clone(),
				uidvalidity,
				uid: meta.uid,
				message_id: parsed.message_id.clone(),
				from: parsed.from.clone(),
				subject: parsed.subject.clone(),
				date: parsed.date.clone(),
			},
			area: rule.hints.area.clone(),
			tags: rule.hints.tags.clone(),
		};
		let files = parsed.files;
		let count = files.len() as u32;
		let mut delivered = row.clone();
		delivered.status = MailStatus::Ingested;
		delivered.files = count;
		delivered.detail = (!parsed.left_out.is_empty()).then(|| parsed.left_out.join("; "));
		let written = blocking(app, move |app| deliver(app, &delivered, files, &hint)).await;
		let id = match written {
			Ok(id) => id,
			Err(error) => {
				row.status = MailStatus::Failed;
				row.error = Some(error.to_string());
				report.failed += 1;
				record(app, row).await?;
				continue;
			}
		};
		report.ingested += 1;
		report.files += count;
		app.wake_watcher.notify_one();
		if let Err(error) = mark(session, meta.uid, &rule.action).await {
			let error = format!("the mail could not be marked on the server: {error:#}");
			blocking(app, move |app| {
				Ok(app.index().mail_set_error(id, &error)?)
			})
			.await?;
			report
				.errors
				.push(format!("{} UID {}: not marked", rule.folder, meta.uid));
		}
	}
	Ok(())
}

async fn mark(session: &mut Session, uid: u32, action: &MailAction) -> anyhow::Result<()> {
	match action {
		MailAction::Keyword(keyword) => session.store(uid, keyword).await,
		MailAction::Seen => session.store(uid, "\\Seen").await,
		MailAction::Flag => session.store(uid, "\\Flagged").await,
		MailAction::Move(folder) => session.move_to(uid, folder).await,
	}
}

/// Writes the files into the inbox (each through a hidden temporary file and a rename) and
/// records the mail with their provenance.
fn deliver(app: &App, row: &MailRecord, files: Vec<mime::Picked>, hint: &MailHint) -> Result<i64> {
	let inbox = app.library.inbox();
	let mut hints = Vec::with_capacity(files.len());
	for file in files {
		let temp = inbox.join(format!(".mail-{}.part", Uuid::new_v4().simple()));
		let written = std::fs::write(&temp, &file.bytes)
			.and_then(|()| std::fs::File::open(&temp)?.sync_all())
			.map_err(violet_core::Error::from)
			.and_then(|()| app.library.into_inbox(&temp, Path::new(&file.name)));
		if let Err(error) = written {
			let _ = std::fs::remove_file(&temp);
			return Err(Error::Internal(format!(
				"cannot write {} to the inbox: {error}",
				file.name
			)));
		}
		hints.push((hash::sha256(&file.bytes), hint.clone()));
	}
	Ok(app.index().mail_record(row, &hints)?)
}

/// Accounts, their last runs, and the newest handled mails.
pub fn view(app: &App) -> Result<MailView> {
	let index = app.index();
	let processed = index.mail_processed(None, PROCESSED_SHOWN)?;
	let Some(mail) = app.mail() else {
		return Ok(MailView {
			enabled: false,
			accounts: Vec::new(),
			processed,
		});
	};
	let mut accounts = Vec::new();
	for account in &mail.config.accounts {
		let mut counts = Counts::default();
		for (status, count) in index.mail_counts(&account.name)? {
			match status {
				MailStatus::Ingested => counts.ingested = count,
				MailStatus::Skipped => counts.skipped = count,
				MailStatus::Failed => counts.failed = count,
			}
		}
		accounts.push(AccountView {
			name: account.name.clone(),
			host: account.host.clone(),
			folders: mail
				.config
				.rules_of(&account.name)
				.map(|rule| rule.folder.clone())
				.collect(),
			interval_secs: account.interval_secs,
			status: mail
				.status(&account.name)
				.map(|status| status.clone())
				.unwrap_or_default(),
			counts,
		});
	}
	Ok(MailView {
		enabled: true,
		accounts,
		processed,
	})
}

/// Forgets a handled mail, so the next sync takes it again if the folder still matches it.
pub fn reprocess(app: &App, id: i64) -> Result<ProcessedMail> {
	app.index()
		.mail_forget(id)?
		.ok_or_else(|| Error::NotFound(format!("processed mail {id}")))
}
