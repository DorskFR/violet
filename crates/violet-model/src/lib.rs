//! Calls an OpenAI-compatible chat completions endpoint (llama.cpp server, Claude through a
//! compatible gateway, ...) for two jobs: reading a page image (OCR) and analysing a whole
//! file (where documents start and end, what each one is, its facts).

mod amount;
mod analyze;
mod chat;
mod date;
mod text;

use std::sync::atomic::AtomicBool;

use serde::{Deserialize, Serialize};
use violet_core::Fact;

pub use amount::{Amount, parse_amount};
pub use date::normalize_date;
pub use text::strip_thinking;

#[derive(Debug, thiserror::Error)]
pub enum Error {
	#[error("http: {0}")]
	Http(#[from] reqwest::Error),
	#[error("model returned {status}: {body}")]
	Status { status: u16, body: String },
	#[error("unusable answer: {0}")]
	Answer(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, Clone)]
pub struct ModelConfig {
	/// Base URL ending before `/chat/completions`, e.g. `http://vision:8080/v1`.
	pub base_url: String,
	pub model: String,
	pub api_key: Option<String>,
	pub timeout: std::time::Duration,
}

/// How much text and how many images go into one request. The defaults fit a model with a
/// context of about 8k tokens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Limits {
	/// Page text, in characters, one analysis request may carry. A file above it is first cut
	/// into documents by comparing adjacent pages, then analysed document by document.
	pub text_budget_chars: usize,
	/// Longest text kept per page; the middle of a longer page is dropped.
	pub page_chars: usize,
	/// Text kept per page when two adjacent pages are compared.
	pub boundary_page_chars: usize,
	/// Most thumbnails sent in one request. A file with more sends only its first page.
	pub max_thumbnails: usize,
}

impl Default for Limits {
	fn default() -> Self {
		Self {
			text_budget_chars: 6000,
			page_chars: 3000,
			boundary_page_chars: 700,
			max_thumbnails: 4,
		}
	}
}

pub struct Client {
	http: reqwest::Client,
	config: ModelConfig,
	limits: Limits,
	schema_refused: AtomicBool,
	images_refused: AtomicBool,
}

#[derive(Debug, Clone)]
pub struct PageInput {
	/// 1-based.
	pub number: u32,
	/// Text layer or OCR text.
	pub text: String,
	/// Small rendering (about 512 px), sent only when the endpoint accepts images.
	pub thumbnail_png: Option<Vec<u8>>,
}

#[derive(Debug, Clone)]
pub struct AnalyzeInput {
	pub file_name: String,
	pub pages: Vec<PageInput>,
	/// Existing values in the library, so new documents reuse them.
	pub known_areas: Vec<String>,
	pub known_issuers: Vec<String>,
	pub known_tags: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Analysis {
	/// Every non-blank page appears in exactly one segment.
	pub segments: Vec<Segment>,
	/// Blank or separator pages to drop.
	pub blank_pages: Vec<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Segment {
	/// 1-based pages of the input, in reading order.
	pub pages: Vec<u32>,
	/// Pages that are upside down or sideways, with the clockwise rotation that fixes them.
	pub rotations: Vec<Rotation>,
	pub title: String,
	pub area: String,
	pub doc_type: String,
	pub issuer: Option<String>,
	/// `YYYY-MM-DD`.
	pub date: Option<String>,
	pub language: Option<String>,
	pub summary: Option<String>,
	pub tags: Vec<String>,
	pub facts: Vec<Fact>,
	/// 0..1.
	pub confidence: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rotation {
	pub page: u32,
	pub degrees: i32,
}

const OCR_PROMPT: &str = include_str!("prompts/ocr.txt");
const OCR_MAX_TOKENS: u32 = 4096;
const READY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

impl Client {
	pub fn new(config: ModelConfig) -> Result<Self> {
		let http = reqwest::Client::builder().timeout(config.timeout).build()?;
		Ok(Self {
			http,
			config,
			limits: Limits::default(),
			schema_refused: AtomicBool::new(false),
			images_refused: AtomicBool::new(false),
		})
	}

	pub fn with_limits(mut self, limits: Limits) -> Self {
		self.limits = limits;
		self
	}

	fn base_url(&self) -> &str {
		self.config.base_url.trim_end_matches('/')
	}

	/// `GET {base_url}/models` answers.
	pub async fn ready(&self) -> bool {
		self.models_answer(READY_TIMEOUT.min(self.config.timeout))
			.await
	}

	/// Transcribes one page image to plain text, keeping line breaks and reading order.
	pub async fn ocr(&self, png: &[u8]) -> Result<String> {
		let messages = [
			chat::Message::text("system", OCR_PROMPT.trim()),
			chat::Message {
				role: "user",
				parts: vec![
					chat::Part::png(png),
					chat::Part::Text("Transcribe this page.".into()),
				],
			},
		];
		let request = chat::Request {
			messages: &messages,
			schema: None,
			max_tokens: OCR_MAX_TOKENS,
			images_optional: false,
		};
		Ok(text::clean_transcription(&self.complete(&request).await?))
	}

	/// The answer is validated (pages covered once, valid dates, amounts parsed), retried once
	/// with the validation errors when it does not hold, and repaired with lowered confidence
	/// when it still does not. A file whose text exceeds [`Limits::text_budget_chars`] is cut
	/// into documents first and analysed one document per call.
	pub async fn analyze(&self, input: &AnalyzeInput) -> Result<Analysis> {
		analyze::analyze(self, input).await
	}
}
