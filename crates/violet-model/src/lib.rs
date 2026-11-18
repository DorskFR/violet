//! Calls an OpenAI-compatible chat completions endpoint (llama.cpp server, Claude through a
//! compatible gateway, ...) for two jobs: reading a page image (OCR) and analysing a whole
//! file (where documents start and end, what each one is, its facts).

mod amount;
mod analyze;
mod chat;
mod date;
mod grounding;
mod text;

use std::sync::atomic::AtomicBool;

use serde::{Deserialize, Serialize};
use violet_core::Fact;

pub use amount::{Amount, parse_amount};
pub use date::normalize_date;
pub use grounding::{TOPIC_TAGS, TagGrounds, title_fits_text};
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
	/// The pages are known to be one document, which is never cut into several.
	pub one_document: bool,
	/// No text could be read from the pages: they are a picture, judged from their images
	/// as one document.
	pub photo: bool,
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

/// The text read from one page image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transcription {
	pub text: String,
	/// The model hit its output limit, so the end of the page may be missing. Repeated lines
	/// it was looping on are already cut down to one.
	pub truncated: bool,
}

/// Pixels of one piece of a page sent to [`Client::ocr_page`]: about the most a Qwen-VL
/// class model looks at in one image (4096 tokens of 32 x 32 pixels). A whole page in one
/// image of that size or less loses amount columns set far from their labels and loops on
/// dense forms.
pub const OCR_TILE_PIXELS: u32 = 4_000_000;

const OCR_PROMPT: &str = include_str!("prompts/ocr.txt");
const OCR_MAX_TOKENS: u32 = 4096;
const MOSTLY_REPEATED: f32 = 0.5;
const READY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

struct Reading {
	transcription: Transcription,
	/// Most of the answer was repetition.
	looped: bool,
}

impl Reading {
	fn beats(&self, other: &Self) -> bool {
		let complete = |reading: &Self| !reading.transcription.truncated;
		let distinct = |reading: &Self| text::distinct_chars(&reading.transcription.text);
		(complete(self), distinct(self)) > (complete(other), distinct(other))
	}
}

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

	/// Transcribes one page image to plain text, keeping line breaks and reading order. An
	/// answer that ran into the output limit while repeating itself is cut back to one copy of
	/// the repeated part, and asked again once with repetition penalties when little else is
	/// left; the reading with more distinct text is kept.
	pub async fn ocr(&self, png: &[u8]) -> Result<Transcription> {
		let first = self.transcribe(png, false).await?;
		if !first.looped {
			return Ok(first.transcription);
		}
		match self.transcribe(png, true).await {
			Ok(second) if second.beats(&first) => Ok(second.transcription),
			Ok(_) => Ok(first.transcription),
			Err(error) => {
				tracing::warn!(%error, "transcription retry failed, keeping the first reading");
				Ok(first.transcription)
			}
		}
	}

	/// Transcribes a page given as images of its parts from top to bottom, each read as
	/// [`Client::ocr`] does, and joins the texts in that order.
	pub async fn ocr_page(&self, tiles: &[Vec<u8>]) -> Result<Transcription> {
		let mut texts = Vec::with_capacity(tiles.len());
		let mut truncated = false;
		for tile in tiles {
			let read = self.ocr(tile).await?;
			truncated |= read.truncated;
			if !read.text.is_empty() {
				texts.push(read.text);
			}
		}
		Ok(Transcription {
			text: texts.join("\n"),
			truncated,
		})
	}

	async fn transcribe(&self, png: &[u8], penalize_repetition: bool) -> Result<Reading> {
		let messages = [chat::Message {
			role: "user",
			parts: vec![
				chat::Part::png(png),
				chat::Part::Text(OCR_PROMPT.trim().into()),
			],
		}];
		let request = chat::Request {
			messages: &messages,
			schema: None,
			max_tokens: OCR_MAX_TOKENS,
			images_optional: false,
			penalize_repetition,
		};
		let answer = self.complete(&request).await?;
		let text = text::clean_transcription(&answer.text);
		if !answer.truncated {
			return Ok(Reading {
				transcription: Transcription {
					text,
					truncated: false,
				},
				looped: false,
			});
		}
		let cut = text::collapse_loops(&text);
		let looped = cut.chars().count() * 2 < text.chars().count()
			|| text::repeated_share(&cut) > MOSTLY_REPEATED;
		tracing::warn!(
			chars = text.chars().count(),
			kept = cut.chars().count(),
			looped,
			penalize_repetition,
			"transcription hit the output limit"
		);
		Ok(Reading {
			transcription: Transcription {
				text: cut,
				truncated: true,
			},
			looped,
		})
	}

	/// The answer is validated (pages covered once, pages with text not blank, amounts parsed,
	/// title in a script of the page), retried once with the validation errors when it does not
	/// hold, and repaired with lowered confidence when it still does not. An answer that puts
	/// no page with text (or no photo) in a segment is retried as one document and then
	/// refused with [`Error::Answer`]; in a file cut into documents, such a document joins the
	/// one before it (or after it) instead. Among the repairs: a title in another
	/// script is replaced by the first line of the page. Tags that are neither a topic of
	/// [`TOPIC_TAGS`] nor printed on the pages are dropped. The model reports the date and the total as printed;
	/// they are converted here and, like every fact, kept only when found in the page text. An
	/// issuer that is an address or an addressee is dropped, one that is not in the page text
	/// is asked about once more and then kept with lowered confidence. A file whose text
	/// exceeds [`Limits::text_budget_chars`] is cut into documents first and analysed one
	/// document per call, unless [`AnalyzeInput::one_document`] is set.
	pub async fn analyze(&self, input: &AnalyzeInput) -> Result<Analysis> {
		analyze::analyze(self, input).await
	}
}
