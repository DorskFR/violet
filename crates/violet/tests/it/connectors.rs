use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use time::OffsetDateTime;
use tower::ServiceExt;
use violet::auth::{Role, Scope, User};
use violet::connectors::{
	self, Auth, BoxFuture, Connector, ConnectorConfig, Connectors, Credentials, Kinds, Login,
	RemoteDoc, Report,
};
use violet::pipeline::{self, Outcome};
use violet::{Error, Models};
use violet_core::{ConnectorSource, ConnectorStore, Sidecar, hash};

use crate::common::{Harness, text_pdf};
use crate::users::{Api, As, household};

const CODE: &str = "123456";
const DEVICE: &str = "device-cookie-0451";
const PROMPT: &str = "Enter the code sent to s***@example.com";
const USERNAME: &str = "sam@example.com";
const PASSWORD: &str = "correct-horse-battery";
const OTHER_PASSWORD: &str = "tr0ub4dor-and-3";

/// A site that signs in with [`USERNAME`] and [`PASSWORD`], mails a code to a browser it does
/// not know, and trusts it afterwards.
#[derive(Default)]
struct Site {
	docs: Mutex<Vec<(RemoteDoc, Vec<u8>)>>,
	broken: Mutex<HashSet<String>>,
	codes_sent: AtomicU32,
	fetched: Mutex<Vec<String>>,
}

impl Site {
	fn add(&self, remote_id: &str, title: &str, bytes: &[u8]) {
		self.docs.lock().unwrap().push((
			RemoteDoc {
				remote_id: remote_id.into(),
				file_name: format!("../{remote_id}.pdf"),
				title: Some(title.into()),
				date: Some(format!("{remote_id}-25")),
			},
			bytes.to_vec(),
		));
	}

	fn fetched(&self) -> Vec<String> {
		self.fetched.lock().unwrap().clone()
	}

	fn codes_sent(&self) -> u32 {
		self.codes_sent.load(Ordering::SeqCst)
	}

	fn kinds(self: &Arc<Self>) -> Kinds {
		let site = self.clone();
		Kinds::new().with("payroll", move |config: &ConnectorConfig| {
			anyhow::ensure!(
				config
					.options
					.as_object()
					.is_some_and(|options| options.keys().all(|key| key == "company")),
				"unknown option"
			);
			Ok(Box::new(Payroll {
				site: site.clone(),
				device: None,
				signed_in: false,
			}) as Box<dyn Connector>)
		})
	}
}

struct Payroll {
	site: Arc<Site>,
	device: Option<String>,
	signed_in: bool,
}

impl Connector for Payroll {
	fn login<'a>(
		&'a mut self,
		credentials: &'a Credentials,
		saved: Option<Value>,
	) -> BoxFuture<'a, anyhow::Result<Login>> {
		Box::pin(async move {
			anyhow::ensure!(
				credentials.username == USERNAME && credentials.password == PASSWORD,
				"the site refused the password"
			);
			if saved.is_some_and(|saved| saved["device"] == DEVICE) {
				self.device = Some(DEVICE.into());
				self.signed_in = true;
				return Ok(Login::Ready);
			}
			self.site.codes_sent.fetch_add(1, Ordering::SeqCst);
			Ok(Login::NeedsCode {
				prompt: PROMPT.into(),
			})
		})
	}

	fn submit_code<'a>(&'a mut self, code: &'a str) -> BoxFuture<'a, anyhow::Result<()>> {
		Box::pin(async move {
			anyhow::ensure!(code == CODE, "wrong code");
			self.device = Some(DEVICE.into());
			self.signed_in = true;
			Ok(())
		})
	}

	fn state(&self) -> Option<Value> {
		self.device
			.as_ref()
			.map(|device| json!({ "device": device }))
	}

	fn list(&mut self) -> BoxFuture<'_, anyhow::Result<Vec<RemoteDoc>>> {
		Box::pin(async move {
			anyhow::ensure!(self.signed_in, "not signed in");
			let docs = self.site.docs.lock().unwrap();
			Ok(docs.iter().map(|(doc, _)| doc.clone()).collect())
		})
	}

	fn fetch<'a>(&'a mut self, remote_id: &'a str) -> BoxFuture<'a, anyhow::Result<Vec<u8>>> {
		Box::pin(async move {
			anyhow::ensure!(self.signed_in, "not signed in");
			self.site.fetched.lock().unwrap().push(remote_id.into());
			anyhow::ensure!(
				!self.site.broken.lock().unwrap().contains(remote_id),
				"the download timed out"
			);
			let docs = self.site.docs.lock().unwrap();
			let (_, bytes) = docs
				.iter()
				.find(|(doc, _)| doc.remote_id == remote_id)
				.ok_or_else(|| anyhow::anyhow!("no such document"))?;
			Ok(bytes.clone())
		})
	}
}

fn pay(schedule: &str) -> Value {
	json!({
		"name": "pay", "kind": "payroll", "username": USERNAME, "password": PASSWORD,
		"schedule": schedule, "utc_offset": "+09:00", "area": "Work", "tags": ["payroll"],
		"options": {"company": "acme"},
	})
}

fn sam() -> User {
	User {
		name: Some("sam".into()),
		..User::implicit()
	}
}

fn open(harness: &Harness, kinds: Kinds) -> Arc<Connectors> {
	Connectors::open(kinds, &harness.app.library.connectors_path()).unwrap()
}

/// The connectors of the library, with sam's `pay` added unless it is kept already.
async fn setup(harness: &Harness, site: &Arc<Site>, schedule: &str) -> Arc<Connectors> {
	let connectors = open(harness, site.kinds());
	if connectors
		.next_run("pay", OffsetDateTime::now_utc())
		.is_err()
	{
		let input = serde_json::from_value(pay(schedule)).unwrap();
		connectors::create(&harness.app, &connectors, input, &sam())
			.await
			.unwrap();
	}
	connectors
}

