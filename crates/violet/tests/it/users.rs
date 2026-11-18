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
}

/// Who sends a request.
#[derive(Clone, Copy)]
enum As<'a> {
	Nobody,
	Token(&'a str),
	Header(&'a str, &'a str),
}

struct Household {
	api: Api,
	alex: String,
	sam: String,
	sam_reader: String,
	/// The pharmacy receipt, in Health, which only alex sees.
	private: String,
	/// The electricity bill, in Housing.
	public: String,
}

impl Api {
	async fn new(config: Option<AuthConfig>) -> Self {
		let fake = splitting_fake().await;
		let harness = Harness::new(fake.models());
		if let Some(config) = config {
			harness.app.auth.configure(config).unwrap();
		}
		harness.ingest("three.pdf", &three_pages()).await;
		let router = router(harness.app.clone());
		Self { harness, router }
	}

	async fn send(&self, mut request: Request<Body>, who: As<'_>) -> Answer {
		match who {
			As::Nobody => {}
			As::Token(token) => {
				request.headers_mut().insert(
					header::AUTHORIZATION,
					format!("Bearer {token}").parse().unwrap(),
				);
			}
			As::Header(peer, value) => {
				request
					.headers_mut()
					.insert("x-forwarded-client-cert", value.parse().unwrap());
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

	async fn get(&self, uri: &str, who: As<'_>) -> Answer {
		self.send(Request::get(uri).body(Body::empty()).unwrap(), who)
			.await
	}

	async fn call(&self, method: &str, uri: &str, body: Value, who: As<'_>) -> Answer {
		let request = Request::builder()
			.method(method)
			.uri(uri)
			.header(header::CONTENT_TYPE, "application/json")
			.body(Body::from(body.to_string()))
			.unwrap();
		self.send(request, who).await
	}

	async fn ok(&self, uri: &str, who: As<'_>) -> Value {
		let answer = self.get(uri, who).await;
		assert_eq!(answer.status, StatusCode::OK, "{uri}: {:?}", answer.json());
		answer.json()
	}

	async fn titles(&self, who: As<'_>) -> Vec<String> {
		let found = self.ok("/api/search", who).await;
		found["hits"]
			.as_array()
			.unwrap()
			.iter()
			.map(|hit| hit["title"].as_str().unwrap().to_string())
			.collect()
	}

	fn id(&self, title: &str) -> String {
		self.harness
			.documents()
			.into_iter()
			.find(|(_, sidecar)| sidecar.title == title)
			.unwrap_or_else(|| panic!("no {title}"))
			.1
			.id
			.to_string()
	}

	fn add_user(&self, name: &str, role: Role, identities: &[&str]) {
		let identities = identities.iter().map(|id| id.to_string()).collect();
		users::add(
			&self.harness.app,
			&User::implicit(),
			NO_CHECK,
			name,
			role,
			identities,
		)
		.unwrap();
	}

	fn token(&self, user: &str, name: &str, scope: Scope) -> String {
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

	fn private(&self, area: &str, members: &[&str]) {
		let areas = serde_json::from_value(json!({
			"private": [{"area": area, "members": members}]
		}))
		.unwrap();
		users::set_areas(&self.harness.app, &User::implicit(), areas).unwrap();
	}
}

fn upload_request(uri: &str, name: &str, bytes: &[u8]) -> Request<Body> {
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
async fn household(config: Option<AuthConfig>) -> Household {
	let api = Api::new(config).await;
	api.add_user("alex", Role::Admin, &["CN=alex-phone"]);
	api.add_user("sam", Role::Member, &["CN=sam-laptop", "sam@example.com"]);
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
		json!({"name": null, "role": "admin", "scope": "admin", "via": "implicit",
			"areas": ["Health", "Housing"], "users": false})
	);
	for who in [
		As::Nobody,
		As::Token("vt_not-a-token"),
		As::Header("127.0.0.1", "Subject=\"CN=alex-phone\""),
	] {
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

	let api = Api::new(Some(AuthConfig {
		default_user: Some("alex".into()),
		..AuthConfig::default()
	}))
	.await;
	assert_eq!(api.ok("/api/me", As::Nobody).await["name"], "alex");
	let added = api
		.call(
			"POST",
			"/api/users",
			json!({"name": "alex", "role": "admin"}),
			As::Nobody,
		)
		.await;
	assert_eq!(added.status, StatusCode::OK, "{:?}", added.json());
	let me = api.ok("/api/me", As::Nobody).await;
	assert_eq!(
		(me["name"].clone(), me["via"].clone(), me["users"].clone()),
		(json!("alex"), json!("default"), json!(true))
	);

	let demote = api
		.call(
			"PATCH",
			"/api/users/alex",
			json!({"role": "member"}),
			As::Nobody,
		)
		.await;
	assert_eq!(demote.status, StatusCode::CONFLICT);
	let token = api
		.call(
			"POST",
			"/api/users/alex/tokens",
			json!({"name": "cli", "scope": "admin"}),
			As::Nobody,
		)
		.await;
	assert_eq!(token.status, StatusCode::OK, "{:?}", token.json());
	assert!(token.json()["token"].as_str().unwrap().starts_with("vt_"));

	let entry = added.json()["entry"].as_str().unwrap().to_string();
	let blocked = api
		.call(
			"POST",
			&format!("/api/activity/{entry}/undo"),
			json!({}),
			As::Nobody,
		)
		.await;
	assert_eq!(
		blocked.status,
		StatusCode::CONFLICT,
		"a later change of the users file builds on it"
	);
}

#[tokio::test]
async fn a_client_certificate_counts_only_from_a_trusted_proxy() {
	let house = household(Some(AuthConfig {
		identity_header: Some("x-forwarded-client-cert".into()),
		trusted_proxies: vec!["10.0.0.0/8".parse().unwrap()],
		..AuthConfig::default()
	}))
	.await;
	let api = &house.api;
	let xfcc = r#"By=spiffe://violet;Hash=ab;Subject="CN=sam-laptop,O=Home";URI=spiffe://home/sam"#;
	let me = api.ok("/api/me", As::Header("10.1.2.3", xfcc)).await;
	assert_eq!(
		(me["name"].clone(), me["via"].clone()),
		(json!("sam"), json!("certificate"))
	);
	assert_eq!(
		api.get("/api/me", As::Header("192.168.1.9", xfcc))
			.await
			.status,
		StatusCode::UNAUTHORIZED
	);
	let unknown = api
		.get("/api/me", As::Header("10.1.2.3", "Subject=\"CN=stranger\""))
		.await;
	assert_eq!(unknown.status, StatusCode::UNAUTHORIZED);
	assert_eq!(
		api.ok(
			"/api/me",
			As::Header("10.1.2.3", "Subject=\"emailAddress=sam@example.com\"")
		)
		.await["name"],
		"sam"
	);
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
	let run = |args: &[&str]| {
		let output = std::process::Command::new(env!("CARGO_BIN_EXE_violet"))
			.arg("--library")
			.arg(&root)
			.args(args)
			.env("VIOLET_USER", "alex")
			.env_remove("VIOLET_DEFAULT_USER")
			.output()
			.unwrap();
		assert!(
			output.status.success(),
			"{args:?}: {}",
			String::from_utf8_lossy(&output.stderr)
		);
		String::from_utf8(output.stdout).unwrap()
	};
	run(&[
		"user",
		"add",
		"alex",
		"--role",
		"admin",
		"--identity",
		"CN=alex-phone",
	]);
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
	assert_eq!(actors, ["other", "alex", "alex"], "{journal}");
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
	assert_eq!(pairs(api.ok("/api/duplicates", sam).await), 1);
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
