use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::extract::{Form, Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{Html, IntoResponse, Response as Answer};
use axum::routing::get;
use axum::{Router, serve};
use tokio::net::TcpListener;

use super::super::Config;
use super::super::session::Cookie;
use super::*;

const EMAIL: &str = "sam@example.com";
const PASSWORD: &str = "correct horse";
const CODE: &str = "123456";
const DEVICE: &str = "device-1";
const PDF: &[u8] = b"%PDF-1.4\n1 0 obj\n<<>>\nendobj\n%%EOF\n";

const SIGN_IN_PAGE: &str = r#"<!DOCTYPE html>
<html lang="ja"><head><meta charset="utf-8">
<title>ログイン | マネーフォワード ID</title>
<meta name="csrf-param" content="authenticity_token" />
<meta name="csrf-token" content="csrf-sign-in" />
<script>window.gon={};gon.locale="ja";gon.signInPath="/sign_in/email";</script>
</head><body><div id="app" data-page="sign_in"></div></body></html>"#;

const CODE_PAGE: &str = r#"<!DOCTYPE html>
<html lang="ja"><head><meta charset="utf-8">
<title>追加認証 | マネーフォワード ID</title>
<meta name="csrf-param" content="authenticity_token" />
<meta name="csrf-token" content="csrf-otp" />
<script>window.gon={};gon.locale="ja";gon.notice="リスクの高いログイン試行を検知したため、認証コードを送信しました";</script>
</head><body><div id="app" data-page="email_otp"></div></body></html>"#;

const PAYSLIPS_PAGE: &str = r#"<!DOCTYPE html>
<html lang="ja"><head><meta charset="utf-8"><title>給与明細 | マネーフォワード クラウド給与</title>
<meta name="csrf-token" content="csrf-payroll" />
<script>window.gon={};gon.office={"name":"Example"};</script></head>
<body><header><a class="logo" href="/">クラウド給与</a><a href="/my/reports/bonus_slips">賞与明細</a></header>
<main><ul class="report-list">
<li class="report-list__item"><a class="report-link is-latest" data-turbo="false" href="/my/reports/periods/QUJD/payslip"><span class="report-link__date">2026年(令和08)年10月23日支給分</span>
	<span class="report-link__kind">給与明細</span></a>
	<a class="button" target="_blank" href="/my/reports/periods/QUJD/payslips_v2.pdf?disposition=inline">PDF</a></li>
<li class="report-list__item"><a href='/my/reports/periods/REVG/payslip' class="report-link"><span>2026/09/25</span></a></li>
</ul></main></body></html>"#;

const PAYSLIP_PAGE_HTML: &str = r#"<!DOCTYPE html>
<html lang="ja"><head><meta charset="utf-8"><title>給与明細</title></head>
<body><aside class="sidebar"><ul>
<li><a class="is-active" href="/my/reports/periods/QUJD/payslip"><span>2026/10/23</span></a></li>
<li><a href="/my/reports/periods/REVG/payslip"><span>2026/09/25</span></a></li>
<li><a href="/my/reports/periods/SFRNTA/payslip"><span>2020/04/24</span></a></li>
</ul></aside><main><a href="/my/reports/periods/QUJD/payslips_v2.pdf">ダウンロード</a></main></body></html>"#;

const BONUS_SLIPS_PAGE: &str = r#"<!DOCTYPE html>
<html lang="ja"><head><meta charset="utf-8"><title>賞与明細</title></head>
<body><main><ul class="report-list">
<li><a href="/my/reports/bonus_periods/Qk9O/bonus_slip"><span>2026年(令和08)年06月30日支給分</span> <span>賞与明細</span></a></li>
</ul></main></body></html>"#;

const WITHHOLDING_SLIPS_PAGE: &str = r#"<!DOCTYPE html>
<html lang="ja"><head><meta charset="utf-8"><title>源泉徴収票 | マネーフォワード クラウド年末調整</title></head>
<body><main><table class="table"><tbody>
<tr><td><a target="_blank" rel="noopener" href="/my/reports/terms/VEVSTQ/withholding_slip.pdf?disposition=inline">2026/01/05&nbsp;令和07年分</a></td></tr>
<tr><td><a target="_blank" rel="noopener" href="/my/reports/terms/T0xE/withholding_slip.pdf?disposition=inline">2025/01/06 令和06年分</a></td></tr>
</tbody></table></main></body></html>"#;

#[derive(Default)]
struct Counts {
	sign_ins: u32,
	codes_sent: u32,
	selections: u32,
}

#[derive(Clone)]
struct Site {
	counts: Arc<Mutex<Counts>>,
	hosts: Hosts,
	/// Of an application: its name at the ID site and its session cookie.
	client: &'static str,
	session: &'static str,
}

type Fields = HashMap<String, String>;

fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
	headers
		.get_all(header::COOKIE)
		.iter()
		.filter_map(|value| value.to_str().ok())
		.flat_map(|value| value.split(';'))
		.filter_map(|pair| pair.trim().split_once('='))
		.find(|(known, _)| *known == name)
		.map(|(_, value)| value.to_owned())
}

