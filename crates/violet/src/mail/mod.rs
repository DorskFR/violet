//! Takes files from IMAP folders into `inbox/`: each rule's matching attachments, or the
//! whole mail as `.eml`, with where they came from for the pipeline to record. A mailbox is a
//! connector of the kind `imap`, which [`kind`] describes and runs.

pub mod config;
pub mod imap;
pub mod kind;
pub mod mime;

use std::path::Path;
use std::sync::Arc;

use serde::Serialize;
use time::OffsetDateTime;
use uuid::Uuid;
use violet_core::{MailHint, MailRecord, MailSource, MailStatus, ProcessedMail, hash};

pub use config::{MailAction, Options, Rule, Scope, Security};
use imap::{Criteria, Session};

use crate::app::blocking;
use crate::auth::User;
use crate::{App, Error, Result};

pub const RETIRED_CONFIG_ENV: &str = "VIOLET_MAIL_CONFIG";
const MAX_MAIL_BYTES: u64 = 100 * 1024 * 1024;
const PROCESSED_SHOWN: usize = 50;
const CHANGED: &str = "the mailbox was changed meanwhile";

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

/// One mailbox to sync: an `imap` connector with its options read.
#[derive(Clone)]
pub struct Mailbox {
	/// The name of the connector, which its handled mails are recorded under.
	pub name: String,
	pub host: String,
	pub port: u16,
	pub security: Security,
	pub username: String,
	pub password: String,
	/// The user its documents are filed for, who may file them in their private areas.
	pub owner: Option<String>,
	pub area: Option<String>,
	pub tags: Vec<String>,
	pub rules: Vec<Rule>,
	/// The mailboxes of the same owner, this one included: a mail one of them took is not
	/// taken again from another.
	pub siblings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MailView {
	/// The newest handled mails of the mailboxes the user sees.
	pub processed: Vec<ProcessedMail>,
}

/// Whether the mailbox is still what it was when the sync began; a sync stops taking mails
/// once it is not.
pub type Still<'a> = &'a (dyn Fn() -> bool + Send + Sync);

/// Syncs one mailbox now: takes what its rules match and is not handled yet into the inbox.
pub async fn sync(app: &Arc<App>, mailbox: &Mailbox, still: Still<'_>) -> anyhow::Result<Report> {
	let mut report = Report::default();
	let mut session = Session::connect(mailbox).await?;
	for rule in &mailbox.rules {
		if let Err(error) = sync_rule(app, &mut session, mailbox, rule, still, &mut report).await {
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
	mailbox: &Mailbox,
	rule: &Rule,
	still: Still<'_>,
	report: &mut Report,
) -> anyhow::Result<()> {
	let uidvalidity = session.select(&rule.folder).await?;
	let uids = session.search(&criteria(rule)).await?;
	if uids.is_empty() {
		return Ok(());
	}
	let handled = {
		let (name, folder, found) = (mailbox.name.clone(), rule.folder.clone(), uids.clone());
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
		anyhow::ensure!(still(), CHANGED);
		report.mails += 1;
		let mut row = MailRecord {
			account: mailbox.name.clone(),
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
			let siblings = mailbox.siblings.clone();
			let seen = blocking(app, move |app| {
				Ok(app.index().mail_message_handled(&message_id, &siblings)?)
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
				account: mailbox.name.clone(),
				folder: rule.folder.clone(),
				uidvalidity,
				uid: meta.uid,
				message_id: parsed.message_id.clone(),
				from: parsed.from.clone(),
				subject: parsed.subject.clone(),
				date: parsed.date.clone(),
			},
			area: mailbox.area.clone(),
			tags: mailbox.tags.clone(),
		};
		let files = parsed.files;
		let count = files.len() as u32;
		let mut delivered = row.clone();
		delivered.status = MailStatus::Ingested;
		delivered.files = count;
		delivered.detail = (!parsed.left_out.is_empty()).then(|| parsed.left_out.join("; "));
		let owner = mailbox.owner.clone();
		let written = blocking(app, move |app| {
			deliver(app, &delivered, files, &hint, owner.as_deref())
		})
		.await;
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
fn deliver(
	app: &App,
	row: &MailRecord,
	files: Vec<mime::Picked>,
	hint: &MailHint,
	owner: Option<&str>,
) -> Result<i64> {
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
	let mut index = app.index();
	let id = index.mail_record(row, &hints)?;
	if let Some(owner) = owner {
		for (sha256, _) in &hints {
			index.set_uploader(sha256, owner)?;
		}
	}
	Ok(id)
}

/// The newest handled mails: all of them for an admin, for anyone else those of their own
/// mailboxes.
pub fn view(app: &App, user: &User) -> Result<MailView> {
	if user.is_admin() {
		let processed = app.index().mail_processed(None, PROCESSED_SHOWN)?;
		return Ok(MailView { processed });
	}
	let mut processed = Vec::new();
	let mailboxes = kind::mailboxes_of(app, user);
	let index = app.index();
	for mailbox in &mailboxes {
		processed.extend(index.mail_processed(Some(mailbox), PROCESSED_SHOWN)?);
	}
	processed.sort_by_key(|mail| std::cmp::Reverse((mail.processed_at, mail.id)));
	processed.truncate(PROCESSED_SHOWN);
	Ok(MailView { processed })
}

/// Forgets a handled mail and runs its mailbox, which takes the mail again if the folder
/// still matches it. A mail of a mailbox `user` does not see is not there.
pub fn reprocess(app: &App, id: i64, user: &User) -> Result<ProcessedMail> {
	let gone = || Error::NotFound(format!("processed mail {id}"));
	if !user.is_admin() {
		let mail = app.index().mail_item(id)?.ok_or_else(gone)?;
		if !kind::mailboxes_of(app, user).contains(&mail.account) {
			return Err(gone());
		}
	}
	let mail = app.index().mail_forget(id)?.ok_or_else(gone)?;
	if let Some(connectors) = app.connectors() {
		connectors.wake(&mail.account);
	}
	Ok(mail)
}
