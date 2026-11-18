use std::collections::HashSet;
use std::path::Path;

use serde::Deserialize;
use violet_core::naming;

const DEFAULT_KEYWORD: &str = "violet-ingested";
const MIN_INTERVAL_SECS: u64 = 60;

/// The mail sync settings, read from the JSON file `VIOLET_MAIL_CONFIG` names.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
	pub accounts: Vec<Account>,
	#[serde(default)]
	pub rules: Vec<Rule>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Security {
	Tls,
	Starttls,
	/// No encryption; only accepted for a loopback host, such as a local mail bridge.
	Plain,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Account {
	pub name: String,
	pub host: String,
	pub port: Option<u16>,
	#[serde(default = "Account::default_security")]
	pub security: Security,
	pub username: String,
	/// The environment variable holding the password.
	pub password_env: String,
	#[serde(default = "Account::default_interval")]
	pub interval_secs: u64,
}

impl Account {
	fn default_security() -> Security {
		Security::Tls
	}
	fn default_interval() -> u64 {
		600
	}
	pub fn port(&self) -> u16 {
		self.port.unwrap_or(match self.security {
			Security::Tls => 993,
			Security::Starttls | Security::Plain => 143,
		})
	}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
	#[default]
	Attachments,
	Eml,
	Both,
}

impl Scope {
	pub fn attachments(self) -> bool {
		matches!(self, Self::Attachments | Self::Both)
	}
	pub fn eml(self) -> bool {
		matches!(self, Self::Eml | Self::Both)
	}
}

/// What marks a mail on the server once its files are in the inbox; the search leaves out
/// mails already marked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MailAction {
	Keyword(String),
	Seen,
	Flag,
	Move(String),
}

impl Default for MailAction {
	fn default() -> Self {
		Self::Keyword(DEFAULT_KEYWORD.into())
	}
}

impl<'de> Deserialize<'de> for MailAction {
	fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
		let raw = String::deserialize(deserializer)?;
		MailAction::parse(&raw).map_err(serde::de::Error::custom)
	}
}

impl MailAction {
	fn parse(raw: &str) -> Result<Self, String> {
		let action = match raw.split_once(':') {
			None if raw == "keyword" => Self::default(),
			None if raw == "seen" => Self::Seen,
			None if raw == "flag" => Self::Flag,
			Some(("keyword", keyword)) => {
				let atom = !keyword.is_empty()
					&& keyword
						.bytes()
						.all(|byte| byte.is_ascii_graphic() && !b"(){%*\"\\]".contains(&byte));
				if !atom {
					return Err(format!("\"{keyword}\" cannot be an IMAP keyword"));
				}
				Self::Keyword(keyword.into())
			}
			Some(("move", folder)) if !folder.trim().is_empty() => Self::Move(folder.into()),
			_ => {
				return Err(format!(
					"unknown action \"{raw}\": use keyword, keyword:<name>, seen, flag or move:<folder>"
				));
			}
		};
		Ok(action)
	}
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hints {
	pub area: Option<String>,
	#[serde(default)]
	pub tags: Vec<String>,
	/// The user its documents are filed for, who may file them in their private areas.
	pub owner: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
	pub account: String,
	#[serde(default = "Rule::default_folder")]
	pub folder: String,
	/// Substrings the server matches in the header.
	pub from: Option<String>,
	pub to: Option<String>,
	pub subject: Option<String>,
	/// Case-insensitive globs (`*`, `?`) on attachment names; exclude wins.
	#[serde(default)]
	pub filename_include: Vec<String>,
	#[serde(default)]
	pub filename_exclude: Vec<String>,
	#[serde(default = "Rule::default_max_age")]
	pub max_age_days: u32,
	#[serde(default)]
	pub scope: Scope,
	/// Also take inline parts; images under 20 KB are still left out.
	#[serde(default)]
	pub include_inline: bool,
	#[serde(default)]
	pub action: MailAction,
	#[serde(default)]
	pub hints: Hints,
}

impl Rule {
	fn default_folder() -> String {
		"INBOX".into()
	}
	fn default_max_age() -> u32 {
		30
	}