fn to(location: &str, cookies: &[&str]) -> Answer {
	let mut answer = (StatusCode::FOUND, [(header::LOCATION, location.to_owned())]).into_response();
	for cookie in cookies {
		answer
			.headers_mut()
			.append(header::SET_COOKIE, cookie.parse().unwrap());
	}
	answer
}

fn field<'a>(fields: &'a Fields, name: &str) -> &'a str {
	fields.get(name).map_or("", String::as_str)
}

fn signed_in(headers: &HeaderMap) -> bool {
	cookie(headers, "mfid").as_deref() == Some("in")
}

async fn sign_in_page() -> Answer {
	let mut answer = Html(SIGN_IN_PAGE).into_response();
	answer.headers_mut().append(
		header::SET_COOKIE,
		"_mfid_session=anonymous; Path=/; HttpOnly".parse().unwrap(),
	);
	answer
}

async fn sign_in(State(site): State<Site>, headers: HeaderMap, Form(form): Form<Fields>) -> Answer {
	if field(&form, "authenticity_token") != "csrf-sign-in" {
		return StatusCode::UNPROCESSABLE_ENTITY.into_response();
	}
	if field(&form, "mfid_user[email]") != EMAIL || field(&form, "mfid_user[password]") != PASSWORD
	{
		return to("/sign_in", &[]);
	}
	let mut counts = site.counts.lock().unwrap();
	counts.sign_ins += 1;
	if cookie(&headers, "sih").as_deref() == Some(DEVICE) {
		to("/me", &["mfid=in; Path=/; HttpOnly"])
	} else {
		counts.codes_sent += 1;
		to("/email_otp", &["otp=pending; Path=/"])
	}
}

async fn code_page(headers: HeaderMap) -> Answer {
	if cookie(&headers, "otp").as_deref() == Some("pending") {
		Html(CODE_PAGE).into_response()
	} else {
		to("/sign_in", &[])
	}
}

async fn code(headers: HeaderMap, Form(form): Form<Fields>) -> Answer {
	if field(&form, "authenticity_token") != "csrf-otp"
		|| field(&form, "_method") != "post"
		|| cookie(&headers, "otp").as_deref() != Some("pending")
	{
		return StatusCode::UNPROCESSABLE_ENTITY.into_response();
	}
	if field(&form, "email_otp") != CODE {
		return to("/email_otp", &[]);
	}
	to(
		"/me",
		&[
			"mfid=in; Path=/; HttpOnly",
			"otp=; Path=/; Max-Age=0",
			"sih=device-1; Path=/; Max-Age=34000000; HttpOnly",
		],
	)
}

async fn me(headers: HeaderMap) -> Answer {
	if signed_in(&headers) {
		Html("<html><head><title>マネーフォワード ID</title></head><body></body></html>")
			.into_response()
	} else {
		to("/sign_in", &[])
	}
}

