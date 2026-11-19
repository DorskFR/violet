use serde::Deserialize;

const DEFAULT_KEYWORD: &str = "violet-ingested";
const DEFAULT_FOLDER: &str = "INBOX";
const DEFAULT_MAX_AGE_DAYS: u32 = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Security {
	Tls,
	Starttls,
	/// No encryption; only accepted for a loopback host, such as a local mail bridge.
	Plain,
}

impl Security {
	pub fn default_port(self) -> u16 {
		match self {
			Self::Tls => 993,
			Self::Starttls | Self::Plain => 143,
		}
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

/// What to take from one folder of a mailbox.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
	#[serde(default = "Rule::default_folder")]
	pub folder: String,
	/// Substrings the server matches in the header.
	#[serde(default)]
	pub from: Option<String>,
	#[serde(default)]
	pub to: Option<String>,
	#[serde(default)]
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
}

impl Default for Rule {
	fn default() -> Self {
		Self {
			folder: Self::default_folder(),
			from: None,
			to: None,
			subject: None,
			filename_include: Vec::new(),
			filename_exclude: Vec::new(),
			max_age_days: Self::default_max_age(),
			scope: Scope::default(),
			include_inline: false,
			action: MailAction::default(),
		}
	}
}

impl Rule {
	fn default_folder() -> String {
		DEFAULT_FOLDER.into()
	}
	fn default_max_age() -> u32 {
		DEFAULT_MAX_AGE_DAYS
	}

	/// Whether an attachment name passes the include and exclude globs.
	pub fn wants(&self, name: &str) -> bool {
		let included = self.filename_include.is_empty()
			|| self.filename_include.iter().any(|glob| matches(glob, name));
		included && !self.filename_exclude.iter().any(|glob| matches(glob, name))
	}

	fn check(&mut self) -> Result<(), String> {
		self.folder = self.folder.trim().to_string();
		if self.folder.is_empty() {
			return Err("no folder".into());
		}
		if self.folder.contains(['\r', '\n']) {
			return Err("the folder spans lines".into());
		}
		if self.max_age_days < 1 {
			return Err("max_age_days must be at least 1".into());
		}
		for (what, filter) in [
			("from", &mut self.from),
			("to", &mut self.to),
			("subject", &mut self.subject),
		] {
			*filter = filter.take().filter(|text| !text.trim().is_empty());
			if filter
				.as_ref()
				.is_some_and(|text| text.contains(['\r', '\n']))
			{
				return Err(format!("{what} spans lines"));
			}
		}
		for (what, globs) in [
			("filename_include", &self.filename_include),
			("filename_exclude", &self.filename_exclude),
		] {
			if globs.iter().any(|glob| glob.trim().is_empty()) {
				return Err(format!("{what} holds an empty pattern"));
			}
		}
		Ok(())
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

pub fn loopback(host: &str) -> bool {
	matches!(host, "localhost" | "127.0.0.1" | "::1" | "[::1]") || host.starts_with("127.")
}

/// The options of an `imap` connector: where its mailbox is and what to take from it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Options {
	pub host: String,
	#[serde(default)]
	pub port: Option<u16>,
	#[serde(default = "Options::default_security")]
	pub security: Security,
	#[serde(default = "Options::default_rules")]
	pub rules: Vec<Rule>,
}

impl Options {
	fn default_security() -> Security {
		Security::Tls
	}
	fn default_rules() -> Vec<Rule> {
		vec![Rule::default()]
	}

	pub fn parse(options: &serde_json::Value) -> Result<Self, String> {
		let mut options: Self =
			serde_json::from_value(options.clone()).map_err(|error| format!("options: {error}"))?;
		options.host = options.host.trim().to_string();
		if options.host.is_empty() {
			return Err("options: enter the host of the mail server".into());
		}
		if options
			.host
			.contains(|c: char| c.is_whitespace() || c == '/')
		{
			return Err("options: the host is a name or an address, without a scheme".into());
		}
		if options.port == Some(0) {
			return Err("options: the port must be between 1 and 65535".into());
		}
		if options.security == Security::Plain && !loopback(&options.host) {
			return Err("options: security \"plain\" is only allowed for a loopback host".into());
		}
		if options.rules.is_empty() {
			return Err("options: a mailbox needs at least one rule".into());
		}
		for (position, rule) in options.rules.iter_mut().enumerate() {
			rule.check()
				.map_err(|error| format!("options: rule {}: {error}", position + 1))?;
		}
		Ok(options)
	}