fn store(harness: &Harness) -> ConnectorStore {
	ConnectorStore::open(&harness.app.library.connectors_path()).unwrap()
}

fn trust(harness: &Harness) {
	store(harness)
		.set_state("pay", Some(&json!({ "device": DEVICE }).to_string()))
		.unwrap();
}

async fn filed(harness: &Harness, name: &str) -> Sidecar {
	match pipeline::ingest(&harness.app, Path::new(name))
		.await
		.unwrap()
	{
		Outcome::Filed { documents, .. } => {
			assert_eq!(documents.len(), 1);
			harness
				.documents()
				.into_iter()
				.find(|(_, sidecar)| sidecar.id == documents[0].0)
				.unwrap()
				.1
		}
		other => panic!("not filed: {other:?}"),
	}
}

fn view(harness: &Harness) -> Value {
	let connectors = harness.app.connectors().unwrap();
	serde_json::to_value(connectors::view(&harness.app, connectors, &User::implicit()).unwrap())
		.unwrap()
}

#[tokio::test]
async fn a_code_login_from_the_settings_lets_runs_file_documents_once_with_their_origin() {
	let harness = Harness::new(Models::default());
	let site = Arc::new(Site::default());
	let october = text_pdf(&["Payslip October 2026 net pay 300000 yen"]);
	site.add("2026-10", "October payslip", &october);
	let connectors = setup(&harness, &site, "monthly 26 09:00").await;
	assert!(harness.app.set_connectors(connectors.clone()).is_ok());
	let now = OffsetDateTime::now_utc();
	assert_eq!(view(&harness)["connectors"][0]["auth"], "needs_login");
	assert_eq!(connectors.next_run("pay", now).unwrap(), None);

	let error = connectors::run(&harness.app, &connectors, "pay")
		.await
		.unwrap_err();
	assert!(error.to_string().contains("asks for a code"), "{error}");
	let stopped = &view(&harness)["connectors"][0];
	assert_eq!(stopped["auth"], "needs_login");
	assert_eq!(stopped["next_run"], Value::Null);
	assert!(stopped["last_error"].as_str().unwrap().contains("code"));
	assert_eq!(stopped["last_success"], Value::Null);
	assert_eq!(connectors.next_run("pay", now).unwrap(), None);
	assert!(site.fetched().is_empty() && harness.inbox().is_empty());

	assert!(matches!(
		connectors::submit_code(&connectors, "pay", CODE).await,
		Err(Error::Conflict(_))
	));
	let asked = connectors::login(&connectors, "pay").await.unwrap();
	assert_eq!(asked.auth, Auth::NeedsCode);
	assert_eq!(asked.prompt.as_deref(), Some(PROMPT));
	assert_eq!(site.codes_sent(), 2);
	let waiting = &view(&harness)["connectors"][0];
	assert_eq!(waiting["auth"], "needs_code");
	assert_eq!(waiting["prompt"], PROMPT);
	let blocked = connectors::run(&harness.app, &connectors, "pay")
		.await
		.unwrap_err();
	assert!(blocked.to_string().contains("waiting for its code"));
	assert_eq!(site.codes_sent(), 2, "a run never asks for another code");

	for refused in ["", " ", "000000"] {
		assert!(matches!(
			connectors::submit_code(&connectors, "pay", refused).await,
			Err(Error::Invalid(_))
		));
	}
	let ready = connectors::submit_code(&connectors, "pay", " 123456 ")
		.await
		.unwrap();
	assert_eq!((ready.auth, ready.prompt), (Auth::Ready, None));
	assert_eq!(view(&harness)["connectors"][0]["auth"], "ready");
	assert_eq!(connectors.next_run("pay", now).unwrap(), Some(now));

	let report = connectors::run(&harness.app, &connectors, "pay")
		.await
		.unwrap();
	assert_eq!(
		report,
		Report {
			listed: 1,
			ingested: 1,
			..Report::default()
		}
	);
	assert_eq!(site.codes_sent(), 2);
	assert_eq!(harness.inbox(), ["2026-10.pdf"]);
	assert_eq!(
		harness
			.app
			.index()
			.uploader(&hash::sha256(&october))
			.unwrap()
			.as_deref(),
		Some("sam")
	);
	let sidecar = filed(&harness, "2026-10.pdf").await;
	assert_eq!(sidecar.area, "Work");
	assert!(sidecar.tags.contains(&"payroll".to_string()));
	assert_eq!(sidecar.sources[0].mail, None);
	assert_eq!(
		sidecar.sources[0].connector,
		Some(ConnectorSource {
			name: "pay".into(),
			kind: "payroll".into(),
			remote_id: "2026-10".into(),
			title: Some("October payslip".into()),
			date: Some("2026-10-25".into()),
		})
	);

	let again = connectors::run(&harness.app, &connectors, "pay")
		.await
		.unwrap();
	assert_eq!((again.listed, again.ingested), (1, 0));
	assert_eq!(site.fetched(), ["2026-10"]);
	assert!(harness.inbox().is_empty());

	site.add(
		"2026-11",
		"November payslip",
		&text_pdf(&["Payslip November 2026"]),
	);
	site.add("2026-12", "Empty", b"");
	site.add(
		"2026-11",
		"Listed twice",
		&text_pdf(&["Payslip November 2026"]),
	);
	site.broken.lock().unwrap().insert("2026-11".into());
	let before = OffsetDateTime::now_utc();
	let partial = connectors::run(&harness.app, &connectors, "pay")
		.await
		.unwrap();
	assert_eq!(
		partial,
		Report {
			listed: 4,
			ingested: 0,
			skipped: 1,
			failed: 1,
			errors: vec!["November payslip: the download timed out".into()],
		}
	);
	let retry = connectors.next_run("pay", before).unwrap().unwrap();
	let wait = (retry - before).whole_seconds();
	assert!((30..=40).contains(&wait), "retries in {wait}s");
	let failed = &view(&harness)["connectors"][0];
	assert_eq!(
		failed["counts"],
		json!({"ingested": 1, "skipped": 1, "failed": 1})
	);
	assert!(failed["last_error"].as_str().unwrap().contains("timed out"));
	assert_eq!(failed["last_report"]["failed"], 1);

	site.broken.lock().unwrap().clear();
	let fixed = connectors::run(&harness.app, &connectors, "pay")
		.await
		.unwrap();
	assert_eq!((fixed.ingested, fixed.skipped, fixed.failed), (1, 0, 0));
	assert_eq!(site.fetched(), ["2026-10", "2026-11", "2026-12", "2026-11"]);
	assert_eq!(harness.inbox(), ["2026-11.pdf"]);
	let healthy = &view(&harness)["connectors"][0];
	assert_eq!(healthy["last_error"], Value::Null);
	assert_eq!(
		healthy["counts"],
		json!({"ingested": 2, "skipped": 1, "failed": 0})
	);
	let next = connectors.next_run("pay", before).unwrap().unwrap();
	assert!(next > before + time::Duration::hours(1), "back on schedule");
	assert_eq!(view(&harness)["items"].as_array().unwrap().len(), 3);
}