async fn authorize(headers: HeaderMap, Query(query): Query<Fields>) -> Answer {
	if signed_in(&headers) {
		to(
			&format!("/account_selector?client={}", field(&query, "client")),
			&[],
		)
	} else {
		to("/sign_in", &[])
	}
}

async fn account_selector(Query(query): Query<Fields>) -> Html<String> {
	let client = field(&query, "client");
	Html(format!(
		r#"<!DOCTYPE html>
<html lang="ja"><head><meta charset="utf-8"><title>アカウントの選択</title>
<meta name="csrf-token" content="csrf-selector" />
<script>
//<![CDATA[
window.gon={{}};gon.locale="ja";gon.accounts=[{{"identifier":"acc-1","email":"old@example.com","signInMethod":"email","formAction":"/oauth/authorize?client={client}&selected=acc-1","active":false,"available":true}},{{"identifier":"acc-2","email":"{EMAIL}","signInMethod":"email","formAction":"/oauth/authorize?client={client}&selected=acc-2","active":true,"available":true}}];gon.addAccountPath="/sign_in";
//]]>
</script></head><body><div id="app"></div></body></html>"#
	))
}

async fn select_account(
	State(site): State<Site>,
	headers: HeaderMap,
	Query(query): Query<Fields>,
	Form(form): Form<Fields>,
) -> Answer {
	if field(&form, "authenticity_token") != "csrf-selector"
		|| field(&query, "selected") != "acc-2"
		|| !signed_in(&headers)
	{
		return StatusCode::UNPROCESSABLE_ENTITY.into_response();
	}
	site.counts.lock().unwrap().selections += 1;
	let host = match field(&query, "client") {
		"payroll" => &site.hosts.payroll,
		_ => &site.hosts.tax_adjustment,
	};
	to(
		&format!("{host}/users/auth/mfid/callback?code=granted"),
		&[],
	)
}

fn id_site(site: Site) -> Router {
	Router::new()
		.route("/sign_in", get(sign_in_page))
		.route("/sign_in/email", axum::routing::post(sign_in))
		.route("/email_otp", get(code_page).post(code))
		.route("/me", get(me))
		.route("/oauth/authorize", get(authorize).post(select_account))
		.route("/account_selector", get(account_selector))
		.with_state(site)
}

fn guarded(site: &Site, headers: &HeaderMap, page: impl IntoResponse) -> Answer {
	if cookie(headers, site.session).as_deref() == Some("ok") {
		page.into_response()
	} else {
		to("/session/new", &[])
	}
}

async fn to_id(State(site): State<Site>) -> Answer {
	to(
		&format!("{}/oauth/authorize?client={}", site.hosts.id, site.client),
		&[],
	)
}

async fn callback(State(site): State<Site>, Query(query): Query<Fields>) -> Answer {
	if field(&query, "code") == "granted" {
		to("/", &[&format!("{}=ok; Path=/; HttpOnly", site.session)])
	} else {
		StatusCode::UNPROCESSABLE_ENTITY.into_response()
	}
}

async fn payslips(State(site): State<Site>, headers: HeaderMap) -> Answer {
	guarded(&site, &headers, Html(PAYSLIPS_PAGE))
}

async fn bonus_slips(State(site): State<Site>, headers: HeaderMap) -> Answer {
	guarded(&site, &headers, Html(BONUS_SLIPS_PAGE))
}

async fn withholding_slips(State(site): State<Site>, headers: HeaderMap) -> Answer {
	guarded(&site, &headers, Html(WITHHOLDING_SLIPS_PAGE))
}

fn pdf() -> Answer {
	([(header::CONTENT_TYPE, "application/pdf")], PDF).into_response()
}

