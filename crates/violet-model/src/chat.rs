use std::sync::atomic::Ordering;

use base64::Engine;
use serde::ser::{Serialize, SerializeMap, Serializer};
use serde_json::{Value, json};

use crate::text::{strip_thinking, truncate_head};
use crate::{Client, Error, Result};

const ERROR_BODY_CHARS: usize = 500;

pub(crate) enum Part {
	Text(String),
	/// A `data:` URL.
	Image(String),
}

impl Part {
	pub(crate) fn png(bytes: &[u8]) -> Self {
		let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
		Self::Image(format!("data:image/png;base64,{encoded}"))
	}
}

pub(crate) struct Message {
	pub role: &'static str,
	pub parts: Vec<Part>,
}

impl Message {
	pub(crate) fn text(role: &'static str, text: impl Into<String>) -> Self {
		Self {
			role,
			parts: vec![Part::Text(text.into())],
		}
	}

	fn has_images(&self) -> bool {
		self.parts.iter().any(|part| matches!(part, Part::Image(_)))
	}

	fn to_json(&self, with_images: bool) -> Value {
		let content = if with_images && self.has_images() {
			Value::Array(
				self.parts
					.iter()
					.map(|part| match part {
						Part::Text(text) => json!({"type": "text", "text": text}),
						Part::Image(url) => json!({"type": "image_url", "image_url": {"url": url}}),
					})
					.collect(),
			)
		} else {
			let texts: Vec<&str> = self
				.parts
				.iter()
				.filter_map(|part| match part {
					Part::Text(text) => Some(text.as_str()),
					Part::Image(_) => None,
				})
				.collect();
			Value::String(texts.join("\n\n"))
		};
		json!({"role": self.role, "content": content})
	}
}

/// A JSON schema whose object properties keep their order: a server that constrains the
/// answer to the schema makes the model write them in that order, so what is read comes
/// before what is concluded from it.
pub(crate) enum Schema {
	Boolean,
	Integer,
	IntegerIn(&'static [i64]),
	Number,
	NullableNumber,
	String,
	NullableString,
	Array {
		items: Box<Schema>,
		least: usize,
		most: Option<usize>,
	},
	/// Every property is required and no other is allowed.
	Object(Vec<(&'static str, Schema)>),
}

impl Schema {
	pub(crate) fn array(items: Schema) -> Self {
		Self::Array {
			items: Box::new(items),
			least: 0,
			most: None,
		}
	}

	pub(crate) fn array_up_to(items: Schema, most: usize) -> Self {
		Self::Array {
			items: Box::new(items),
			least: 0,
			most: Some(most),
		}
	}

	pub(crate) fn array_at_least(items: Schema, least: usize) -> Self {
		Self::Array {
			items: Box::new(items),
			least,
			most: None,
		}
	}
}

struct Properties<'a>(&'a [(&'static str, Schema)]);

impl Serialize for Properties<'_> {
	fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
		let mut map = serializer.serialize_map(Some(self.0.len()))?;
		for (name, schema) in self.0 {
			map.serialize_entry(name, schema)?;
		}
		map.end()
	}
}

impl Serialize for Schema {
	fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
		let mut map = serializer.serialize_map(None)?;
		match self {
			Self::Boolean => map.serialize_entry("type", "boolean")?,
			Self::Integer => map.serialize_entry("type", "integer")?,
			Self::IntegerIn(values) => {
				map.serialize_entry("type", "integer")?;
				map.serialize_entry("enum", values)?;
			}
			Self::Number => map.serialize_entry("type", "number")?,
			Self::NullableNumber => map.serialize_entry("type", &["number", "null"])?,
			Self::String => map.serialize_entry("type", "string")?,
			Self::NullableString => map.serialize_entry("type", &["string", "null"])?,
			Self::Array { items, least, most } => {
				map.serialize_entry("type", "array")?;
				map.serialize_entry("items", items)?;
				if *least > 0 {
					map.serialize_entry("minItems", least)?;
				}
				if let Some(most) = most {
					map.serialize_entry("maxItems", most)?;
				}
			}
			Self::Object(properties) => {
				let required: Vec<&str> = properties.iter().map(|(name, _)| *name).collect();
				map.serialize_entry("type", "object")?;
				map.serialize_entry("properties", &Properties(properties))?;
				map.serialize_entry("required", &required)?;
				map.serialize_entry("additionalProperties", &false)?;
			}
		}
		map.end()
	}
}

#[derive(serde::Serialize)]
struct JsonSchema<'a> {
	name: &'static str,
	strict: bool,
	schema: &'a Schema,
}

#[derive(serde::Serialize)]
struct ResponseFormat<'a> {
	r#type: &'static str,
	json_schema: JsonSchema<'a>,
}

#[derive(serde::Serialize)]
struct Body<'a> {
	model: &'a str,
	temperature: u8,
	max_tokens: u32,
	stream: bool,
	messages: Vec<Value>,
	#[serde(skip_serializing_if = "Option::is_none")]
	response_format: Option<ResponseFormat<'a>>,
	#[serde(skip_serializing_if = "Option::is_none")]
	repeat_penalty: Option<f64>,
	#[serde(skip_serializing_if = "Option::is_none")]
	presence_penalty: Option<f64>,
	#[serde(skip_serializing_if = "Option::is_none")]
	dry_multiplier: Option<f64>,
}