#[tokio::test]
async fn the_saved_state_signs_in_after_a_restart_and_is_never_shown() {
	let harness = Harness::new(Models::default());
	let site = Arc::new(Site::default());
	site.add(
		"2026-10",
		"October payslip",
		&text_pdf(&["Payslip October"]),
	);
	{
		let connectors = setup(&harness, &site, "every 6h").await;
		connectors::login(&connectors, "pay").await.unwrap();
		connectors::submit_code(&connectors, "pay", CODE)
			.await
			.unwrap();
	}
	assert_eq!(
		store(&harness).state("pay").unwrap().as_deref(),
		Some(json!({ "device": DEVICE }).to_string().as_str())
	);

	let connectors = setup(&harness, &site, "every 6h").await;
	assert!(harness.app.set_connectors(connectors.clone()).is_ok());
	let signed_in = connectors::login(&connectors, "pay").await.unwrap();
	assert_eq!(signed_in.auth, Auth::Ready);
	let report = connectors::run(&harness.app, &connectors, "pay")
		.await
		.unwrap();
	assert_eq!(report.ingested, 1);
	assert_eq!(site.codes_sent(), 1);

	let (status, body) = call(&harness, "GET", "/api/connectors", None).await;
	assert_eq!(status, StatusCode::OK);
	let text = body.to_string();
	for hidden in [DEVICE, "device", PASSWORD, "\"password\"", "state"] {
		assert!(!text.contains(hidden), "{hidden} in {text}");
	}
}

async fn until(what: &str, mut done: impl FnMut() -> bool) {
	for _ in 0..500 {
		if done() {
			return;
		}
		tokio::time::sleep(std::time::Duration::from_millis(10)).await;
	}
	panic!("{what} did not happen");
}

#[tokio::test]
async fn a_run_missed_while_down_runs_once_after_the_start_and_a_kept_one_does_not() {
	let harness = Harness::new(Models::default());
	let site = Arc::new(Site::default());
	site.add(
		"2026-10",
		"October payslip",
		&text_pdf(&["Payslip October"]),
	);
	let connectors = setup(&harness, &site, "monthly 26 09:00").await;
	trust(&harness);
	let now = OffsetDateTime::now_utc();
	let long_ago = now - time::Duration::days(40);
	store(&harness)
		.finish_run("pay", long_ago.unix_timestamp(), None, None)
		.unwrap();
	store(&harness).set_needs_login("pay", false).unwrap();
	let missed = connectors.next_run("pay", now).unwrap().unwrap();
	assert!(missed < now && missed > long_ago);
	let tasks = connectors::spawn(&harness.app, &connectors);
	until("the missed run", || harness.inbox() == ["2026-10.pdf"]).await;
	until("the run to be recorded", || {
		store(&harness)
			.run("pay")
			.unwrap()
			.last_success
			.is_some_and(|at| at >= now.unix_timestamp())
	})
	.await;
	let next = connectors.next_run("pay", now).unwrap().unwrap();
	assert!(next > now, "the next run is the next 26th");
	assert!(next <= now + time::Duration::days(32));
	for task in tasks {
		task.abort();
	}
	drop(connectors);

	site.add(
		"2026-11",
		"November payslip",
		&text_pdf(&["Payslip November"]),
	);
	let restarted = setup(&harness, &site, "monthly 26 09:00").await;
	assert_eq!(restarted.next_run("pay", now).unwrap(), Some(next));
	let tasks = connectors::spawn(&harness.app, &restarted);
	tokio::time::sleep(std::time::Duration::from_millis(200)).await;
	assert_eq!(site.fetched(), ["2026-10"], "nothing runs before it is due");

	assert!(restarted.wake("pay") && !restarted.wake("bank"));
	until("the run asked for", || site.fetched().len() == 2).await;
	for task in tasks {
		task.abort();
	}
}

fn no_secret(text: &str) {
	for hidden in [PASSWORD, OTHER_PASSWORD, DEVICE] {
		assert!(!text.contains(hidden), "{hidden} in {text}");
	}
}

