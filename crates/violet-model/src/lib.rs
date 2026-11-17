//! Calls an OpenAI-compatible chat completions endpoint (llama.cpp server, Claude through a
//! compatible gateway, ...) for two jobs: reading a page image (OCR) and analysing a whole
//! file (where documents start and end, what each one is, its facts).

use serde::{Deserialize, Serialize};
use violet_core::Fact;

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

pub struct Client {
	http: reqwest::Client,
	config: ModelConfig,
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

impl Client {
	pub fn new(config: ModelConfig) -> Result<Self> {
		todo!()
	}
	/// `GET {base_url}/models` answers.
	pub async fn ready(&self) -> bool {
		todo!()
	}
	/// Transcribes one page image to plain text, keeping line breaks and reading order.
	pub async fn ocr(&self, png: &[u8]) -> Result<String> {
		todo!()
	}
	/// One call over the whole file; the answer is validated (pages covered once, valid dates,
	/// amounts parsed) and retried once with the validation error when it does not hold.
	pub async fn analyze(&self, input: &AnalyzeInput) -> Result<Analysis> {
		todo!()
	}
}
