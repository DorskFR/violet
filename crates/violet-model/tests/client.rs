use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use serde_json::{Value, json};
use violet_model::{AnalyzeInput, Client, Error, Limits, ModelConfig, PageInput};

type Handler = dyn Fn(&Value, usize) -> (u16, Value) + Send + Sync;

#[derive(Clone)]
struct Fake {
	handler: Arc<Handler>,
	requests: Arc<Mutex<Vec<Value>>>,
	keys: Arc<Mutex<Vec<Option<String>>>>,
	base_url: String,
}

impl Fake {
	async fn start(
		handler: impl Fn(&Value, usize) -> (u16, Value) + Send + Sync + 'static,
	) -> Self {
		let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
		let fake = Self {
			handler: Arc::new(handler),
			requests: Arc::default(),
			keys: Arc::default(),
			base_url: format!("http://{}/v1/", listener.local_addr().unwrap()),
		};
		let app = Router::new()
			.route("/v1/chat/completions", post(completions))
			.route("/v1/models", get(models))
			.with_state(fake.clone());
		tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
		fake
	}

	fn client(&self) -> Client {
		Client::new(ModelConfig {
			base_url: self.base_url.clone(),
			model: "qwen3-vl".into(),
			api_key: Some("sekrit".into()),
			timeout: Duration::from_secs(10),
		})
		.unwrap()
	}

	fn requests(&self) -> Vec<Value> {
		self.requests.lock().unwrap().clone()
	}
}

fn authorization(headers: &HeaderMap) -> Option<String> {
	Some(headers.get("authorization")?.to_str().ok()?.to_string())
}

async fn completions(
	State(fake): State<Fake>,
	headers: HeaderMap,
	Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
	let index = {
		let mut requests = fake.requests.lock().unwrap();
		requests.push(body.clone());
		requests.len() - 1
	};
	fake.keys.lock().unwrap().push(authorization(&headers));
	let (status, answer) = (fake.handler)(&body, index);
	(StatusCode::from_u16(status).unwrap(), Json(answer))
}

async fn models(headers: HeaderMap) -> (StatusCode, Json<Value>) {
	if authorization(&headers).as_deref() == Some("Bearer sekrit") {
		(
			StatusCode::OK,
			Json(json!({"object": "list", "data": [{"id": "qwen3-vl"}]})),
		)
	} else {
		(StatusCode::UNAUTHORIZED, Json(json!({"error": "no key"})))
	}
}

fn reply(content: impl Into<String>) -> (u16, Value) {
	let message = json!({"role": "assistant", "content": content.into()});
	(
		200,
		json!({"choices": [{"index": 0, "message": message, "finish_reason": "stop"}]}),
	)
}

fn cut_off(content: impl Into<String>) -> (u16, Value) {
	let message = json!({"role": "assistant", "content": content.into()});
	(
		200,
		json!({"choices": [{"index": 0, "message": message, "finish_reason": "length"}]}),
	)
}

fn refusal(message: &str) -> (u16, Value) {
	(400, json!({"error": {"message": message}}))
}

fn user_text(body: &Value) -> String {
	let content = &body["messages"][1]["content"];
	match content.as_array() {
		Some(parts) => parts
			.iter()
			.filter_map(|part| part["text"].as_str())
			.collect::<Vec<_>>()
			.join("\n"),
		None => content.as_str().unwrap().to_string(),
	}
}

fn image_urls(body: &Value) -> Vec<String> {
	body["messages"]
		.as_array()
		.unwrap()
		.iter()
		.filter_map(|message| message["content"].as_array())
		.flatten()
		.filter_map(|part| part["image_url"]["url"].as_str())
		.map(String::from)
		.collect()
}

fn listed_pages(body: &Value) -> Vec<u32> {
	let text = user_text(body);
	let line = text
		.lines()
		.find_map(|line| line.strip_prefix("Pages: "))
		.unwrap();
	line.split(", ").map(|page| page.parse().unwrap()).collect()
}

fn page(number: u32, text: &str) -> PageInput {
	PageInput {
		number,
		text: text.into(),
		thumbnail_png: None,
	}
}

fn input(pages: Vec<PageInput>) -> AnalyzeInput {
	AnalyzeInput {
		file_name: "scan.pdf".into(),
		pages,
		one_document: false,
		photo: false,
		known_areas: vec!["Health".into()],
		known_issuers: vec!["さくら薬局".into()],
		known_tags: vec!["pharmacy".into()],
	}
}