async fn call(
	harness: &Harness,
	method: &str,
	uri: &str,
	body: Option<Value>,
) -> (StatusCode, Value) {
	let request = Request::builder().method(method).uri(uri);
	let request = match body {
		Some(body) => request
			.header("content-type", "application/json")
			.body(Body::from(body.to_string())),
		None => request.body(Body::empty()),
	}
	.unwrap();
	let response = violet::http::router(harness.app.clone())
		.oneshot(request)
		.await
		.unwrap();
	let status = response.status();
	let bytes = response.into_body().collect().await.unwrap().to_bytes();
	no_secret(&String::from_utf8_lossy(&bytes));
	(status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn the_api_shows_connectors_signs_in_runs_and_refetches() {
	let off = Harness::new(Models::default());
	for (method, uri) in [
		("GET", "/api/connectors"),
		("GET", "/api/connectors/kinds"),
		("POST", "/api/connectors/pay/run"),
		("POST", "/api/connectors/pay/login"),
	] {
		let (status, _) = call(&off, method, uri, None).await;
		assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
	}

	let harness = Harness::new(Models::default());
	let site = Arc::new(Site::default());
	site.add(
		"2026-10",
		"October payslip",
		&text_pdf(&["Payslip October"]),
	);
	let connectors = setup(&harness, &site, "weekly mon 06:30").await;
	assert!(harness.app.set_connectors(connectors.clone()).is_ok());

	let (_, body) = call(&harness, "GET", "/api/connectors", None).await;
	assert_eq!(body["items"], json!([]));
	let shown = &body["connectors"][0];
	assert_eq!(shown["name"], "pay");
	assert_eq!(shown["kind"], "payroll");
	assert_eq!(shown["owner"], "sam");
	assert_eq!(shown["username"], USERNAME);
	assert_eq!(shown["password_set"], true);
	assert_eq!(shown["schedule"], "weekly mon 06:30");
	assert_eq!(shown["utc_offset"], "+09:00");
	assert_eq!(shown["area"], "Work");
	assert_eq!(shown["tags"], json!(["payroll"]));
	assert_eq!(shown["options"], json!({"company": "acme"}));
	assert_eq!(shown["running"], false);
	assert_eq!(shown["auth"], "needs_login");

	let (status, body) = call(&harness, "POST", "/api/connectors/pay/login", None).await;
	assert_eq!(status, StatusCode::OK);
	assert_eq!(
		body,
		json!({"name": "pay", "auth": "needs_code", "prompt": PROMPT})
	);
	let code = "/api/connectors/pay/login/code";
	let (status, body) = call(&harness, "POST", code, Some(json!({"code": "1"}))).await;
	assert_eq!(status, StatusCode::BAD_REQUEST);
	assert_eq!(body["error"], "the code was not accepted: wrong code");
	assert_eq!(
		call(&harness, "POST", code, Some(json!({"code": CODE, "x": 1})))
			.await
			.0,
		StatusCode::BAD_REQUEST
	);
	let (status, body) = call(&harness, "POST", code, Some(json!({"code": CODE}))).await;
	assert_eq!(status, StatusCode::OK);
	assert_eq!(body["auth"], "ready");
	assert_eq!(
		call(&harness, "POST", code, Some(json!({"code": CODE})))
			.await
			.0,
		StatusCode::CONFLICT
	);
	assert_eq!(
		call(&harness, "POST", "/api/connectors/bank/login", None)
			.await
			.0,
		StatusCode::NOT_FOUND
	);

	let (status, body) = call(&harness, "POST", "/api/connectors/pay/run", None).await;
	assert_eq!(status, StatusCode::ACCEPTED);
	assert_eq!(body, json!({"name": "pay", "started": true}));
	assert_eq!(
		call(&harness, "POST", "/api/connectors/bank/run", None)
			.await
			.0,
		StatusCode::NOT_FOUND
	);
	connectors::run(&harness.app, &connectors, "pay")
		.await
		.unwrap();

	let (_, body) = call(&harness, "GET", "/api/connectors", None).await;
	let shown = &body["connectors"][0];
	assert_eq!(shown["auth"], "ready");
	assert_eq!(shown["last_report"]["ingested"], 1);
	assert!(shown["next_run"].is_string() && shown["last_success"].is_string());
	let item = &body["items"][0];
	assert_eq!(item["connector"], "pay");
	assert_eq!(item["remote_id"], "2026-10");
	assert_eq!(item["title"], "October payslip");
	assert_eq!(item["status"], "ingested");
	let id = item["id"].as_i64().unwrap();

	let refetch = format!("/api/connectors/items/{id}/refetch");
	let (status, body) = call(&harness, "POST", &refetch, None).await;
	assert_eq!(status, StatusCode::OK);
	assert_eq!(body["id"], id);
	assert_eq!(
		call(&harness, "POST", &refetch, None).await.0,
		StatusCode::NOT_FOUND
	);
	assert_eq!(
		call(&harness, "POST", "/api/connectors/items/x/refetch", None)
			.await
			.0,
		StatusCode::BAD_REQUEST
	);
	let again = connectors::run(&harness.app, &connectors, "pay")
		.await
		.unwrap();
	assert_eq!(again.ingested, 1);
	assert_eq!(harness.inbox(), ["2026-10 (2).pdf", "2026-10.pdf"]);
}

async fn sign_in(harness: &Harness, name: &str) {
	let login = format!("/api/connectors/{name}/login");
	let (status, body) = call(harness, "POST", &login, None).await;
	assert_eq!(status, StatusCode::OK, "{body}");
	if body["auth"] == "needs_code" {
		let code = format!("{login}/code");
		let (status, body) = call(harness, "POST", &code, Some(json!({"code": CODE}))).await;
		assert_eq!((status, &body["auth"]), (StatusCode::OK, &json!("ready")));
	}
}

#[tokio::test]
async fn the_kinds_are_listed_with_the_options_the_form_asks_for() {
	let harness = Harness::new(Models::default());
	let connectors = open(&harness, connectors::kinds::builtin());
	assert!(harness.app.set_connectors(connectors).is_ok());
	let (status, body) = call(&harness, "GET", "/api/connectors/kinds", None).await;
	assert_eq!(status, StatusCode::OK);
	assert_eq!(
		body,
		json!({"kinds": [{
			"kind": "moneyforward",
			"label": "Money Forward Cloud",
			"options": [
				{
					"name": "documents", "label": "Documents", "help": null, "type": "choices",
					"choices": [
						{"value": "payslips", "label": "Payslips"},
						{"value": "bonus_slips", "label": "Bonus slips"},
						{"value": "withholding_slips", "label": "Withholding slips"},
					],
					"default": ["payslips", "bonus_slips", "withholding_slips"],
				},
				{
					"name": "since", "label": "Since", "type": "date",
					"help": "Older documents are left on the site; empty fetches everything.",
				},
			],
		}]})
	);

	let mut mf = pay("monthly 26 09:00");
	mf["name"] = json!("mf");
	mf["kind"] = json!("moneyforward");
	mf["options"] = json!({"documents": ["payslips"], "since": "2026-01-01"});
	let (status, body) = call(&harness, "POST", "/api/connectors", Some(mf.clone())).await;
	assert_eq!(status, StatusCode::CREATED, "{body}");
	assert_eq!(body["options"], mf["options"]);
	mf["name"] = json!("mf2");
	mf["options"] = json!({"documents": ["letters"]});
	let (status, body) = call(&harness, "POST", "/api/connectors", Some(mf)).await;
	assert_eq!(status, StatusCode::BAD_REQUEST);
	assert!(body["error"].as_str().unwrap().starts_with("options: "));
}

#[tokio::test]
async fn connectors_are_added_edited_and_removed_from_the_api() {
	let harness = Harness::new(Models::default());
	let site = Arc::new(Site::default());
	site.add(
		"2026-10",
		"October payslip",
		&text_pdf(&["Payslip October"]),
	);
	let connectors = open(&harness, site.kinds());
	assert!(harness.app.set_connectors(connectors.clone()).is_ok());
	let (_, body) = call(&harness, "GET", "/api/connectors", None).await;
	assert_eq!(body, json!({"connectors": [], "items": []}));
	let (_, body) = call(&harness, "GET", "/api/connectors/kinds", None).await;
	assert_eq!(
		body,
		json!({"kinds": [{"kind": "payroll", "label": "payroll", "options": []}]})
	);

	let with = |field: &str, value: Value| {
		let mut body = pay("every 6h");
		body[field] = value;
		body
	};
	for (body, expected) in [
		(
			with("kind", json!("bank")),
			"unknown kind \"bank\" (known: payroll)",
		),
		(with("options", json!({"colour": 1})), "unknown option"),
		(with("schedule", json!("sometimes")), "unknown schedule"),
		(with("utc_offset", json!("JST")), "not a UTC offset"),
		(with("name", json!("pay slips")), "the name must be letters"),
		(with("name", json!("kinds")), "cannot be named"),
		(with("name", Value::Null), "no name"),
		(with("username", json!(" ")), "enter the username"),
		(with("password", json!("")), "enter the password"),
		(with("password", Value::Null), "enter the password"),
		(with("area", json!("a/b")), "cannot be an area folder"),
		(with("tags", json!([""])), "a tag is empty"),
		(with("password_env", json!("P")), "unknown field"),
	] {
		let (status, answer) = call(&harness, "POST", "/api/connectors", Some(body)).await;
		assert_eq!(status, StatusCode::BAD_REQUEST, "{expected}");
		let error = answer["error"].as_str().unwrap();
		assert!(error.contains(expected), "{expected}: {error}");
	}
	assert!(store(&harness).definitions().unwrap().is_empty());

	let (status, added) = call(&harness, "POST", "/api/connectors", Some(pay("every 6h"))).await;
	assert_eq!(status, StatusCode::CREATED, "{added}");
	assert_eq!(added["name"], "pay");
	assert_eq!(added["owner"], Value::Null);
	assert_eq!(added["password_set"], true);
	assert_eq!(added["auth"], "needs_login");
	assert!(added.get("password").is_none());
	let (status, body) = call(&harness, "POST", "/api/connectors", Some(pay("every 1d"))).await;
	assert_eq!(status, StatusCode::CONFLICT);
	assert_eq!(body["error"], "a connector named pay exists already");
	let kept = store(&harness).definition("pay").unwrap().unwrap();
	assert_eq!(kept.password, PASSWORD);
	assert_eq!((kept.owner, kept.schedule.as_str()), (None, "every 6h"));

	sign_in(&harness, "pay").await;
	let ran = connectors::run(&harness.app, &connectors, "pay")
		.await
		.unwrap();
	assert_eq!(ran.ingested, 1);
	assert_eq!(
		harness
			.app
			.index()
			.uploader(&hash::sha256(&text_pdf(&["Payslip October"])))
			.unwrap(),
		None
	);

	let edit = |fields: Value| {
		let mut body = pay("daily 07:00");
		body.as_object_mut().unwrap().remove("name");
		body.as_object_mut().unwrap().remove("kind");
		for (key, value) in fields.as_object().unwrap() {
			body[key] = value.clone();
		}
		body
	};
	for blank in [json!(""), Value::Null] {
		let body = edit(json!({"password": blank, "area": null, "tags": ["pay", "2026"]}));
		let (status, edited) = call(&harness, "PUT", "/api/connectors/pay", Some(body)).await;
		assert_eq!(status, StatusCode::OK, "{edited}");
		assert_eq!(edited["schedule"], "daily 07:00");
		assert_eq!(edited["area"], Value::Null);
		assert_eq!(edited["tags"], json!(["pay", "2026"]));
		assert_eq!(edited["auth"], "ready", "the sign-in is kept");
	}
	assert_eq!(
		store(&harness).definition("pay").unwrap().unwrap().password,
		PASSWORD
	);
	assert!(store(&harness).state("pay").unwrap().is_some());
	let next = connectors
		.next_run("pay", OffsetDateTime::now_utc())
		.unwrap()
		.unwrap();
	assert_eq!(
		(
			next.to_offset(time::macros::offset!(+9)).hour(),
			next.minute()
		),
		(7, 0)
	);

	for (uri, body, status, expected) in [
		(
			"/api/connectors/pay",
			edit(json!({"name": "bank"})),
			400,
			"cannot be renamed",
		),
		(
			"/api/connectors/pay",
			edit(json!({"kind": "bank"})),
			400,
			"cannot change",
		),
		(
			"/api/connectors/pay",
			edit(json!({"schedule": "never"})),
			400,
			"unknown schedule",
		),
		(
			"/api/connectors/pay",
			edit(json!({"options": {"x": 1}})),
			400,
			"unknown option",
		),
		(
			"/api/connectors/bank",
			edit(json!({})),
			404,
			"connector bank",
		),
	] {
		let (answered, answer) = call(&harness, "PUT", uri, Some(body)).await;
		assert_eq!(answered.as_u16(), status, "{expected}");
		let error = answer["error"].as_str().unwrap();
		assert!(error.contains(expected), "{expected}: {error}");
	}
	assert_eq!(view(&harness)["connectors"][0]["auth"], "ready");

	let body = edit(json!({"password": OTHER_PASSWORD}));
	let (status, edited) = call(&harness, "PUT", "/api/connectors/pay", Some(body)).await;
	assert_eq!(status, StatusCode::OK);
	assert_eq!(edited["auth"], "needs_login");
	assert_eq!(edited["next_run"], Value::Null);
	assert_eq!(store(&harness).state("pay").unwrap(), None);
	assert!(store(&harness).run("pay").unwrap().needs_login);
	assert_eq!(
		store(&harness).definition("pay").unwrap().unwrap().password,
		OTHER_PASSWORD
	);
	let (status, body) = call(&harness, "POST", "/api/connectors/pay/login", None).await;
	assert_eq!(status, StatusCode::CONFLICT);
	assert_eq!(
		body["error"],
		"could not sign in: the site refused the password"
	);

	let body = edit(json!({"password": PASSWORD}));
	call(&harness, "PUT", "/api/connectors/pay", Some(body)).await;
	sign_in(&harness, "pay").await;
	let body = edit(json!({"username": "alex@example.com", "password": null}));
	let (_, edited) = call(&harness, "PUT", "/api/connectors/pay", Some(body)).await;
	assert_eq!(edited["username"], "alex@example.com");
	assert_eq!(
		edited["auth"], "needs_login",
		"another username signs in again"
	);
	assert_eq!(store(&harness).state("pay").unwrap(), None);

	let (status, body) = call(&harness, "DELETE", "/api/connectors/pay", None).await;
	assert_eq!(status, StatusCode::OK);
	assert_eq!(body, json!({"name": "pay", "removed": true}));
	assert!(store(&harness).definitions().unwrap().is_empty());
	assert_eq!(store(&harness).state("pay").unwrap(), None);
	let (_, body) = call(&harness, "GET", "/api/connectors", None).await;
	assert_eq!(body["connectors"], json!([]));
	assert_eq!(body["items"][0]["remote_id"], "2026-10");
	for (method, uri) in [
		("DELETE", "/api/connectors/pay"),
		("POST", "/api/connectors/pay/run"),
		("POST", "/api/connectors/pay/login"),
	] {
		let (status, _) = call(&harness, method, uri, None).await;
		assert_eq!(status, StatusCode::NOT_FOUND, "{method} {uri}");
	}
	assert!(
		connectors::run(&harness.app, &connectors, "pay")
			.await
			.is_err()
	);

	let (status, again) = call(&harness, "POST", "/api/connectors", Some(pay("every 6h"))).await;
	assert_eq!(status, StatusCode::CREATED);
	assert_eq!(again["auth"], "needs_login");
	assert_eq!(again["counts"]["ingested"], 1);
	sign_in(&harness, "pay").await;
	let rerun = connectors::run(&harness.app, &connectors, "pay")
		.await
		.unwrap();
	assert_eq!((rerun.listed, rerun.ingested), (1, 0));
	assert_eq!(site.fetched(), ["2026-10"]);
}

#[tokio::test]
async fn a_connector_runs_from_when_it_is_added_until_it_is_removed() {
	let harness = Harness::new(Models::default());
	let site = Arc::new(Site::default());
	site.add(
		"2026-10",
		"October payslip",
		&text_pdf(&["Payslip October"]),
	);
	let connectors = open(&harness, site.kinds());
	assert!(harness.app.set_connectors(connectors.clone()).is_ok());
	assert!(connectors::spawn(&harness.app, &connectors).is_empty());

	let (status, _) = call(&harness, "POST", "/api/connectors", Some(pay("every 1d"))).await;
	assert_eq!(status, StatusCode::CREATED);
	tokio::time::sleep(std::time::Duration::from_millis(100)).await;
	assert!(site.fetched().is_empty() && site.codes_sent() == 0);
	sign_in(&harness, "pay").await;
	until("the first run", || harness.inbox() == ["2026-10.pdf"]).await;
	until("the run to be recorded", || {
		store(&harness).run("pay").unwrap().last_success.is_some()
	})
	.await;

	site.add(
		"2026-11",
		"November payslip",
		&text_pdf(&["Payslip November"]),
	);
	let (status, _) = call(&harness, "POST", "/api/connectors/pay/run", None).await;
	assert_eq!(status, StatusCode::ACCEPTED);
	until("the run asked for", || site.fetched().len() == 2).await;

	let (status, _) = call(&harness, "DELETE", "/api/connectors/pay", None).await;
	assert_eq!(status, StatusCode::OK);
	site.add(
		"2026-12",
		"December payslip",
		&text_pdf(&["Payslip December"]),
	);
	assert!(!connectors.wake("pay"));
	tokio::time::sleep(std::time::Duration::from_millis(200)).await;
	assert_eq!(site.fetched().len(), 2, "a removed connector runs no more");
}

#[tokio::test]
async fn members_see_and_manage_only_the_connectors_they_added() {
	let home = household(None).await;
	let api: &Api = &home.api;
	api.add_user("kim", Role::Member);
	let kim = api.token("kim", "phone", Scope::Write);
	let (alex, sam, kim) = (As::Token(&home.alex), As::Token(&home.sam), As::Token(&kim));
	let reader = As::Token(&home.sam_reader);
	let site = Arc::new(Site::default());
	let payslip = text_pdf(&["Payslip October"]);
	site.add("2026-10", "October payslip", &payslip);
	let connectors = open(&api.harness, site.kinds());
	assert!(api.harness.app.set_connectors(connectors.clone()).is_ok());

	let call = async |method: &str, uri: &str, body: Value, who: As<'_>| {
		let answer = api.call(method, uri, body, who).await;
		no_secret(&String::from_utf8_lossy(&answer.body));
		(answer.status, answer.json())
	};
	let names = async |who: As<'_>| {
		let (status, body) = call("GET", "/api/connectors", Value::Null, who).await;
		assert_eq!(status, StatusCode::OK);
		let names: Vec<String> = body["connectors"]
			.as_array()
			.unwrap()
			.iter()
			.map(|connector| connector["name"].as_str().unwrap().to_string())
			.collect();
		(names, body["items"].as_array().unwrap().len())
	};
	let mut bank = pay("every 6h");
	bank["name"] = json!("bank");
	bank["area"] = Value::Null;

	for uri in ["/api/connectors", "/api/connectors/kinds"] {
		let (status, _) = call("GET", uri, Value::Null, reader).await;
		assert_eq!(status, StatusCode::FORBIDDEN, "{uri}");
		let (status, _) = call("GET", uri, Value::Null, kim).await;
		assert_eq!(status, StatusCode::OK, "{uri}");
	}
	let (status, _) = call("POST", "/api/connectors", bank.clone(), reader).await;
	assert_eq!(status, StatusCode::FORBIDDEN);

	let (status, added) = call("POST", "/api/connectors", pay("every 6h"), sam).await;
	assert_eq!(status, StatusCode::CREATED, "{added}");
	assert_eq!(added["owner"], "sam");
	let (status, added) = call("POST", "/api/connectors", bank.clone(), alex).await;
	assert_eq!(
		(status, &added["owner"]),
		(StatusCode::CREATED, &json!("alex"))
	);
	let (status, _) = call("POST", "/api/connectors", bank.clone(), kim).await;
	assert_eq!(status, StatusCode::CONFLICT);

	assert_eq!(names(alex).await.0, ["pay", "bank"]);
	assert_eq!(names(sam).await.0, ["pay"]);
	assert!(names(kim).await.0.is_empty());

	let mut edit = pay("daily 07:00");
	edit.as_object_mut().unwrap().remove("name");
	edit["password"] = Value::Null;
	for (who, name) in [(kim, "pay"), (kim, "bank"), (sam, "bank")] {
		for (method, path, body) in [
			("PUT", "", edit.clone()),
			("DELETE", "", Value::Null),
			("POST", "/run", Value::Null),
			("POST", "/login", Value::Null),
			("POST", "/login/code", json!({"code": CODE})),
		] {
			let uri = format!("/api/connectors/{name}{path}");
			let (status, body) = call(method, &uri, body, who).await;
			assert_eq!(status, StatusCode::NOT_FOUND, "{method} {uri}");
			assert_eq!(body["error"], format!("not found: connector {name}"));
		}
	}
	assert_eq!(site.codes_sent(), 0);
	assert_eq!(store(&api.harness).definitions().unwrap().len(), 2);
	assert_eq!(
		store(&api.harness)
			.definition("pay")
			.unwrap()
			.unwrap()
			.schedule,
		"every 6h"
	);

	for (who, name) in [(sam, "pay"), (alex, "bank")] {
		let login = format!("/api/connectors/{name}/login");
		let (status, _) = call("POST", &login, Value::Null, who).await;
		assert_eq!(status, StatusCode::OK);
		let (status, body) =
			call("POST", &format!("{login}/code"), json!({"code": CODE}), who).await;
		assert_eq!((status, &body["auth"]), (StatusCode::OK, &json!("ready")));
	}
	connectors::run(&api.harness.app, &connectors, "bank")
		.await
		.unwrap();
	let uploader = |app: &violet::App| app.index().uploader(&hash::sha256(&payslip)).unwrap();
	assert_eq!(uploader(&api.harness.app).as_deref(), Some("alex"));
	assert_eq!(names(alex).await.1, 1);
	assert_eq!(names(sam).await.1, 0);
	assert_eq!(names(kim).await.1, 0);
	let item = store_item(&api.harness);
	let refetch = format!("/api/connectors/items/{item}/refetch");
	for who in [sam, kim] {
		let (status, _) = call("POST", &refetch, Value::Null, who).await;
		assert_eq!(status, StatusCode::NOT_FOUND);
	}
	assert_eq!(names(alex).await.1, 1);

	connectors::run(&api.harness.app, &connectors, "pay")
		.await
		.unwrap();
	assert_eq!(names(sam).await.1, 1);
	assert_eq!(names(alex).await.1, 2);
	let (_, mine) = call("GET", "/api/connectors", Value::Null, sam).await;
	assert_eq!(mine["items"][0]["connector"], "pay");
	let own = format!(
		"/api/connectors/items/{}/refetch",
		mine["items"][0]["id"].as_i64().unwrap()
	);
	let (status, _) = call("POST", &own, Value::Null, sam).await;
	assert_eq!(status, StatusCode::OK);
	let (status, _) = call("POST", &refetch, Value::Null, alex).await;
	assert_eq!(status, StatusCode::OK);

	let (status, edited) = call("PUT", "/api/connectors/pay", edit.clone(), alex).await;
	assert_eq!(status, StatusCode::OK, "{edited}");
	assert_eq!(edited["owner"], "sam", "an admin's edit leaves the owner");
	assert_eq!(edited["schedule"], "daily 07:00");
	let (status, _) = call("DELETE", "/api/connectors/pay", Value::Null, sam).await;
	assert_eq!(status, StatusCode::OK);
	let (status, _) = call("DELETE", "/api/connectors/bank", Value::Null, alex).await;
	assert_eq!(status, StatusCode::OK);
	assert!(names(alex).await.0.is_empty());
}

