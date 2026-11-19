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
use violet::connectors::{
	self, Auth, BoxFuture, Config, Connector, ConnectorConfig, Connectors, Credentials, Kinds,
	Login, RemoteDoc, Report,
};
use violet::pipeline::{self, Outcome};
use violet::{Error, Models};
use violet_core::{ConnectorSource, ConnectorStore, Sidecar, hash};

use crate::common::{Harness, text_pdf};

const CODE: &str = "123456";
const DEVICE: &str = "device-cookie-0451";
const PROMPT: &str = "Enter the code sent to s***@example.com";

/// A site that signs in with the value of `PATH` as user and password, mails a code to a
/// browser it does not know, and trusts it afterwards.
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
			let expected = std::env::var("PATH").unwrap();
			anyhow::ensure!(
				credentials.username == expected && credentials.password == expected,
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

fn config(schedule: &str) -> Config {
	Config::parse(&format!(
		r#"{{"utc_offset": "+09:00", "connectors": [{{"name": "pay", "kind": "payroll",
			"username_env": "PATH", "password_env": "PATH", "schedule": "{schedule}",
			"hints": {{"owner": "sam", "area": "Work", "tags": ["payroll"]}},
			"options": {{"company": "acme"}}}}]}}"#
	))
	.unwrap()
}

fn setup(harness: &Harness, site: &Arc<Site>, schedule: &str) -> Arc<Connectors> {
	Connectors::new(
		config(schedule),
		&site.kinds(),
		&harness.app.library.connectors_path(),
	)
	.unwrap()
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
	serde_json::to_value(connectors::view(&harness.app).unwrap()).unwrap()
}

#[tokio::test]
async fn a_code_login_from_the_settings_lets_runs_file_documents_once_with_their_origin() {
	let harness = Harness::new(Models::default());
	let site = Arc::new(Site::default());
	let october = text_pdf(&["Payslip October 2026 net pay 300000 yen"]);
	site.add("2026-10", "October payslip", &october);
	let connectors = setup(&harness, &site, "monthly 26 09:00");
	assert!(harness.app.set_connectors(connectors.clone()).is_ok());
	let now = OffsetDateTime::now_utc();
	assert_eq!(view(&harness)["connectors"][0]["auth"], "needs_login");
	assert_eq!(connectors.next_run("pay", now).unwrap(), Some(now));

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
		let connectors = setup(&harness, &site, "every 6h");
		connectors::login(&connectors, "pay").await.unwrap();
		connectors::submit_code(&connectors, "pay", CODE)
			.await
			.unwrap();
	}
	assert_eq!(
		store(&harness).state("pay").unwrap().as_deref(),
		Some(json!({ "device": DEVICE }).to_string().as_str())
	);

	let connectors = setup(&harness, &site, "every 6h");
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
	let secret = std::env::var("PATH").unwrap();
	for hidden in [DEVICE, "device", secret.as_str(), "PATH", "password"] {
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
	trust(&harness);
	let now = OffsetDateTime::now_utc();
	let long_ago = now - time::Duration::days(40);
	store(&harness)
		.finish_run("pay", long_ago.unix_timestamp(), None, None)
		.unwrap();

	let connectors = setup(&harness, &site, "monthly 26 09:00");
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
	let restarted = setup(&harness, &site, "monthly 26 09:00");
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

#[test]
fn a_config_is_checked_against_its_kind() {
	let harness = Harness::new(Models::default());
	let site = Arc::new(Site::default());
	let path = harness.app.library.connectors_path();
	let parse = |kind: &str, options: &str| {
		Config::parse(&format!(
			r#"{{"connectors": [{{"name": "pay", "kind": "{kind}", "username_env": "PATH",
				"password_env": "PATH", "schedule": "every 6h", "options": {options}}}]}}"#
		))
		.unwrap()
	};
	let error = |config| {
		Connectors::new(config, &site.kinds(), &path)
			.err()
			.unwrap()
			.to_string()
	};
	assert_eq!(
		error(parse("bank", "{}")),
		"connector pay: unknown kind \"bank\" (known: payroll)"
	);
	assert_eq!(
		error(parse("payroll", r#"{"colour": 1}"#)),
		"connector pay: unknown option"
	);
	let builtin = Connectors::new(parse("payroll", "{}"), &connectors::kinds::builtin(), &path)
		.err()
		.unwrap()
		.to_string();
	assert!(builtin.contains("unknown kind \"payroll\""), "{builtin}");
	assert!(Connectors::new(parse("payroll", "{}"), &site.kinds(), &path).is_ok());
}

#[tokio::test]
async fn a_missing_credential_fails_the_run_and_the_login_by_name_only() {
	let harness = Harness::new(Models::default());
	let site = Arc::new(Site::default());
	let config = Config::parse(
		r#"{"connectors": [{"name": "pay", "kind": "payroll", "username_env": "PATH",
			"password_env": "VIOLET_TEST_NO_SUCH_VARIABLE", "schedule": "every 6h"}]}"#,
	)
	.unwrap();
	let connectors = Connectors::new(
		config,
		&site.kinds(),
		&harness.app.library.connectors_path(),
	)
	.unwrap();
	let error = connectors::run(&harness.app, &connectors, "pay")
		.await
		.unwrap_err()
		.to_string();
	assert_eq!(
		error,
		"the environment variable VIOLET_TEST_NO_SUCH_VARIABLE is not set"
	);
	assert!(matches!(
		connectors::login(&connectors, "pay").await,
		Err(Error::Conflict(message)) if message.ends_with("is not set")
	));
	assert!(
		connectors::run(&harness.app, &connectors, "bank")
			.await
			.is_err()
	);
	assert!(matches!(
		connectors::login(&connectors, "bank").await,
		Err(Error::NotFound(_))
	));
	assert_eq!(site.codes_sent(), 0);
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
	(status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn the_api_shows_connectors_signs_in_runs_and_refetches() {
	let off = Harness::new(Models::default());
	let (status, body) = call(&off, "GET", "/api/connectors", None).await;
	assert_eq!(status, StatusCode::OK);
	assert_eq!(
		body,
		json!({"enabled": false, "utc_offset": null, "connectors": [], "items": []})
	);
	for uri in ["/api/connectors/pay/run", "/api/connectors/pay/login"] {
		assert_eq!(call(&off, "POST", uri, None).await.0, StatusCode::NOT_FOUND);
	}

	let harness = Harness::new(Models::default());
	let site = Arc::new(Site::default());
	site.add(
		"2026-10",
		"October payslip",
		&text_pdf(&["Payslip October"]),
	);
	let connectors = setup(&harness, &site, "weekly mon 06:30");
	assert!(harness.app.set_connectors(connectors.clone()).is_ok());

	let (_, body) = call(&harness, "GET", "/api/connectors", None).await;
	assert_eq!(body["enabled"], true);
	assert_eq!(body["utc_offset"], "+09:00");
	let shown = &body["connectors"][0];
	assert_eq!(shown["name"], "pay");
	assert_eq!(shown["kind"], "payroll");
	assert_eq!(shown["schedule"], "weekly mon 06:30");
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
