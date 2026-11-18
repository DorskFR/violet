use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use violet::http::router;
use violet_core::hash;

use crate::common::{Harness, splitting_fake, text_pdf, three_pages};

struct Api {
	harness: Harness,
	router: Router,
}

struct Answer {
	status: StatusCode,
	headers: axum::http::HeaderMap,
	body: Vec<u8>,
}

impl Answer {
	fn json(&self) -> Value {
		serde_json::from_slice(&self.body)
			.unwrap_or_else(|_| panic!("not JSON: {}", String::from_utf8_lossy(&self.body)))
	}

	fn header(&self, name: header::HeaderName) -> &str {
		self.headers.get(name).unwrap().to_str().unwrap()
	}
}

impl Api {
	/// A library holding the pharmacy receipt and the electricity bill.
	async fn new() -> Self {
		let fake = splitting_fake().await;
		let harness = Harness::new(fake.models());
		harness.ingest("three.pdf", &three_pages()).await;
		let router = router(harness.app.clone());
		Self { harness, router }
	}

	async fn send(&self, request: Request<Body>) -> Answer {
		let response = self.router.clone().oneshot(request).await.unwrap();
		Answer {
			status: response.status(),
			headers: response.headers().clone(),
			body: response
				.into_body()
				.collect()
				.await
				.unwrap()
				.to_bytes()
				.to_vec(),
		}
	}

	async fn get(&self, uri: &str) -> Answer {
		self.send(Request::get(uri).body(Body::empty()).unwrap())
			.await
	}

	async fn call(&self, method: &str, uri: &str, body: Value) -> Answer {
		let request = Request::builder()
			.method(method)
			.uri(uri)
			.header(header::CONTENT_TYPE, "application/json")
			.body(Body::from(body.to_string()))
			.unwrap();
		self.send(request).await
	}

	async fn ok(&self, uri: &str) -> Value {
		let answer = self.get(uri).await;
		assert_eq!(answer.status, StatusCode::OK, "{uri}: {:?}", answer.json());
		answer.json()
	}

	/// The id of the document whose title is `title`.
	async fn id(&self, title: &str) -> String {
		let found = self.ok("/api/search").await;
		found["hits"]
			.as_array()
			.unwrap()
			.iter()
			.find(|hit| hit["title"] == title)
			.unwrap_or_else(|| panic!("no {title} in {found}"))["id"]
			.as_str()
			.unwrap()
			.to_string()
	}

	async fn upload(&self, parts: &[(&str, &str, &[u8])]) -> Answer {
		self.send(upload_request(parts)).await
	}
}

fn upload_request(parts: &[(&str, &str, &[u8])]) -> Request<Body> {
	let boundary = "XBOUNDARYX";
	let mut body = Vec::new();
	for (field, file_name, bytes) in parts {
		body.extend_from_slice(
			format!(
				"--{boundary}\r\nContent-Disposition: form-data; name=\"{field}\"; filename=\"{file_name}\"\r\nContent-Type: application/octet-stream\r\n\r\n"
			)
			.as_bytes(),
		);
		body.extend_from_slice(bytes);
		body.extend_from_slice(b"\r\n");
	}
	body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
	Request::post("/api/upload")
		.header(
			header::CONTENT_TYPE,
			format!("multipart/form-data; boundary={boundary}"),
		)
		.body(Body::from(body))
		.unwrap()
}

