use std::sync::atomic::Ordering;

use base64::Engine;
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

pub(crate) struct Request<'a> {
	pub messages: &'a [Message],
	/// Name and JSON schema the answer must follow.
	pub schema: Option<(&'static str, &'a Value)>,
	pub max_tokens: u32,
	/// Whether the request still makes sense with its images removed.
	pub images_optional: bool,
}

/// Statuses a server uses to refuse a request feature it does not support.
fn refuses_feature(status: u16) -> bool {
	matches!(status, 400 | 415 | 422 | 500 | 501)
}

impl Client {
	fn body(&self, request: &Request<'_>, with_schema: bool, with_images: bool) -> Value {
		let messages: Vec<Value> = request
			.messages
			.iter()
			.map(|message| message.to_json(with_images))
			.collect();
		let mut body = json!({
			"model": self.config.model,
			"temperature": 0,
			"max_tokens": request.max_tokens,
			"stream": false,
			"messages": messages,
		});
		if let Some((name, schema)) = request.schema.filter(|_| with_schema) {
			body["response_format"] = json!({
				"type": "json_schema",
				"json_schema": {"name": name, "strict": true, "schema": schema},
			});
		}
		body
	}

	fn authorized(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
		match self.config.api_key.as_deref().filter(|key| !key.is_empty()) {
			Some(key) => request.bearer_auth(key),
			None => request,
		}
	}

	async fn post(&self, body: &Value) -> Result<String> {
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
		Ok(strip_thinking(&content))
	}

	/// Sends the request and returns the answer text without reasoning blocks. When the server
	/// refuses the JSON schema or the images, the request is sent again without them and the
	/// refusal is remembered for later calls.
	pub(crate) async fn complete(&self, request: &Request<'_>) -> Result<String> {
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
