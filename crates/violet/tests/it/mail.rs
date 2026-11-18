use std::path::Path;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::Value;
use time::{Date, Duration, OffsetDateTime};
use tokio::net::TcpStream;
use tower::ServiceExt;
use violet::Models;
use violet::mail::imap::{Criteria, Session};
use violet::mail::{self, Config, Mail, Report, mime};
use violet::pipeline::{self, Outcome};
use violet_core::{MailSource, Sidecar};

use crate::common::{Harness, text_pdf};
use crate::mail_server::FakeImap;

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
		&rule(r#"{"account": "home"}"#),
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
		&rule(r#"{"account": "home", "filename_exclude": ["INVOICE*"]}"#),
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
	let default = mime::pick(&raw, &rule(r#"{"account": "home"}"#));
	assert_eq!(names(&default), ["receipt.pdf"]);
	assert!(
		default
			.left_out
			.iter()
			.any(|reason| reason == "logo.png: inline")
	);
	let inline = mime::pick(
		&raw,
		&rule(r#"{"account": "home", "include_inline": true}"#),
	);
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
	let parsed = mime::pick(&raw, &rule(r#"{"account": "home"}"#));
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
	let attachments = mime::pick(&raw, &rule(r#"{"account": "home"}"#));
	assert!(attachments.files.is_empty());
	assert!(attachments.left_out.is_empty());
	let whole = mime::pick(&raw, &rule(r#"{"account": "home", "scope": "eml"}"#));
	assert_eq!(whole.files.len(), 1);
	assert_eq!(whole.files[0].name, "Your order 123.eml");
	assert_eq!(whole.files[0].bytes, raw);
	let both = mime::pick(
		&invoice_mail("i@x", "a@b", b"%PDF"),
		&rule(r#"{"account": "home", "scope": "both"}"#),
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

fn config(port: u16, rule: &str) -> Config {
	Config::parse(&format!(
		r#"{{
			"accounts": [{{"name": "home", "host": "127.0.0.1", "port": {port}, "security": "plain",
				"username": "me", "password_env": "PATH", "interval_secs": 600}}],
			"rules": [{rule}]
		}}"#
	))
	.unwrap()
}

/// A server whose password is the value of `PATH`, which the config names as the
/// variable to read it from.
async fn server() -> FakeImap {
	FakeImap::start("me", &std::env::var("PATH").unwrap(), &["MOVE"]).await
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
	let mail = Mail::new(config(
		fake.port,
		r#"{"account": "home", "from": "billing@gas", "hints": {"area": "Tax", "tags": ["mail"]}}"#,
	));
	assert!(harness.app.set_mail(mail.clone()).is_ok());

	let report = mail::sync(&harness.app, &mail, "home").await.unwrap();
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
	let again = mail::sync(&harness.app, &mail, "home").await.unwrap();
	assert_eq!(again, Report::default());
	assert_eq!(fake.count("UID FETCH"), fetches);

	fake.renumber("INBOX");
	let renumbered = mail::sync(&harness.app, &mail, "home").await.unwrap();
	assert_eq!(
		(renumbered.mails, renumbered.ingested, renumbered.skipped),
		(2, 0, 2)
	);
	assert!(harness.inbox().is_empty());
	assert_eq!(harness.documents().len(), 1);

	let view = serde_json::to_value(mail::view(&harness.app).unwrap()).unwrap();
	assert_eq!(view["enabled"], true);
	let account = &view["accounts"][0];
	assert_eq!(account["name"], "home");
	assert_eq!(account["counts"]["ingested"], 1);
	assert_eq!(account["counts"]["skipped"], 3);
	assert_eq!(account["last_error"], Value::Null);
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
	let mail = Mail::new(config(
		fake.port,
		r#"{"account": "home", "subject": "order", "scope": "eml", "action": "move:Receipts"}"#,
	));
	let report = mail::sync(&harness.app, &mail, "home").await.unwrap();
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
async fn a_missing_password_variable_fails_only_that_account() {
	let harness = Harness::new(Models::default());
	let fake = server().await;
	let text = format!(
		r#"{{"accounts": [{{"name": "home", "host": "127.0.0.1", "port": {}, "security": "plain",
			"username": "me", "password_env": "VIOLET_TEST_UNSET_PASSWORD_VARIABLE"}}],
			"rules": [{{"account": "home"}}]}}"#,
		fake.port
	);
	let mail = Mail::new(Config::parse(&text).unwrap());
	assert!(harness.app.set_mail(mail.clone()).is_ok());
	let error = mail::sync(&harness.app, &mail, "home").await.unwrap_err();
	assert!(
		error
			.to_string()
			.contains("VIOLET_TEST_UNSET_PASSWORD_VARIABLE"),
		"{error}"
	);
	let view = mail::view(&harness.app).unwrap();
	assert!(
		view.accounts[0]
			.status
			.last_error
			.as_deref()
			.unwrap()
			.contains("is not set")
	);
	assert!(mail::sync(&harness.app, &mail, "work").await.is_err());
	assert!(fake.commands().is_empty());
}

async fn call(app: &std::sync::Arc<violet::App>, method: &str, uri: &str) -> (StatusCode, Value) {
	let request = Request::builder()
		.method(method)
		.uri(uri)
		.body(Body::empty())
		.unwrap();
	let response = violet::http::router(app.clone())
		.oneshot(request)
		.await
		.unwrap();
	let status = response.status();
	let bytes = response.into_body().collect().await.unwrap().to_bytes();
	(status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn the_api_shows_accounts_starts_syncs_and_forgets_mails() {
	let off = Harness::new(Models::default());
	let (status, body) = call(&off.app, "GET", "/api/mail").await;
	assert_eq!(status, StatusCode::OK);
	assert_eq!(body["enabled"], false);
	assert_eq!(
		call(&off.app, "POST", "/api/mail/home/sync").await.0,
		StatusCode::NOT_FOUND
	);

	let harness = Harness::new(Models::default());
	let fake = server().await;
	fake.add("INBOX", today(), &html_mail("h@x", "shop@example.com"));
	let mail = Mail::new(config(fake.port, r#"{"account": "home"}"#));
	assert!(harness.app.set_mail(mail.clone()).is_ok());
	mail::sync(&harness.app, &mail, "home").await.unwrap();

	let (status, body) = call(&harness.app, "GET", "/api/mail").await;
	assert_eq!(status, StatusCode::OK);
	let text = body.to_string();
	assert!(
		!text.contains("password") && !text.contains("PATH"),
		"{text}"
	);
	assert_eq!(body["accounts"][0]["folders"][0], "INBOX");
	assert_eq!(body["accounts"][0]["running"], false);
	let processed = &body["processed"][0];
	assert_eq!(processed["status"], "skipped");
	assert_eq!(processed["detail"], "no attachment");
	let id = processed["id"].as_i64().unwrap();

	let (status, body) = call(&harness.app, "POST", "/api/mail/home/sync").await;
	assert_eq!(status, StatusCode::ACCEPTED);
	assert_eq!(body["started"], true);
	assert_eq!(
		call(&harness.app, "POST", "/api/mail/work/sync").await.0,
		StatusCode::NOT_FOUND
	);
	let reprocess = format!("/api/mail/processed/{id}/reprocess");
	let (status, body) = call(&harness.app, "POST", &reprocess).await;
	assert_eq!(status, StatusCode::OK);
	assert_eq!(body["id"], id);
	assert_eq!(
		call(&harness.app, "POST", &reprocess).await.0,
		StatusCode::NOT_FOUND
	);
	assert_eq!(
		call(&harness.app, "POST", "/api/mail/processed/x/reprocess")
			.await
			.0,
		StatusCode::BAD_REQUEST
	);
	let again = mail::sync(&harness.app, &mail, "home").await.unwrap();
	assert_eq!((again.mails, again.skipped), (1, 1));
}

#[tokio::test]
async fn tls_and_starttls_fail_cleanly_against_a_plaintext_server() {
	let fake = FakeImap::start("me", "pw", &[]).await;
	for (security, expected) in [("tls", "TLS handshake"), ("starttls", "BAD")] {
		let config = Config::parse(&format!(
			r#"{{"accounts": [{{"name": "home", "host": "127.0.0.1", "port": {}, "security": "{security}",
				"username": "me", "password_env": "P"}}]}}"#,
			fake.port
		))
		.unwrap();
		let error = tokio::time::timeout(
			std::time::Duration::from_secs(10),
			Session::connect(&config.accounts[0], "pw"),
		)
		.await
		.unwrap()
		.err()
		.unwrap();
		let error = format!("{error:#}");
		assert!(error.contains(expected), "{security}: {error}");
	}
	assert_eq!(fake.count("LOGIN"), 0);
}
