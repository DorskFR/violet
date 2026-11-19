use std::collections::HashSet;
use std::path::Path;

use serde::Deserialize;
use time::UtcOffset;
use violet_core::naming;

use super::schedule::{self, Schedule};

/// The connector settings, read from the JSON file `VIOLET_CONNECTORS_CONFIG` names.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
	/// The offset the clock times of the schedules are in; UTC when not set.
	#[serde(default = "utc", deserialize_with = "offset")]
	pub utc_offset: UtcOffset,
	pub connectors: Vec<ConnectorConfig>,
}

fn utc() -> UtcOffset {
	UtcOffset::UTC
}

fn offset<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<UtcOffset, D::Error> {
	let raw = String::deserialize(deserializer)?;
	schedule::parse_offset(&raw).map_err(serde::de::Error::custom)
}

impl<'de> Deserialize<'de> for Schedule {
	fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
		let raw = String::deserialize(deserializer)?;
		Schedule::parse(&raw).map_err(serde::de::Error::custom)
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
pub struct ConnectorConfig {
	pub name: String,
	pub kind: String,
	/// The environment variables holding what to sign in with.
	pub username_env: String,
	pub password_env: String,
	pub schedule: Schedule,
	#[serde(default)]
	pub hints: Hints,
	/// Settings of the kind, which the kind checks.
	#[serde(default = "no_options")]
	pub options: serde_json::Value,
}

fn no_options() -> serde_json::Value {
	serde_json::Value::Object(serde_json::Map::new())
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
		for connector in &self.connectors {
			let name = &connector.name;
			let plain_name = !name.is_empty()
				&& name
					.bytes()
					.all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte));
			anyhow::ensure!(
				plain_name,
				"connector name \"{name}\" must be letters, digits, '-', '_' or '.'"
			);
			anyhow::ensure!(names.insert(name), "connector \"{name}\" is listed twice");
			anyhow::ensure!(!connector.kind.is_empty(), "connector {name}: no kind");
			for (field, variable) in [
				("username_env", &connector.username_env),
				("password_env", &connector.password_env),
			] {
				anyhow::ensure!(
					!variable.trim().is_empty(),
					"connector {name}: {field} names no variable"
				);
			}
			anyhow::ensure!(
				connector.options.is_object(),
				"connector {name}: options must be an object"
			);
			if let Some(area) = &connector.hints.area {
				anyhow::ensure!(
					!area.is_empty() && naming::sanitize(area) == *area,
					"connector {name}: \"{area}\" cannot be an area folder"
				);
			}
			anyhow::ensure!(
				connector
					.hints
					.tags
					.iter()
					.all(|tag| !tag.trim().is_empty()),
				"connector {name}: empty tag"
			);
			if let Some(owner) = &connector.hints.owner {
				anyhow::ensure!(!owner.trim().is_empty(), "connector {name}: empty owner");
			}
		}
		Ok(())
	}

	pub fn connector(&self, name: &str) -> Option<&ConnectorConfig> {
		self.connectors
			.iter()
			.find(|connector| connector.name == name)
	}
}

#[cfg(test)]
mod tests {
	use time::macros::{offset, time};

	use super::*;

	const FULL: &str = r#"{"utc_offset": "+09:00",
		"connectors": [{"name": "mf", "kind": "moneyforward", "username_env": "VIOLET_MF_USER",
			"password_env": "VIOLET_MF_PASSWORD", "schedule": "monthly 26 09:00",
			"hints": {"owner": "dorsk", "area": "Work", "tags": ["payroll"]},
			"options": {"anything": [1, {"the": "kind"}, "reads"]}}]}"#;

	#[test]
	fn a_full_config_is_read() {
		let config = Config::parse(FULL).unwrap();
		assert_eq!(config.utc_offset, offset!(+9));
		let connector = config.connector("mf").unwrap();
		assert_eq!(connector.kind, "moneyforward");
		assert_eq!(connector.schedule, Schedule::Monthly(26, time!(09:00)));
		assert_eq!(connector.hints.owner.as_deref(), Some("dorsk"));
		assert_eq!(connector.hints.area.as_deref(), Some("Work"));
		assert_eq!(connector.hints.tags, ["payroll"]);
		assert_eq!(connector.options["anything"][1]["the"], "kind");
		assert!(config.connector("bank").is_none());
	}

	#[test]
	fn defaults_are_utc_no_hints_and_empty_options() {
		let config = Config::parse(
			r#"{"connectors": [{"name": "a", "kind": "k", "username_env": "U",
				"password_env": "P", "schedule": "every 6h"}]}"#,
		)
		.unwrap();
		assert_eq!(config.utc_offset, UtcOffset::UTC);
		let connector = &config.connectors[0];
		assert!(connector.hints.area.is_none() && connector.hints.tags.is_empty());
		assert_eq!(connector.options, serde_json::json!({}));
	}

	fn one(fields: &str) -> anyhow::Result<Config> {
		Config::parse(&format!(r#"{{"connectors": [{{{fields}}}]}}"#))
	}

	#[test]
	fn what_does_not_add_up_is_refused() {
		const BASE: &str = r#""kind": "k", "username_env": "U", "password_env": "P""#;
		for (config, expected) in [
			(
				Config::parse(r#"{"connectors": [], "interval": 5}"#),
				"unknown field",
			),
			(
				Config::parse(r#"{"utc_offset": "JST", "connectors": []}"#),
				"not a UTC offset",
			),
			(
				one(&format!(
					r#""name": "a", {BASE}, "schedule": "every 6h", "password": "x""#
				)),
				"unknown field",
			),
			(
				one(&format!(r#""name": "a", {BASE}, "schedule": "sometimes""#)),
				"unknown schedule",
			),
			(one(&format!(r#""name": "a", {BASE}"#)), "missing field"),
			(
				one(&format!(r#""name": "a b", {BASE}, "schedule": "every 6h""#)),
				"must be letters",
			),
			(
				one(&format!(
					r#""name": "a", {BASE}, "schedule": "every 6h", "hints": {{"folder": "x"}}"#
				)),
				"unknown field",
			),
			(
				one(&format!(
					r#""name": "a", {BASE}, "schedule": "every 6h", "hints": {{"area": "a/b"}}"#
				)),
				"cannot be an area",
			),
			(
				one(&format!(
					r#""name": "a", {BASE}, "schedule": "every 6h", "hints": {{"tags": [" "]}}"#
				)),
				"empty tag",
			),
			(
				one(&format!(
					r#""name": "a", {BASE}, "schedule": "every 6h", "options": []"#
				)),
				"must be an object",
			),
			(
				one(
					r#""name": "a", "kind": "k", "username_env": " ", "password_env": "P",
					"schedule": "every 6h""#,
				),
				"names no variable",
			),
			(
				Config::parse(&format!(
					r#"{{"connectors": [{{"name": "a", {BASE}, "schedule": "every 6h"}},
						{{"name": "a", {BASE}, "schedule": "every 6h"}}]}}"#
				)),
				"listed twice",
			),
		] {
			let error = config.unwrap_err().to_string();
			assert!(error.contains(expected), "{expected}: {error}");
		}
	}
}
