use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};
use lopdf::{Dictionary, Document, Object, Stream, dictionary};
use serde_json::{Value, json};
use violet::pipeline::{self, Outcome};
use violet::{App, Models};
use violet_core::{Action, Entry, Sidecar};
use violet_model::ModelConfig;

type Handler = dyn Fn(&Value) -> (u16, String) + Send + Sync;

/// Status a handler returns for an answer that stopped at the output limit.
pub const CUT_OFF: u16 = 206;

/// An OpenAI-compatible server that answers from a closure and records what it was asked.
#[derive(Clone)]
pub struct Fake {
	handler: Arc<Handler>,
	requests: Arc<Mutex<Vec<Value>>>,
	pub base_url: String,
}

impl Fake {
	pub async fn start(handler: impl Fn(&Value) -> (u16, String) + Send + Sync + 'static) -> Self {
		let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
		let fake = Self {
			handler: Arc::new(handler),
			requests: Arc::default(),
			base_url: format!("http://{}/v1", listener.local_addr().unwrap()),
		};
		let app = Router::new()
			.route("/v1/chat/completions", post(completions))
			.route("/v1/models", get(|| async { Json(json!({"data": []})) }))
			.with_state(fake.clone());
		tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
		fake
	}

	pub fn models(&self) -> Models {
		Models {
			vision: Some(ModelConfig {
				base_url: self.base_url.clone(),
				model: "fake-vl".into(),
				api_key: None,
				timeout: Duration::from_secs(20),
			}),
			text: None,
		}
	}

	pub fn ocr_requests(&self) -> usize {
		self.requests
			.lock()
			.unwrap()
			.iter()
			.filter(|body| is_ocr(body))
			.count()
	}

	pub fn analysis_prompts(&self) -> Vec<String> {
		self.requests
			.lock()
			.unwrap()
			.iter()
			.filter(|body| !is_ocr(body))
			.map(user_text)
			.collect()
	}

	pub fn analysis_requests(&self) -> usize {
		let all = self.requests.lock().unwrap().len();
		all - self.ocr_requests()
	}
}

async fn completions(
	State(fake): State<Fake>,
	Json(body): Json<Value>,
) -> (axum::http::StatusCode, Json<Value>) {
	fake.requests.lock().unwrap().push(body.clone());
	let (status, content) = (fake.handler)(&body);
	let (status, finish_reason) = match status {
		CUT_OFF => (200, "length"),
		_ => (status, "stop"),
	};
	let answer = if status == 200 {
		json!({"choices": [{
			"index": 0,
			"message": {"role": "assistant", "content": content},
			"finish_reason": finish_reason,
		}]})
	} else {
		json!({"error": {"message": content}})
	};
	(
		axum::http::StatusCode::from_u16(status).unwrap(),
		Json(answer),
	)
}

pub fn user_text(body: &Value) -> String {
	let content = &body["messages"][1]["content"];
	match content.as_array() {
		Some(parts) => parts
			.iter()
			.filter_map(|part| part["text"].as_str())
			.collect::<Vec<_>>()
			.join("\n"),
		None => content.as_str().unwrap_or_default().to_string(),
	}
}

pub fn is_ocr(body: &Value) -> bool {
	body["messages"][0]["role"] == "user"
}

pub fn segment(pages: &[u32], title: &str, area: &str, issuer: &str, date: &str) -> Value {
	json!({
		"pages": pages,
		"rotations": [],
		"title": title,
		"area": area,
		"doc_type": "receipt",
		"issuer": issuer,
		"date_printed": date,
		"language": "en",
		"summary": "A canned summary.",
		"tags": ["canned"],
		"facts": [{"key": "total", "value": "1,200円", "amount": 1200, "currency": "JPY"}],
		"confidence": 0.9,
	})
}

pub fn analysis(segments: Vec<Value>, blank_pages: &[u32]) -> (u16, String) {
	(
		200,
		json!({"segments": segments, "blank_pages": blank_pages}).to_string(),
	)
}

/// A fake that reads blank for every OCR request and splits `three.pdf` (pharmacy receipt,
/// blank page, electricity bill) into its two documents; any other file is one receipt.
pub async fn splitting_fake() -> Fake {
	Fake::start(|body| {
		if is_ocr(body) {
			return (200, "[BLANK]".into());
		}
		let text = user_text(body);
		if text.contains("File: three.pdf") {
			analysis(
				vec![
					segment(&[1], "Receipt", "Health", "Sakura Pharmacy", "2024-03-14"),
					segment(
						&[3],
						"Electricity Bill",
						"Housing",
						"Tokyo Power",
						"2023-11-02",
					),
				],
				&[2],
			)
		} else {
			let pages: Vec<u32> = text
				.lines()
				.find_map(|line| line.strip_prefix("Pages: "))
				.unwrap()
				.split(", ")
				.map(|page| page.parse().unwrap())
				.collect();
			analysis(
				vec![segment(&pages, "Memo", "Work", "Acme", "2022-05-06")],
				&[],
			)
		}
	})
	.await
}