fn segment(pages: &[u32], title: &str) -> Value {
	json!({
		"pages": pages,
		"rotations": [],
		"title": title,
		"area": "Health",
		"doc_type": "receipt",
		"issuer": "さくら薬局",
		"date_printed": "2024-03-14",
		"language": "ja",
		"summary": "Pharmacy receipt.",
		"tags": ["pharmacy"],
		"facts": [{"key": "total", "value": "1,200円", "amount": 1200, "currency": "JPY"}],
		"confidence": 0.9,
	})
}

fn analysis(segments: Vec<Value>, blank_pages: &[u32]) -> String {
	json!({"segments": segments, "blank_pages": blank_pages}).to_string()
}

const PNG: &[u8] = b"\x89PNG\r\n\x1a\nnot really a png";

#[tokio::test]
async fn ocr_sends_the_image_and_unwraps_the_answer() {
	let fake =
		Fake::start(|_, _| reply("<think>\nA receipt.\n</think>\n\n```\n領収書\n合計 ¥1,200\n```"))
			.await;
	let read = fake.client().ocr(PNG).await.unwrap();
	assert_eq!(read.text, "領収書\n合計 ¥1,200");
	assert!(!read.truncated);

	let requests = fake.requests();
	assert_eq!(requests.len(), 1);
	let body = &requests[0];
	assert_eq!(body["model"], "qwen3-vl");
	assert_eq!(body["temperature"], 0);
	assert_eq!(body["stream"], false);
	assert!(body.get("response_format").is_none());
	assert_eq!(body["messages"].as_array().unwrap().len(), 1);
	assert_eq!(body["messages"][0]["role"], "user");
	assert!(
		body["messages"][0]["content"][1]["text"]
			.as_str()
			.unwrap()
			.contains("[BLANK]")
	);
	let urls = image_urls(body);
	let encoded = urls[0].strip_prefix("data:image/png;base64,").unwrap();
	assert_eq!(
		base64::engine::general_purpose::STANDARD
			.decode(encoded)
			.unwrap(),
		PNG
	);
	assert_eq!(
		fake.keys.lock().unwrap()[0].as_deref(),
		Some("Bearer sekrit")
	);
}

fn looping_transcription() -> String {
	format!("領収書\nさくら薬局\n{}", "かぜ薬 | 600円\n".repeat(400))
}

const QUICK_RETRIES: [Duration; 2] = [Duration::from_millis(10), Duration::from_millis(20)];

#[tokio::test]
async fn transient_failures_are_tried_again_with_backoff() {
	let fake = Fake::start(|_, index| match index {
		0 => (503, json!({"error": "loading model"})),
		1 => (502, json!({"error": "bad gateway"})),
		_ => reply("領収書"),
	})
	.await;
	let client = fake.client().with_retry_delays(QUICK_RETRIES.to_vec());
	assert_eq!(client.ocr(PNG).await.unwrap().text, "領収書");
	assert_eq!(fake.requests().len(), 3);
}

#[tokio::test]
async fn transient_failures_give_up_after_the_last_delay() {
	let fake = Fake::start(|_, _| (504, json!({"error": "gateway timeout"}))).await;
	let client = fake.client().with_retry_delays(QUICK_RETRIES.to_vec());
	assert!(matches!(
		client.ocr(PNG).await,
		Err(Error::Status { status: 504, .. })
	));
	assert_eq!(fake.requests().len(), 3);
}

#[tokio::test]
async fn other_failures_are_not_tried_again() {
	let fake = Fake::start(|_, _| (401, json!({"error": "no key"}))).await;
	let client = fake.client().with_retry_delays(QUICK_RETRIES.to_vec());
	assert!(matches!(
		client.ocr(PNG).await,
		Err(Error::Status { status: 401, .. })
	));
	assert_eq!(fake.requests().len(), 1);
}

#[tokio::test]
async fn a_model_that_cannot_be_reached_is_tried_again() {
	let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
	let address = listener.local_addr().unwrap();
	drop(listener);
	let client = Client::new(ModelConfig {
		base_url: format!("http://{address}/v1"),
		model: "qwen3-vl".into(),
		api_key: None,
		timeout: Duration::from_secs(10),
	})
	.unwrap()
	.with_retry_delays(vec![Duration::from_millis(300)]);
	let started = std::time::Instant::now();
	assert!(matches!(client.ocr(PNG).await, Err(Error::Http(_))));
	assert!(started.elapsed() >= Duration::from_millis(300));
}