	/// Whether an attachment name passes the include and exclude globs.
	pub fn wants(&self, name: &str) -> bool {
		let included = self.filename_include.is_empty()
			|| self.filename_include.iter().any(|glob| matches(glob, name));
		included && !self.filename_exclude.iter().any(|glob| matches(glob, name))
	}
}

/// Case-insensitive glob match with `*` and `?`.
pub fn matches(glob: &str, name: &str) -> bool {
	let glob: Vec<char> = glob.to_lowercase().chars().collect();
	let name: Vec<char> = name.to_lowercase().chars().collect();
	let (mut g, mut n) = (0, 0);
	let mut backtrack: Option<(usize, usize)> = None;
	while n < name.len() {
		match glob.get(g) {
			Some('*') => {
				backtrack = Some((g, n));
				g += 1;
			}
			Some(c) if *c == '?' || *c == name[n] => {
				g += 1;
				n += 1;
			}
			_ => match backtrack {
				Some((star, at)) => {
					g = star + 1;
					n = at + 1;
					backtrack = Some((star, at + 1));
				}
				None => return false,
			},
		}
	}
	glob[g..].iter().all(|c| *c == '*')
}

fn loopback(host: &str) -> bool {
	matches!(host, "localhost" | "127.0.0.1" | "::1" | "[::1]") || host.starts_with("127.")
}

impl Config {
	pub fn load(path: &Path) -> anyhow::Result<Self> {
		let text = std::fs::read_to_string(path)
			.map_err(|error| anyhow::anyhow!("cannot read {}: {error}", path.display()))?;
		Self::parse(&text).map_err(|error| anyhow::anyhow!("{}: {error}", path.display()))
	}

	pub fn parse(text: &str) -> anyhow::Result<Self> {
		let config: Self = serde_json::from_str(text)?;
		config.validate()?;
		Ok(config)
	}

