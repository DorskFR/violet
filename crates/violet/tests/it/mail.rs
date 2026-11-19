use std::path::Path;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use time::{Date, Duration, OffsetDateTime};
use tokio::net::TcpStream;
use tower::ServiceExt;
use violet::Models;
use violet::auth::{Role, Scope, User};
use violet::connectors::Connectors;
use violet::mail::imap::{Criteria, Session};
use violet::mail::{self, Mailbox, Report, Security, mime};
use violet::pipeline::{self, Outcome};
use violet_core::{MailRecord, MailSource, MailStatus, Sidecar, hash};

use crate::common::{Harness, text_pdf};
use crate::mail_server::FakeImap;
use crate::users::{Api, As, household};

fn crlf(text: &str) -> Vec<u8> {
	text.replace("\r\n", "\n")
		.replace('\n', "\r\n")
		.into_bytes()
}

fn base64(bytes: &[u8]) -> String {
	const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
	let mut out = String::new();
	for chunk in bytes.chunks(3) {
		let n = chunk
			.iter()
			.enumerate()
			.fold(0u32, |n, (i, byte)| n | u32::from(*byte) << (16 - 8 * i));
		for i in 0..4 {
			if i <= chunk.len() {
				out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
			} else {
				out.push('=');
			}
		}
	}
	out.as_bytes()
		.chunks(76)
		.map(|line| String::from_utf8(line.to_vec()).unwrap())
		.collect::<Vec<_>>()
		.join("\n")
}

fn today() -> Date {
	OffsetDateTime::now_utc().date()
}

fn invoice_mail(message_id: &str, from: &str, pdf: &[u8]) -> Vec<u8> {
	crlf(&format!(
		"Message-ID: <{message_id}>
From: {from}
To: me@example.com
Subject: Your November invoice
Date: Sun, 01 Nov 2026 09:00:00 +0900
MIME-Version: 1.0
Content-Type: multipart/mixed; boundary=\"b1\"

--b1
Content-Type: text/plain; charset=utf-8

Please find your invoice attached.
--b1
Content-Type: application/pdf; name=\"invoice.pdf\"
Content-Disposition: attachment; filename=\"invoice.pdf\"
Content-Transfer-Encoding: base64

{}
--b1
Content-Type: text/calendar; name=\"meeting.ics\"
Content-Disposition: attachment; filename=\"meeting.ics\"

BEGIN:VCALENDAR
END:VCALENDAR
--b1--
",
		base64(pdf)
	))
}

fn html_mail(message_id: &str, from: &str) -> Vec<u8> {
	crlf(&format!(
		"Message-ID: <{message_id}>
From: {from}
Subject: Your order 123
Date: Mon, 02 Nov 2026 10:00:00 +0900
MIME-Version: 1.0
Content-Type: text/html; charset=utf-8

<html><body><p>Thank you for your order &amp; see you</p><p>Total 3,300 yen</p></body></html>
"
	))
}

fn rule(json: &str) -> violet::mail::Rule {
	serde_json::from_str(json).unwrap()
}