#[tokio::test]
async fn looping_ocr_is_asked_again_with_repetition_penalties() {
	let fake = Fake::start(|body, _| {
		if body.get("repeat_penalty").is_some() {
			reply("領収書\nさくら薬局\nかぜ薬 | 600円\n合計 | 600円")
		} else {
			cut_off(looping_transcription())
		}
	})
	.await;
	let read = fake.client().ocr(PNG).await.unwrap();
	assert_eq!(
		read.text,
		"領収書\nさくら薬局\nかぜ薬 | 600円\n合計 | 600円"
	);
	assert!(!read.truncated);

	let requests = fake.requests();
	assert_eq!(requests.len(), 2);
	assert!(requests[0].get("presence_penalty").is_none());
	assert!(requests[0].get("dry_multiplier").is_none());
	assert!(requests[1]["presence_penalty"].as_f64().unwrap() > 0.0);
	assert!(requests[1]["dry_multiplier"].as_f64().unwrap() > 0.0);
	assert_eq!(image_urls(&requests[1]).len(), 1);
}

#[tokio::test]
async fn looping_ocr_keeps_one_copy_when_asking_again_does_not_help() {
	for second in [
		refusal("unknown parameter repeat_penalty"),
		cut_off("領収書\n"),
	] {
		let fake = Fake::start(move |_, index| match index {
			0 => cut_off(looping_transcription()),
			_ => second.clone(),
		})
		.await;
		let read = fake.client().ocr(PNG).await.unwrap();
		assert_eq!(read.text, "領収書\nさくら薬局\nかぜ薬 | 600円");
		assert!(read.truncated);
		assert_eq!(fake.requests().len(), 2);
	}
}

#[tokio::test]
async fn cut_off_ocr_without_a_loop_is_kept_and_flagged() {
	let page: String = (1..=300).map(|line| format!("第{line}条 本文\n")).collect();
	let fake = Fake::start({
		let page = page.clone();
		move |_, _| cut_off(page.clone())
	})
	.await;
	let read = fake.client().ocr(PNG).await.unwrap();
	assert_eq!(read.text, page.trim());
	assert!(read.truncated);
	assert_eq!(fake.requests().len(), 1);
}

#[tokio::test]
async fn a_page_in_tiles_is_read_tile_by_tile_and_joined() {
	let fake = Fake::start(|_, index| match index {
		0 => reply("領収書\nさくら薬局"),
		1 => reply("[BLANK]"),
		_ => reply("合計 | ¥1,200"),
	})
	.await;
	let tiles = [b"top".to_vec(), b"middle".to_vec(), b"bottom".to_vec()];
	let read = fake.client().ocr_page(&tiles).await.unwrap();
	assert_eq!(read.text, "領収書\nさくら薬局\n合計 | ¥1,200");
	assert!(!read.truncated);

	let requests = fake.requests();
	assert_eq!(requests.len(), 3);
	for (body, tile) in requests.iter().zip(&tiles) {
		let encoded = base64::engine::general_purpose::STANDARD.encode(tile);
		assert_eq!(
			image_urls(body),
			[format!("data:image/png;base64,{encoded}")]
		);
	}
}

#[tokio::test]
async fn a_page_with_a_cut_off_tile_is_flagged() {
	let page: String = (1..=300).map(|line| format!("第{line}条 本文\n")).collect();
	let fake = Fake::start({
		let page = page.clone();
		move |_, index| match index {
			0 => cut_off(page.clone()),
			_ => reply("以上"),
		}
	})
	.await;
	let tiles = [b"top".to_vec(), b"bottom".to_vec()];
	let read = fake.client().ocr_page(&tiles).await.unwrap();
	assert_eq!(read.text, format!("{page}以上"));
	assert!(read.truncated);
}

#[tokio::test]
async fn ocr_of_a_blank_page_is_empty() {
	let fake = Fake::start(|_, _| reply("[BLANK]")).await;
	assert_eq!(fake.client().ocr(PNG).await.unwrap().text, "");
}

#[tokio::test]
async fn ocr_reports_the_status_and_never_drops_the_image() {
	let fake = Fake::start(|_, _| refusal("image input is not supported")).await;
	match fake.client().ocr(PNG).await {
		Err(Error::Status { status: 400, body }) => assert!(body.contains("not supported")),
		other => panic!("unexpected {other:?}"),
	}
	assert_eq!(fake.requests().len(), 1);
}

#[tokio::test]
async fn message_content_given_as_parts_is_read() {
	let fake = Fake::start(|_, _| {
		let parts =
			json!([{"type": "text", "text": "一行目\n"}, {"type": "text", "text": "二行目"}]);
		(
			200,
			json!({"choices": [{"message": {"role": "assistant", "content": parts}}]}),
		)
	})
	.await;
	assert_eq!(fake.client().ocr(PNG).await.unwrap().text, "一行目\n二行目");
}

