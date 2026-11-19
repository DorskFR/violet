use std::net::SocketAddr;

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use violet::auth::{AREAS_FILE, AuthConfig, Role, Scope, USERS_FILE, User};
use violet::http::router;
use violet::ops::{self, Patch};
use violet::pipeline::{self, Outcome};
use violet::users::{self, NO_CHECK};

use crate::common::{Harness, splitting_fake, text_pdf, three_pages};

pub struct Api {
	pub harness: Harness,
	pub router: Router,
}

pub struct Answer {
	pub status: StatusCode,
	pub headers: axum::http::HeaderMap,
	pub body: Vec<u8>,
}

/// The host the tests reach violet at, and the origin of its pages.
pub const HOST: &str = "violet.test";
pub const ORIGIN: &str = "http://violet.test";

/// A signed-in browser: its session cookie and the CSRF token that goes with it.
#[derive(Clone, Debug)]
pub struct Browser {
	pub cookie: String,
	pub csrf: String,
}

impl Answer {
	pub fn json(&self) -> Value {
		serde_json::from_slice(&self.body)
			.unwrap_or_else(|_| panic!("not JSON: {}", String::from_utf8_lossy(&self.body)))
	}

	/// The session token in a `Set-Cookie` of this answer.
	pub fn cookie(&self) -> Option<String> {
		self.headers
			.get_all(header::SET_COOKIE)
			.iter()
			.filter_map(|value| value.to_str().ok())
			.find_map(|value| value.strip_prefix("__Host-violet_session="))
			.map(|value| value.split(';').next().unwrap_or_default().to_string())
			.filter(|value| !value.is_empty())
	}

	/// The browser this sign-in answer leaves behind.
	pub fn browser(&self) -> Browser {
		assert_eq!(self.status, StatusCode::OK, "{:?}", self.json());
		Browser {
			cookie: self.cookie().expect("a session cookie"),
			csrf: self.json()["csrf"]
				.as_str()
				.expect("a CSRF token")
				.to_string(),
		}
	}
}