	pub fn port(&self) -> u16 {
		self.port.unwrap_or(self.security.default_port())
	}
}

#[cfg(test)]
mod tests {
	use serde_json::json;

	use super::*;

	#[test]
	fn defaults_fill_minimal_options() {
		let options = Options::parse(&json!({"host": " imap.example.com "})).unwrap();
		assert_eq!(options.host, "imap.example.com");
		assert_eq!(options.security, Security::Tls);
		assert_eq!(options.port(), 993);
		assert_eq!(options.rules, [Rule::default()]);
		let rule = &options.rules[0];
		assert_eq!(rule.folder, "INBOX");
		assert_eq!(rule.max_age_days, 30);
		assert_eq!(rule.scope, Scope::Attachments);
		assert!(!rule.include_inline);
		assert_eq!(rule.action, MailAction::Keyword("violet-ingested".into()));

		let local = Options::parse(&json!({
			"host": "127.0.0.1", "security": "plain", "port": null,
			"rules": [{"from": " ", "subject": "invoice"}, {"folder": "Bills", "action": "seen"}],
		}))
		.unwrap();
		assert_eq!(local.port(), 143);
		assert_eq!(local.rules[0].from, None);
		assert_eq!(local.rules[0].subject.as_deref(), Some("invoice"));
		assert_eq!(local.rules[1].action, MailAction::Seen);
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
	fn unknown_fields_and_bad_values_are_refused() {
		for (options, expected) in [
			(json!({}), "missing field `host`"),
			(json!({"host": " "}), "enter the host"),
			(
				json!({"host": "imap://mail.example.com"}),
				"without a scheme",
			),
			(json!({"host": "h", "port": 0}), "between 1 and 65535"),
			(json!({"host": "h", "port": 70000}), "options: "),
			(json!({"host": "h", "port": "993"}), "options: "),
			(json!({"host": "h", "security": "ssl"}), "unknown variant"),
			(json!({"host": "h", "security": "plain"}), "loopback"),
			(json!({"host": "h", "password": "hunter2"}), "unknown field"),
			(json!({"host": "h", "interval_secs": 600}), "unknown field"),
			(json!({"host": "h", "rules": []}), "at least one rule"),
			(json!({"host": "h", "rules": {}}), "options: "),
			(
				json!({"host": "h", "rules": [{"account": "home"}]}),
				"unknown field",
			),
			(
				json!({"host": "h", "rules": [{"hints": {}}]}),
				"unknown field",
			),
			(
				json!({"host": "h", "rules": [{"delete": true}]}),
				"unknown field",
			),
			(
				json!({"host": "h", "rules": [{}, {"folder": " "}]}),
				"rule 2: no folder",
			),
			(
				json!({"host": "h", "rules": [{"max_age_days": 0}]}),
				"rule 1: max_age_days",
			),
			(
				json!({"host": "h", "rules": [{"max_age_days": "30"}]}),
				"options: ",
			),
			(
				json!({"host": "h", "rules": [{"subject": "a\nb"}]}),
				"subject spans lines",
			),
			(
				json!({"host": "h", "rules": [{"scope": "all"}]}),
				"unknown variant",
			),
			(
				json!({"host": "h", "rules": [{"action": "delete"}]}),
				"unknown action",
			),
			(
				json!({"host": "h", "rules": [{"include_inline": "yes"}]}),
				"options: ",
			),
			(
				json!({"host": "h", "rules": [{"filename_include": ["*.pdf", " "]}]}),
				"filename_include holds an empty pattern",
			),
			(
				json!({"host": "h", "rules": [{"filename_exclude": "*.ics"}]}),
				"options: ",
			),
		] {
			let error = Options::parse(&options)
				.err()
				.unwrap_or_else(|| panic!("{options}"));
			assert!(error.contains(expected), "{expected}: {error}");
		}
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
		let mut rule = Rule::default();
		assert!(rule.wants("anything.bin"));
		rule.filename_include = vec!["*.pdf".into()];
		rule.filename_exclude = vec!["*terms*".into()];
		assert!(rule.wants("bill.pdf"));
		assert!(!rule.wants("Terms of service.pdf"));
		assert!(!rule.wants("photo.jpg"));
	}
}