#[tokio::test]
async fn ready_follows_the_models_endpoint() {
	let fake = Fake::start(|_, _| reply("")).await;
	assert!(fake.client().ready().await);
	let config = |base_url: &str, api_key: Option<&str>| ModelConfig {
		base_url: base_url.into(),
		model: "m".into(),
		api_key: api_key.map(String::from),
		timeout: Duration::from_secs(2),
	};
	assert!(
		!Client::new(config(&fake.base_url, None))
			.unwrap()
			.ready()
			.await
	);
	assert!(
		!Client::new(config("http://127.0.0.1:9/v1", Some("sekrit")))
			.unwrap()
			.ready()
			.await
	);
}

#[tokio::test]
async fn analyze_sends_schema_thumbnails_and_known_values() {
	let fake = Fake::start(|_, _| {
		let mut receipt = segment(&[1], "さくら薬局 領収書");
		receipt["date_printed"] = json!("令和6年3月14日");
		receipt["issuer"] = json!("さくら 薬局");
		receipt["facts"] = json!([
			{"key": "total", "value": "１，２００円", "amount": 1200, "currency": "JPY"},
			{"key": "tax", "value": "109円", "amount": null, "currency": null},
		]);
		reply(format!(
			"<think>two docs</think>{}",
			analysis(vec![receipt, segment(&[3], "診療明細書")], &[2])
		))
	})
	.await;
	let mut pages = vec![
		page(
			1,
			"さくら薬局\n領収書 令和6年3月14日\n合計 1,200円 消費税 109円",
		),
		page(2, ""),
		page(3, "診療明細書"),
	];
	pages[0].thumbnail_png = Some(PNG.to_vec());
	pages[2].thumbnail_png = Some(b"third".to_vec());
	let result = fake.client().analyze(&input(pages)).await.unwrap();

	assert_eq!(result.blank_pages, [2]);
	assert_eq!(result.segments.len(), 2);
	let receipt = &result.segments[0];
	assert_eq!(receipt.title, "領収書");
	assert_eq!(receipt.issuer.as_deref(), Some("さくら薬局"));
	assert_eq!(receipt.date.as_deref(), Some("2024-03-14"));
	assert_eq!(receipt.facts[0].amount_minor, Some(1200));
	assert_eq!(receipt.facts[1].amount_minor, Some(109));
	assert_eq!(receipt.facts[1].currency.as_deref(), Some("JPY"));
	assert_eq!(receipt.confidence, 0.9);

	let requests = fake.requests();
	assert_eq!(requests.len(), 1);
	let body = &requests[0];
	assert_eq!(body["temperature"], 0);
	let format = &body["response_format"];
	assert_eq!(format["type"], "json_schema");
	assert_eq!(format["json_schema"]["name"], "analysis");
	assert_eq!(format["json_schema"]["strict"], true);
	let schema = &format["json_schema"]["schema"];
	assert_eq!(schema["type"], "object");
	assert!(schema["properties"]["segments"]["items"]["properties"]["facts"].is_object());
	let text = user_text(body);
	for expected in [
		"File: scan.pdf",
		"Known areas: Health",
		"Known issuers: さくら薬局",
		"Known tags: pharmacy",
		"Pages: 1, 2, 3",
		"<page n=\"1\">\nさくら薬局\n領収書 令和6年3月14日\n合計 1,200円 消費税 109円\n</page>",
		"Image of page 1:",
		"Image of page 3:",
	] {
		assert!(text.contains(expected), "{expected} missing from:\n{text}");
	}
	let urls = image_urls(body);
	assert_eq!(urls.len(), 2);
	assert!(
		urls.iter()
			.all(|url| url.starts_with("data:image/png;base64,"))
	);
}

#[tokio::test]
async fn refused_schema_falls_back_to_plain_prompting() {
	let fake = Fake::start(|body, _| {
		if body.get("response_format").is_some() {
			return refusal("response_format json_schema is not supported");
		}
		reply(format!(
			"Here is the result:\n```json\n{}\n```\nLet me know if you need more.",
			analysis(vec![segment(&[1], "領収書")], &[])
		))
	})
	.await;
	let client = fake.client();
	let input = input(vec![page(1, "領収書")]);
	assert_eq!(
		client.analyze(&input).await.unwrap().segments[0].title,
		"領収書"
	);
	assert_eq!(client.analyze(&input).await.unwrap().segments[0].pages, [1]);

	let with_schema: Vec<bool> = fake
		.requests()
		.iter()
		.map(|body| body.get("response_format").is_some())
		.collect();
	assert_eq!(with_schema, [true, false, false]);
}