/// A PDF with one page per label, each showing the label as text; an empty label makes a
/// page without any text.
pub fn text_pdf(labels: &[&str]) -> Vec<u8> {
	let mut doc = Document::with_version("1.5");
	let root = doc.new_object_id();
	let font = doc.add_object(dictionary! {
		"Type" => "Font",
		"Subtype" => "Type1",
		"BaseFont" => "Helvetica",
		"Encoding" => "WinAnsiEncoding",
	});
	let mut kids: Vec<Object> = Vec::new();
	for label in labels {
		let content = if label.is_empty() {
			String::new()
		} else {
			format!("BT /F1 12 Tf 20 150 Td ({label}) Tj ET")
		};
		let content = doc.add_object(Stream::new(Dictionary::new(), content.into_bytes()));
		let page = doc.add_object(dictionary! {
			"Type" => "Page",
			"Parent" => root,
			"Contents" => content,
		});
		kids.push(page.into());
	}
	let pages = dictionary! {
		"Type" => "Pages",
		"Count" => kids.len() as i64,
		"Kids" => kids,
		"MediaBox" => vec![0.into(), 0.into(), 300.into(), 200.into()],
		"Resources" => dictionary! { "Font" => dictionary! { "F1" => font } },
	};
	doc.objects.insert(root, pages.into());
	let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => root });
	doc.trailer.set("Root", catalog);
	let mut out = Vec::new();
	doc.save_to(&mut out).unwrap();
	out
}

pub fn three_pages() -> Vec<u8> {
	text_pdf(&[
		"Sakura Pharmacy receipt 2024-03-14 total 1200 yen thank you",
		"",
		"Tokyo Power electricity bill 2023-11-02 amount due 1200 yen",
	])
}

pub struct Harness {
	pub dir: tempfile::TempDir,
	pub app: Arc<App>,
}

impl Harness {
	pub fn new(models: Models) -> Self {
		let dir = tempfile::tempdir().unwrap();
		let app = App::open(dir.path().join("root"), models).unwrap();
		Self { dir, app }
	}

	pub fn root(&self) -> PathBuf {
		self.dir.path().join("root")
	}

	/// A second process over the same library.
	pub fn reopen(&self, models: Models) -> Arc<App> {
		App::open(self.root(), models).unwrap()
	}

	pub fn drop_file(&self, name: &str, bytes: &[u8]) -> PathBuf {
		let path = self.app.library.inbox().join(name);
		std::fs::create_dir_all(path.parent().unwrap()).unwrap();
		std::fs::write(&path, bytes).unwrap();
		PathBuf::from(name)
	}

	pub async fn ingest(&self, name: &str, bytes: &[u8]) -> Outcome {
		let relative = self.drop_file(name, bytes);
		pipeline::ingest(&self.app, &relative).await.unwrap()
	}

	/// Ingests a file that becomes exactly one document and returns its id.
	pub async fn one(&self, name: &str, bytes: &[u8]) -> uuid::Uuid {
		match self.ingest(name, bytes).await {
			Outcome::Filed { documents, .. } => {
				assert_eq!(documents.len(), 1);
				documents[0].0
			}
			other => panic!("not filed: {other:?}"),
		}
	}

	pub fn documents(&self) -> Vec<(PathBuf, Sidecar)> {
		self.app.library.sidecars().unwrap()
	}

	pub fn paths(&self) -> Vec<String> {
		self.documents()
			.into_iter()
			.map(|(path, _)| path.to_string_lossy().into_owned())
			.collect()
	}

	pub fn entries(&self) -> Vec<Entry> {
		self.app.journal().entries().unwrap()
	}

	pub fn last_entry(&self) -> Entry {
		self.entries().pop().unwrap()
	}

	pub fn file_sha(&self, relative: &Path) -> String {
		violet_core::hash::sha256_file(&self.app.library.library_dir().join(relative)).unwrap()
	}

	pub fn inbox(&self) -> Vec<String> {
		names(&self.app.library.inbox())
	}

	pub fn originals(&self) -> Vec<String> {
		names(&self.app.library.originals())
	}
}

pub fn names(dir: &Path) -> Vec<String> {
	let mut names: Vec<String> = std::fs::read_dir(dir)
		.unwrap()
		.map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
		.collect();
	names.sort();
	names
}

pub fn kind(entry: &Entry) -> &'static str {
	match entry.action {
		Action::Ingest { .. } => "ingest",
		Action::Duplicate { .. } => "duplicate",
		Action::Split { .. } => "split",
		Action::Merge { .. } => "merge",
		Action::EditPages { .. } => "edit_pages",
		Action::Update { .. } => "update",
		Action::Delete { .. } => "delete",
		Action::Undo { .. } => "undo",
	}
}