async fn period(
	State(site): State<Site>,
	headers: HeaderMap,
	Path((id, file)): Path<(String, String)>,
) -> Answer {
	let page = match (id.as_str(), file.as_str()) {
		(_, "payslip") => Html(PAYSLIP_PAGE_HTML).into_response(),
		("QUJD" | "REVG", "payslips_v2.pdf") => pdf(),
		("SFRNTA", "payslips_v2.pdf") => {
			Html("<html><body>準備中です</body></html>").into_response()
		}
		_ => StatusCode::NOT_FOUND.into_response(),
	};
	guarded(&site, &headers, page)
}

async fn bonus_period(
	State(site): State<Site>,
	headers: HeaderMap,
	Path((id, file)): Path<(String, String)>,
) -> Answer {
	let page = match (id.as_str(), file.as_str()) {
		("Qk9O", "bonus_slips_v2.pdf") => pdf(),
		_ => StatusCode::NOT_FOUND.into_response(),
	};
	guarded(&site, &headers, page)
}

async fn term(
	State(site): State<Site>,
	headers: HeaderMap,
	Path((id, file)): Path<(String, String)>,
) -> Answer {
	let page = match (id.as_str(), file.as_str()) {
		("VEVSTQ" | "T0xE", "withholding_slip.pdf") => pdf(),
		_ => StatusCode::NOT_FOUND.into_response(),
	};
	guarded(&site, &headers, page)
}

fn application(site: Site) -> Router {
	Router::new()
		.route(
			"/",
			get(|| async { Html("<html><body>ホーム</body></html>") }),
		)
		.route(
			"/session/new",
			get(|| async { to("/users/auth/mfid", &[]) }),
		)
		.route("/users/auth/mfid", get(to_id))
		.route("/users/auth/mfid/callback", get(callback))
		.route("/my/reports/payslips", get(payslips))
		.route("/my/reports/periods/{id}/{file}", get(period))
		.route("/my/reports/bonus_slips", get(bonus_slips))
		.route("/my/reports/bonus_periods/{id}/{file}", get(bonus_period))
		.route("/my/reports/withholding_slips", get(withholding_slips))
		.route("/my/reports/terms/{id}/{file}", get(term))
		.with_state(site)
}

struct Fake {
	hosts: Hosts,
	counts: Arc<Mutex<Counts>>,
}

impl Fake {
	async fn start() -> Self {
		let mut listeners = Vec::new();
		for _ in 0..3 {
			listeners.push(TcpListener::bind("127.0.0.1:0").await.unwrap());
		}
		let at = |index: usize| format!("http://{}", listeners[index].local_addr().unwrap());
		let hosts = Hosts {
			id: at(0),
			payroll: at(1),
			tax_adjustment: at(2),
		};
		let counts = Arc::new(Mutex::new(Counts::default()));
		let site = |client, session| Site {
			counts: counts.clone(),
			hosts: hosts.clone(),
			client,
			session,
		};
		let routers = [
			id_site(site("id", "mfid")),
			application(site("payroll", "_payroll_session")),
			application(site("tax-adjustment", "_ta_session")),
		];
		for (listener, router) in listeners.into_iter().zip(routers) {
			tokio::spawn(async move { serve(listener, router).await.unwrap() });
		}
		Self { hosts, counts }
	}

	fn connector(&self, options: &str) -> MoneyForward {
		let options = Options::parse(&serde_json::from_str(options).unwrap()).unwrap();
		MoneyForward::new(options, self.hosts.clone()).unwrap()
	}

	/// What a connector keeps once the code was entered.
	async fn enrolled(&self) -> serde_json::Value {
		let mut connector = self.connector("{}");
		assert!(matches!(
			connector.login(&credentials(), None).await.unwrap(),
			Login::NeedsCode { .. }
		));
		connector.submit_code(CODE).await.unwrap();
		connector.state().unwrap()
	}

	/// A connector signed in as a device the site knows.
	async fn signed_in(&self, options: &str) -> MoneyForward {
		let saved = self.enrolled().await;
		let mut connector = self.connector(options);
		let login = connector.login(&credentials(), Some(saved)).await.unwrap();
		assert_eq!(login, Login::Ready);
		connector
	}