#[tokio::test]
async fn refused_images_fall_back_to_text_only() {
	let fake = Fake::start(|body, _| {
		if !image_urls(body).is_empty() {
			return refusal("image input is not supported");
		}
		reply(analysis(vec![segment(&[1], "領収書")], &[]))
	})
	.await;
	let client = fake.client();
	let mut input = input(vec![page(1, "領収書")]);
	input.pages[0].thumbnail_png = Some(PNG.to_vec());
	client.analyze(&input).await.unwrap();
	client.analyze(&input).await.unwrap();

	let shapes: Vec<(bool, bool)> = fake
		.requests()
		.iter()
		.map(|body| {
			(
				body.get("response_format").is_some(),
				!image_urls(body).is_empty(),
			)
		})
		.collect();
	assert_eq!(
		shapes,
		[(true, true), (false, true), (true, false), (true, false)]
	);
	assert!(fake.requests()[2]["messages"][1]["content"].is_string());
}

#[tokio::test]
async fn invalid_answer_is_retried_with_the_errors() {
	let fake = Fake::start(|_, index| match index {
		0 => reply(analysis(vec![segment(&[1], "領収書")], &[])),
		_ => reply(analysis(
			vec![segment(&[1], "領収書"), segment(&[2, 3], "診療明細書")],
			&[],
		)),
	})
	.await;
	let pages = vec![
		page(1, "さくら薬局 領収書"),
		page(2, "さくら薬局 診療明細書"),
		page(3, "続き"),
	];
	let result = fake.client().analyze(&input(pages)).await.unwrap();
	assert_eq!(result.segments.len(), 2);
	assert_eq!(result.segments[1].pages, [2, 3]);
	assert_eq!(result.segments[1].confidence, 0.9);

	let requests = fake.requests();
	assert_eq!(requests.len(), 2);
	let messages = requests[1]["messages"].as_array().unwrap();
	assert_eq!(messages.len(), 4);
	assert_eq!(messages[2]["role"], "assistant");
	assert!(
		messages[2]["content"]
			.as_str()
			.unwrap()
			.contains("\"segments\"")
	);
	assert_eq!(messages[3]["role"], "user");
	let correction = messages[3]["content"].as_str().unwrap();
	assert!(
		correction.contains("pages missing from both segments and blank_pages: 2, 3"),
		"{correction}"
	);
}

const LEAFLET: &str = "Venez planter avec nous !\nJournée de reboisement au parc du Mont Vénus\nSamedi 2 mars, Ville de Port-Céleste";

fn leaflet(title: &str) -> String {
	let mut leaflet = segment(&[1], title);
	leaflet["issuer"] = json!("Ville de Port-Céleste");
	leaflet["date_printed"] = Value::Null;
	leaflet["facts"] = json!([]);
	leaflet["tags"] = json!(["travel", "event", "france", "paris"]);
	analysis(vec![leaflet], &[])
}

#[tokio::test]
async fn title_in_another_script_is_asked_about_then_taken_from_the_page() {
	let corrected = Fake::start(|_, index| match index {
		0 => reply(leaflet("イベント告知")),
		_ => reply(leaflet("Journée de reboisement")),
	})
	.await;
	let pages = vec![page(1, LEAFLET)];
	let result = corrected
		.client()
		.analyze(&input(pages.clone()))
		.await
		.unwrap();
	assert_eq!(result.segments[0].title, "Journée de reboisement");
	assert_eq!(result.segments[0].tags, ["travel", "event"]);
	assert_eq!(result.segments[0].confidence, 0.9);
	let correction = corrected.requests()[1]["messages"][3]["content"]
		.as_str()
		.unwrap()
		.to_string();
	assert!(
		correction.contains("title \"イベント告知\" is not in the language of the page text"),
		"{correction}"
	);

	let stubborn = Fake::start(|_, _| reply(leaflet("イベント告知"))).await;
	let result = stubborn.client().analyze(&input(pages)).await.unwrap();
	assert_eq!(stubborn.requests().len(), 2);
	assert_eq!(result.segments[0].title, "Venez planter avec nous");
	assert_eq!(result.segments[0].confidence, 0.45);
}

