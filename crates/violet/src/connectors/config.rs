use std::fmt;

use serde::Deserialize;
use time::UtcOffset;
use violet_core::{ConnectorDefinition, naming};

use super::schedule::{self, Schedule};

const MAX_NAME_CHARS: usize = 64;
const RESERVED_NAMES: &[&str] = &["kinds", "items"];

/// A connector, checked.
#[derive(Clone)]
pub struct ConnectorConfig {
	pub name: String,
	pub kind: String,
	/// The user it is for, for whom its documents are filed and who may file them in their
	/// private areas; none for the implicit user of a library without users.
	pub owner: Option<String>,
	pub username: String,
	pub(super) password: String,
	pub schedule: Schedule,
	/// The offset the clock times of the schedule are in.
	pub utc_offset: UtcOffset,
	pub area: Option<String>,
	pub tags: Vec<String>,
	/// Settings of the kind, which the kind checks.
	pub options: serde_json::Value,
}

impl fmt::Debug for ConnectorConfig {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("ConnectorConfig")
			.field("name", &self.name)
			.field("kind", &self.kind)
			.field("owner", &self.owner)
			.finish_non_exhaustive()
	}
}

/// What the settings send to add a connector or to replace what one is.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectorInput {
	pub name: Option<String>,
	pub kind: Option<String>,
	/// Whose it is, which only an admin chooses; left out, an added connector is its adder's
	/// and an edited one stays whose it was.
	#[serde(default)]
	pub owner: Option<String>,
	pub username: String,
	/// Kept as it is when an edit sends none or an empty one.
	#[serde(default)]
	pub password: Option<String>,
	pub schedule: String,
	#[serde(default)]
	pub utc_offset: Option<String>,
	#[serde(default)]
	pub area: Option<String>,
	#[serde(default)]
	pub tags: Vec<String>,
	#[serde(default)]
	pub options: Option<serde_json::Value>,
}

fn plain_name(name: &str) -> Result<(), String> {
	let plain = !name.is_empty()
		&& name
			.bytes()
			.all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte));
	if !plain {
		return Err("the name must be letters, digits, '-', '_' or '.'".into());
	}
	if name.len() > MAX_NAME_CHARS {
		return Err(format!(
			"the name is longer than {MAX_NAME_CHARS} characters"
		));
	}
	if RESERVED_NAMES.contains(&name) || name.bytes().all(|byte| byte == b'.') {
		return Err(format!("a connector cannot be named \"{name}\""));
	}
	Ok(())
}

impl ConnectorInput {
	/// The connector `owner` adds.
	pub fn added(self, owner: Option<String>) -> Result<ConnectorConfig, String> {
		let name = self.name.clone().ok_or("the connector has no name")?;
		let kind = self.kind.clone().ok_or("the connector has no kind")?;
		let password = self.password.clone().unwrap_or_default();
		self.checked(name, kind, owner, password)
	}

	/// `current` with what the settings sent in place of what it was, `owner`'s from now on.
	pub fn replacing(
		self,
		current: &ConnectorConfig,
		owner: Option<String>,
	) -> Result<ConnectorConfig, String> {
		if self.name.as_ref().is_some_and(|name| *name != current.name) {
			return Err("a connector cannot be renamed".into());
		}
		if self.kind.as_ref().is_some_and(|kind| *kind != current.kind) {
			return Err("the kind of a connector cannot change".into());
		}
		let password = match &self.password {
			Some(password) if !password.is_empty() => password.clone(),
			_ => current.password.clone(),
		};
		self.checked(current.name.clone(), current.kind.clone(), owner, password)
	}

	fn checked(
		self,
		name: String,
		kind: String,
		owner: Option<String>,
		password: String,
	) -> Result<ConnectorConfig, String> {
		plain_name(&name)?;
		if kind.trim().is_empty() {
			return Err("the connector has no kind".into());
		}
		let username = self.username.trim().to_string();
		if username.is_empty() {
			return Err("enter the username the site signs in with".into());
		}
		if password.is_empty() {
			return Err("enter the password the site signs in with".into());
		}
		let schedule = Schedule::parse(&self.schedule)?;
		let utc_offset = match self.utc_offset.as_deref().map(str::trim) {
			None | Some("") => UtcOffset::UTC,
			Some(offset) => schedule::parse_offset(offset)?,
		};
		let area = self
			.area
			.map(|area| area.trim().to_string())
			.filter(|area| !area.is_empty());
		if let Some(area) = &area
			&& naming::sanitize(area) != *area
		{
			return Err(format!("\"{area}\" cannot be an area folder"));
		}
		let mut tags = Vec::new();
		for tag in &self.tags {
			let tag = tag.trim();
			if tag.is_empty() {
				return Err("a tag is empty".into());
			}
			if !tags.iter().any(|known| known == tag) {
				tags.push(tag.to_string());
			}
		}
		let options = self
			.options
			.unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()));
		if !options.is_object() {
			return Err("options must be an object".into());
		}
		Ok(ConnectorConfig {
			name,
			kind,
			owner,
			username,
			password,
			schedule,
			utc_offset,
			area,
			tags,
			options,
		})
	}
}

impl ConnectorConfig {
	/// A connector as it was stored.
	pub fn stored(definition: ConnectorDefinition) -> Result<Self, String> {
		let options = serde_json::from_str(&definition.options)
			.map_err(|error| format!("options: {error}"))?;
		ConnectorInput {
			name: None,
			kind: None,
			owner: None,
			username: definition.username,
			password: None,
			schedule: definition.schedule,
			utc_offset: Some(definition.utc_offset),
			area: definition.area,
			tags: definition.tags,
			options: Some(options),
		}
		.checked(
			definition.name,
			definition.kind,
			definition.owner,
			definition.password,
		)
	}