	fn counts(&self) -> (u32, u32, u32) {
		let counts = self.counts.lock().unwrap();
		(counts.sign_ins, counts.codes_sent, counts.selections)
	}
}

fn credentials() -> Credentials {
	Credentials {
		username: EMAIL.into(),
		password: PASSWORD.into(),
	}
}

fn doc(remote_id: &str, file_name: &str, title: &str, date: Option<&str>) -> RemoteDoc {
	RemoteDoc {
		remote_id: remote_id.into(),
		file_name: file_name.into(),
		title: Some(title.into()),
		date: date.map(str::to_owned),
	}
}

fn slip(id: &str, label: &str) -> Slip {
	Slip {
		id: id.into(),
		label: label.into(),
	}
}

fn cookie_names(state: &serde_json::Value) -> Vec<String> {
	state
		.as_array()
		.unwrap()
		.iter()
		.map(|cookie| cookie["name"].as_str().unwrap().to_owned())
		.collect()
}

#[tokio::test]
async fn a_new_device_needs_the_code_and_keeps_only_the_device_cookie() {
	let fake = Fake::start().await;
	let mut connector = fake.connector("{}");
	assert!(connector.state().is_none());
	let login = connector.login(&credentials(), None).await.unwrap();
	assert_eq!(
		login,
		Login::NeedsCode {
			prompt: CODE_PROMPT.into()
		}
	);
	assert!(connector.state().is_none());

	let refused = connector.submit_code("000000").await.unwrap_err();
	assert_eq!(refused.to_string(), "Money Forward refused the code");
	connector.submit_code(" 123456\n").await.unwrap();
	assert_eq!(fake.counts(), (1, 1, 0));

	let state = connector.state().unwrap();
	assert_eq!(cookie_names(&state), ["sih"]);
	assert_eq!(state[0]["value"], DEVICE);
	let late = connector.submit_code(CODE).await.unwrap_err();
	assert_eq!(
		late.to_string(),
		"no Money Forward sign-in waits for a code"
	);
}

#[tokio::test]
async fn a_known_device_signs_in_without_a_code() {
	let fake = Fake::start().await;
	let saved = fake.enrolled().await;
	let mut connector = fake.connector("{}");
	let login = connector.login(&credentials(), Some(saved.clone())).await;
	assert_eq!(login.unwrap(), Login::Ready);
	assert_eq!(fake.counts(), (2, 1, 0));
	assert_eq!(connector.state().unwrap(), saved);
	assert!(connector.session.jar.get("mfid").is_some());
}

#[tokio::test]
async fn a_wrong_password_is_refused_without_naming_the_account() {
	let fake = Fake::start().await;
	let mut connector = fake.connector("{}");
	let wrong = Credentials {
		username: EMAIL.into(),
		password: "guess".into(),
	};
	let error = connector.login(&wrong, None).await.unwrap_err();
	let error = format!("{error:#}");
	assert_eq!(error, "Money Forward refused the email or the password");
	assert_eq!(fake.counts(), (0, 0, 0));
}