#[tokio::test]
async fn one_document_is_never_cut_on_page_boundaries() {
	let fake = Fake::start(|body, _| {
		let pages = listed_pages(body);
		reply(analysis(vec![segment(&pages, "契約書")], &[]))
	})
	.await;
	let limits = Limits {
		text_budget_chars: 150,
		page_chars: 100,
		boundary_page_chars: 20,
		max_thumbnails: 4,
	};
	let pages = (1..=3)
		.map(|number| page(number, &format!("契約書 {}", "あ".repeat(300))))
		.collect();
	let mut input = input(pages);
	input.one_document = true;
	let result = fake
		.client()
		.with_limits(limits)
		.analyze(&input)
		.await
		.unwrap();
	assert_eq!(result.segments.len(), 1);
	assert_eq!(result.segments[0].pages, [1, 2, 3]);
	let requests = fake.requests();
	assert_eq!(requests.len(), 1);
	assert!(user_text(&requests[0]).contains("exactly one segment"));
}

#[tokio::test]
async fn answer_still_invalid_after_the_retry_is_repaired() {
	let fake = Fake::start(|_, index| match index {
		0 => reply("I could not read this file."),
		_ => {
			let mut receipt = segment(&[1, 7], "領収書");
			receipt["date_printed"] = json!("2024-02-31");
			receipt["rotations"] = json!([{"page": 3, "degrees": 90}, {"page": 1, "degrees": 180}]);
			reply(analysis(
				vec![receipt, segment(&[4], "診療明細書")],
				&[2, 4],
			))
		}
	})
	.await;
	let pages = (1..=5).map(|number| page(number, "さくら薬局")).collect();
	let result = fake.client().analyze(&input(pages)).await.unwrap();

	assert_eq!(fake.requests().len(), 2);
	let correction = fake.requests()[1]["messages"][3]["content"]
		.as_str()
		.unwrap()
		.to_string();
	assert!(correction.contains("no JSON object"), "{correction}");
	assert!(result.blank_pages.is_empty());
	assert_eq!(result.segments.len(), 2);
	assert_eq!(result.segments[0].pages, [1, 2, 3]);
	assert_eq!(result.segments[0].date, None);
	assert_eq!(result.segments[0].rotations.len(), 1);
	assert_eq!(result.segments[0].rotations[0].page, 1);
	assert_eq!(result.segments[1].pages, [4, 5]);
	assert_eq!(result.segments[0].confidence, 0.45);
	assert_eq!(result.segments[1].confidence, 0.45);
}

#[tokio::test]
async fn issuer_missing_from_the_page_is_asked_about_once() {
	let stated = |issuer: &str| {
		let mut receipt = segment(&[1], "領収書");
		receipt["issuer"] = json!(issuer);
		reply(analysis(vec![receipt], &[]))
	};
	let pages = || vec![page(1, "ひまわり薬局 領収書 2024-03-14 合計 1,200円")];

	let corrected = Fake::start(move |_, index| match index {
		0 => stated("医療機関"),
		_ => stated("ひまわり薬局"),
	})
	.await;
	let result = corrected.client().analyze(&input(pages())).await.unwrap();
	assert_eq!(result.segments[0].issuer.as_deref(), Some("ひまわり薬局"));
	assert_eq!(result.segments[0].confidence, 0.9);
	let requests = corrected.requests();
	assert_eq!(requests.len(), 2);
	let correction = requests[1]["messages"][3]["content"].as_str().unwrap();
	assert!(
		correction.contains("\"医療機関\" is not in the page text"),
		"{correction}"
	);

	let insisted = Fake::start(move |_, _| stated("医療機関")).await;
	let result = insisted.client().analyze(&input(pages())).await.unwrap();
	assert_eq!(result.segments[0].issuer.as_deref(), Some("医療機関"));
	assert!((result.segments[0].confidence - 0.63).abs() < 1e-6);
	assert_eq!(insisted.requests().len(), 2);

	let worse = Fake::start(move |_, index| match index {
		0 => stated("医療機関"),
		_ => reply("I cannot tell."),
	})
	.await;
	let result = worse.client().analyze(&input(pages())).await.unwrap();
	assert_eq!(result.segments[0].issuer.as_deref(), Some("医療機関"));
	assert!((result.segments[0].confidence - 0.63).abs() < 1e-6);

	let borrowed = Fake::start(move |_, _| stated("さくら薬局")).await;
	let result = borrowed.client().analyze(&input(pages())).await.unwrap();
	assert_eq!(result.segments[0].issuer, None);
	assert_eq!(borrowed.requests().len(), 1);
}

