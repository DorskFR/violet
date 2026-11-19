//! Mailboxes as connectors: the kind `imap`, whose options say where the mailbox is and what
//! to take from it, and whose run is one sync of that mailbox.

use violet_core::{Index, MailStatus};

use super::{Mailbox, Options};
use crate::auth::User;
use crate::connectors::{
	BoxFuture, Choice, ConnectorConfig, Counts, KindView, OptionInput, OptionSpec, Report, Run,
	Runner,
};
use crate::{App, Result};

pub const KIND: &str = "imap";
const SCHEDULE: &str = "every 10m";

fn spec(name: &str, label: &str, help: Option<&str>, input: OptionInput) -> OptionSpec {
	OptionSpec {
		name: name.into(),
		label: label.into(),
		help: help.map(Into::into),
		input,
	}
}

fn text(default: Option<&str>, required: bool, placeholder: Option<&str>) -> OptionInput {
	OptionInput::Text {
		default: default.map(Into::into),
		required,
		placeholder: placeholder.map(Into::into),
	}
}

fn choice(choices: &[(&str, &str)], default: &str) -> OptionInput {
	OptionInput::Choice {
		choices: choices
			.iter()
			.map(|(value, label)| Choice {
				value: (*value).into(),
				label: (*label).into(),
			})
			.collect(),
		default: default.into(),
	}
}

pub fn kind() -> KindView {
	let rule = vec![
		spec("folder", "Folder", None, text(Some("INBOX"), true, None)),
		spec(
			"from",
			"From contains",
			None,
			text(None, false, Some("billing@example.com")),
		),
		spec("to", "To contains", None, text(None, false, None)),
		spec("subject", "Subject contains", None, text(None, false, None)),
		spec(
			"filename_include",
			"Only attachments named",
			Some("Patterns with * and ?, such as *.pdf; none takes every attachment."),
			OptionInput::TextList,
		),
		spec(
			"filename_exclude",
			"Never attachments named",
			None,
			OptionInput::TextList,
		),
		spec(
			"max_age_days",
			"Not older than, in days",
			None,
			OptionInput::Number {
				default: Some(30),
				min: Some(1),
				max: None,
			},
		),
		spec(
			"scope",
			"Take",
			None,
			choice(
				&[
					("attachments", "Attachments"),
					("eml", "The whole mail"),
					("both", "Both"),
				],
				"attachments",
			),
		),
		spec(
			"include_inline",
			"Also take inline images",
			None,
			OptionInput::Boolean { default: false },
		),
		spec(
			"action",
			"Afterwards",
			Some(
				"What marks a taken mail on the server: keyword, keyword:<name>, seen, flag or \
				 move:<folder>.",
			),
			text(Some("keyword"), true, None),
		),
	];
	KindView {
		kind: KIND.into(),
		label: "Mailbox (IMAP)".into(),
		signs_in: false,
		schedule: SCHEDULE.into(),
		options: vec![
			spec(
				"host",
				"Host",
				None,
				text(None, true, Some("imap.example.com")),
			),
			spec(
				"port",
				"Port",
				Some("Empty uses 993 with TLS and 143 otherwise."),
				OptionInput::Number {
					default: None,
					min: Some(1),
					max: Some(65535),
				},
			),
			spec(
				"security",
				"Security",
				Some("Without encryption only a mail server on this machine is accepted."),
				choice(
					&[("tls", "TLS"), ("starttls", "STARTTLS"), ("plain", "None")],
					"tls",
				),
			),
			spec(
				"rules",
				"Rules",
				Some("Each rule takes the matching mails of one folder."),
				OptionInput::Group {
					item: "Rule".into(),
					fields: rule,
					min: 1,
				},
			),
		],
	}
}

/// The mailbox a connector of this kind describes.
pub fn mailbox(config: &ConnectorConfig, siblings: Vec<String>) -> anyhow::Result<Mailbox> {
	let options = Options::parse(&config.options).map_err(anyhow::Error::msg)?;
	let credentials = config.credentials();
	Ok(Mailbox {
		name: config.name.clone(),
		port: options.port(),
		host: options.host,
		security: options.security,
		username: credentials.username,
		password: credentials.password,
		owner: config.owner.clone(),
		area: config.area.clone(),
		tags: config.tags.clone(),
		rules: options.rules,
		siblings,
	})
}

/// The mailboxes `user` sees, by name; none while connectors are not running.
pub fn mailboxes_of(app: &App, user: &User) -> Vec<String> {
	app.connectors()
		.map(|connectors| connectors.names_of(KIND, user))
		.unwrap_or_default()
}

pub struct Imap;

impl Runner for Imap {
	fn check(&self, config: &ConnectorConfig) -> anyhow::Result<()> {
		mailbox(config, Vec::new()).map(drop)
	}

	fn run<'a>(&'a self, run: Run<'a>) -> BoxFuture<'a, anyhow::Result<Report>> {
		Box::pin(async move {
			let mailbox = mailbox(run.config, run.siblings)?;
			let report = super::sync(run.app, &mailbox, run.still).await?;
			Ok(Report {
				listed: report.mails,
				ingested: report.files,
				skipped: report.skipped,
				failed: report.failed,
				errors: report.errors,
			})
		})
	}

	fn counts(&self, index: &Index, name: &str) -> Result<Counts> {
		let mut counts = Counts::default();
		for (status, count) in index.mail_counts(name)? {
			match status {
				MailStatus::Ingested => counts.ingested = count,
				MailStatus::Skipped => counts.skipped = count,
				MailStatus::Failed => counts.failed = count,
			}
		}
		Ok(counts)
	}

	fn kept(&self, index: &Index, name: &str) -> Result<bool> {
		Ok(index.mail_known(name)?)
	}

	fn forget(&self, index: &mut Index, name: &str) -> Result<()> {
		Ok(index.mail_forget_all(name)?)
	}
}