	pub(crate) fn credentials(&self) -> super::Credentials {
		super::Credentials {
			username: self.username.clone(),
			password: self.password.clone(),
		}
	}

	pub fn definition(&self) -> ConnectorDefinition {
		ConnectorDefinition {
			name: self.name.clone(),
			kind: self.kind.clone(),
			owner: self.owner.clone(),
			username: self.username.clone(),
			password: self.password.clone(),
			schedule: self.schedule.to_string(),
			utc_offset: schedule::offset_text(self.utc_offset),
			area: self.area.clone(),
			tags: self.tags.clone(),
			options: self.options.to_string(),
		}
	}

	/// Whether both sign in with the same username and password, so that what one saved to
	/// sign in serves the other.
	pub fn signs_in_like(&self, other: &Self) -> bool {
		self.username == other.username && self.password == other.password
	}
}

#[cfg(test)]
mod tests {
	use serde_json::json;
	use time::macros::{offset, time};

	use super::*;

	fn input(fields: serde_json::Value) -> ConnectorInput {
		let mut body = json!({
			"name": "mf", "kind": "moneyforward", "username": " sam@example.com ",
			"password": "hunter2", "schedule": "monthly 26 09:00",
		});
		for (key, value) in fields.as_object().unwrap() {
			body[key] = value.clone();
		}
		serde_json::from_value(body).unwrap()
	}

	#[test]
	fn a_full_connector_is_read_and_stored_as_it_was() {
		let config = input(json!({
			"utc_offset": "+09:00", "area": " Work ", "tags": ["payroll", " payroll", "pay"],
			"options": {"anything": [1, {"the": "kind"}, "reads"]},
		}))
		.added(Some("sam".into()))
		.unwrap();
		assert_eq!(config.username, "sam@example.com");
		assert_eq!(config.schedule, Schedule::Monthly(26, time!(09:00)));
		assert_eq!(config.utc_offset, offset!(+9));
		assert_eq!(config.area.as_deref(), Some("Work"));
		assert_eq!(config.tags, ["payroll", "pay"]);
		assert_eq!(config.options["anything"][1]["the"], "kind");

		let definition = config.definition();
		assert_eq!(definition.utc_offset, "+09:00");
		assert_eq!(definition.schedule, "monthly 26 09:00");
		let again = ConnectorConfig::stored(definition.clone()).unwrap();
		assert_eq!(again.definition(), definition);
		assert!(!format!("{config:?}").contains("hunter2"));
	}

	#[test]
	fn defaults_are_utc_no_area_and_empty_options() {
		let config = input(json!({"area": " "})).added(None).unwrap();
		assert_eq!(config.utc_offset, UtcOffset::UTC);
		assert!(config.owner.is_none() && config.area.is_none() && config.tags.is_empty());
		assert_eq!(config.options, json!({}));
	}

	#[test]
	fn an_edit_keeps_the_password_it_does_not_send_and_what_cannot_change() {
		let current = input(json!({})).added(Some("sam".into())).unwrap();
		for blank in [json!(null), json!("")] {
			let kept = input(json!({"password": blank, "schedule": "every 6h"}))
				.replacing(&current, current.owner.clone())
				.unwrap();
			assert_eq!(kept.password, "hunter2");
			assert_eq!(kept.owner.as_deref(), Some("sam"));
			assert!(kept.signs_in_like(&current));
		}
		let given = input(json!({"password": null}))
			.replacing(&current, Some("alex".into()))
			.unwrap();
		assert_eq!(given.owner.as_deref(), Some("alex"));
		let changed = input(json!({"password": "correct horse"}))
			.replacing(&current, current.owner.clone())
			.unwrap();
		assert!(!changed.signs_in_like(&current));
		let other_user = input(json!({"username": "alex@example.com", "password": null}))
			.replacing(&current, current.owner.clone())
			.unwrap();
		assert!(!other_user.signs_in_like(&current));
		for (fields, expected) in [
			(json!({"name": "bank"}), "cannot be renamed"),
			(json!({"kind": "bank"}), "kind of a connector cannot change"),
		] {
			let error = input(fields)
				.replacing(&current, current.owner.clone())
				.err()
				.unwrap();
			assert!(error.contains(expected), "{expected}: {error}");
		}
	}

	#[test]
	fn what_does_not_add_up_is_refused() {
		for (fields, expected) in [
			(json!({"name": "a b"}), "must be letters"),
			(json!({"name": ""}), "must be letters"),
			(json!({"name": "kinds"}), "cannot be named"),
			(json!({"name": ".."}), "cannot be named"),
			(json!({"name": "a".repeat(65)}), "longer than"),
			(json!({"name": null}), "no name"),
			(json!({"kind": null}), "no kind"),
			(json!({"username": " "}), "enter the username"),
			(json!({"password": ""}), "enter the password"),
			(json!({"password": null}), "enter the password"),
			(json!({"schedule": "sometimes"}), "unknown schedule"),
			(json!({"utc_offset": "JST"}), "not a UTC offset"),
			(json!({"area": "a/b"}), "cannot be an area"),
			(json!({"tags": [" "]}), "a tag is empty"),
			(json!({"options": []}), "must be an object"),
		] {
			let error = input(fields).added(None).err().unwrap();
			assert!(error.contains(expected), "{expected}: {error}");
			assert!(!error.contains("hunter2"), "{error}");
		}
		for unknown in [json!({"hints": {}}), json!({"password_env": "P"})] {
			let mut body = json!({"username": "u", "schedule": "every 6h"});
			body[unknown.as_object().unwrap().keys().next().unwrap()] = json!("x");
			assert!(serde_json::from_value::<ConnectorInput>(body).is_err());
		}
	}
}