#[test]
fn a_pdf_attachment_is_taken_and_other_kinds_are_left() {
	let pdf = text_pdf(&["Invoice"]);
	let parsed = mime::pick(
		&invoice_mail("a@b", "Tokyo Gas <billing@gas.example>", &pdf),
		&rule("{}"),
	);
	assert_eq!(parsed.message_id.as_deref(), Some("a@b"));
	assert_eq!(
		parsed.from.as_deref(),
		Some("Tokyo Gas <billing@gas.example>")
	);
	assert_eq!(parsed.subject.as_deref(), Some("Your November invoice"));
	assert_eq!(parsed.date.as_deref(), Some("2026-11-01T09:00:00+09:00"));
	assert_eq!(parsed.files.len(), 1);
	assert_eq!(parsed.files[0].name, "invoice.pdf");
	assert_eq!(parsed.files[0].bytes, pdf);
	assert_eq!(
		parsed.left_out,
		["meeting.ics: not a kind of file violet reads"]
	);

	let excluded = mime::pick(
		&invoice_mail("a@b", "x@y", &pdf),
		&rule(r#"{"filename_exclude": ["INVOICE*"]}"#),
	);
	assert!(excluded.files.is_empty());
}

#[test]
fn inline_images_are_skipped_unless_asked_and_never_when_small() {
	let small = vec![7u8; 1_000];
	let large = vec![9u8; 30_000];
	let raw = crlf(&format!(
		"Message-ID: <inline@x>
From: shop@example.com
Subject: Receipt
MIME-Version: 1.0
Content-Type: multipart/mixed; boundary=\"outer\"

--outer
Content-Type: multipart/related; boundary=\"rel\"

--rel
Content-Type: text/html

<p>Hello <img src=\"cid:logo\"> <img src=\"cid:chart\"></p>
--rel
Content-Type: image/png; name=\"logo.png\"
Content-Disposition: inline; filename=\"logo.png\"
Content-ID: <logo>
Content-Transfer-Encoding: base64

{}
--rel
Content-Type: image/png; name=\"chart.png\"
Content-Disposition: inline; filename=\"chart.png\"
Content-ID: <chart>
Content-Transfer-Encoding: base64

{}
--rel--
--outer
Content-Type: application/pdf
Content-Disposition: attachment; filename=\"receipt.pdf\"
Content-Transfer-Encoding: base64

{}
--outer--
",
		base64(&small),
		base64(&large),
		base64(b"%PDF-1.4 receipt")
	));
	let names = |parsed: &mime::Parsed| -> Vec<String> {
		parsed.files.iter().map(|file| file.name.clone()).collect()
	};
	let default = mime::pick(&raw, &rule("{}"));
	assert_eq!(names(&default), ["receipt.pdf"]);
	assert!(
		default
			.left_out
			.iter()
			.any(|reason| reason == "logo.png: inline")
	);
	let inline = mime::pick(&raw, &rule(r#"{"include_inline": true}"#));
	assert_eq!(names(&inline), ["chart.png", "receipt.pdf"]);
	assert_eq!(inline.files[0].bytes, large);
	assert!(
		inline
			.left_out
			.iter()
			.any(|reason| reason == "logo.png: small inline image")
	);
}

#[test]
fn encoded_file_names_are_decoded() {
	let raw = crlf(
		"From: =?UTF-8?B?44GU5rOo5paH44GC44KK44GM44Go44GG44GU44GW44GE44G+44GZ?= <shop@example.jp>
Subject: =?UTF-8?Q?=E8=AB=8B=E6=B1=82=E6=9B=B8?=
MIME-Version: 1.0
Content-Type: multipart/mixed; boundary=\"b\"

--b
Content-Type: application/pdf
Content-Disposition: attachment; filename=\"=?UTF-8?B?6KuL5rGC5pu4LnBkZg==?=\"

%PDF-1.4 a
--b
Content-Type: application/pdf
Content-Disposition: attachment; filename*=UTF-8''%E9%A0%98%E5%8F%8E%E6%9B%B8.pdf

%PDF-1.4 b
--b
Content-Type: application/pdf
Content-Disposition: attachment; filename=\"../../etc/passwd\"

%PDF-1.4 c
--b
Content-Type: application/pdf
Content-Disposition: attachment

%PDF-1.4 d
--b--
",
	);
	let parsed = mime::pick(&raw, &rule("{}"));
	let names: Vec<&str> = parsed.files.iter().map(|file| file.name.as_str()).collect();
	assert_eq!(
		names,
		["請求書.pdf", "領収書.pdf", "passwd.pdf", "attachment.pdf"]
	);
	assert_eq!(parsed.subject.as_deref(), Some("請求書"));
	assert_eq!(
		parsed.from.as_deref(),
		Some("ご注文ありがとうございます <shop@example.jp>")
	);
}

#[test]
fn an_html_only_mail_is_taken_whole_with_the_eml_scope() {
	let raw = html_mail("h@x", "shop@example.com");
	let attachments = mime::pick(&raw, &rule("{}"));
	assert!(attachments.files.is_empty());
	assert!(attachments.left_out.is_empty());
	let whole = mime::pick(&raw, &rule(r#"{"scope": "eml"}"#));
	assert_eq!(whole.files.len(), 1);
	assert_eq!(whole.files[0].name, "Your order 123.eml");
	assert_eq!(whole.files[0].bytes, raw);
	let both = mime::pick(
		&invoice_mail("i@x", "a@b", b"%PDF"),
		&rule(r#"{"scope": "both"}"#),
	);
	let names: Vec<&str> = both.files.iter().map(|file| file.name.as_str()).collect();
	assert_eq!(names, ["Your November invoice.eml", "invoice.pdf"]);
}

async fn session(fake: &FakeImap, username: &str, password: &str) -> Session {
	let stream = TcpStream::connect(("127.0.0.1", fake.port)).await.unwrap();
	let mut session = Session::start(Box::new(stream)).await.unwrap();
	session.login(username, password).await.unwrap();
	session
}

#[tokio::test]
async fn the_client_searches_fetches_stores_and_moves() {
	let password = "pässwörd \"x\"";
	let fake = FakeImap::start("me@example.com", password, &["MOVE", "UIDPLUS"]).await;
	fake.add_folder("Archive");
	let tricky = crlf(
		"Message-ID: <t@x>\nFrom: Billing <billing@example.com>\nSubject: tricky\n\nv2 OK fake\n)\n{5}\n* 1 FETCH (\n",
	);
	let a = fake.add("INBOX", today(), &tricky);
	fake.add(
		"INBOX",
		today() - Duration::days(40),
		&crlf("From: billing@example.com\n\nold\n"),
	);
	let c = fake.add(
		"INBOX",
		today(),
		&crlf("From: news@other.example\n\nnews\n"),
	);

	let mut session = session(&fake, "me@example.com", password).await;
	assert_eq!(fake.state.lock().unwrap().literals, 1);
	assert!(
		fake.commands()
			.contains(&"LOGIN me@example.com ***".to_string())
	);
	assert!(session.has("MOVE") && session.has("UIDPLUS"));
	assert_eq!(session.select("INBOX").await.unwrap(), 1000);

	let since = Some(today() - Duration::days(30));
	let billing = Criteria {
		since,
		from: Some("BILLING".into()),
		..Criteria::default()
	};
	assert_eq!(session.search(&billing).await.unwrap(), [a]);
	let recent = Criteria {
		since,
		..Criteria::default()
	};
	assert_eq!(session.search(&recent).await.unwrap(), [a, c]);

	let metas = session.fetch_meta(&[a, c]).await.unwrap();
	assert_eq!(metas.len(), 2);
	assert_eq!(metas[0].uid, a);
	assert_eq!(metas[0].size, tricky.len() as u64);
	let received = metas[0].received.as_deref().unwrap();
	assert!(received.ends_with("T09:30:00+09:00"), "{received}");
	assert_eq!(session.fetch(a).await.unwrap(), tricky);
	assert!(fake.count("UID FETCH") == 2);
	assert!(
		fake.commands()
			.iter()
			.any(|command| command.contains("BODY.PEEK[]"))
	);

	session.store(a, "violet-ingested").await.unwrap();
	let unmarked = Criteria {
		since,
		without_keyword: Some("violet-ingested".into()),
		..Criteria::default()
	};
	assert_eq!(session.search(&unmarked).await.unwrap(), [c]);

	session.move_to(a, "Archive").await.unwrap();
	assert_eq!(fake.flags("INBOX").len(), 2);
	assert_eq!(
		fake.flags("Archive"),
		[(1, vec!["violet-ingested".to_string()])]
	);
	assert!(session.select("Nowhere").await.is_err());
	session.logout().await;
	assert_eq!(fake.count("UID MOVE"), 1);
	assert_eq!(fake.count("EXPUNGE"), 0);
	assert_eq!(fake.count("LOGOUT"), 1);
}

#[tokio::test]
async fn without_move_a_mail_is_copied_and_never_expunged_with_others() {
	for (capabilities, expunged) in [(&[][..], false), (&["UIDPLUS"][..], true)] {
		let fake = FakeImap::start("me", "pw", capabilities).await;
		fake.add_folder("Done");
		let a = fake.add("INBOX", today(), &crlf("From: a@x\n\na\n"));
		let b = fake.add("INBOX", today(), &crlf("From: b@x\n\nb\n"));
		let mut session = session(&fake, "me", "pw").await;
		session.select("INBOX").await.unwrap();
		session.store(b, "\\Deleted").await.unwrap();
		session.move_to(a, "Done").await.unwrap();
		session.logout().await;

		assert_eq!(fake.flags("Done").len(), 1);
		assert_eq!(fake.count("UID COPY"), 1);
		assert_eq!(fake.count("UID MOVE"), 0);
		assert_eq!(fake.count("EXPUNGE"), 0);
		assert_eq!(fake.count("UID EXPUNGE"), usize::from(expunged));
		let left: Vec<u32> = fake.flags("INBOX").iter().map(|(uid, _)| *uid).collect();
		if expunged {
			assert_eq!(left, [b]);
		} else {
			assert_eq!(left, [a, b]);
		}
	}
}

#[tokio::test]
async fn a_wrong_password_is_refused() {
	let fake = FakeImap::start("me", "right", &[]).await;
	let stream = TcpStream::connect(("127.0.0.1", fake.port)).await.unwrap();
	let mut session = Session::start(Box::new(stream)).await.unwrap();
	let error = session.login("me", "wrong").await.unwrap_err().to_string();
	assert!(error.contains("login refused"), "{error}");
}

const IMAP_USER: &str = "me";
const IMAP_PASSWORD: &str = "imap-pass-0451";

fn mailbox(port: u16, rule: &str) -> Mailbox {
	Mailbox {
		name: "home".into(),
		host: "127.0.0.1".into(),
		port,
		security: Security::Plain,
		username: IMAP_USER.into(),
		password: IMAP_PASSWORD.into(),
		owner: None,
		area: None,
		tags: Vec::new(),
		rules: vec![self::rule(rule)],
		siblings: vec!["home".into()],
	}
}

async fn sync(harness: &Harness, mailbox: &Mailbox) -> Report {
	mail::sync(&harness.app, mailbox, &|| true).await.unwrap()
}

async fn server() -> FakeImap {
	FakeImap::start(IMAP_USER, IMAP_PASSWORD, &["MOVE"]).await
}

fn filed(harness: &Harness, name: &str) -> impl std::future::Future<Output = Sidecar> {
	let app = harness.app.clone();
	let name = name.to_string();
	async move {
		match pipeline::ingest(&app, Path::new(&name)).await.unwrap() {
			Outcome::Filed { documents, .. } => {
				assert_eq!(documents.len(), 1);
				app.library
					.sidecars()
					.unwrap()
					.into_iter()
					.find(|(_, sidecar)| sidecar.id == documents[0].0)
					.unwrap()
					.1
			}
			other => panic!("not filed: {other:?}"),
		}
	}
}

#[tokio::test]
async fn synced_attachments_are_filed_with_where_they_came_from() {
	let harness = Harness::new(Models::default());
	let fake = server().await;
	let pdf = text_pdf(&["Tokyo Gas invoice 2026-11-01 total 4200 yen"]);
	let invoice = fake.add(
		"INBOX",
		today(),
		&invoice_mail("inv-1@gas.example", "Tokyo Gas <billing@gas.example>", &pdf),
	);
	fake.add(
		"INBOX",
		today(),
		&html_mail("html-1@gas.example", "billing@gas.example"),
	);
	fake.add(
		"INBOX",
		today(),
		&invoice_mail("news-1@other.example", "news@other.example", &pdf),
	);
	let mut home = mailbox(fake.port, r#"{"from": "billing@gas"}"#);
	home.area = Some("Tax".into());
	home.tags = vec!["mail".into()];

	let report = sync(&harness, &home).await;
	assert_eq!(
		report,
		Report {
			mails: 2,
			ingested: 1,
			files: 1,
			skipped: 1,
			failed: 0,
			errors: Vec::new(),
		}
	);
	assert_eq!(harness.inbox(), ["invoice.pdf"]);
	let sidecar = filed(&harness, "invoice.pdf").await;
	assert_eq!(sidecar.area, "Tax");
	assert!(sidecar.tags.contains(&"mail".to_string()));
	assert_eq!(
		sidecar.sources[0].mail,
		Some(MailSource {
			account: "home".into(),
			folder: "INBOX".into(),
			uidvalidity: 1000,
			uid: invoice,
			message_id: Some("inv-1@gas.example".into()),
			from: Some("Tokyo Gas <billing@gas.example>".into()),
			subject: Some("Your November invoice".into()),
			date: Some("2026-11-01T09:00:00+09:00".into()),
		})
	);
	let flags = fake.flags("INBOX");
	assert_eq!(flags[0].1, ["violet-ingested"]);
	assert!(flags[1].1.is_empty() && flags[2].1.is_empty());

	let fetches = fake.count("UID FETCH");
	let again = sync(&harness, &home).await;
	assert_eq!(again, Report::default());
	assert_eq!(fake.count("UID FETCH"), fetches);

	fake.renumber("INBOX");
	let renumbered = sync(&harness, &home).await;
	assert_eq!(
		(renumbered.mails, renumbered.ingested, renumbered.skipped),
		(2, 0, 2)
	);
	assert!(harness.inbox().is_empty());
	assert_eq!(harness.documents().len(), 1);

	let view = serde_json::to_value(mail::view(&harness.app, &User::implicit()).unwrap()).unwrap();
	assert_eq!(view["processed"].as_array().unwrap().len(), 4);
	assert_eq!(fake.count("EXPUNGE"), 0);
}

#[tokio::test]
async fn an_html_receipt_is_filed_as_its_mail() {
	let harness = Harness::new(Models::default());
	let fake = server().await;
	fake.add(
		"INBOX",
		today(),
		&html_mail("order-123@shop.example", "shop@example.com"),
	);
	fake.add_folder("Receipts");
	let home = mailbox(
		fake.port,
		r#"{"subject": "order", "scope": "eml", "action": "move:Receipts"}"#,
	);
	let report = sync(&harness, &home).await;
	assert_eq!((report.ingested, report.files), (1, 1));
	assert_eq!(harness.inbox(), ["Your order 123.eml"]);
	let sidecar = filed(&harness, "Your order 123.eml").await;
	let text = &sidecar.pages[0].text;
	assert!(
		text.contains("Thank you for your order & see you"),
		"{text}"
	);
	assert_eq!(sidecar.sources[0].original_name, "Your order 123.eml");
	let source = sidecar.sources[0].mail.as_ref().unwrap();
	assert_eq!(source.message_id.as_deref(), Some("order-123@shop.example"));
	assert!(fake.flags("INBOX").is_empty());
	assert_eq!(fake.flags("Receipts").len(), 1);
}

#[tokio::test]
async fn a_sync_stops_taking_mails_once_its_mailbox_changed() {
	let harness = Harness::new(Models::default());
	let fake = server().await;
	fake.add("INBOX", today(), &html_mail("h@x", "shop@example.com"));
	let home = mailbox(fake.port, "{}");
	let report = mail::sync(&harness.app, &home, &|| false).await.unwrap();
	assert_eq!(report.mails, 0);
	assert!(report.errors[0].contains("changed meanwhile"), "{report:?}");
	let view = mail::view(&harness.app, &User::implicit()).unwrap();
	assert!(view.processed.is_empty());
}

#[tokio::test]
async fn tls_and_starttls_fail_cleanly_against_a_plaintext_server() {
	let fake = FakeImap::start(IMAP_USER, IMAP_PASSWORD, &[]).await;
	for (security, expected) in [
		(Security::Tls, "TLS handshake"),
		(Security::Starttls, "BAD"),
	] {
		let mut home = mailbox(fake.port, "{}");
		home.security = security;
		let error =
			tokio::time::timeout(std::time::Duration::from_secs(10), Session::connect(&home))
				.await
				.unwrap()
				.err()
				.unwrap();
		let error = format!("{error:#}");
		assert!(error.contains(expected), "{security:?}: {error}");
	}
	assert_eq!(fake.count("LOGIN"), 0);
}

fn imap(name: &str, port: u16) -> Value {
	json!({
		"name": name, "kind": "imap", "username": IMAP_USER, "password": IMAP_PASSWORD,
		"schedule": "every 10m",
		"options": {"host": "127.0.0.1", "port": port, "security": "plain"},
	})
}

fn connectors(harness: &Harness) -> Arc<Connectors> {
	let connectors = Connectors::open(
		violet::connectors::kinds::builtin(),
		&harness.app.library.connectors_path(),
	)
	.unwrap();
	assert!(harness.app.set_connectors(connectors.clone()).is_ok());
	connectors
}

fn no_password(body: &[u8]) {
	let text = String::from_utf8_lossy(body);
	assert!(!text.contains(IMAP_PASSWORD), "the password is in {text}");
}

async fn call(harness: &Harness, method: &str, uri: &str, body: Value) -> (StatusCode, Value) {
	let request = Request::builder()
		.method(method)
		.uri(uri)
		.header("content-type", "application/json")
		.body(Body::from(body.to_string()))
		.unwrap();
	let response = violet::http::router(harness.app.clone())
		.oneshot(request)
		.await
		.unwrap();
	let status = response.status();
	let bytes = response.into_body().collect().await.unwrap().to_bytes();
	no_password(&bytes);
	(status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn a_mailbox_is_added_as_a_connector_and_its_runs_are_syncs() {
	let harness = Harness::new(Models::default());
	let (status, body) = call(&harness, "GET", "/api/mail", Value::Null).await;
	assert_eq!((status, body), (StatusCode::OK, json!({"processed": []})));
	let (status, _) = call(&harness, "POST", "/api/mail/home/sync", Value::Null).await;
	assert_eq!(status, StatusCode::NOT_FOUND);

	let connectors = connectors(&harness);
	let fake = server().await;
	let pdf = text_pdf(&["Tokyo Gas invoice 2026-11-01 total 4200 yen"]);
	fake.add(
		"INBOX",
		today(),
		&invoice_mail("inv-1@gas.example", "billing@gas.example", &pdf),
	);
	fake.add("INBOX", today(), &html_mail("h@x", "shop@example.com"));

	let with = |change: fn(&mut Value)| {
		let mut body = imap("home", 143);
		change(&mut body);
		body
	};
	for (body, expected) in [
		(
			with(|body| body["options"] = json!({})),
			"missing field `host`",
		),
		(
			with(|body| body["options"]["host"] = json!("")),
			"enter the host",
		),
		(
			with(|body| body["options"]["host"] = json!("imap.example.com")),
			"only allowed for a loopback host",
		),
		(
			with(|body| body["options"]["port"] = json!(0)),
			"between 1 and 65535",
		),
		(
			with(|body| body["options"]["security"] = json!("ssl")),
			"unknown variant",
		),
		(
			with(|body| body["options"]["password_env"] = json!("P")),
			"unknown field",
		),
		(
			with(|body| body["options"]["rules"] = json!([])),
			"at least one rule",
		),
		(
			with(|body| body["options"]["rules"] = json!([{}, {"max_age_days": 0}])),
			"rule 2: max_age_days must be at least 1",
		),
		(
			with(|body| body["options"]["rules"] = json!([{"action": "delete"}])),
			"unknown action",
		),
		(
			with(|body| body["options"]["rules"] = json!([{"hints": {"area": "Tax"}}])),
			"unknown field",
		),
		(
			with(|body| body["password"] = json!("")),
			"enter the password",
		),
		(
			with(|body| body["schedule"] = json!("often")),
			"unknown schedule",
		),
	] {
		let (status, answer) = call(&harness, "POST", "/api/connectors", body.clone()).await;
		assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
		let error = answer["error"].as_str().unwrap();
		assert!(error.contains(expected), "{expected}: {error}");
	}

	let mut home = imap("home", fake.port);
	home["area"] = json!("Tax");
	home["tags"] = json!(["mail"]);
	home["options"]["rules"] = json!([{"filename_include": ["*.pdf"]}]);
	let (status, added) = call(&harness, "POST", "/api/connectors", home.clone()).await;
	assert_eq!(status, StatusCode::CREATED, "{added}");
	assert_eq!(added["kind"], "imap");
	assert_eq!(added["auth"], "ready");
	assert_eq!(added["password_set"], true);
	assert_eq!(added["schedule"], "every 10m");
	assert_eq!(added["options"], home["options"]);
	assert!(added["next_run"].is_string(), "{added}");
	let (status, body) = call(&harness, "POST", "/api/connectors/home/login", Value::Null).await;
	assert_eq!(status, StatusCode::BAD_REQUEST);
	assert!(
		body["error"]
			.as_str()
			.unwrap()
			.contains("signs in by itself")
	);

	let report = violet::connectors::run(&harness.app, &connectors, "home")
		.await
		.unwrap();
	assert_eq!(
		(
			report.listed,
			report.ingested,
			report.skipped,
			report.failed
		),
		(2, 1, 1, 0)
	);
	assert_eq!(harness.inbox(), ["invoice.pdf"]);
	let sidecar = filed(&harness, "invoice.pdf").await;
	assert_eq!(sidecar.area, "Tax");
	assert!(sidecar.tags.contains(&"mail".to_string()));
	let source = sidecar.sources[0].mail.as_ref().unwrap();
	assert_eq!(
		(source.account.as_str(), source.folder.as_str()),
		("home", "INBOX")
	);
	assert_eq!(fake.flags("INBOX")[0].1, ["violet-ingested"]);

	let (status, listed) = call(&harness, "GET", "/api/connectors", Value::Null).await;
	assert_eq!(status, StatusCode::OK);
	let shown = &listed["connectors"][0];
	assert_eq!(
		shown["counts"],
		json!({"ingested": 1, "skipped": 1, "failed": 0})
	);
	assert_eq!(shown["last_error"], Value::Null);
	assert_eq!(shown["last_report"]["ingested"], 1);
	assert!(shown["last_success"].is_string());
	assert_eq!(listed["items"], json!([]));

	let (status, body) = call(&harness, "GET", "/api/mail", Value::Null).await;
	assert_eq!(status, StatusCode::OK);
	let processed = body["processed"].as_array().unwrap();
	assert_eq!(processed.len(), 2);
	let skipped = processed
		.iter()
		.find(|mail| mail["status"] == "skipped")
		.unwrap();
	assert_eq!(skipped["detail"], "no attachment");
	assert_eq!(skipped["account"], "home");
	let id = skipped["id"].as_i64().unwrap();

	let reprocess = format!("/api/mail/processed/{id}/reprocess");
	let (status, body) = call(&harness, "POST", &reprocess, Value::Null).await;
	assert_eq!((status, &body["id"]), (StatusCode::OK, &json!(id)));
	let (status, _) = call(&harness, "POST", &reprocess, Value::Null).await;
	assert_eq!(status, StatusCode::NOT_FOUND);
	let (status, _) = call(
		&harness,
		"POST",
		"/api/mail/processed/x/reprocess",
		Value::Null,
	)
	.await;
	assert_eq!(status, StatusCode::BAD_REQUEST);
	let again = violet::connectors::run(&harness.app, &connectors, "home")
		.await
		.unwrap();
	assert_eq!((again.listed, again.ingested, again.skipped), (1, 0, 1));

	let mut edit = home.clone();
	edit.as_object_mut().unwrap().remove("name");
	edit["password"] = json!("not-the-password");
	let (status, edited) = call(&harness, "PUT", "/api/connectors/home", edit).await;
	assert_eq!(status, StatusCode::OK, "{edited}");
	assert_eq!(edited["auth"], "ready");
	assert!(edited["next_run"].is_string(), "{edited}");
	let error = violet::connectors::run(&harness.app, &connectors, "home")
		.await
		.unwrap_err();
	assert!(format!("{error:#}").contains("login refused"), "{error:#}");
	let (_, listed) = call(&harness, "GET", "/api/connectors", Value::Null).await;
	let shown = &listed["connectors"][0];
	let last_error = shown["last_error"].as_str().unwrap();
	assert!(last_error.contains("login refused"), "{last_error}");
	assert!(!listed.to_string().contains("not-the-password"));
	assert_eq!(shown["auth"], "ready");
	assert!(
		shown["next_run"].is_string(),
		"a failed run is retried: {shown}"
	);
}

#[tokio::test]
async fn a_connector_named_like_an_old_account_takes_none_of_its_mail_again() {
	let harness = Harness::new(Models::default());
	let fake = server().await;
	let pdf = text_pdf(&["Tokyo Gas invoice 2026-11-01 total 4200 yen"]);
	let old = fake.add(
		"INBOX",
		today(),
		&invoice_mail("inv-1@gas.example", "billing@gas.example", &pdf),
	);
	harness
		.app
		.index()
		.mail_record(
			&MailRecord {
				account: "dosdane".into(),
				folder: "INBOX".into(),
				uidvalidity: 1000,
				uid: old,
				message_id: Some("inv-1@gas.example".into()),
				sender: Some("billing@gas.example".into()),
				subject: Some("Your November invoice".into()),
				received: None,
				status: MailStatus::Ingested,
				files: 1,
				detail: None,
				error: None,
			},
			&[],
		)
		.unwrap();
	assert!(fake.flags("INBOX")[0].1.is_empty());

	let connectors = connectors(&harness);
	let dosdane = imap("dosdane", fake.port);
	let (status, added) = call(&harness, "POST", "/api/connectors", dosdane).await;
	assert_eq!(status, StatusCode::CREATED, "{added}");
	assert_eq!(added["counts"]["ingested"], 1);

	let report = violet::connectors::run(&harness.app, &connectors, "dosdane")
		.await
		.unwrap();
	assert_eq!(report, violet::connectors::Report::default());
	assert_eq!(fake.count("UID FETCH"), 0);
	assert!(harness.inbox().is_empty());
	let (_, body) = call(&harness, "GET", "/api/mail", Value::Null).await;
	assert_eq!(body["processed"].as_array().unwrap().len(), 1);

	let other = text_pdf(&["Tokyo Gas invoice 2026-12-01 total 3900 yen"]);
	fake.add_folder("Archive");
	fake.add(
		"Archive",
		today(),
		&invoice_mail("inv-1@gas.example", "billing@gas.example", &pdf),
	);
	fake.add(
		"INBOX",
		today(),
		&invoice_mail("inv-2@gas.example", "billing@gas.example", &other),
	);
	let mut both = imap("dosdane", fake.port);
	both.as_object_mut().unwrap().remove("name");
	both["password"] = Value::Null;
	both["options"]["rules"] = json!([{}, {"folder": "Archive"}]);
	let (status, _) = call(&harness, "PUT", "/api/connectors/dosdane", both).await;
	assert_eq!(status, StatusCode::OK);
	let report = violet::connectors::run(&harness.app, &connectors, "dosdane")
		.await
		.unwrap();
	assert_eq!(
		(report.listed, report.ingested, report.skipped),
		(2, 1, 1),
		"the copy in another folder is known by its Message-ID"
	);
	assert_eq!(harness.inbox(), ["invoice.pdf"]);
	let (_, body) = call(&harness, "GET", "/api/mail", Value::Null).await;
	assert!(
		body["processed"]
			.as_array()
			.unwrap()
			.iter()
			.any(|mail| mail["detail"] == "already taken from another folder")
	);
}

#[tokio::test]
async fn members_see_and_touch_only_their_own_mailboxes_and_mails() {
	let home = household(None).await;
	let api: &Api = &home.api;
	api.add_user("kim", Role::Member);
	let kim = api.token("kim", "phone", Scope::Write);
	let (alex, sam, kim) = (As::Token(&home.alex), As::Token(&home.sam), As::Token(&kim));
	let reader = As::Token(&home.sam_reader);
	let connectors = connectors(&api.harness);
	let app = &api.harness.app;
	let pdf = text_pdf(&["Tokyo Gas invoice 2026-11-01 total 4200 yen"]);
	let (sams, alexs) = (server().await, server().await);
	for fake in [&sams, &alexs] {
		fake.add(
			"INBOX",
			today(),
			&invoice_mail("inv-1@gas.example", "billing@gas.example", &pdf),
		);
	}
	alexs.add(
		"INBOX",
		today(),
		&html_mail("private@x", "clinic@example.com"),
	);

	let call = async |method: &str, uri: &str, body: Value, who: As<'_>| {
		let answer = api.call(method, uri, body, who).await;
		no_password(&answer.body);
		(answer.status, answer.json())
	};
	let mails = async |who: As<'_>| {
		let (status, body) = call("GET", "/api/mail", Value::Null, who).await;
		assert_eq!(status, StatusCode::OK);
		body["processed"].as_array().unwrap().clone()
	};
	let names = async |who: As<'_>| {
		let (_, body) = call("GET", "/api/connectors", Value::Null, who).await;
		body["connectors"]
			.as_array()
			.unwrap()
			.iter()
			.map(|connector| connector["name"].as_str().unwrap().to_string())
			.collect::<Vec<_>>()
	};
	let owned = |name: &str, port: u16, owner: &str| {
		let mut body = imap(name, port);
		body["owner"] = json!(owner);
		body
	};

	let (status, _) = call("GET", "/api/mail", Value::Null, reader).await;
	assert_eq!(status, StatusCode::FORBIDDEN);

	let body = owned("sam-mail", sams.port, "alex");
	let (status, refused) = call("POST", "/api/connectors", body, sam).await;
	assert_eq!(status, StatusCode::FORBIDDEN, "{refused}");
	let body = owned("sam-mail", sams.port, "sam");
	let (status, added) = call("POST", "/api/connectors", body, sam).await;
	assert_eq!(
		(status, &added["owner"]),
		(StatusCode::CREATED, &json!("sam"))
	);
	let body = owned("alex-mail", alexs.port, "nobody");
	let (status, refused) = call("POST", "/api/connectors", body, alex).await;
	assert_eq!(status, StatusCode::BAD_REQUEST);
	assert_eq!(refused["error"], "there is no user named nobody");
	let body = imap("alex-mail", alexs.port);
	let (status, added) = call("POST", "/api/connectors", body, alex).await;
	assert_eq!(
		(status, &added["owner"]),
		(StatusCode::CREATED, &json!("alex"))
	);
	let body = owned("kim-mail", sams.port, "kim");
	let (status, added) = call("POST", "/api/connectors", body, alex).await;
	assert_eq!(
		(status, &added["owner"]),
		(StatusCode::CREATED, &json!("kim"))
	);

	assert_eq!(names(alex).await, ["sam-mail", "alex-mail", "kim-mail"]);
	assert_eq!(names(sam).await, ["sam-mail"]);
	assert_eq!(names(kim).await, ["kim-mail"]);

	for name in ["sam-mail", "alex-mail"] {
		violet::connectors::run(app, &connectors, name)
			.await
			.unwrap();
	}
	let sha = hash::sha256(&pdf);
	assert_eq!(app.index().uploader(&sha).unwrap().as_deref(), Some("alex"));
	let all = mails(alex).await;
	assert_eq!(all.len(), 3);
	let sams_mails = mails(sam).await;
	assert_eq!(sams_mails.len(), 1);
	assert_eq!(sams_mails[0]["account"], "sam-mail");
	assert_eq!(sams_mails[0]["status"], "ingested");
	assert!(mails(kim).await.is_empty());
	let theirs: Vec<&Value> = all
		.iter()
		.filter(|mail| mail["account"] == "alex-mail")
		.collect();
	assert_eq!(theirs.len(), 2);
	assert!(
		theirs
			.iter()
			.all(|mail| mail["detail"] != "already taken from another folder"),
		"a mail somebody else took is still taken: {theirs:?}"
	);

	let mut edit = imap("alex-mail", alexs.port);
	edit.as_object_mut().unwrap().remove("name");
	for (method, path, body) in [
		("PUT", "/api/connectors/alex-mail", edit.clone()),
		("DELETE", "/api/connectors/alex-mail", Value::Null),
		("POST", "/api/connectors/alex-mail/run", Value::Null),
		("POST", "/api/connectors/alex-mail/login", Value::Null),
	] {
		let (status, _) = call(method, path, body, sam).await;
		assert_eq!(status, StatusCode::NOT_FOUND, "{method} {path}");
	}
	let reprocess = |mail: &Value| format!("/api/mail/processed/{}/reprocess", mail["id"]);
	for who in [sam, kim] {
		let (status, _) = call("POST", &reprocess(theirs[0]), Value::Null, who).await;
		assert_eq!(status, StatusCode::NOT_FOUND);
	}
	let (status, _) = call("POST", &reprocess(&sams_mails[0]), Value::Null, reader).await;
	assert_eq!(status, StatusCode::FORBIDDEN);
	assert_eq!(mails(alex).await.len(), 3);
	let (status, _) = call("POST", &reprocess(&sams_mails[0]), Value::Null, sam).await;
	assert_eq!(status, StatusCode::OK);
	assert!(mails(sam).await.is_empty());

	let mut sams_edit = imap("sam-mail", sams.port);
	sams_edit.as_object_mut().unwrap().remove("name");
	sams_edit["password"] = Value::Null;
	sams_edit["owner"] = json!("kim");
	let (status, _) = call("PUT", "/api/connectors/sam-mail", sams_edit.clone(), sam).await;
	assert_eq!(status, StatusCode::FORBIDDEN);
	sams_edit["owner"] = json!("sam");
	let (status, kept) = call("PUT", "/api/connectors/sam-mail", sams_edit.clone(), sam).await;
	assert_eq!((status, &kept["owner"]), (StatusCode::OK, &json!("sam")));
	sams_edit["owner"] = json!("nobody");
	let (status, _) = call("PUT", "/api/connectors/sam-mail", sams_edit.clone(), alex).await;
	assert_eq!(status, StatusCode::BAD_REQUEST);

	edit["password"] = Value::Null;
	edit["owner"] = json!("kim");
	let (status, given) = call("PUT", "/api/connectors/alex-mail", edit.clone(), alex).await;
	assert_eq!((status, &given["owner"]), (StatusCode::OK, &json!("kim")));
	assert_eq!(names(kim).await, ["alex-mail", "kim-mail"]);
	assert_eq!(mails(kim).await.len(), 2);
	edit.as_object_mut().unwrap().remove("owner");
	let (status, kept) = call("PUT", "/api/connectors/alex-mail", edit, alex).await;
	assert_eq!((status, &kept["owner"]), (StatusCode::OK, &json!("kim")));

	let (status, _) = call("DELETE", "/api/connectors/alex-mail", Value::Null, kim).await;
	assert_eq!(status, StatusCode::OK);
	assert!(mails(kim).await.is_empty());
	assert_eq!(mails(alex).await.len(), 2);
	let body = imap("alex-mail", alexs.port);
	let (status, added) = call("POST", "/api/connectors", body, sam).await;
	assert_eq!(status, StatusCode::CREATED, "{added}");
	assert_eq!(
		added["counts"],
		json!({"ingested": 0, "skipped": 0, "failed": 0})
	);
	assert!(mails(sam).await.is_empty());
	assert!(mails(alex).await.is_empty());

	app.index()
		.mail_record(
			&MailRecord {
				account: "dosdane".into(),
				folder: "INBOX".into(),
				uidvalidity: 1000,
				uid: 1,
				message_id: None,
				sender: Some("clinic@example.com".into()),
				subject: Some("Your results".into()),
				received: None,
				status: MailStatus::Ingested,
				files: 1,
				detail: None,
				error: None,
			},
			&[],
		)
		.unwrap();
	let (status, refused) = call("POST", "/api/connectors", imap("dosdane", sams.port), sam).await;
	assert_eq!(status, StatusCode::CONFLICT, "{refused}");
	let body = owned("dosdane", sams.port, "sam");
	let (status, added) = call("POST", "/api/connectors", body, alex).await;
	assert_eq!(status, StatusCode::CREATED, "{added}");
	assert_eq!(mails(sam).await[0]["subject"], "Your results");
}