fn store_item(harness: &Harness) -> i64 {
	harness
		.app
		.index()
		.connector_items(Some("bank"), 1)
		.unwrap()[0]
		.id
}

#[tokio::test]
async fn a_removed_name_starts_clean_for_anyone_but_the_owner_who_removed_it() {
	let home = household(None).await;
	let api: &Api = &home.api;
	api.add_user("kim", Role::Member);
	let kim = api.token("kim", "phone", Scope::Write);
	let (alex, sam, kim) = (As::Token(&home.alex), As::Token(&home.sam), As::Token(&kim));
	let site = Arc::new(Site::default());
	site.add(
		"2026-10",
		"October payslip",
		&text_pdf(&["Payslip October"]),
	);
	let connectors = open(&api.harness, site.kinds());
	assert!(api.harness.app.set_connectors(connectors.clone()).is_ok());
	let app = &api.harness.app;

	let call = async |method: &str, uri: &str, body: Value, who: As<'_>| {
		let answer = api.call(method, uri, body, who).await;
		no_secret(&String::from_utf8_lossy(&answer.body));
		(answer.status, answer.json())
	};
	let add = async |who: As<'_>| {
		let (status, added) = call("POST", "/api/connectors", pay("every 6h"), who).await;
		assert_eq!(status, StatusCode::CREATED, "{added}");
		added
	};
	let run = async |who: As<'_>| {
		let (status, _) = call("POST", "/api/connectors/pay/login", Value::Null, who).await;
		assert_eq!(status, StatusCode::OK);
		let code = json!({"code": CODE});
		let (status, _) = call("POST", "/api/connectors/pay/login/code", code, who).await;
		assert_eq!(status, StatusCode::OK);
		connectors::run(app, &connectors, "pay").await.unwrap()
	};
	let remove = async |who: As<'_>| {
		let (status, _) = call("DELETE", "/api/connectors/pay", Value::Null, who).await;
		assert_eq!(status, StatusCode::OK);
	};
	let listed = async |who: As<'_>| call("GET", "/api/connectors", Value::Null, who).await.1;
	let waiting = |app: &violet::App| {
		let sha = hash::sha256(&text_pdf(&["Payslip October"]));
		app.index().connector_hint(&sha).unwrap().is_some()
	};

	add(sam).await;
	assert_eq!(run(sam).await.ingested, 1);
	assert!(waiting(app));
	remove(sam).await;
	assert_eq!(listed(alex).await["items"][0]["title"], "October payslip");
	assert_eq!(listed(sam).await["items"], json!([]));

	let back = add(sam).await;
	assert_eq!(
		back["counts"]["ingested"], 1,
		"the owner picks up where it stopped"
	);
	assert_eq!(listed(sam).await["items"][0]["title"], "October payslip");
	assert_eq!(run(sam).await.ingested, 0);
	assert!(waiting(app));
	remove(sam).await;

	let theirs = add(kim).await;
	assert_eq!(theirs["owner"], "kim");
	assert_eq!(
		theirs["counts"],
		json!({"ingested": 0, "skipped": 0, "failed": 0})
	);
	let seen = listed(kim).await;
	assert_eq!(seen["items"], json!([]));
	assert!(!seen.to_string().contains("October"), "{seen}");
	assert_eq!(listed(alex).await["items"], json!([]));
	assert!(!waiting(app));
	assert_eq!(
		run(kim).await.ingested,
		1,
		"nothing of sam's holds a fetch back"
	);
	assert_eq!(listed(kim).await["items"].as_array().unwrap().len(), 1);
	remove(kim).await;

	let admins = add(alex).await;
	assert_eq!(admins["owner"], "alex");
	assert_eq!(admins["counts"]["ingested"], 0);
	assert_eq!(listed(alex).await["items"], json!([]));
	assert_eq!(run(alex).await.ingested, 1);
	assert_eq!(site.fetched(), ["2026-10", "2026-10", "2026-10"]);
}