/// Who sends a request.
#[derive(Clone, Copy)]
pub enum As<'a> {
	Nobody,
	Token(&'a str),
	/// A page of violet, without a session.
	Page,
	Browser(&'a Browser),
	/// The cookie of a browser alone, as a request from another site would carry it.
	Cookie(&'a Browser),
	/// A browser behind the proxy at `peer` that forwards for `client`.
	Forwarded(&'a str, &'a str),
}

pub struct Household {
	pub api: Api,
	pub alex: String,
	pub sam: String,
	pub sam_reader: String,
	/// The pharmacy receipt, in Health, which only alex sees.
	pub private: String,
	/// The electricity bill, in Housing.
	pub public: String,
}

impl Api {
	pub async fn new(config: Option<AuthConfig>) -> Self {
		let fake = splitting_fake().await;
		let harness = Harness::new(fake.models());
		if let Some(config) = config {
			harness.app.auth.configure(config).unwrap();
		}
		harness.ingest("three.pdf", &three_pages()).await;
		let router = router(harness.app.clone());
		Self { harness, router }
	}

	pub async fn send(&self, mut request: Request<Body>, who: As<'_>) -> Answer {
		let headers = request.headers_mut();
		let page = |headers: &mut axum::http::HeaderMap| {
			headers.insert(header::HOST, HOST.parse().unwrap());
			headers.insert(header::ORIGIN, ORIGIN.parse().unwrap());
		};
		let cookie = |headers: &mut axum::http::HeaderMap, browser: &Browser| {
			headers.insert(
				header::COOKIE,
				format!("theme=dark; __Host-violet_session={}", browser.cookie)
					.parse()
					.unwrap(),
			);
		};
		match who {
			As::Nobody => {}
			As::Token(token) => {
				headers.insert(
					header::AUTHORIZATION,
					format!("Bearer {token}").parse().unwrap(),
				);
			}
			As::Page => page(headers),
			As::Browser(browser) => {
				page(headers);
				cookie(headers, browser);
				headers.insert("x-csrf-token", browser.csrf.parse().unwrap());
			}
			As::Cookie(browser) => cookie(headers, browser),
			As::Forwarded(peer, client) => {
				page(headers);
				headers.insert(
					"x-forwarded-for",
					format!("198.51.100.1, {client}").parse().unwrap(),
				);
				let peer: SocketAddr = format!("{peer}:4000").parse().unwrap();
				request.extensions_mut().insert(ConnectInfo(peer));
			}
		}
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

	pub async fn get(&self, uri: &str, who: As<'_>) -> Answer {
		self.send(Request::get(uri).body(Body::empty()).unwrap(), who)
			.await
	}

	pub async fn call(&self, method: &str, uri: &str, body: Value, who: As<'_>) -> Answer {
		let request = Request::builder()
			.method(method)
			.uri(uri)
			.header(header::CONTENT_TYPE, "application/json")
			.body(Body::from(body.to_string()))
			.unwrap();
		self.send(request, who).await
	}

	pub async fn ok(&self, uri: &str, who: As<'_>) -> Value {
		let answer = self.get(uri, who).await;
		assert_eq!(answer.status, StatusCode::OK, "{uri}: {:?}", answer.json());
		answer.json()
	}

	pub async fn titles(&self, who: As<'_>) -> Vec<String> {
		let found = self.ok("/api/search", who).await;
		found["hits"]
			.as_array()
			.unwrap()
			.iter()
			.map(|hit| hit["title"].as_str().unwrap().to_string())
			.collect()
	}

	pub fn id(&self, title: &str) -> String {
		self.harness
			.documents()
			.into_iter()
			.find(|(_, sidecar)| sidecar.title == title)
			.unwrap_or_else(|| panic!("no {title}"))
			.1
			.id
			.to_string()
	}

	pub fn add_user(&self, name: &str, role: Role) {
		users::add(
			&self.harness.app,
			&User::implicit(),
			NO_CHECK,
			name,
			role,
			None,
		)
		.unwrap();
	}

	/// Gives `name` a password, as `violet user password` does.
	pub fn set_password(&self, name: &str, password: &str) {
		let phc = violet::login::hash_now(password).unwrap();
		users::set_password(
			&self.harness.app,
			&User::implicit(),
			NO_CHECK,
			name,
			phc,
			None,
		)
		.unwrap();
	}

	pub fn token(&self, user: &str, name: &str, scope: Scope) -> String {
		users::add_token(
			&self.harness.app,
			&User::implicit(),
			NO_CHECK,
			user,
			name,
			scope,
			None,
		)
		.unwrap()
		.0
	}

	pub fn private(&self, area: &str, members: &[&str]) {
		let areas = serde_json::from_value(json!({
			"private": [{"area": area, "members": members}]
		}))
		.unwrap();
		users::set_areas(&self.harness.app, &User::implicit(), areas).unwrap();
	}
}

pub fn upload_request(uri: &str, name: &str, bytes: &[u8]) -> Request<Body> {
	let boundary = "XBOUNDARYX";
	let mut body = format!(
		"--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\nContent-Type: application/octet-stream\r\n\r\n"
	)
	.into_bytes();
	body.extend_from_slice(bytes);
	body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
	Request::post(uri)
		.header(
			header::CONTENT_TYPE,
			format!("multipart/form-data; boundary={boundary}"),
		)
		.body(Body::from(body))
		.unwrap()
}

/// alex (admin, sees Health) and sam (member, does not), each with a token, over the
/// library of `three.pdf`.
pub async fn household(config: Option<AuthConfig>) -> Household {
	let api = Api::new(config).await;
	api.add_user("alex", Role::Admin);
	api.add_user("sam", Role::Member);
	let alex = api.token("alex", "laptop", Scope::Admin);
	let sam = api.token("sam", "phone", Scope::Write);
	let sam_reader = api.token("sam", "reader", Scope::Read);
	api.private("Health", &["alex"]);
	let private = api.id("Receipt");
	let public = api.id("Electricity Bill");
	Household {
		api,
		alex,
		sam,
		sam_reader,
		private,
		public,
	}
}

#[tokio::test]
async fn without_a_users_file_everything_is_as_before() {
	let api = Api::new(None).await;
	let me = api.ok("/api/me", As::Nobody).await;
	assert_eq!(
		me,
		json!({"csrf": null, "name": null, "role": "admin", "scope": "admin", "via": "implicit",
			"areas": ["Health", "Housing"], "users": false, "password": false, "passkeys": []})
	);
	for who in [As::Nobody, As::Token("vt_not-a-token")] {
		assert_eq!(api.titles(who).await.len(), 2);
		assert_eq!(api.get("/api/mail", who).await.status, StatusCode::OK);
	}
	let id = api.id("Receipt");
	let answer = api
		.call(
			"PATCH",
			&format!("/api/documents/{id}"),
			json!({"title": "Pharmacy"}),
			As::Token("garbage"),
		)
		.await;
	assert_eq!(answer.status, StatusCode::OK, "{:?}", answer.json());

	let journal = std::fs::read_to_string(api.harness.app.library.journal_path()).unwrap();
	assert!(!journal.contains("on_behalf_of"), "{journal}");
	let actors: Vec<String> = api
		.harness
		.entries()
		.iter()
		.map(|entry| entry.actor.clone())
		.collect();
	assert_eq!(actors, ["agent", "user"]);
	let activity = api.ok("/api/activity", As::Nobody).await;
	assert_eq!(activity[0]["entry"]["actor"], "user");
	assert_eq!(activity[0]["undoable"], true);
	assert_eq!(activity[1]["entry"]["actor"], "agent");
	assert_eq!(
		api.ok("/api/users", As::Nobody).await,
		json!({"configured": false, "users": []})
	);
	assert_eq!(
		api.ok("/api/areas", As::Nobody).await,
		json!({"private": []})
	);
	assert!(!api.harness.root().join(USERS_FILE).exists());
}

/// Every endpoint that answers with a document or changes one, for a document a user
/// cannot see: it does not exist for them.
#[tokio::test]
async fn a_document_in_a_private_area_does_not_exist_for_others() {
	let house = household(None).await;
	let api = &house.api;
	let (p, q) = (&house.private, &house.public);
	let sam = As::Token(&house.sam);
	let alex = As::Token(&house.alex);
	let reads = [
		format!("/api/documents/{p}"),
		format!("/api/documents/{p}/file"),
		format!("/api/documents/{p}/pages/1"),
	];
	for uri in &reads {
		assert_eq!(api.get(uri, alex).await.status, StatusCode::OK, "{uri}");
		assert_eq!(
			api.get(uri, sam).await.status,
			StatusCode::NOT_FOUND,
			"{uri}"
		);
	}
	let ingest = api.harness.entries()[0].id;
	let before = api.harness.entries().len();
	let changes = [
		(
			"PATCH",
			format!("/api/documents/{p}"),
			json!({"title": "x"}),
		),
		("DELETE", format!("/api/documents/{p}"), json!({})),
		(
			"POST",
			format!("/api/documents/{p}/split"),
			json!({"groups": [[1]]}),
		),
		(
			"POST",
			format!("/api/documents/{p}/rotate"),
			json!({"pages": [1], "degrees": 90}),
		),
		(
			"POST",
			format!("/api/documents/{p}/delete-pages"),
			json!({"pages": [1]}),
		),
		("POST", format!("/api/documents/{p}/reanalyze"), json!({})),
		("POST", format!("/api/documents/{p}/reread"), json!({})),
		("POST", "/api/merge".into(), json!({"ids": [q, p]})),
		("POST", format!("/api/activity/{ingest}/undo"), json!({})),
	];
	for (method, uri, body) in &changes {
		let answer = api.call(method, uri, body.clone(), sam).await;
		assert_eq!(
			answer.status,
			StatusCode::NOT_FOUND,
			"{method} {uri}: {:?}",
			answer.json()
		);
	}
	let replace = api
		.send(
			upload_request(
				&format!("/api/documents/{p}/replace"),
				"x.pdf",
				&text_pdf(&["x"]),
			),
			sam,
		)
		.await;
	assert_eq!(replace.status, StatusCode::NOT_FOUND);

	assert_eq!(api.titles(sam).await, ["Electricity Bill"]);
	assert_eq!(api.titles(alex).await.len(), 2);
	let searched = api.ok("/api/search?q=pharmacy", sam).await;
	assert_eq!(searched["total"], 0);
	let facets = api.ok("/api/facets", sam).await;
	assert_eq!(facets["areas"], json!([{"value": "Housing", "count": 1}]));
	assert!(!facets.to_string().contains("Sakura"));
	let activity = api.ok("/api/activity", sam).await;
	assert!(!activity.to_string().contains("Health"), "{activity}");
	assert!(activity.as_array().unwrap().is_empty(), "{activity}");
	let seen = api.ok("/api/activity", alex).await;
	assert!(seen.to_string().contains("Health"));

	assert_eq!(api.harness.documents().len(), 2);
	assert_eq!(api.harness.entries().len(), before);
	assert_eq!(api.get(&reads[0], alex).await.status, StatusCode::OK);
}

#[tokio::test]
async fn requests_need_a_known_user_once_there_is_a_users_file() {
	let house = household(None).await;
	let api = &house.api;
	let nobody = api.get("/api/search", As::Nobody).await;
	assert_eq!(nobody.status, StatusCode::UNAUTHORIZED);
	assert!(
		nobody.headers[header::WWW_AUTHENTICATE]
			.to_str()
			.unwrap()
			.starts_with("Bearer")
	);
	for bad in ["vt_0000", "nope", ""] {
		let answer = api.get("/api/search", As::Token(bad)).await;
		assert_eq!(answer.status, StatusCode::UNAUTHORIZED, "{bad}");
	}
	assert_eq!(api.get("/healthz", As::Nobody).await.status, StatusCode::OK);
	assert_eq!(api.get("/readyz", As::Nobody).await.status, StatusCode::OK);

	let me = api.ok("/api/me", As::Token(&house.sam)).await;
	assert_eq!(me["name"], "sam");
	assert_eq!(me["role"], "member");
	assert_eq!(me["via"], "token");
	assert_eq!(me["areas"], json!(["Housing"]));
	assert_eq!(me["users"], true);

	let reader = As::Token(&house.sam_reader);
	assert_eq!(api.titles(reader).await, ["Electricity Bill"]);
	let refused = api
		.call(
			"PATCH",
			&format!("/api/documents/{}", house.public),
			json!({"title": "x"}),
			reader,
		)
		.await;
	assert_eq!(refused.status, StatusCode::FORBIDDEN);

	let sam = As::Token(&house.sam);
	for uri in ["/api/users", "/api/areas", "/api/mail"] {
		assert_eq!(
			api.get(uri, sam).await.status,
			StatusCode::FORBIDDEN,
			"{uri}"
		);
		assert_eq!(
			api.get(uri, As::Token(&house.alex)).await.status,
			StatusCode::OK,
			"{uri}"
		);
	}
	let refused = api
		.call(
			"POST",
			"/api/users",
			json!({"name": "kim", "role": "admin"}),
			sam,
		)
		.await;
	assert_eq!(refused.status, StatusCode::FORBIDDEN);

	let listed = api.ok("/api/users", As::Token(&house.alex)).await;
	assert_eq!(listed["configured"], true);
	assert!(!listed.to_string().contains("sha256"), "{listed}");
	assert_eq!(
		listed["users"][1]["tokens"],
		json!([{"name": "phone", "scope": "write", "expires": null},
			{"name": "reader", "scope": "read", "expires": null}])
	);
	let stored = std::fs::read_to_string(api.harness.root().join(USERS_FILE)).unwrap();
	assert!(
		!stored.contains(&house.sam),
		"the secret itself is never stored"
	);
	assert!(stored.contains(&violet_core::hash::sha256(house.sam.as_bytes())));
}

#[tokio::test]
async fn an_expired_token_is_refused() {
	let house = household(None).await;
	let app = &house.api.harness.app;
	let soon = time::OffsetDateTime::now_utc() + time::Duration::seconds(1);
	let (secret, _) = users::add_token(
		app,
		&User::implicit(),
		NO_CHECK,
		"sam",
		"brief",
		Scope::Read,
		Some(soon),
	)
	.unwrap();
	assert_eq!(
		house.api.get("/api/me", As::Token(&secret)).await.status,
		StatusCode::OK
	);
	let past = time::OffsetDateTime::now_utc() - time::Duration::seconds(1);
	assert!(
		users::add_token(
			app,
			&User::implicit(),
			NO_CHECK,
			"sam",
			"old",
			Scope::Read,
			Some(past)
		)
		.is_err()
	);
	tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
	let answer = house.api.get("/api/me", As::Token(&secret)).await;
	assert_eq!(answer.status, StatusCode::UNAUTHORIZED);
	assert!(answer.json()["error"].as_str().unwrap().contains("expired"));
}

#[tokio::test]
async fn revoking_a_token_is_journaled_and_undone() {
	let house = household(None).await;
	let api = &house.api;
	let alex = As::Token(&house.alex);
	let sam = As::Token(&house.sam);
	let revoked = api
		.call("DELETE", "/api/users/sam/tokens/phone", json!({}), alex)
		.await;
	assert_eq!(revoked.status, StatusCode::OK, "{:?}", revoked.json());
	let entry = revoked.json()["entry"].as_str().unwrap().to_string();
	assert_eq!(
		api.get("/api/me", sam).await.status,
		StatusCode::UNAUTHORIZED
	);
	assert!(api.harness.root().join(USERS_FILE).is_file());
	let last = api.harness.last_entry();
	assert_eq!(last.actor, "user:alex");
	assert_eq!(last.summary, "Revoked token phone of sam");

	let activity = api.ok("/api/activity", alex).await;
	assert_eq!(activity[0]["entry"]["id"], entry);
	assert_eq!(activity[0]["undoable"], true);
	assert!(
		api.ok("/api/activity", sam_reader(&house))
			.await
			.as_array()
			.unwrap()
			.iter()
			.all(|item| item["entry"]["action"]["kind"] != "settings")
	);

	let undone = api
		.call(
			"POST",
			&format!("/api/activity/{entry}/undo"),
			json!({}),
			alex,
		)
		.await;
	assert_eq!(undone.status, StatusCode::OK, "{:?}", undone.json());
	assert_eq!(api.ok("/api/me", sam).await["name"], "sam");

	let originals = api.harness.originals();
	assert!(
		originals.iter().any(|name| name.ends_with(".users.json")),
		"{originals:?}"
	);
}

fn sam_reader(house: &Household) -> As<'_> {
	As::Token(&house.sam_reader)
}

#[tokio::test]
async fn the_first_admin_cannot_lock_the_web_out() {
	let api = Api::new(None).await;
	let refused = api
		.call(
			"POST",
			"/api/users",
			json!({"name": "alex", "role": "admin"}),
			As::Nobody,
		)
		.await;
	assert_eq!(refused.status, StatusCode::CONFLICT, "{:?}", refused.json());
	assert!(!api.harness.root().join(USERS_FILE).exists());

	let house = household(None).await;
	let api = &house.api;
	let alex = As::Token(&house.alex);
	let demote = api
		.call("PATCH", "/api/users/alex", json!({"role": "member"}), alex)
		.await;
	assert_eq!(demote.status, StatusCode::CONFLICT);
	let removed = api.call("DELETE", "/api/users/alex", json!({}), alex).await;
	assert_eq!(removed.status, StatusCode::CONFLICT);
	let revoked = api
		.call("DELETE", "/api/users/alex/tokens/laptop", json!({}), alex)
		.await;
	assert_eq!(revoked.status, StatusCode::CONFLICT);
	let promoted = api
		.call("PATCH", "/api/users/sam", json!({"role": "admin"}), alex)
		.await;
	assert_eq!(promoted.status, StatusCode::OK, "{:?}", promoted.json());
	let same = api
		.call("PATCH", "/api/users/sam", json!({"role": "admin"}), alex)
		.await;
	assert_eq!(same.status, StatusCode::BAD_REQUEST);
	for body in [
		json!({"role": "admin", "identities": ["CN=sam"]}),
		json!({}),
	] {
		let refused = api
			.call("PATCH", "/api/users/sam", body.clone(), alex)
			.await;
		assert_eq!(refused.status, StatusCode::BAD_REQUEST, "{body}");
	}
	let refused = api
		.call(
			"POST",
			"/api/users",
			json!({"name": "kim", "role": "member", "identities": ["CN=kim"]}),
			alex,
		)
		.await;
	assert_eq!(refused.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn the_model_files_into_a_private_area_only_for_its_members() {
	let house = household(None).await;
	let api = &house.api;
	api.private("Work", &["alex"]);
	for (who, name, area) in [
		(&house.sam, "memo-sam.pdf", "Unsorted"),
		(&house.alex, "memo-alex.pdf", "Work"),
	] {
		let uploaded = api
			.send(
				upload_request("/api/upload", name, &text_pdf(&[name])),
				As::Token(who),
			)
			.await;
		assert_eq!(uploaded.status, StatusCode::OK);
		let outcome = pipeline::ingest(&api.harness.app, std::path::Path::new(name))
			.await
			.unwrap();
		let Outcome::Filed { documents, .. } = outcome else {
			panic!("{outcome:?}");
		};
		assert!(documents[0].1.starts_with(area), "{name}: {documents:?}");
	}
	let entries = api.harness.entries();
	let ingests: Vec<_> = entries.iter().filter(|e| e.actor == "agent").collect();
	assert_eq!(
		ingests.last().unwrap().on_behalf_of.as_deref(),
		Some("alex")
	);
	assert_eq!(
		ingests[ingests.len() - 2].on_behalf_of.as_deref(),
		Some("sam")
	);
}

#[tokio::test]
async fn members_undo_their_own_changes_and_merge_within_one_audience() {
	let house = household(None).await;
	let api = &house.api;
	let sam = As::Token(&house.sam);
	let alex = As::Token(&house.alex);
	let edited = api
		.call(
			"PATCH",
			&format!("/api/documents/{}", house.public),
			json!({"title": "Power"}),
			alex,
		)
		.await;
	assert_eq!(edited.status, StatusCode::OK);
	let by_alex = edited.json()["entry"].as_str().unwrap().to_string();
	assert_eq!(api.harness.last_entry().actor, "user:alex");
	let activity = api.ok("/api/activity", sam).await;
	assert_eq!(activity[0]["entry"]["actor"], "user:alex");
	assert_eq!(activity[0]["undoable"], false);
	let refused = api
		.call(
			"POST",
			&format!("/api/activity/{by_alex}/undo"),
			json!({}),
			sam,
		)
		.await;
	assert_eq!(refused.status, StatusCode::FORBIDDEN);

	let undone = api
		.call(
			"POST",
			&format!("/api/activity/{by_alex}/undo"),
			json!({}),
			alex,
		)
		.await;
	assert_eq!(undone.status, StatusCode::OK);
	let edited = api
		.call(
			"PATCH",
			&format!("/api/documents/{}", house.public),
			json!({"title": "Bill"}),
			sam,
		)
		.await;
	let by_sam = edited.json()["entry"].as_str().unwrap().to_string();
	assert_eq!(api.harness.last_entry().actor, "user:sam");
	let undone = api
		.call(
			"POST",
			&format!("/api/activity/{by_sam}/undo"),
			json!({}),
			sam,
		)
		.await;
	assert_eq!(undone.status, StatusCode::OK, "{:?}", undone.json());

	let moved = api
		.call(
			"PATCH",
			&format!("/api/documents/{}", house.public),
			json!({"area": "Health"}),
			sam,
		)
		.await;
	assert_eq!(moved.status, StatusCode::BAD_REQUEST);
	let merged = api
		.call(
			"POST",
			"/api/merge",
			json!({"ids": [house.public, house.private]}),
			alex,
		)
		.await;
	assert_eq!(
		merged.status,
		StatusCode::BAD_REQUEST,
		"{:?}",
		merged.json()
	);
	assert!(
		merged.json()["error"]
			.as_str()
			.unwrap()
			.contains("cannot be merged")
	);
}

#[tokio::test]
async fn private_areas_and_users_files_are_reloaded_and_validated() {
	let house = household(None).await;
	let api = &house.api;
	let alex = As::Token(&house.alex);
	let refused = api
		.call(
			"PUT",
			"/api/areas",
			json!({"private": [{"area": "Health", "members": ["nobody"]}]}),
			alex,
		)
		.await;
	assert_eq!(refused.status, StatusCode::BAD_REQUEST);
	let set = api
		.call(
			"PUT",
			"/api/areas",
			json!({"private": [{"area": "Health", "members": ["alex", "sam"]}]}),
			alex,
		)
		.await;
	assert_eq!(set.status, StatusCode::OK, "{:?}", set.json());
	assert_eq!(api.titles(As::Token(&house.sam)).await.len(), 2);

	let areas_file = api.harness.root().join(AREAS_FILE);
	std::fs::write(
		&areas_file,
		r#"{"private": [{"area": "Housing", "members": []}]}"#,
	)
	.unwrap();
	assert_eq!(api.titles(As::Token(&house.sam)).await, ["Receipt"]);

	std::fs::write(api.harness.root().join(USERS_FILE), "{ broken").unwrap();
	let broken = api.get("/api/search", As::Token(&house.alex)).await;
	assert_eq!(broken.status, StatusCode::INTERNAL_SERVER_ERROR);
	assert_eq!(
		api.get("/api/search", As::Nobody).await.status,
		StatusCode::INTERNAL_SERVER_ERROR
	);
}

#[tokio::test]
async fn the_command_line_manages_users_and_prints_a_token_once() {
	let dir = tempfile::tempdir().unwrap();
	let root = dir.path().join("root");
	let attempt = |args: &[&str], stdin: &str| {
		let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_violet"))
			.arg("--library")
			.arg(&root)
			.args(args)
			.env("VIOLET_USER", "alex")
			.env_remove("VIOLET_LOGIN")
			.env_remove("VIOLET_PUBLIC_URL")
			.stdin(std::process::Stdio::piped())
			.stdout(std::process::Stdio::piped())
			.stderr(std::process::Stdio::piped())
			.spawn()
			.unwrap();
		use std::io::Write;
		child
			.stdin
			.take()
			.unwrap()
			.write_all(stdin.as_bytes())
			.unwrap();
		child.wait_with_output().unwrap()
	};
	let run_with = |args: &[&str], stdin: &str| {
		let output = attempt(args, stdin);
		assert!(
			output.status.success(),
			"{args:?}: {}",
			String::from_utf8_lossy(&output.stderr)
		);
		String::from_utf8(output.stdout).unwrap()
	};
	let run = |args: &[&str]| run_with(args, "");
	run(&["user", "add", "alex", "--role", "admin"]);
	run_with(
		&["user", "add", "sam", "--password-stdin"],
		"correct horse battery\n",
	);
	let refused = attempt(&["user", "password", "alex"], "short\n");
	assert!(!refused.status.success());
	assert!(
		String::from_utf8_lossy(&refused.stderr).contains("12 to 1024 bytes"),
		"{}",
		String::from_utf8_lossy(&refused.stderr)
	);
	run_with(&["user", "password", "alex"], "a long passphrase\r\n");
	let users: Value =
		serde_json::from_slice(&std::fs::read(root.join(USERS_FILE)).unwrap()).unwrap();
	for user in [0, 1] {
		let phc = users["users"][user]["password"].as_str().unwrap();
		assert!(phc.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"), "{phc}");
	}
	assert!(!users.to_string().contains("correct horse"));
	let listed = run(&["user", "list"]);
	assert!(listed.contains("sam\tMember\tpassword"), "{listed}");
	let refused = attempt(&["user", "add", "kim", "--identity", "CN=kim"], "");
	assert!(!refused.status.success());
	let token = run(&[
		"user",
		"token",
		"add",
		"alex",
		"scanner",
		"--scope",
		"write",
		"--expires",
		"2099-01-01",
	]);
	let token = token.trim();
	assert!(token.starts_with("vt_") && token.len() > 40, "{token}");
	let listed = run(&["user", "list"]);
	assert!(
		listed.contains("alex") && listed.contains("scanner"),
		"{listed}"
	);
	run(&["user", "token", "revoke", "alex", "scanner"]);
	let stored = std::fs::read_to_string(root.join(USERS_FILE)).unwrap();
	assert!(!stored.contains("scanner"));
	let journal = std::fs::read_to_string(root.join(".journal.jsonl")).unwrap();
	let actors: Vec<&str> = journal
		.lines()
		.map(|line| {
			if line.contains(r#""actor":"user:alex""#) {
				"alex"
			} else {
				"other"
			}
		})
		.collect();
	assert_eq!(
		actors,
		["other", "alex", "alex", "alex", "alex"],
		"{journal}"
	);
	assert!(
		journal
			.lines()
			.next()
			.unwrap()
			.contains(r#""actor":"user""#)
	);
}

#[tokio::test]
async fn a_near_duplicate_pair_is_listed_only_to_who_sees_both() {
	let house = household(None).await;
	let api = &house.api;
	let (alex, sam) = (As::Token(&house.alex), As::Token(&house.sam));
	let text = "Acme memo 2022-05-06 about moving the office on Friday";
	let shared = api.harness.one("memo.pdf", &text_pdf(&[text])).await;
	let hidden = api
		.harness
		.one("memo (2).pdf", &text_pdf(&[&format!("{text}.")]))
		.await;
	let pairs = |answer: Value| answer["pairs"].as_array().unwrap().len();
	let listed = api.ok("/api/duplicates", sam).await;
	assert_eq!(pairs(listed.clone()), 1);
	let copy = listed["pairs"][0]
		.as_array()
		.unwrap()
		.iter()
		.find(|copy| copy["id"] == shared.to_string())
		.unwrap()
		.clone();
	let document = api.ok(&format!("/api/documents/{shared}"), sam).await;
	assert_eq!(copy["size"], document["size"]);
	assert_eq!(copy["sha256"], document["sidecar"]["sha256"]);
	assert_eq!(copy["created_at"], document["sidecar"]["created_at"]);
	assert_eq!(copy["sources"], json!([{ "original_name": "memo.pdf" }]));
	let patch = Patch {
		area: Some("Health".into()),
		..Patch::default()
	};
	ops::update(&api.harness.app, hidden, patch).unwrap();

	assert_eq!(pairs(api.ok("/api/duplicates", alex).await), 1);
	assert_eq!(pairs(api.ok("/api/duplicates", sam).await), 0);
	let document = format!("/api/documents/{shared}");
	assert_eq!(
		api.ok(&document, alex).await["duplicates"],
		json!([hidden.to_string()])
	);
	assert_eq!(api.ok(&document, sam).await["duplicates"], json!([]));
	let before = api.harness.entries().len();
	let refused = api
		.call(
			"POST",
			&format!("{document}/not-duplicate"),
			json!({"of": hidden.to_string()}),
			sam,
		)
		.await;
	assert_eq!(refused.status, StatusCode::NOT_FOUND);
	assert_eq!(api.harness.entries().len(), before);
	let reader = api
		.call(
			"POST",
			&format!("{document}/not-duplicate"),
			json!({"of": hidden.to_string()}),
			As::Token(&house.sam_reader),
		)
		.await;
	assert_eq!(reader.status, StatusCode::FORBIDDEN);
}
