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
	/// `YYYY-MM-DD`, the date printed on the document. A period (a payslip's month) is its
	/// first day, with `date_precision` saying so.
	pub date: Option<String>,
	#[serde(default, skip_serializing_if = "DatePrecision::is_day")]
	pub date_precision: DatePrecision,
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
	/// Documents a user said this one is not a copy of.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub not_duplicates: Vec<Uuid>,
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

/// How much of `date` is printed on the document.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DatePrecision {
	#[default]
	Day,
	/// Only the month and the year: the day is 01.
	Month,
}

impl DatePrecision {
	pub fn is_day(&self) -> bool {
		*self == Self::Day
	}
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
	/// Set when the file came from a mail.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub mail: Option<MailSource>,
	/// Set when a connector fetched the file.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub connector: Option<ConnectorSource>,
}

/// The remote document a connector fetched a file as.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectorSource {
	pub name: String,
	pub kind: String,
	pub remote_id: String,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub title: Option<String>,
	/// As the site gives it, `YYYY-MM-DD` when it can.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub date: Option<String>,
}

/// The mail a file was taken from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MailSource {
	pub account: String,
	pub folder: String,
	pub uidvalidity: u32,
	pub uid: u32,
	pub message_id: Option<String>,
	pub from: Option<String>,
	pub subject: Option<String>,
	/// RFC 3339, from the `Date` header.
	pub date: Option<String>,
}

#[cfg(test)]
mod tests {
	use serde_json::json;

	use super::*;

	fn sample() -> serde_json::Value {
		json!({
			"version": 1,
			"id": "5b0c1f0e-3a55-4f0c-9d5e-0a8f1f3f2c11",
			"title": "国民健康保険料 領収書",
			"area": "Health",
			"doc_type": "receipt",
			"issuer": "渋谷区役所",
			"date": "2024-04-10",
			"language": "ja",
			"summary": null,
			"tags": ["insurance"],
			"facts": [
				{"key": "total", "value": "12,300円", "amount_minor": 12300, "currency": "JPY"},
				{"key": "account", "value": "1234567", "amount_minor": null, "currency": null}
			],
			"pages": [
				{"number": 1, "text": "領収書", "source": "ocr"},
				{"number": 2, "text": "", "source": "none"},
				{"number": 3, "text": "a", "source": "layer"},
				{"number": 4, "text": "b", "source": "extracted"}
			],
			"sources": [{"sha256": "abc", "original_name": "scan.pdf", "pages": [1, 2]}],
			"sha256": "def",
			"confidence": 0.5,
			"created_at": "2024-04-11T09:30:00+09:00",
			"updated_at": "2024-04-12T00:00:00Z",
			"reviewed_by": "someone",
			"layout": {"columns": 2, "regions": [[0, 1], [2, 3]]}
		})
	}

	#[test]
	fn unknown_fields_round_trip() {
		let sidecar: Sidecar = serde_json::from_value(sample()).unwrap();
		assert_eq!(sidecar.version, SIDECAR_VERSION);
		assert_eq!(sidecar.extra.len(), 2);
		assert_eq!(sidecar.extra["reviewed_by"], "someone");
		assert_eq!(sidecar.extra["layout"]["regions"][1], json!([2, 3]));

		let text = serde_json::to_string_pretty(&sidecar).unwrap();
		let again: Sidecar = serde_json::from_str(&text).unwrap();
		assert_eq!(again, sidecar);
		let value = serde_json::to_value(&again).unwrap();
		assert_eq!(value["reviewed_by"], "someone");
		assert_eq!(value["layout"], sample()["layout"]);
		assert!(value.get("extra").is_none());
	}

	#[test]
	fn known_fields_keep_their_json_shape() {
		let sidecar: Sidecar = serde_json::from_value(sample()).unwrap();
		assert_eq!(sidecar.facts[0].amount_minor, Some(12_300));
		assert_eq!(sidecar.facts[1].currency, None);
		let sources: Vec<TextSource> = sidecar.pages.iter().map(|page| page.source).collect();
		assert_eq!(
			sources,
			[
				TextSource::Ocr,
				TextSource::None,
				TextSource::Layer,
				TextSource::Extracted
			]
		);
		assert_eq!(sidecar.sources[0].pages, [1, 2]);
		assert_eq!(sidecar.created_at.unix_timestamp(), 1_712_795_400);

		let value = serde_json::to_value(&sidecar).unwrap();
		assert_eq!(
			value["pages"][0],
			json!({"number": 1, "text": "領収書", "source": "ocr"})
		);
		assert_eq!(value["summary"], serde_json::Value::Null);
		assert_eq!(value["date"], "2024-04-10");
		assert!(
			value["updated_at"]
				.as_str()
				.unwrap()
				.starts_with("2024-04-12T00:00:00")
		);
	}

	#[test]
	fn date_precision_is_written_only_for_a_period() {
		let mut sidecar: Sidecar = serde_json::from_value(sample()).unwrap();
		assert_eq!(sidecar.date_precision, DatePrecision::Day);
		assert!(
			serde_json::to_value(&sidecar)
				.unwrap()
				.get("date_precision")
				.is_none()
		);
		sidecar.date_precision = DatePrecision::Month;
		let value = serde_json::to_value(&sidecar).unwrap();
		assert_eq!(value["date_precision"], "month");
		let again: Sidecar = serde_json::from_value(value).unwrap();
		assert_eq!(again.date_precision, DatePrecision::Month);
		assert!(again.extra.get("date_precision").is_none());
	}

	#[test]
	fn missing_required_fields_are_rejected() {
		let mut value = sample();
		value.as_object_mut().unwrap().remove("title");
		assert!(serde_json::from_value::<Sidecar>(value).is_err());
	}
}