#[tokio::test]
async fn every_slip_is_listed_through_the_account_selector() {
	let fake = Fake::start().await;
	let mut connector = fake.signed_in("{}").await;
	let listed = connector.list().await.unwrap();
	assert_eq!(
		listed,
		[
			doc(
				"payslip:QUJD",
				"moneyforward-payslip-2026-10-23.pdf",
				"2026年(令和08)年10月23日支給分 給与明細",
				Some("2026-10-23"),
			),
			doc(
				"payslip:REVG",
				"moneyforward-payslip-2026-09-25.pdf",
				"2026/09/25 給与明細",
				Some("2026-09-25"),
			),
			doc(
				"payslip:SFRNTA",
				"moneyforward-payslip-2020-04-24.pdf",
				"2020/04/24 給与明細",
				Some("2020-04-24"),
			),
			doc(
				"bonus:Qk9O",
				"moneyforward-bonus-2026-06-30.pdf",
				"2026年(令和08)年06月30日支給分 賞与明細",
				Some("2026-06-30"),
			),
			doc(
				"withholding:VEVSTQ",
				"moneyforward-withholding-R07.pdf",
				"2026/01/05 令和07年分 源泉徴収票",
				Some("2026-01-05"),
			),
			doc(
				"withholding:T0xE",
				"moneyforward-withholding-R06.pdf",
				"2025/01/06 令和06年分 源泉徴収票",
				Some("2025-01-06"),
			),
		]
	);
	assert_eq!(fake.counts().2, 2, "one account choice per application");
	assert_eq!(cookie_names(&connector.state().unwrap()), ["sih"]);

	assert_eq!(connector.list().await.unwrap(), listed);
	assert_eq!(
		fake.counts(),
		(2, 1, 2),
		"the sessions are kept for the run"
	);
}

#[tokio::test]
async fn the_options_narrow_the_listing() {
	let fake = Fake::start().await;
	let options = r#"{"documents": ["withholding_slips", "payslips"], "since": "2026-01-01"}"#;
	let mut connector = fake.signed_in(options).await;
	let ids: Vec<String> = connector
		.list()
		.await
		.unwrap()
		.into_iter()
		.map(|doc| doc.remote_id)
		.collect();
	assert_eq!(ids, ["withholding:VEVSTQ", "payslip:QUJD", "payslip:REVG"]);
}

async fn refusal(connector: &mut MoneyForward, remote_id: &str) -> String {
	connector.fetch(remote_id).await.unwrap_err().to_string()
}

#[tokio::test]
async fn a_document_is_fetched_only_as_a_pdf() {
	let fake = Fake::start().await;
	let mut connector = fake.signed_in("{}").await;
	for remote_id in ["payslip:QUJD", "bonus:Qk9O", "withholding:VEVSTQ"] {
		assert_eq!(
			connector.fetch(remote_id).await.unwrap(),
			PDF,
			"{remote_id}"
		);
	}
	assert_eq!(fake.counts().2, 2);

	assert_eq!(
		refusal(&mut connector, "payslip:SFRNTA").await,
		"Money Forward answered something that is not a PDF"
	);
	assert_eq!(
		refusal(&mut connector, "payslip:bm9uZQ").await,
		"Money Forward answered 404 for the PDF"
	);
	for remote_id in ["invoice:QUJD", "payslip:", "payslip:../../me", "QUJD"] {
		assert_eq!(
			refusal(&mut connector, remote_id).await,
			"not a Money Forward document",
			"{remote_id}"
		);
	}
}