#[tokio::test]
async fn two_unusable_answers_are_an_error() {
	let fake = Fake::start(|_, _| reply("<think>thinking forever")).await;
	let result = fake.client().analyze(&input(vec![page(1, "text")])).await;
	assert!(matches!(result, Err(Error::Answer(_))), "{result:?}");
	assert_eq!(fake.requests().len(), 2);
}

const NO_DOCUMENT: &str = r#"{"segments": [], "blank_pages": []}"#;

#[tokio::test]
async fn an_answer_without_documents_is_retried_as_one_document_then_refused() {
	let fake = Fake::start(|_, _| reply(NO_DOCUMENT)).await;
	let pages = vec![page(1, "Statement July 2018 balance 6,551"), page(2, "")];
	let result = fake.client().analyze(&input(pages.clone())).await;
	assert!(
		matches!(&result, Err(Error::Answer(message)) if message.contains("no page in a document")),
		"{result:?}"
	);
	let requests = fake.requests();
	assert_eq!(requests.len(), 2);
	let schema = &requests[0]["response_format"]["json_schema"]["schema"];
	assert_eq!(schema["properties"]["segments"]["minItems"], json!(1));
	assert!(!user_text(&requests[0]).contains("exactly one segment"));
	assert!(user_text(&requests[1]).contains("exactly one segment"));
	let correction = requests[1]["messages"][3]["content"].as_str().unwrap();
	assert!(correction.contains("segments is empty"), "{correction}");

	let fixed = Fake::start(|body, index| match index {
		0 => reply(NO_DOCUMENT),
		_ => reply(analysis(vec![segment(&listed_pages(body), "明細")], &[])),
	})
	.await;
	let result = fixed.client().analyze(&input(pages)).await.unwrap();
	assert_eq!(result.segments.len(), 1);
	assert_eq!(result.segments[0].pages, [1, 2]);
	assert_eq!(result.segments[0].area, "Health");
}

#[tokio::test]
async fn a_photo_is_analysed_from_its_image_as_one_document() {
	let fake = Fake::start(|_, index| match index {
		0 => reply(analysis(Vec::new(), &[1])),
		_ => {
			let mut photo = segment(&[1], "Drain cover in the garden");
			photo["doc_type"] = json!("photo");
			photo["issuer"] = json!(null);
			photo["date_printed"] = json!(null);
			photo["facts"] = json!([]);
			reply(analysis(vec![photo], &[]))
		}
	})
	.await;
	let mut photo = input(vec![page(1, "")]);
	photo.pages[0].thumbnail_png = Some(PNG.to_vec());
	photo.one_document = true;
	photo.photo = true;
	let result = fake.client().analyze(&photo).await.unwrap();
	assert_eq!(result.segments.len(), 1);
	assert_eq!(result.segments[0].doc_type, "photo");
	assert_eq!(result.segments[0].title, "Drain cover in the garden");
	assert!(result.blank_pages.is_empty());
	let requests = fake.requests();
	assert_eq!(requests.len(), 2);
	assert!(user_text(&requests[0]).contains("No text could be read"));
	assert_eq!(image_urls(&requests[0]).len(), 1);
}

