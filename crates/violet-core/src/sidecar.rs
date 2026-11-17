use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

pub const SIDECAR_VERSION: u32 = 1;

/// Metadata stored next to each document as `<stem>.json`. Unknown fields are kept so a newer
/// sidecar survives an older binary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sidecar {
	pub version: u32,
	pub id: Uuid,
	pub title: String,
	/// Top-level folder, e.g. "Health", "Tax", "Housing".
	pub area: String,
	/// Kind of document, e.g. "receipt", "payslip", "contract".
	pub doc_type: String,
	pub issuer: Option<String>,
	/// `YYYY-MM-DD`, the date printed on the document.
	pub date: Option<String>,
	pub language: Option<String>,
	pub summary: Option<String>,
	pub tags: Vec<String>,
	pub facts: Vec<Fact>,
	pub pages: Vec<PageText>,
	/// Files this document came from (one for a plain ingest, several after a merge).
	pub sources: Vec<Source>,
	/// SHA-256 of the document file as written.
	pub sha256: String,
	/// 0..1 from the analysis; 1.0 when set by hand.
	pub confidence: f32,
	#[serde(with = "time::serde::rfc3339")]
	pub created_at: OffsetDateTime,
	#[serde(with = "time::serde::rfc3339")]
	pub updated_at: OffsetDateTime,
	#[serde(flatten)]
	pub extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Fact {
	/// e.g. "total", "tax", "net_pay", "due_date", "account".
	pub key: String,
	/// The value as printed.
	pub value: String,
	/// Amount in minor units of `currency` (JPY has none: 1200 JPY = 1200).
	pub amount_minor: Option<i64>,
	/// ISO 4217, e.g. "JPY".
	pub currency: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PageText {
	/// 1-based.
	pub number: u32,
	pub text: String,
	pub source: TextSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextSource {
	/// The PDF's own text layer.
	Layer,
	Ocr,
	/// Extracted from an office file or email.
	Extracted,
	None,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Source {
	pub sha256: String,
	pub original_name: String,
	/// 1-based pages of the source used, in order. Empty means all.
	pub pages: Vec<u32>,
}