#[tokio::test]
async fn a_lost_session_signs_in_again_but_never_waits_for_a_code() {
	let fake = Fake::start().await;
	let mut connector = fake.signed_in(r#"{"documents": ["bonus_slips"]}"#).await;
	assert_eq!(connector.list().await.unwrap().len(), 1);
	assert_eq!(fake.counts(), (2, 1, 1));

	for name in ["mfid", "_payroll_session"] {
		connector.session.jar.remove(name);
	}
	assert_eq!(connector.list().await.unwrap().len(), 1);
	assert_eq!(fake.counts(), (3, 1, 2));

	for name in ["mfid", "_payroll_session", "sih"] {
		connector.session.jar.remove(name);
	}
	let error = connector.list().await.unwrap_err().to_string();
	assert_eq!(
		error,
		"Money Forward asks for a code again; connect from the settings"
	);
	assert_eq!(fake.counts(), (4, 2, 2));
}

#[test]
fn only_the_device_cookies_of_the_id_site_are_kept() {
	let options = Options::parse(&serde_json::json!({})).unwrap();
	let mut connector = MoneyForward::new(options, Hosts::default()).unwrap();
	let cookie = |name: &str, domain: &str, host_only: bool| Cookie {
		name: name.into(),
		value: "v".into(),
		domain: domain.into(),
		host_only,
		path: "/".into(),
		secure: true,
		expires: None,
	};
	for cookie in [
		cookie("sih", "id.moneyforward.com", true),
		cookie("mfid", "id.moneyforward.com", true),
		cookie("_mfid_session", "id.moneyforward.com", true),
		cookie("cf_clearance", "moneyforward.com", false),
		cookie("sih", "payroll.moneyforward.com", true),
		cookie("sih", "notmoneyforward.com", false),
		cookie("_payroll_session", "payroll.moneyforward.com", true),
	] {
		connector.session.jar.insert(cookie);
	}
	let state = connector.state().unwrap();
	let kept: Vec<(&str, &str)> = state
		.as_array()
		.unwrap()
		.iter()
		.map(|cookie| {
			(
				cookie["name"].as_str().unwrap(),
				cookie["domain"].as_str().unwrap(),
			)
		})
		.collect();
	assert_eq!(
		kept,
		[
			("sih", "id.moneyforward.com"),
			("cf_clearance", "moneyforward.com")
		]
	);
}

#[test]
fn options_are_checked() {
	let parse = |options: &str| Options::parse(&serde_json::from_str(options).unwrap());
	let all = parse("{}").unwrap();
	assert_eq!(all.documents, every_document());
	assert_eq!(all.since, None);
	let some = parse(r#"{"documents": ["bonus_slips", "bonus_slips"], "since": "2024-04-01"}"#);
	let some = some.unwrap();
	assert_eq!(some.documents, [Documents::BonusSlips]);
	assert_eq!(some.since.as_deref(), Some("2024-04-01"));
	for refused in [
		r#"{"colour": 1}"#,
		r#"{"documents": ["invoices"]}"#,
		r#"{"documents": "payslips"}"#,
		r#"{"since": "2024/04/01"}"#,
		r#"{"since": "2024-02-30"}"#,
		r#"{"id_url": "http://127.0.0.1"}"#,
	] {
		let error = parse(refused).unwrap_err().to_string();
		assert!(error.starts_with("options: "), "{refused}: {error}");
	}
}

#[test]
fn the_kind_is_built_from_a_config() {
	let config = |options: &str| {
		Config::parse(&format!(
			r#"{{"connectors": [{{"name": "mf", "kind": "moneyforward", "username_env": "U",
				"password_env": "P", "schedule": "monthly 26 09:00", "options": {options}}}]}}"#
		))
		.unwrap()
	};
	assert!(connector(&config("{}").connectors[0]).is_ok());
	assert!(connector(&config(r#"{"documents": ["payslips"]}"#).connectors[0]).is_ok());
	assert!(connector(&config(r#"{"pages": 3}"#).connectors[0]).is_err());
}

#[test]
fn documents_are_named_after_their_date_or_else_their_id() {
	assert_eq!(
		payslip(&slip("QUJD", "表示")),
		doc(
			"payslip:QUJD",
			"moneyforward-payslip-QUJD.pdf",
			"表示 給与明細",
			None
		)
	);
	assert_eq!(
		bonus_slip(&slip("Qk9O", "")),
		doc(
			"bonus:Qk9O",
			"moneyforward-bonus-Qk9O.pdf",
			"賞与明細",
			None
		)
	);
	assert_eq!(
		withholding_slip(&slip("VEVSTQ", "令和元年分 源泉徴収票")),
		doc(
			"withholding:VEVSTQ",
			"moneyforward-withholding-R01.pdf",
			"令和元年分 源泉徴収票",
			None
		)
	);
	assert_eq!(
		withholding_slip(&slip("VEVSTQ", "PDF")),
		doc(
			"withholding:VEVSTQ",
			"moneyforward-withholding-VEVSTQ.pdf",
			"PDF 源泉徴収票",
			None
		)
	);
}