	fn validate(&self) -> anyhow::Result<()> {
		let mut names = HashSet::new();
		for account in &self.accounts {
			let name = &account.name;
			let plain_name = !name.is_empty()
				&& name
					.bytes()
					.all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte));
			anyhow::ensure!(
				plain_name,
				"account name \"{name}\" must be letters, digits, '-', '_' or '.'"
			);
			anyhow::ensure!(names.insert(name), "account \"{name}\" is listed twice");
			anyhow::ensure!(!account.host.trim().is_empty(), "account {name}: no host");
			anyhow::ensure!(!account.username.is_empty(), "account {name}: no username");
			anyhow::ensure!(
				!account.password_env.trim().is_empty(),
				"account {name}: password_env names no variable"
			);
			anyhow::ensure!(
				account.interval_secs >= MIN_INTERVAL_SECS,
				"account {name}: interval_secs must be at least {MIN_INTERVAL_SECS}"
			);
			anyhow::ensure!(
				account.security != Security::Plain || loopback(&account.host),
				"account {name}: security \"plain\" is only allowed for a loopback host"
			);
		}
		for (position, rule) in self.rules.iter().enumerate() {
			let at = format!("rule {} ({})", position + 1, rule.account);
			anyhow::ensure!(
				names.contains(&rule.account),
				"{at}: no account named \"{}\"",
				rule.account
			);
			anyhow::ensure!(!rule.folder.trim().is_empty(), "{at}: no folder");
			anyhow::ensure!(
				rule.max_age_days >= 1,
				"{at}: max_age_days must be at least 1"
			);
			for text in [&rule.from, &rule.to, &rule.subject].into_iter().flatten() {
				anyhow::ensure!(
					!text.is_empty() && !text.contains(['\r', '\n']),
					"{at}: a filter is empty or spans lines"
				);
			}
			if let Some(area) = &rule.hints.area {
				anyhow::ensure!(
					!area.is_empty() && naming::sanitize(area) == *area,
					"{at}: \"{area}\" cannot be an area folder"
				);
			}
			anyhow::ensure!(
				rule.hints.tags.iter().all(|tag| !tag.trim().is_empty()),
				"{at}: empty tag"
			);
		}
		Ok(())
	}

	pub fn account(&self, name: &str) -> Option<&Account> {
		self.accounts.iter().find(|account| account.name == name)
	}

	pub fn rules_of<'a>(&'a self, account: &'a str) -> impl Iterator<Item = &'a Rule> + 'a {
		self.rules
			.iter()
			.filter(move |rule| rule.account == account)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	const MINIMAL: &str = r#"{
		"accounts": [{"name": "home", "host": "imap.example.com", "username": "me", "password_env": "HOME_IMAP"}],
		"rules": [{"account": "home"}]
	}"#;

	#[test]
	fn defaults_fill_a_minimal_config() {
		let config = Config::parse(MINIMAL).unwrap();
		let account = &config.accounts[0];
		assert_eq!(account.security, Security::Tls);
		assert_eq!(account.port(), 993);
		assert_eq!(account.interval_secs, 600);
		let rule = &config.rules[0];
		assert_eq!(rule.folder, "INBOX");
		assert_eq!(rule.max_age_days, 30);
		assert_eq!(rule.scope, Scope::Attachments);
		assert!(!rule.include_inline);
		assert_eq!(rule.action, MailAction::Keyword("violet-ingested".into()));
	}

	#[test]
	fn actions_parse() {
		for (raw, action) in [
			("keyword", MailAction::Keyword("violet-ingested".into())),
			("keyword:$Filed", MailAction::Keyword("$Filed".into())),
			("seen", MailAction::Seen),
			("flag", MailAction::Flag),
			("move:Archive/2026", MailAction::Move("Archive/2026".into())),
		] {
			assert_eq!(MailAction::parse(raw).unwrap(), action);
		}
		for raw in ["delete", "move:", "keyword:a b", "keyword:", "seen:x"] {
			assert!(MailAction::parse(raw).is_err(), "{raw}");
		}
	}

	#[test]
	fn unknown_fields_and_bad_values_are_rejected() {
		let broken = [
			MINIMAL.replace(r#""username""#, r#""password": "hunter2", "username""#),
			MINIMAL.replace(
				r#"{"account": "home"}"#,
				r#"{"account": "home", "delete": true}"#,
			),
			MINIMAL.replace(
				r#"{"account": "home"}"#,
				r#"{"account": "home", "hints": {"title": "x"}}"#,
			),
			MINIMAL.replace(r#""rules""#, r#""extra": 1, "rules""#),
			MINIMAL.replace(r#"{"account": "home"}"#, r#"{"account": "work"}"#),
			MINIMAL.replace(
				r#"{"account": "home"}"#,
				r#"{"account": "home", "action": "delete"}"#,
			),
			MINIMAL.replace(
				r#"{"account": "home"}"#,
				r#"{"account": "home", "scope": "all"}"#,
			),
			MINIMAL.replace(
				r#"{"account": "home"}"#,
				r#"{"account": "home", "hints": {"area": "a/b"}}"#,
			),
			MINIMAL.replace(r#""name": "home""#, r#""name": "home/x""#),
			MINIMAL.replace(r#""username""#, r#""interval_secs": 5, "username""#),
			MINIMAL.replace(r#""username""#, r#""security": "plain", "username""#),
			MINIMAL.replace(r#""password_env": "HOME_IMAP""#, r#""password_env": """#),
		];
		for text in &broken {
			assert!(Config::parse(text).is_err(), "accepted {text}");
		}
		let twice = MINIMAL.replace(
			r#""accounts": [{"#,
			r#""accounts": [{"name": "home", "host": "h", "username": "u", "password_env": "P"}, {"#,
		);
		assert!(Config::parse(&twice).is_err());
		let local = MINIMAL
			.replace("imap.example.com", "127.0.0.1")
			.replace(r#""username""#, r#""security": "plain", "username""#);
		assert_eq!(Config::parse(&local).unwrap().accounts[0].port(), 143);
	}

	#[test]
	fn globs_are_case_insensitive_and_exclude_wins() {
		assert!(matches("*.pdf", "Invoice.PDF"));
		assert!(matches("*invoice*", "2026 Invoice 3.pdf"));
		assert!(matches("scan-??.jpg", "scan-01.jpg"));
		assert!(!matches("scan-??.jpg", "scan-1.jpg"));
		assert!(matches("*", ""));
		assert!(matches("a*b*c", "aXbYbZc"));
		assert!(!matches("*.pdf", "pdf"));
		let mut rule: Rule = serde_json::from_str(r#"{"account": "home"}"#).unwrap();
		assert!(rule.wants("anything.bin"));
		rule.filename_include = vec!["*.pdf".into()];
		rule.filename_exclude = vec!["*terms*".into()];
		assert!(rule.wants("bill.pdf"));
		assert!(!rule.wants("Terms of service.pdf"));
		assert!(!rule.wants("photo.jpg"));
	}
}