pub(crate) struct Request<'a> {
	pub messages: &'a [Message],
	/// Name and JSON schema the answer must follow.
	pub schema: Option<(&'static str, &'a Schema)>,
	pub max_tokens: u32,
	/// Whether the request still makes sense with its images removed.
	pub images_optional: bool,
	/// Sampling penalties against an answer that repeats itself until the token limit.
	pub penalize_repetition: bool,
}

pub(crate) struct Completion {
	/// The answer without reasoning blocks.
	pub text: String,
	/// The answer stopped at `max_tokens`.
	pub truncated: bool,
}

const REPEAT_PENALTY: f64 = 1.15;
const PRESENCE_PENALTY: f64 = 1.5;
const DRY_MULTIPLIER: f64 = 1.5;

/// Statuses a server uses to refuse a request feature it does not support.
fn refuses_feature(status: u16) -> bool {
	matches!(status, 400 | 415 | 422 | 500 | 501)
}

impl Client {
	fn body<'a>(&'a self, request: &Request<'a>, with_schema: bool, with_images: bool) -> Body<'a> {
		let penalty = |value: f64| request.penalize_repetition.then_some(value);
		Body {
			model: &self.config.model,
			temperature: 0,
			max_tokens: request.max_tokens,
			stream: false,
			messages: request
				.messages
				.iter()
				.map(|message| message.to_json(with_images))
				.collect(),
			response_format: request
				.schema
				.filter(|_| with_schema)
				.map(|(name, schema)| ResponseFormat {
					r#type: "json_schema",
					json_schema: JsonSchema {
						name,
						strict: true,
						schema,
					},
				}),
			repeat_penalty: penalty(REPEAT_PENALTY),
			presence_penalty: penalty(PRESENCE_PENALTY),
			dry_multiplier: penalty(DRY_MULTIPLIER),
		}
	}

	fn authorized(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
		match self.config.api_key.as_deref().filter(|key| !key.is_empty()) {
			Some(key) => request.bearer_auth(key),
			None => request,
		}
	}

	async fn post(&self, body: &Body<'_>) -> Result<Completion> {
		let url = format!("{}/chat/completions", self.base_url());
		let response = self
			.authorized(self.http.post(url))
			.json(body)
			.send()
			.await?;
		let status = response.status();
		let text = response.text().await?;
		if !status.is_success() {
			return Err(Error::Status {
				status: status.as_u16(),
				body: truncate_head(text.trim(), ERROR_BODY_CHARS).to_string(),
			});
		}
		let answer: Value = serde_json::from_str(&text)
			.map_err(|error| Error::Answer(format!("response is not JSON: {error}")))?;
		let content = match &answer["choices"][0]["message"]["content"] {
			Value::String(content) => content.clone(),
			Value::Array(parts) => parts
				.iter()
				.filter_map(|part| part["text"].as_str())
				.collect(),
			_ => return Err(Error::Answer("response has no message content".into())),
		};
		Ok(Completion {
			text: strip_thinking(&content),
			truncated: answer["choices"][0]["finish_reason"] == "length",
		})
	}

	/// Sends the request. When the server refuses the JSON schema or the images, the request
	/// is sent again without them and the refusal is remembered for later calls.
	pub(crate) async fn complete(&self, request: &Request<'_>) -> Result<Completion> {
		let schema = request.schema.is_some() && !self.schema_refused.load(Ordering::Relaxed);
		let images = request.messages.iter().any(Message::has_images)
			&& !(request.images_optional && self.images_refused.load(Ordering::Relaxed));
		let mut attempts = vec![(schema, images)];
		if schema {
			attempts.push((false, images));
		}
		if images && request.images_optional {
			if schema {
				attempts.push((true, false));
			}
			attempts.push((false, false));
		}
		let last = attempts.len() - 1;
		let mut remember = true;
		for (index, &(with_schema, with_images)) in attempts.iter().enumerate() {
			match self
				.post(&self.body(request, with_schema, with_images))
				.await
			{
				Err(Error::Status { status, body }) if index < last && refuses_feature(status) => {
					tracing::warn!(
						status,
						body,
						with_schema,
						with_images,
						"request refused, degrading"
					);
					remember &= status != 500;
				}
				Ok(answer) => {
					if remember && schema && !with_schema {
						self.schema_refused.store(true, Ordering::Relaxed);
					}
					if remember && images && !with_images {
						self.images_refused.store(true, Ordering::Relaxed);
					}
					return Ok(answer);
				}
				Err(error) => return Err(error),
			}
		}
		unreachable!("the last attempt always returns")
	}

	pub(crate) async fn models_answer(&self, timeout: std::time::Duration) -> bool {
		let request = self
			.http
			.get(format!("{}/models", self.base_url()))
			.timeout(timeout);
		matches!(self.authorized(request).send().await, Ok(response) if response.status().is_success())
	}
}