#[tokio::test]
async fn a_part_of_a_long_file_without_documents_joins_its_neighbours() {
	let fake = Fake::start(|body, _| {
		let system = body["messages"][0]["content"].as_str().unwrap();
		if system.contains("new_document") {
			let text = user_text(body);
			let new_document =
				text.contains("Does page 2 start") || text.contains("Does page 6 start");
			return reply(format!("{{\"new_document\": {new_document}}}"));
		}
		match listed_pages(body).as_slice() {
			[2, 3, 4, 5] => reply(analysis(vec![segment(&[2, 3, 5], "契約書")], &[4])),
			_ => reply(NO_DOCUMENT),
		}
	})
	.await;
	let limits = Limits {
		text_budget_chars: 150,
		page_chars: 100,
		boundary_page_chars: 20,
		max_thumbnails: 4,
	};
	let long = |label: &str| format!("{label} {}", "あ".repeat(300));
	let pages = vec![
		page(1, &long("規約")),
		page(2, &long("契約書1")),
		page(3, &long("契約書2")),
		page(4, ""),
		page(5, &long("契約書3")),
		page(6, &long("別紙")),
	];
	let result = fake
		.client()
		.with_limits(limits.clone())
		.analyze(&input(pages))
		.await
		.unwrap();
	assert_eq!(result.segments.len(), 1);
	assert_eq!(result.segments[0].title, "契約書");
	assert_eq!(result.segments[0].pages, [1, 2, 3, 5, 6]);
	assert_eq!(result.blank_pages, [4]);
	assert!((result.segments[0].confidence - 0.9 * 0.7 * 0.25).abs() < 1e-6);

	let none = Fake::start(|body, _| {
		let system = body["messages"][0]["content"].as_str().unwrap();
		if system.contains("new_document") {
			return reply(r#"{"new_document": true}"#);
		}
		reply(NO_DOCUMENT)
	})
	.await;
	let pages = (1..=3).map(|n| page(n, &long("頁"))).collect();
	let result = none
		.client()
		.with_limits(limits)
		.analyze(&input(pages))
		.await;
	assert!(matches!(result, Err(Error::Answer(_))), "{result:?}");
}

#[tokio::test]
async fn empty_file_needs_no_call() {
	let fake = Fake::start(|_, _| reply("{}")).await;
	let result = fake.client().analyze(&input(Vec::new())).await.unwrap();
	assert!(result.segments.is_empty() && result.blank_pages.is_empty());
	assert!(fake.requests().is_empty());
}

#[tokio::test]
async fn long_file_is_split_on_page_boundaries_then_analysed_per_document() {
	let fake = Fake::start(|body, _| {
		let system = body["messages"][0]["content"].as_str().unwrap();
		if system.contains("new_document") {
			let text = user_text(body);
			let new_document =
				text.contains("Does page 2 start") || text.contains("Does page 6 start");
			return reply(format!(
				"```json\n{{\"new_document\": {new_document}}}\n```"
			));
		}
		let pages = listed_pages(body);
		match pages.as_slice() {
			[1] => reply(analysis(vec![segment(&[1], "領収書")], &[])),
			[2, 3, 4, 5] => reply(analysis(vec![segment(&[2, 3, 5], "契約書")], &[4])),
			[6] => reply(analysis(vec![segment(&[6], "給与明細")], &[])),
			other => panic!("unexpected chunk {other:?}"),
		}
	})
	.await;
	let limits = Limits {
		text_budget_chars: 150,
		page_chars: 100,
		boundary_page_chars: 20,
		max_thumbnails: 4,
	};
	let client = fake.client().with_limits(limits);
	let long = |label: &str| format!("{label} 開始 {} 終了 {label}", "あ".repeat(300));
	let mut pages = vec![
		page(1, &long("領収書")),
		page(2, &long("契約書1")),
		page(3, &long("契約書2")),
		page(4, "  "),
		page(5, &long("契約書3")),
		page(6, &long("給与明細")),
	];
	for page in &mut pages {
		page.thumbnail_png = Some(PNG.to_vec());
	}
	let result = client.analyze(&input(pages)).await.unwrap();

	let titles: Vec<(&str, &[u32])> = result
		.segments
		.iter()
		.map(|s| (s.title.as_str(), s.pages.as_slice()))
		.collect();
	assert_eq!(
		titles,
		[
			("領収書", &[1][..]),
			("契約書", &[2, 3, 5][..]),
			("給与明細", &[6][..])
		]
	);
	assert_eq!(result.blank_pages, [4]);

	let requests = fake.requests();
	let (boundaries, chunks): (Vec<&Value>, Vec<&Value>) = requests.iter().partition(|body| {
		body["messages"][0]["content"]
			.as_str()
			.unwrap()
			.contains("new_document")
	});
	let questions: Vec<String> = boundaries
		.iter()
		.map(|body| user_text(body).lines().last().unwrap().to_string())
		.collect();
	assert_eq!(
		questions,
		[
			"Does page 2 start a new document?",
			"Does page 3 start a new document?",
			"Does page 5 start a new document?",
			"Does page 6 start a new document?",
		]
	);
	let first = boundaries[0];
	assert_eq!(first["response_format"]["json_schema"]["name"], "boundary");
	assert!(image_urls(first).is_empty());
	let comparison = user_text(first);
	assert!(
		comparison
			.contains("<page n=\"1\">\n領収書 開始 あああああ\n[...]\nあ 終了 領収書\n</page>"),
		"{comparison}"
	);
	assert!(
		comparison.contains("<page n=\"2\">\n契約書1 開始 ああああああああああああ\n</page>"),
		"{comparison}"
	);

	assert_eq!(chunks.len(), 3);
	let contract = user_text(chunks[1]);
	assert!(contract.contains("These pages were grouped as probably one document."));
	assert!(contract.contains("[...]"));
	assert!(
		contract.chars().count() < 1200,
		"{}",
		contract.chars().count()
	);
	for chunk in &chunks {
		assert_eq!(image_urls(chunk).len(), 1);
	}
	assert!(contract.contains("Image of page 2:"));
}