fn error(answer: &Answer, status: StatusCode) -> String {
	assert_eq!(
		answer.status,
		status,
		"{}",
		String::from_utf8_lossy(&answer.body)
	);
	answer.json()["error"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn health_status_and_unknown_endpoints() {
	let api = Api::new().await;
	assert_eq!(api.get("/healthz").await.status, StatusCode::OK);
	assert_eq!(api.get("/readyz").await.status, StatusCode::OK);

	api.harness.drop_file("waiting.pdf", b"%PDF-");
	let status = api.ok("/api/status").await;
	assert_eq!(status["version"], env!("CARGO_PKG_VERSION"));
	assert_eq!(status["inbox"], 1);
	assert_eq!(
		status["jobs"],
		json!({"pending": 0, "running": 0, "done": 0, "failed": 0})
	);
	assert_eq!(status["failed"], json!([]));
	assert_eq!(status["model"], json!({"vision": true, "text": true}));

	error(&api.get("/api/nope").await, StatusCode::NOT_FOUND);
	error(&api.get("/api").await, StatusCode::NOT_FOUND);
	let ui = api.get("/some/client/route").await;
	assert!(ui.status == StatusCode::OK || ui.status == StatusCode::NOT_FOUND);
	if ui.status == StatusCode::OK {
		assert!(ui.header(header::CONTENT_TYPE).starts_with("text/html"));
		assert_eq!(ui.header(header::CACHE_CONTROL), "no-cache");
	}
	let post = Request::post("/some/client/route")
		.body(Body::empty())
		.unwrap();
	assert_eq!(api.send(post).await.status, StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn search_and_facets() {
	let api = Api::new().await;
	let all = api.ok("/api/search").await;
	assert_eq!(all["total"], 2);
	let hit = &api.ok("/api/search?q=electricity").await["hits"][0];
	assert_eq!(hit["title"], "Electricity Bill");
	assert_eq!(
		hit["path"],
		"Housing/2023/2023-11-02 Tokyo Power Electricity Bill.pdf"
	);
	assert!(hit["snippet"].as_str().unwrap().contains("<mark>"));
	assert_eq!(
		(&hit["amount_minor"], &hit["currency"]),
		(&json!(1200), &json!("JPY"))
	);

	for (query, total) in [
		("area=Health", 1),
		("type=receipt&issuer=Tokyo%20Power", 1),
		("tag=payment&tag=missing", 0),
		("tag=payment", 2),
		("from=2024-01-01", 1),
		("to=2023%E5%B9%B412%E6%9C%8831%E6%97%A5", 1),
		("min=1000&max=1500&currency=jpy", 2),
		("min=1201", 0),
		("limit=1&offset=1", 2),
		("q=&area=", 2),
		("unknown=1", 2),
	] {
		let found = api.ok(&format!("/api/search?{query}")).await;
		assert_eq!(found["total"], total, "{query}");
	}
	let page = api.ok("/api/search?limit=1&offset=1").await;
	assert_eq!(page["hits"].as_array().unwrap().len(), 1);
	error(
		&api.get("/api/search?min=lots").await,
		StatusCode::BAD_REQUEST,
	);
	error(
		&api.get("/api/search?from=someday").await,
		StatusCode::BAD_REQUEST,
	);

	let facets = api.ok("/api/facets").await;
	assert_eq!(
		facets["areas"],
		json!([{"value": "Health", "count": 1}, {"value": "Housing", "count": 1}])
	);
	assert_eq!(
		facets["doc_types"],
		json!([{"value": "receipt", "count": 2}])
	);
	assert_eq!(facets["tags"], json!([{"value": "payment", "count": 2}]));
	assert_eq!(
		facets["years"],
		json!([{"value": "2024", "count": 1}, {"value": "2023", "count": 1}])
	);
	assert_eq!(facets["currencies"], json!([{"value": "JPY", "count": 2}]));
	assert_eq!(facets["issuers"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn search_reads_dates_amounts_and_falls_back_to_partial_matches() {
	let api = Api::new().await;
	let found = api.ok("/api/search?q=receipt%202024").await;
	assert_eq!(
		(&found["total"], &found["partial"]),
		(&json!(1), &json!(false))
	);
	assert_eq!(found["hits"][0]["title"], "Receipt");
	assert_eq!(
		found["understood"],
		json!({
			"from": "2024-01-01",
			"to": "2024-12-31",
			"terms": [{"text": "2024", "kind": "date"}],
		})
	);

	let explicit = api.ok("/api/search?q=receipt%202024&from=2023-01-01").await;
	assert_eq!(explicit["total"], 2);
	assert_eq!(explicit["understood"], json!({"terms": []}));

	let amount = api.ok("/api/search?q=1200%E5%86%86").await;
	assert_eq!(amount["total"], 2);
	assert_eq!(
		(
			&amount["understood"]["min"],
			&amount["understood"]["currency"]
		),
		(&json!(1200), &json!("JPY"))
	);

	let partial = api.ok("/api/search?q=electricity%20zzznothing").await;
	assert_eq!(
		(&partial["total"], &partial["partial"]),
		(&json!(1), &json!(true))
	);
	assert_eq!(partial["hits"][0]["title"], "Electricity Bill");

	let latest = api.ok("/api/search?q=dernier%20receipt").await;
	assert_eq!(latest["understood"]["latest"], true);
	assert_eq!(latest["hits"][0]["title"], "Receipt");
	assert_eq!(latest["hits"][1]["title"], "Electricity Bill");
}

#[tokio::test]
async fn document_file_and_page_image() {
	let api = Api::new().await;
	let id = api.id("Receipt").await;
	let document = api.ok(&format!("/api/documents/{id}")).await;
	assert_eq!(
		document["path"],
		"Health/2024/2024-03-14 Sakura Pharmacy Receipt.pdf"
	);
	assert_eq!(document["sidecar"]["id"], id.as_str());
	let sha = document["sidecar"]["sha256"].as_str().unwrap();

	let file = api.get(&format!("/api/documents/{id}/file")).await;
	assert_eq!(file.status, StatusCode::OK);
	assert_eq!(file.body.len() as u64, document["size"].as_u64().unwrap());
	assert_eq!(hash::sha256(&file.body), sha);
	assert_eq!(file.header(header::CONTENT_TYPE), "application/pdf");
	assert_eq!(file.header(header::X_CONTENT_TYPE_OPTIONS), "nosniff");
	assert_eq!(
		file.header(header::CONTENT_DISPOSITION),
		"inline; filename=\"2024-03-14 Sakura Pharmacy Receipt.pdf\"; filename*=UTF-8''2024-03-14%20Sakura%20Pharmacy%20Receipt.pdf"
	);

	let uri = format!("/api/documents/{id}/pages/1");
	let png = api.get(&format!("{uri}?w=300&v={}", &sha[..12])).await;
	assert_eq!(png.status, StatusCode::OK);
	assert_eq!(png.header(header::CONTENT_TYPE), "image/png");
	assert!(png.header(header::CACHE_CONTROL).contains("immutable"));
	assert!(png.body.starts_with(b"\x89PNG\r\n\x1a\n"));
	let cached = api.get(&format!("{uri}?w=300")).await;
	assert_eq!(cached.body, png.body);
	assert_eq!(cached.header(header::CACHE_CONTROL), "private, no-cache");
	assert!(api.harness.app.pages.bytes() >= png.body.len());
	let larger = api.get(&uri).await;
	assert!(larger.body.len() > png.body.len());

	let tag = png.header(header::ETAG).to_string();
	let revalidate = Request::get(format!("{uri}?w=300"))
		.header(header::IF_NONE_MATCH, &tag)
		.body(Body::empty())
		.unwrap();
	let unchanged = api.send(revalidate).await;
	assert_eq!(unchanged.status, StatusCode::NOT_MODIFIED);
	assert!(unchanged.body.is_empty());

	error(
		&api.get(&format!("{uri}?w=0")).await,
		StatusCode::BAD_REQUEST,
	);
	error(
		&api.get(&format!("{uri}?w=99999")).await,
		StatusCode::BAD_REQUEST,
	);
	error(
		&api.get(&format!("{uri}?w=big")).await,
		StatusCode::BAD_REQUEST,
	);
	error(
		&api.get(&format!("/api/documents/{id}/pages/2")).await,
		StatusCode::NOT_FOUND,
	);
	error(
		&api.get(&format!("/api/documents/{id}/pages/0")).await,
		StatusCode::NOT_FOUND,
	);
	error(
		&api.get(&format!("/api/documents/{id}/pages/x")).await,
		StatusCode::BAD_REQUEST,
	);
	let missing = uuid::Uuid::new_v4();
	for path in ["", "/file", "/pages/1"] {
		error(
			&api.get(&format!("/api/documents/{missing}{path}")).await,
			StatusCode::NOT_FOUND,
		);
	}
}

#[tokio::test]
async fn files_that_a_browser_could_run_are_downloads() {
	let api = Api::new().await;
	let note = api
		.harness
		.one("note.txt", "こんにちは 2022-05-06".as_bytes())
		.await;
	let id = api
		.harness
		.one("page.html", b"<script>alert(1)</script>")
		.await;
	let file = api.get(&format!("/api/documents/{id}/file")).await;
	assert_eq!(
		file.header(header::CONTENT_TYPE),
		"application/octet-stream"
	);
	assert!(
		file.header(header::CONTENT_DISPOSITION)
			.starts_with("attachment;")
	);
	error(
		&api.get(&format!("/api/documents/{id}/pages/1")).await,
		StatusCode::NOT_FOUND,
	);

	let id = note;
	let renamed = api
		.call(
			"PATCH",
			&format!("/api/documents/{id}"),
			json!({"title": "メモ"}),
		)
		.await;
	assert_eq!(renamed.json()["path"], "Work/2022/2022-05-06 Acme メモ.txt");
	let file = api.get(&format!("/api/documents/{id}/file")).await;
	assert_eq!(
		file.header(header::CONTENT_TYPE),
		"text/plain; charset=utf-8"
	);
	let disposition = file.header(header::CONTENT_DISPOSITION);
	assert!(
		disposition.starts_with("inline; filename=\""),
		"{disposition}"
	);
	assert!(disposition.is_ascii());
	assert!(disposition.ends_with("filename*=UTF-8''2022-05-06%20Acme%20%E3%83%A1%E3%83%A2.txt"));
}

#[tokio::test]
async fn patch_then_undo_through_the_activity_feed() {
	let api = Api::new().await;
	let id = api.id("Receipt").await;
	let uri = format!("/api/documents/{id}");
	let patched = api
		.call(
			"PATCH",
			&uri,
			json!({"title": "Cold medicine", "issuer": null, "date": "2024/04/01", "tags": ["medicine"]}),
		)
		.await;
	assert_eq!(patched.status, StatusCode::OK, "{:?}", patched.json());
	let patched = patched.json();
	assert_eq!(patched["path"], "Health/2024/2024-04-01 Cold medicine.pdf");
	assert_eq!(patched["sidecar"]["issuer"], Value::Null);
	assert_eq!(patched["sidecar"]["doc_type"], "receipt");
	assert!(patched["size"].as_u64().unwrap() > 0);
	let entry = patched["entry"].as_str().unwrap().to_string();
	assert_eq!(api.ok(&uri).await["path"], patched["path"]);

	let kept = api
		.call("PATCH", &uri, json!({"tags": ["medicine"]}))
		.await
		.json();
	assert_eq!(kept["entry"], Value::Null);
	assert_eq!(kept["sidecar"]["date"], "2024-04-01");

	error(
		&api.call("PATCH", &uri, json!({"date": "never"})).await,
		StatusCode::BAD_REQUEST,
	);
	error(
		&api.call("PATCH", &uri, json!({"tags": "one"})).await,
		StatusCode::BAD_REQUEST,
	);
	let not_json = Request::patch(&uri)
		.header(header::CONTENT_TYPE, "application/json")
		.body(Body::from("{"))
		.unwrap();
	error(&api.send(not_json).await, StatusCode::BAD_REQUEST);

	let activity = api.ok("/api/activity").await;
	let items = activity.as_array().unwrap();
	assert_eq!(items.len(), 2);
	assert_eq!(items[0]["entry"]["id"], entry.as_str());
	assert_eq!(items[0]["entry"]["action"]["kind"], "update");
	assert_eq!(
		(&items[0]["undone"], &items[0]["undoable"]),
		(&json!(false), &json!(true))
	);
	assert_eq!(items[1]["entry"]["action"]["kind"], "ingest");
	assert_eq!(items[1]["undoable"], false);
	assert_eq!(
		api.ok("/api/activity?limit=1")
			.await
			.as_array()
			.unwrap()
			.len(),
		1
	);
	error(
		&api.get("/api/activity?limit=x").await,
		StatusCode::BAD_REQUEST,
	);

	let undo_uri = format!("/api/activity/{entry}/undo");
	let undone = api.call("POST", &undo_uri, Value::Null).await;
	assert_eq!(undone.status, StatusCode::OK, "{:?}", undone.json());
	assert_eq!(
		undone.json()["entry"]["action"],
		json!({"kind": "undo", "entry": entry})
	);
	assert_eq!(
		api.ok(&uri).await["path"],
		"Health/2024/2024-03-14 Sakura Pharmacy Receipt.pdf"
	);
	assert_eq!(
		error(
			&api.call("POST", &undo_uri, Value::Null).await,
			StatusCode::CONFLICT
		),
		"already undone"
	);
	let first = api.ok("/api/activity").await;
	assert_eq!(first[1]["undone"], true);
	let unknown = format!("/api/activity/{}/undo", uuid::Uuid::new_v4());
	error(
		&api.call("POST", &unknown, Value::Null).await,
		StatusCode::NOT_FOUND,
	);
}

#[tokio::test]
async fn page_edits_split_merge_and_delete() {
	let api = Api::new().await;
	let pdf = text_pdf(&["page one of the memo", "page two of the memo", "page three"]);
	let memo = api.harness.one("memo.pdf", &pdf).await;
	let uri = format!("/api/documents/{memo}");
	let before = api.ok(&uri).await["sidecar"]["sha256"].clone();

	let rotated = api
		.call(
			"POST",
			&format!("{uri}/rotate"),
			json!({"pages": [1], "degrees": 90}),
		)
		.await
		.json();
	assert_ne!(rotated["sidecar"]["sha256"], before);
	assert!(rotated["entry"].is_string());
	error(
		&api.call(
			"POST",
			&format!("{uri}/rotate"),
			json!({"pages": [1], "degrees": 30}),
		)
		.await,
		StatusCode::BAD_REQUEST,
	);

	let shorter = api
		.call(
			"POST",
			&format!("{uri}/delete-pages"),
			json!({"pages": [3]}),
		)
		.await
		.json();
	assert_eq!(shorter["sidecar"]["pages"].as_array().unwrap().len(), 2);
	assert_ne!(shorter["sidecar"]["sha256"], rotated["sidecar"]["sha256"]);
	assert!(shorter["entry"].is_string());
	error(
		&api.call(
			"POST",
			&format!("{uri}/delete-pages"),
			json!({"pages": [7]}),
		)
		.await,
		StatusCode::BAD_REQUEST,
	);

	let split = api
		.call(
			"POST",
			&format!("{uri}/split"),
			json!({"groups": [[1], [2]]}),
		)
		.await;
	assert_eq!(split.status, StatusCode::OK, "{:?}", split.json());
	let split = split.json();
	let parts: Vec<String> = split["documents"]
		.as_array()
		.unwrap()
		.iter()
		.map(|id| id.as_str().unwrap().to_string())
		.collect();
	assert_eq!(parts.len(), 2);
	assert!(split["entry"].is_string());
	error(&api.get(&uri).await, StatusCode::NOT_FOUND);
	error(
		&api.call("POST", &format!("{uri}/split"), json!({"groups": [[1]]}))
			.await,
		StatusCode::NOT_FOUND,
	);

	let merged = api.call("POST", "/api/merge", json!({"ids": parts})).await;
	assert_eq!(merged.status, StatusCode::OK, "{:?}", merged.json());
	let merged = merged.json();
	assert_eq!(merged["sidecar"]["pages"].as_array().unwrap().len(), 2);
	assert!(merged["entry"].is_string());
	error(
		&api.call("POST", "/api/merge", json!({"ids": ["../x", "y"]}))
			.await,
		StatusCode::BAD_REQUEST,
	);
	error(
		&api.call("POST", "/api/merge", json!({"ids": [parts[0]]}))
			.await,
		StatusCode::BAD_REQUEST,
	);

	let merged_uri = format!(
		"/api/documents/{}",
		merged["sidecar"]["id"].as_str().unwrap()
	);
	let deleted = api.call("DELETE", &merged_uri, Value::Null).await;
	assert_eq!(deleted.status, StatusCode::OK);
	let entry = deleted.json()["entry"].as_str().unwrap().to_string();
	error(&api.get(&merged_uri).await, StatusCode::NOT_FOUND);
	error(
		&api.call("DELETE", &merged_uri, Value::Null).await,
		StatusCode::NOT_FOUND,
	);
	assert_eq!(api.ok("/api/search").await["total"], 2);

	let undone = api
		.call("POST", &format!("/api/activity/{entry}/undo"), Value::Null)
		.await;
	assert_eq!(undone.status, StatusCode::OK);
	assert_eq!(
		api.ok(&merged_uri).await["sidecar"]["sha256"],
		merged["sidecar"]["sha256"]
	);
}

#[tokio::test]
async fn uploads_land_in_the_inbox_under_safe_names() {
	let api = Api::new().await;
	let pdf = text_pdf(&["uploaded words"]);
	let answer = api
		.upload(&[
			("file", "../../evil.pdf", &pdf),
			("file", "C:\\Users\\me\\report.txt", b"report"),
			("file", "report.txt", b"second report"),
			("file", "..", b"dots"),
			("other", "ignored.txt", b"ignored"),
		])
		.await;
	assert_eq!(answer.status, StatusCode::OK, "{:?}", answer.json());
	assert_eq!(answer.json(), json!({"queued": 4}));
	assert_eq!(
		api.harness.inbox(),
		["evil.pdf", "report (2).txt", "report.txt", "upload"]
	);
	assert_eq!(
		std::fs::read(api.harness.app.library.inbox().join("evil.pdf")).unwrap(),
		pdf
	);
	assert!(!api.harness.root().join("evil.pdf").exists());
	assert!(!api.harness.dir.path().join("evil.pdf").exists());
	assert_eq!(api.ok("/api/status").await["inbox"], 4);

	error(
		&api.upload(&[("other", "x.txt", b"x")]).await,
		StatusCode::BAD_REQUEST,
	);
	let not_multipart = api.call("POST", "/api/upload", json!({})).await;
	error(&not_multipart, StatusCode::UNSUPPORTED_MEDIA_TYPE);
}

#[tokio::test]
async fn concurrent_same_name_uploads_keep_every_file() {
	let api = Api::new().await;
	let bodies: Vec<Vec<u8>> = (0..6).map(|n| format!("scan {n}").into_bytes()).collect();
	let uploads: Vec<_> = bodies
		.iter()
		.map(|bytes| {
			let request = upload_request(&[("file", "scan.pdf", bytes.as_slice())]);
			tokio::spawn(api.router.clone().oneshot(request))
		})
		.collect();
	for upload in uploads {
		assert_eq!(upload.await.unwrap().unwrap().status(), StatusCode::OK);
	}
	let inbox = api.harness.inbox();
	assert_eq!(inbox.len(), 6, "{inbox:?}");
	let mut kept: Vec<Vec<u8>> = inbox
		.iter()
		.map(|name| std::fs::read(api.harness.app.library.inbox().join(name)).unwrap())
		.collect();
	kept.sort();
	assert_eq!(kept, bodies);
}

fn with_headers(method: &str, uri: &str, headers: &[(&str, &str)], body: &str) -> Request<Body> {
	let mut request = Request::builder().method(method).uri(uri);
	for (name, value) in headers {
		request = request.header(*name, *value);
	}
	request.body(Body::from(body.to_string())).unwrap()
}

#[tokio::test]
async fn cross_site_changes_are_refused() {
	let api = Api::new().await;
	let id = api.harness.documents()[0].1.id.to_string();
	let uri = format!("/api/documents/{id}");
	let patch = json!({"title": "Renamed"}).to_string();
	let json_type = ("content-type", "application/json");
	for headers in [
		vec![json_type, ("sec-fetch-site", "cross-site")],
		vec![
			json_type,
			("host", "violet.example"),
			("origin", "https://evil.example"),
		],
		vec![json_type, ("host", "violet.example"), ("origin", "null")],
		vec![
			json_type,
			("host", "violet.example"),
			("origin", "https://violet.example.evil.example"),
		],
	] {
		let answer = api
			.send(with_headers("PATCH", &uri, &headers, &patch))
			.await;
		error(&answer, StatusCode::FORBIDDEN);
	}
	let undo = api
		.send(with_headers(
			"POST",
			"/api/activity/00000000-0000-0000-0000-000000000000/undo",
			&[("sec-fetch-site", "cross-site")],
			"",
		))
		.await;
	error(&undo, StatusCode::FORBIDDEN);
	let upload = api
		.send(with_headers(
			"POST",
			"/api/upload",
			&[
				("content-type", "multipart/form-data; boundary=X"),
				("origin", "https://evil.example"),
				("host", "violet.example"),
			],
			"--X--\r\n",
		))
		.await;
	error(&upload, StatusCode::FORBIDDEN);
	assert_eq!(api.harness.entries().len(), 1);

	let read = api
		.send(with_headers(
			"GET",
			&uri,
			&[("sec-fetch-site", "cross-site")],
			"",
		))
		.await;
	assert_eq!(read.status, StatusCode::OK);

	for headers in [
		vec![
			json_type,
			("host", "violet.example"),
			("origin", "https://violet.example"),
		],
		vec![
			json_type,
			("host", "violet.example:443"),
			("origin", "https://VIOLET.example"),
			("sec-fetch-site", "same-origin"),
		],
		vec![
			json_type,
			("host", "violet:8080"),
			("x-forwarded-host", "violet.example"),
			("origin", "https://violet.example"),
		],
		vec![json_type],
	] {
		let answer = api
			.send(with_headers("PATCH", &uri, &headers, &patch))
			.await;
		assert_eq!(
			answer.status,
			StatusCode::OK,
			"{headers:?}: {:?}",
			answer.json()
		);
	}
	let http2 = with_headers(
		"PATCH",
		&format!("https://violet.example{uri}"),
		&[json_type, ("origin", "https://violet.example")],
		&patch,
	);
	assert_eq!(api.send(http2).await.status, StatusCode::OK);
}

#[tokio::test]
async fn json_endpoints_need_a_json_content_type() {
	let api = Api::new().await;
	let id = api.harness.documents()[0].1.id.to_string();
	let uri = format!("/api/documents/{id}");
	for (method, path, body) in [
		("PATCH", uri.clone(), json!({"title": "x"})),
		("POST", format!("{uri}/split"), json!({"groups": [[1]]})),
		(
			"POST",
			format!("{uri}/rotate"),
			json!({"pages": [1], "degrees": 90}),
		),
		("POST", format!("{uri}/delete-pages"), json!({"pages": [1]})),
		("POST", "/api/merge".to_string(), json!({"ids": [id, id]})),
	] {
		for content_type in [
			None,
			Some("text/plain"),
			Some("application/x-www-form-urlencoded"),
		] {
			let headers: Vec<(&str, &str)> = content_type
				.map(|value| vec![("content-type", value)])
				.unwrap_or_default();
			let answer = api
				.send(with_headers(method, &path, &headers, &body.to_string()))
				.await;
			error(&answer, StatusCode::UNSUPPORTED_MEDIA_TYPE);
		}
	}
	let replace = api
		.send(with_headers(
			"POST",
			&format!("{uri}/replace"),
			&[json_content()],
			"{}",
		))
		.await;
	error(&replace, StatusCode::UNSUPPORTED_MEDIA_TYPE);
	let charset = api
		.send(with_headers(
			"PATCH",
			&uri,
			&[("content-type", "Application/JSON; charset=utf-8")],
			&json!({"title": "Kept"}).to_string(),
		))
		.await;
	assert_eq!(charset.status, StatusCode::OK);
	assert_eq!(api.harness.entries().len(), 2);
}

fn json_content() -> (&'static str, &'static str) {
	("content-type", "application/json")
}

#[tokio::test]
async fn split_groups_must_cover_every_page_once() {
	let api = Api::new().await;
	let pdf = text_pdf(&["first page words", "second page words", "third page words"]);
	let id = api.harness.one("pages.pdf", &pdf).await;
	let uri = format!("/api/documents/{id}/split");
	for (groups, message) in [
		(json!([[1], [2]]), "pages missing from the groups: 3"),
		(json!([[1, 2], [2, 3]]), "page 2 is in more than one group"),
		(json!([[1, 2, 3, 4]]), "page 4 is not in the document"),
		(json!([[0], [1, 2, 3]]), "page 0 is not in the document"),
		(json!([[1, 2, 3], []]), "a split group is empty"),
	] {
		let answer = api.call("POST", &uri, json!({ "groups": groups })).await;
		let error = error(&answer, StatusCode::BAD_REQUEST);
		assert!(error.contains(message), "{groups}: {error}");
	}
	let entries = api.harness.entries().len();
	let split = api
		.call("POST", &uri, json!({"groups": [[3, 1], [2]]}))
		.await;
	assert_eq!(split.status, StatusCode::OK, "{:?}", split.json());
	assert_eq!(api.harness.entries().len(), entries + 1);
}

#[tokio::test]
async fn ids_that_are_paths_are_rejected() {
	let api = Api::new().await;
	std::fs::write(api.harness.root().join("secret.txt"), b"secret").unwrap();
	for id in [
		"..%2F..%2Fsecret.txt",
		"%2Fetc%2Fpasswd",
		"..",
		"Health%2F2024",
		"not-a-uuid",
	] {
		for suffix in ["", "/file", "/pages/1"] {
			let answer = api.get(&format!("/api/documents/{id}{suffix}")).await;
			assert!(
				answer.status == StatusCode::BAD_REQUEST || answer.status == StatusCode::NOT_FOUND,
				"{id}{suffix}: {}",
				answer.status
			);
			assert!(!answer.body.windows(6).any(|window| window == b"secret"));
		}
		let uri = format!("/api/documents/{id}");
		for (method, path, body) in [
			("PATCH", "", json!({"title": "x"})),
			("DELETE", "", Value::Null),
			("POST", "/split", json!({"groups": [[1]]})),
			("POST", "/rotate", json!({"pages": [1], "degrees": 90})),
			("POST", "/delete-pages", json!({"pages": [1]})),
		] {
			let answer = api.call(method, &format!("{uri}{path}"), body).await;
			assert!(answer.status.is_client_error(), "{method} {id}{path}");
		}
		let undo = api
			.call("POST", &format!("/api/activity/{id}/undo"), Value::Null)
			.await;
		assert!(undo.status.is_client_error());
	}
	let traversal = api.get("/api/documents/../../secret.txt").await;
	assert!(!traversal.body.windows(6).any(|window| window == b"secret"));
	let ui = api.get("/../secret.txt").await;
	assert!(!ui.body.windows(6).any(|window| window == b"secret"));
	assert_eq!(api.harness.entries().len(), 1);
	assert_eq!(api.ok("/api/search").await["total"], 2);
}

#[tokio::test]
async fn a_patched_month_is_a_period() {
	let api = Api::new().await;
	let id = api.id("Receipt").await;
	let uri = format!("/api/documents/{id}");
	let month = api.call("PATCH", &uri, json!({"date": "mars 2014"})).await;
	assert_eq!(month.status, StatusCode::OK, "{:?}", month.json());
	let sidecar = &month.json()["sidecar"];
	assert_eq!(sidecar["date"], "2014-03-01");
	assert_eq!(sidecar["date_precision"], "month");

	let day = api
		.call("PATCH", &uri, json!({"date": "27/04/18"}))
		.await
		.json();
	assert_eq!(day["sidecar"]["date"], "2018-04-27");
	assert_eq!(day["sidecar"].get("date_precision"), None);
}
