use std::io::Write;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use kagi::Url;
use serde_json::{Value, json};
use time::OffsetDateTime;
use violet::auth::{
	AuthConfig, LoginMode, Role, Scope, StoredPasskey, USERS_FILE, User, passkey_credential,
};
use violet::login;
use violet::users;

use crate::users::{Answer, Api, As, Browser, ORIGIN};

pub const ALEX: &str = "alex's long passphrase";
pub const SAM: &str = "sam's long passphrase";

fn config(mode: LoginMode) -> AuthConfig {
	AuthConfig {
		mode,
		public_url: Some(Url::parse(ORIGIN).unwrap()),
		..AuthConfig::default()
	}
}

pub async fn api(mode: LoginMode) -> Api {
	Api::new(Some(config(mode))).await
}

/// alex (admin) and sam (member), both with a password.
pub async fn accounts(mode: LoginMode) -> Api {
	let api = api(mode).await;
	api.add_user("alex", Role::Admin);
	api.add_user("sam", Role::Member);
	api.set_password("alex", ALEX);
	api.set_password("sam", SAM);
	api
}

impl Api {
	pub async fn login(&self, name: &str, password: &str) -> Answer {
		self.call(
			"POST",
			"/api/auth/login",
			json!({"name": name, "password": password}),
			As::Page,
		)
		.await
	}

	pub async fn signed_in(&self, name: &str, password: &str) -> Browser {
		self.login(name, password).await.browser()
	}

	/// A session opened with a passkey, as a passkey sign-in leaves.
	fn passkey_session(&self, user: &str, id: u8) -> Browser {
		let sessions = self.harness.app.auth.sessions().unwrap();
		let (token, session) = login::open_session(
			sessions,
			user,
			passkey_credential(&[id; 16]),
			"test".into(),
			None,
			None,
		)
		.unwrap();
		Browser {
			cookie: token,
			csrf: session.csrf,
		}
	}

	fn add_passkey(&self, user: &str, name: &str, id: u8) {
		users::add_passkey(
			&self.harness.app,
			&User::implicit(),
			user,
			StoredPasskey {
				name: name.into(),
				created: OffsetDateTime::UNIX_EPOCH,
				credential: serde_json::from_value(passkey(id)).unwrap(),
			},
		)
		.unwrap();
	}

	pub fn users_file(&self) -> Value {
		serde_json::from_slice(&std::fs::read(self.harness.root().join(USERS_FILE)).unwrap())
			.unwrap()
	}
}

pub fn base64url(bytes: &[u8]) -> String {
	const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
	let mut out = String::new();
	for chunk in bytes.chunks(3) {
		let n = chunk
			.iter()
			.enumerate()
			.fold(0u32, |n, (at, byte)| n | u32::from(*byte) << (16 - 8 * at));
		for at in 0..=chunk.len() {
			out.push(ALPHABET[(n >> (18 - 6 * at) & 63) as usize] as char);
		}
	}
	out
}

/// A stored passkey whose public key is the P-256 base point: valid, though nobody holds its
/// private key.
fn passkey(id: u8) -> Value {
	json!({"cred": {
		"cred_id": base64url(&[id; 16]),
		"cred": {"type_": "ES256", "key": {"EC_EC2": {
			"curve": "SECP256R1",
			"x": "axfR8uEsQkf4vOblY6RA8ncDfYEt6zOg9KE5RdiYwpY",
			"y": "T-NC4v4af5uO5-tKfA-eFivOM1drMV7Oy7ZAaDe_UfU"
		}}},
		"counter": 0,
		"transports": null,
		"user_verified": true,
		"backup_eligible": false,
		"backup_state": false,
		"registration_policy": "required",
		"extensions": {},
		"attestation": {"data": "None", "metadata": "None"},
		"attestation_format": "none"
	}})
}

fn request(method: &str, uri: &str, body: Value, headers: &[(&str, &str)]) -> Request<Body> {
	let mut request = Request::builder()
		.method(method)
		.uri(uri)
		.header(header::CONTENT_TYPE, "application/json")
		.header(header::HOST, crate::users::HOST);
	for (name, value) in headers {
		request = request.header(*name, *value);
	}
	request.body(Body::from(body.to_string())).unwrap()
}

#[tokio::test]
async fn the_login_mode_decides_who_runs_a_request_without_credentials() {
	let me = async |api: &Api| {
		let answer = api.get("/api/me", As::Nobody).await;
		match answer.status {
			StatusCode::OK => answer.json()["via"].as_str().unwrap().to_string(),
			status => status.as_u16().to_string(),
		}
	};
	for (mode, without_file, without_credential, with_credential) in [
		(LoginMode::Off, "implicit", "401", "401"),
		(LoginMode::Optional, "implicit", "implicit", "implicit"),
		(LoginMode::Required, "implicit", "implicit", "401"),
	] {
		let api = api(mode).await;
		let state = |answer: Answer| {
			assert_eq!(answer.status, StatusCode::OK);
			let state = answer.json();
			(
				state["setup"].as_bool().unwrap(),
				state["user"]["via"].clone(),
			)
		};
		let on = mode != LoginMode::Off;
		assert_eq!(
			me(&api).await,
			without_file,
			"{mode:?} without a users file"
		);
		let answer = api.get("/api/auth", As::Nobody).await;
		assert_eq!(answer.json()["mode"], format!("{mode:?}").to_lowercase());
		assert_eq!(state(answer), (on, json!("implicit")));

		api.add_user("alex", Role::Admin);
		let token = api.token("alex", "cli", Scope::Admin);
		assert_eq!(
			me(&api).await,
			without_credential,
			"{mode:?} without a credential"
		);
		let answer = api.get("/api/auth", As::Nobody).await;
		let user = if without_credential == "401" {
			Value::Null
		} else {
			json!("implicit")
		};
		assert_eq!(state(answer), (on, user));

		api.set_password("alex", ALEX);
		assert_eq!(
			me(&api).await,
			with_credential,
			"{mode:?} with a credential"
		);
		let answer = api.get("/api/auth", As::Nobody).await;
		let user = if with_credential == "401" {
			Value::Null
		} else {
			json!("implicit")
		};
		assert_eq!(state(answer), (false, user));
		let answer = api.get("/api/auth", As::Token(&token)).await;
		assert_eq!(answer.json()["user"]["via"], "token");
		assert_eq!(answer.json()["user"]["csrf"], Value::Null);
		assert_eq!(api.ok("/api/me", As::Token(&token)).await["via"], "token");

		if mode == LoginMode::Off {
			for (method, uri) in [
				("POST", "/api/auth/login"),
				("POST", "/api/auth/setup/start"),
				("POST", "/api/auth/setup/finish"),
				("POST", "/api/auth/passkey/start"),
				("POST", "/api/auth/passkey/finish"),
				("POST", "/api/auth/logout"),
				("PUT", "/api/account/password"),
				("GET", "/api/account/passkeys"),
				("GET", "/api/account/sessions"),
				("DELETE", "/api/account/sessions/0123456789abcdef"),
			] {
				let answer = api.call(method, uri, json!({}), As::Token(&token)).await;
				assert_eq!(answer.status, StatusCode::NOT_FOUND, "{method} {uri}");
			}
			let session = api.passkey_session("alex", 1);
			let ignored = api.get("/api/me", As::Browser(&session)).await;
			assert_eq!(
				ignored.status,
				StatusCode::UNAUTHORIZED,
				"no cookies when off"
			);
		}
	}
}

#[tokio::test]
async fn a_password_login_opens_a_new_session_each_time() {
	let api = accounts(LoginMode::Required).await;
	let wrong = api.login("sam", "not sam's passphrase").await;
	let unknown = api.login("nobody", "not sam's passphrase").await;
	assert_eq!(wrong.status, StatusCode::UNAUTHORIZED);
	assert_eq!(wrong.body, unknown.body);
	assert_eq!(wrong.json(), json!({"error": "wrong name or password"}));
	assert!(wrong.cookie().is_none());
	let no_origin = api
		.call(
			"POST",
			"/api/auth/login",
			json!({"name": "sam", "password": SAM}),
			As::Nobody,
		)
		.await;
	assert_eq!(no_origin.status, StatusCode::FORBIDDEN);
	let elsewhere = api
		.send(
			request(
				"POST",
				"/api/auth/login",
				json!({"name": "sam", "password": SAM}),
				&[("origin", "https://evil.example")],
			),
			As::Nobody,
		)
		.await;
	assert_eq!(elsewhere.status, StatusCode::FORBIDDEN);

	let first = api.login("SAM", SAM).await;
	let me = first.json();
	assert_eq!(
		(&me["name"], &me["role"], &me["scope"], &me["via"]),
		(
			&json!("sam"),
			&json!("member"),
			&json!("write"),
			&json!("session")
		)
	);
	assert_eq!(me["password"], true);
	assert_eq!(me["passkeys"], json!([]));
	let (first, second) = (first.browser(), api.signed_in("sam", SAM).await);
	assert_ne!(first.cookie, second.cookie);
	assert_ne!(first.csrf, second.csrf);
	for browser in [&first, &second] {
		assert_eq!(api.ok("/api/me", As::Browser(browser)).await["name"], "sam");
		assert_eq!(api.ok("/api/me", As::Cookie(browser)).await["name"], "sam");
	}
	let forged = Browser {
		cookie: login::random_hex(),
		csrf: first.csrf.clone(),
	};
	assert_eq!(
		api.get("/api/me", As::Browser(&forged)).await.status,
		StatusCode::UNAUTHORIZED
	);
	let stored = std::fs::read(api.harness.root().join(".sessions.sqlite")).unwrap();
	let stored = String::from_utf8_lossy(&stored);
	assert!(!stored.contains(&first.cookie), "only the hash is stored");
}

#[tokio::test]
async fn a_cookie_request_that_changes_something_needs_the_csrf_token_and_an_origin() {
	let api = accounts(LoginMode::Required).await;
	let browser = api.signed_in("sam", SAM).await;
	let id = api.id("Electricity Bill");
	let uri = format!("/api/documents/{id}");
	let cookie = format!("__Host-violet_session={}", browser.cookie);
	let patch = |headers: Vec<(&'static str, String)>| {
		let mut all: Vec<(&str, &str)> = vec![("cookie", cookie.as_str())];
		all.extend(headers.iter().map(|(name, value)| (*name, value.as_str())));
		request("PATCH", &uri, json!({"title": "Power"}), &all)
	};
	let csrf = browser.csrf.clone();
	let cases = [
		(vec![], StatusCode::FORBIDDEN, "cookie alone"),
		(
			vec![("x-csrf-token", csrf.clone())],
			StatusCode::FORBIDDEN,
			"no Origin",
		),
		(
			vec![
				("x-csrf-token", csrf.clone()),
				("origin", "https://evil.example".into()),
			],
			StatusCode::FORBIDDEN,
			"another Origin",
		),
		(
			vec![
				("x-csrf-token", login::random_hex()),
				("origin", ORIGIN.into()),
			],
			StatusCode::FORBIDDEN,
			"another token",
		),
		(
			vec![
				("x-csrf-token", csrf[..63].to_string()),
				("origin", ORIGIN.into()),
			],
			StatusCode::FORBIDDEN,
			"a shorter token",
		),
		(
			vec![("origin", ORIGIN.into())],
			StatusCode::FORBIDDEN,
			"Origin without the token",
		),
		(
			vec![
				("x-csrf-token", csrf.clone()),
				("sec-fetch-site", "same-origin".into()),
			],
			StatusCode::OK,
			"same-origin fetch",
		),
		(
			vec![("x-csrf-token", csrf.clone()), ("origin", ORIGIN.into())],
			StatusCode::OK,
			"token and Origin",
		),
	];
	for (headers, status, case) in cases {
		let answer = api.send(patch(headers), As::Nobody).await;
		assert_eq!(answer.status, status, "{case}: {:?}", answer.json());
	}
	assert_eq!(api.harness.last_entry().actor, "user:sam");
	assert_eq!(
		api.get(&uri, As::Cookie(&browser)).await.status,
		StatusCode::OK,
		"reads need no token"
	);
	let token = api.token("sam", "phone", Scope::Write);
	let by_token = api
		.call("PATCH", &uri, json!({"title": "Bill"}), As::Token(&token))
		.await;
	assert_eq!(by_token.status, StatusCode::OK, "a bearer needs no Origin");
	let upload = api
		.send(
			crate::users::upload_request("/api/upload", "x.pdf", b"%PDF-1.4"),
			As::Cookie(&browser),
		)
		.await;
	assert_eq!(upload.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn too_many_wrong_passwords_wait_fifteen_minutes() {
	let api = Api::new(Some(AuthConfig {
		trusted_proxies: vec!["10.0.0.0/8".parse().unwrap()],
		..config(LoginMode::Required)
	}))
	.await;
	api.add_user("sam", Role::Member);
	api.set_password("sam", SAM);
	for _ in 0..5 {
		let answer = api.login("sam", "not sam's passphrase").await;
		assert_eq!(answer.status, StatusCode::UNAUTHORIZED);
	}
	let blocked = api.login("sam", SAM).await;
	assert_eq!(
		blocked.status,
		StatusCode::TOO_MANY_REQUESTS,
		"even the right one"
	);
	let wait: u64 = blocked.headers[header::RETRY_AFTER]
		.to_str()
		.unwrap()
		.parse()
		.unwrap();
	assert!((890..=900).contains(&wait), "{wait}");
	assert!(
		blocked.json()["error"]
			.as_str()
			.unwrap()
			.contains("15 minutes")
	);
	assert_eq!(
		api.login("SAM ", "x".repeat(12).as_str()).await.status,
		StatusCode::TOO_MANY_REQUESTS
	);

	let login_from = async |peer: &str, client: &str, name: &str| {
		api.call(
			"POST",
			"/api/auth/login",
			json!({"name": name, "password": "not the passphrase"}),
			As::Forwarded(peer, client),
		)
		.await
		.status
	};
	for at in 0..20 {
		let status = login_from("10.1.2.3", "203.0.113.7", &format!("user{at}")).await;
		assert_eq!(status, StatusCode::UNAUTHORIZED);
	}
	assert_eq!(
		login_from("10.9.9.9", "203.0.113.7", "kim").await,
		StatusCode::TOO_MANY_REQUESTS,
		"the client behind any trusted proxy"
	);
	assert_eq!(
		login_from("10.1.2.3", "203.0.113.8", "kim").await,
		StatusCode::UNAUTHORIZED,
		"another client"
	);
	assert_eq!(
		login_from("192.168.1.9", "203.0.113.7", "kim").await,
		StatusCode::UNAUTHORIZED,
		"an untrusted peer is the client itself"
	);
}

#[tokio::test]
async fn sessions_end_when_the_credential_they_were_opened_with_changes() {
	let api = accounts(LoginMode::Required).await;
	let alex = api.signed_in("alex", ALEX).await;
	let sam = api.signed_in("sam", SAM).await;
	let other = api.signed_in("sam", SAM).await;
	let change = async |browser: &Browser, body: Value| {
		api.call("PUT", "/api/account/password", body, As::Browser(browser))
			.await
	};
	let new = "sam's newer passphrase";
	assert_eq!(
		change(&sam, json!({"password": new})).await.status,
		StatusCode::BAD_REQUEST,
		"the current one is needed"
	);
	let wrong = change(&sam, json!({"current": "nope nope nope", "password": new})).await;
	assert_eq!(wrong.status, StatusCode::FORBIDDEN);
	assert_eq!(
		change(&sam, json!({"current": SAM, "password": "sam"}))
			.await
			.status,
		StatusCode::BAD_REQUEST
	);
	let changed = change(&sam, json!({"current": SAM, "password": new})).await;
	assert_eq!(changed.status, StatusCode::OK, "{:?}", changed.json());
	assert!(changed.json()["entry"].is_string());
	let renewed = Browser {
		cookie: changed.cookie().expect("a new session"),
		csrf: sam.csrf.clone(),
	};
	assert_ne!(renewed.cookie, sam.cookie);
	for gone in [&sam, &other] {
		assert_eq!(
			api.get("/api/me", As::Browser(gone)).await.status,
			StatusCode::UNAUTHORIZED
		);
	}
	assert_eq!(
		api.ok("/api/me", As::Browser(&renewed)).await["name"],
		"sam"
	);
	assert_eq!(
		api.call(
			"PATCH",
			&format!("/api/documents/{}", api.id("Electricity Bill")),
			json!({"title": "Power"}),
			As::Browser(&renewed),
		)
		.await
		.status,
		StatusCode::OK,
		"the CSRF token carries over"
	);
	assert_eq!(api.harness.last_entry().actor, "user:sam");
	let summaries: Vec<String> = api
		.harness
		.entries()
		.iter()
		.map(|e| e.summary.clone())
		.collect();
	assert!(summaries.contains(&"Changed the password of sam".to_string()));
	assert!(!summaries.iter().any(|summary| summary.contains(new)));
	assert_eq!(api.login("sam", SAM).await.status, StatusCode::UNAUTHORIZED);
	assert_eq!(api.ok("/api/me", As::Browser(&alex)).await["name"], "alex");

	let reset = "a reset passphrase";
	let by_member = api
		.call(
			"PUT",
			"/api/users/alex/password",
			json!({"password": reset}),
			As::Browser(&renewed),
		)
		.await;
	assert_eq!(by_member.status, StatusCode::FORBIDDEN);
	let unknown = api
		.call(
			"PUT",
			"/api/users/kim/password",
			json!({"password": reset}),
			As::Browser(&alex),
		)
		.await;
	assert_eq!(unknown.status, StatusCode::NOT_FOUND);
	let own = api
		.call(
			"PUT",
			"/api/users/alex/password",
			json!({"password": reset}),
			As::Browser(&alex),
		)
		.await;
	assert_eq!(
		own.status,
		StatusCode::CONFLICT,
		"it would sign the admin out"
	);
	let reset_answer = api
		.call(
			"PUT",
			"/api/users/sam/password",
			json!({"password": reset}),
			As::Browser(&alex),
		)
		.await;
	assert_eq!(
		reset_answer.status,
		StatusCode::OK,
		"{:?}",
		reset_answer.json()
	);
	assert_eq!(
		api.get("/api/me", As::Browser(&renewed)).await.status,
		StatusCode::UNAUTHORIZED
	);
	let after_reset = api.signed_in("sam", reset).await;
	let entry = reset_answer.json()["entry"].as_str().unwrap().to_string();
	let undone = api
		.call(
			"POST",
			&format!("/api/activity/{entry}/undo"),
			json!({}),
			As::Browser(&alex),
		)
		.await;
	assert_eq!(undone.status, StatusCode::OK, "{:?}", undone.json());
	assert_eq!(
		api.get("/api/me", As::Browser(&after_reset)).await.status,
		StatusCode::UNAUTHORIZED,
		"the undo brought back the password before"
	);
	let back = api.signed_in("sam", new).await;
	let removed = api
		.call("DELETE", "/api/users/sam", json!({}), As::Browser(&alex))
		.await;
	assert_eq!(removed.status, StatusCode::OK);
	assert_eq!(
		api.get("/api/me", As::Browser(&back)).await.status,
		StatusCode::UNAUTHORIZED
	);
	assert_eq!(api.login("sam", new).await.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn passkeys_are_listed_renamed_and_removed_but_never_the_last_credential() {
	let api = accounts(LoginMode::Required).await;
	api.add_passkey("alex", "phone", 1);
	api.add_passkey("alex", "laptop", 2);
	api.add_user("kim", Role::Member);
	api.add_passkey("kim", "key", 3);
	let stored = api.users_file();
	assert_eq!(stored["users"][0]["passkeys"][0]["name"], "phone");
	assert!(stored["users"][0]["passkeys"][0]["credential"]["cred"].is_object());

	let by_phone = api.passkey_session("alex", 1);
	let by_laptop = api.passkey_session("alex", 2);
	let me = api.ok("/api/me", As::Browser(&by_phone)).await;
	assert_eq!(
		(&me["via"], &me["passkeys"]),
		(&json!("session"), &json!(["phone", "laptop"]))
	);
	assert_eq!(
		api.ok("/api/account/passkeys", As::Browser(&by_phone))
			.await,
		json!([{"name": "phone", "created": "1970-01-01T00:00:00Z"},
			{"name": "laptop", "created": "1970-01-01T00:00:00Z"}])
	);
	let listed = api.ok("/api/users", As::Browser(&by_phone)).await;
	assert_eq!(listed["users"][0]["passkeys"], json!(["phone", "laptop"]));
	assert_eq!(listed["users"][0]["password"], true);
	assert_eq!(listed["users"][2]["password"], false);
	assert!(!listed.to_string().contains("cred_id"), "{listed}");

	let rename = async |from: &str, to: &str| {
		api.call(
			"PATCH",
			&format!("/api/account/passkeys/{from}"),
			json!({"name": to}),
			As::Browser(&by_phone),
		)
		.await
	};
	assert_eq!(rename("phone", "laptop").await.status, StatusCode::CONFLICT);
	assert_eq!(rename("tablet", "x").await.status, StatusCode::NOT_FOUND);
	assert_eq!(rename("phone", " x").await.status, StatusCode::BAD_REQUEST);
	let renamed = rename("phone", "Alex's phone").await;
	assert_eq!(renamed.status, StatusCode::OK, "{:?}", renamed.json());
	assert_eq!(
		api.harness.last_entry().summary,
		"Renamed passkey phone of alex to Alex's phone"
	);
	assert_eq!(
		api.ok("/api/me", As::Browser(&by_phone)).await["passkeys"],
		json!(["Alex's phone", "laptop"]),
		"a renamed passkey keeps its sessions"
	);

	let removed = api
		.call(
			"DELETE",
			"/api/account/passkeys/laptop",
			json!({}),
			As::Browser(&by_phone),
		)
		.await;
	assert_eq!(removed.status, StatusCode::OK, "{:?}", removed.json());
	assert_eq!(
		api.get("/api/me", As::Browser(&by_laptop)).await.status,
		StatusCode::UNAUTHORIZED
	);
	assert_eq!(
		api.ok("/api/me", As::Browser(&by_phone)).await["name"],
		"alex"
	);

	let kim = api.passkey_session("kim", 3);
	let last = api
		.call(
			"DELETE",
			"/api/account/passkeys/key",
			json!({}),
			As::Browser(&kim),
		)
		.await;
	assert_eq!(last.status, StatusCode::CONFLICT);
	assert!(
		last.json()["error"]
			.as_str()
			.unwrap()
			.contains("last way to sign in")
	);
	assert_eq!(
		api.ok("/api/me", As::Browser(&kim)).await["passkeys"],
		json!(["key"])
	);
	let set = api
		.call(
			"PUT",
			"/api/account/password",
			json!({"password": "kim's first passphrase"}),
			As::Browser(&kim),
		)
		.await;
	assert_eq!(set.status, StatusCode::OK, "no current password to give");
	let kim = Browser {
		cookie: set.cookie().unwrap(),
		csrf: kim.csrf,
	};
	let now_fine = api
		.call(
			"DELETE",
			"/api/account/passkeys/key",
			json!({}),
			As::Browser(&kim),
		)
		.await;
	assert_eq!(now_fine.status, StatusCode::OK, "{:?}", now_fine.json());
}

#[tokio::test]
async fn passkey_ceremonies_hand_out_options_and_refuse_what_they_cannot_verify() {
	let api = accounts(LoginMode::Required).await;
	api.add_passkey("alex", "phone", 1);
	let alex = api.signed_in("alex", ALEX).await;
	let other = api.signed_in("alex", ALEX).await;

	let start = api
		.call(
			"POST",
			"/api/account/passkeys/start",
			json!({"name": "laptop"}),
			As::Browser(&alex),
		)
		.await;
	assert_eq!(start.status, StatusCode::OK, "{:?}", start.json());
	let started = start.json();
	let ceremony = started["ceremony"].as_str().unwrap().to_string();
	assert_eq!(ceremony.len(), 64);
	let options = &started["options"]["publicKey"];
	assert_eq!(options["rp"]["id"], "violet.test");
	assert_eq!(options["user"]["name"], "alex");
	assert_eq!(options["attestation"], "none");
	assert_eq!(
		options["authenticatorSelection"]["userVerification"],
		"required"
	);
	assert!(options["challenge"].as_str().unwrap().len() >= 43);
	assert_eq!(
		options["excludeCredentials"][0]["id"],
		base64url(&[1; 16]),
		"{options}"
	);
	let same_name = api
		.call(
			"POST",
			"/api/account/passkeys/start",
			json!({"name": "PHONE"}),
			As::Browser(&alex),
		)
		.await;
	assert_eq!(same_name.status, StatusCode::CONFLICT);

	let finish = async |browser: &Browser, ceremony: &str, credential: Value| {
		api.call(
			"POST",
			"/api/account/passkeys/finish",
			json!({"ceremony": ceremony, "credential": credential}),
			As::Browser(browser),
		)
		.await
	};
	let garbage = json!({"id": "AAAA", "rawId": "AAAA", "type": "public-key",
		"response": {"attestationObject": "AAAA", "clientDataJSON": "AAAA"}, "extensions": {}});
	assert_eq!(
		finish(&other, &ceremony, garbage.clone()).await.status,
		StatusCode::BAD_REQUEST,
		"another session"
	);
	assert_eq!(
		finish(&alex, &ceremony, garbage.clone()).await.status,
		StatusCode::BAD_REQUEST,
		"used up by the attempt of the other session"
	);
	let ceremony = api
		.call(
			"POST",
			"/api/account/passkeys/start",
			json!({"name": "laptop"}),
			As::Browser(&alex),
		)
		.await
		.json()["ceremony"]
		.as_str()
		.unwrap()
		.to_string();
	let refused = finish(&alex, &ceremony, garbage.clone()).await;
	assert_eq!(
		refused.status,
		StatusCode::BAD_REQUEST,
		"{:?}",
		refused.json()
	);
	assert_eq!(
		finish(&alex, &ceremony, json!("not a credential"))
			.await
			.status,
		StatusCode::BAD_REQUEST
	);
	assert_eq!(
		api.users_file()["users"][0]["passkeys"]
			.as_array()
			.unwrap()
			.len(),
		1
	);

	let sign_in_start = async |name: &str, who: As<'_>| {
		api.call(
			"POST",
			"/api/auth/passkey/start",
			json!({"name": name}),
			who,
		)
		.await
	};
	let shape = |answer: &Answer| {
		assert_eq!(answer.status, StatusCode::OK, "{:?}", answer.json());
		let options = answer.json()["options"]["publicKey"].clone();
		let ids: Vec<String> = options["allowCredentials"]
			.as_array()
			.unwrap()
			.iter()
			.map(|credential| credential["id"].as_str().unwrap().to_string())
			.collect();
		let mut keys: Vec<String> = options.as_object().unwrap().keys().cloned().collect();
		keys.sort();
		(keys, ids)
	};
	let (unknown_keys, unknown_ids) = shape(&sign_in_start("nobody", As::Page).await);
	let (without_keys, without_ids) = shape(&sign_in_start("sam", As::Page).await);
	let (real_keys, _) = shape(&sign_in_start("alex", As::Page).await);
	assert_eq!(unknown_keys, real_keys, "a decoy looks like a real answer");
	assert_eq!(without_keys, real_keys);
	assert!(!unknown_ids.is_empty() && !without_ids.is_empty());
	assert_ne!(unknown_ids, without_ids);
	assert_eq!(
		shape(&sign_in_start("Nobody", As::Page).await).1,
		unknown_ids,
		"the same name gets the same decoy"
	);
	let decoy = sign_in_start("nobody", As::Page).await.json();
	let refused = api
		.call(
			"POST",
			"/api/auth/passkey/finish",
			json!({"ceremony": decoy["ceremony"], "credential": {
				"id": base64url(&[1; 16]), "rawId": base64url(&[1; 16]), "type": "public-key",
				"extensions": {}, "response": {"authenticatorData": "AAAA",
				"clientDataJSON": "AAAA", "signature": "AAAA"}}}),
			As::Page,
		)
		.await;
	assert_eq!(refused.status, StatusCode::UNAUTHORIZED);
	assert_eq!(refused.json(), json!({"error": "wrong name or passkey"}));
	assert_eq!(
		sign_in_start("alex", As::Nobody).await.status,
		StatusCode::FORBIDDEN,
		"no Origin"
	);
	let started = sign_in_start("alex", As::Page).await;
	assert_eq!(started.status, StatusCode::OK, "{:?}", started.json());
	let started = started.json();
	let options = &started["options"]["publicKey"];
	assert_eq!(options["allowCredentials"][0]["id"], base64url(&[1; 16]));
	assert_eq!(options["userVerification"], "required");
	assert_eq!(options["rpId"], "violet.test");
	let ceremony = started["ceremony"].as_str().unwrap();
	let assertion = json!({"id": base64url(&[1; 16]), "rawId": base64url(&[1; 16]),
		"type": "public-key", "extensions": {},
		"response": {"authenticatorData": "AAAA", "clientDataJSON": "AAAA", "signature": "AAAA"}});
	let sign_in_finish = async |ceremony: &str, credential: Value| {
		api.call(
			"POST",
			"/api/auth/passkey/finish",
			json!({"ceremony": ceremony, "credential": credential}),
			As::Page,
		)
		.await
	};
	let refused = sign_in_finish(ceremony, assertion.clone()).await;
	assert_eq!(refused.status, StatusCode::UNAUTHORIZED);
	assert_eq!(refused.json(), json!({"error": "wrong name or passkey"}));
	assert!(refused.cookie().is_none());
	assert_eq!(
		sign_in_finish(ceremony, assertion).await.status,
		StatusCode::UNAUTHORIZED,
		"once"
	);
	let fresh = sign_in_start("alex", As::Page).await.json()["ceremony"]
		.as_str()
		.unwrap()
		.to_string();
	assert_eq!(
		sign_in_finish(&fresh, json!({"not": "a credential"}))
			.await
			.status,
		StatusCode::UNAUTHORIZED
	);
	assert_eq!(
		sign_in_finish(&login::random_hex(), json!({})).await.status,
		StatusCode::UNAUTHORIZED
	);
}

#[tokio::test]
async fn account_endpoints_need_a_session() {
	let api = accounts(LoginMode::Optional).await;
	let token = api.token("alex", "cli", Scope::Admin);
	for who in [As::Nobody, As::Page, As::Token(&token)] {
		for (method, uri) in [
			("GET", "/api/account/passkeys"),
			("GET", "/api/account/sessions"),
			("PUT", "/api/account/password"),
			("POST", "/api/account/passkeys/start"),
		] {
			let answer = api.call(method, uri, json!({}), who).await;
			assert_eq!(answer.status, StatusCode::FORBIDDEN, "{method} {uri}");
			assert_eq!(answer.json()["error"], "sign in first");
		}
	}
	let api = accounts(LoginMode::Required).await;
	let answer = api.get("/api/account/sessions", As::Nobody).await;
	assert_eq!(answer.status, StatusCode::UNAUTHORIZED);
	let browser = api.signed_in("sam", SAM).await;
	let refused = api
		.call(
			"PUT",
			"/api/account/password",
			json!({"current": SAM, "password": "sam's newer passphrase"}),
			As::Cookie(&browser),
		)
		.await;
	assert_eq!(refused.status, StatusCode::FORBIDDEN, "CSRF");
	assert_eq!(api.login("sam", SAM).await.status, StatusCode::OK);
}

#[tokio::test]
async fn sessions_are_listed_and_ended_one_by_one() {
	let api = accounts(LoginMode::Required).await;
	let firefox = "Mozilla/5.0 (X11; Linux x86_64; rv:130.0) Gecko/20100101 Firefox/130.0";
	let with_agent = request(
		"POST",
		"/api/auth/login",
		json!({"name": "sam", "password": SAM}),
		&[("origin", ORIGIN), ("user-agent", firefox)],
	);
	let laptop = api.send(with_agent, As::Nobody).await.browser();
	let phone = api.signed_in("sam", SAM).await;
	let alex = api.signed_in("alex", ALEX).await;
	let listed = api.ok("/api/account/sessions", As::Browser(&laptop)).await;
	let listed = listed.as_array().unwrap();
	assert_eq!(listed.len(), 2, "{listed:?}");
	let current: Vec<&Value> = listed.iter().filter(|s| s["current"] == true).collect();
	assert_eq!(current.len(), 1);
	assert_eq!(current[0]["label"], "Firefox on Linux");
	assert_eq!(current[0]["credential"], "password");
	let other = listed.iter().find(|s| s["current"] == false).unwrap();
	assert_eq!(other["label"], "unknown browser");
	let id = other["id"].as_str().unwrap().to_string();
	assert_eq!(id.len(), 16);
	assert!(
		!listed
			.iter()
			.any(|s| s.to_string().contains(&laptop.cookie))
	);
	let created = OffsetDateTime::parse(
		other["created"].as_str().unwrap(),
		&time::format_description::well_known::Rfc3339,
	)
	.unwrap();
	let expires = OffsetDateTime::parse(
		other["expires"].as_str().unwrap(),
		&time::format_description::well_known::Rfc3339,
	)
	.unwrap();
	assert_eq!((expires - created).whole_days(), 30);

	let alex_id = api.ok("/api/account/sessions", As::Browser(&alex)).await[0]["id"]
		.as_str()
		.unwrap()
		.to_string();
	let end = async |id: &str, browser: &Browser| {
		api.call(
			"DELETE",
			&format!("/api/account/sessions/{id}"),
			json!({}),
			As::Browser(browser),
		)
		.await
	};
	assert_eq!(end(&alex_id, &laptop).await.status, StatusCode::NOT_FOUND);
	assert_eq!(end(&id[..8], &laptop).await.status, StatusCode::NOT_FOUND);
	let ended = end(&id, &laptop).await;
	assert_eq!(ended.status, StatusCode::NO_CONTENT);
	assert!(!ended.headers.contains_key(header::SET_COOKIE));
	assert_eq!(
		api.get("/api/me", As::Browser(&phone)).await.status,
		StatusCode::UNAUTHORIZED
	);
	assert_eq!(end(&id, &laptop).await.status, StatusCode::NOT_FOUND);
	let own = api.ok("/api/account/sessions", As::Browser(&laptop)).await[0]["id"]
		.as_str()
		.unwrap()
		.to_string();
	let ended = end(&own, &laptop).await;
	assert_eq!(ended.status, StatusCode::NO_CONTENT);
	assert!(
		ended.headers[header::SET_COOKIE]
			.to_str()
			.unwrap()
			.contains("Max-Age=0")
	);
	assert_eq!(
		api.get("/api/me", As::Browser(&laptop)).await.status,
		StatusCode::UNAUTHORIZED
	);
	assert_eq!(api.ok("/api/me", As::Browser(&alex)).await["name"], "alex");
}

#[tokio::test]
async fn logging_out_ends_the_session_and_clears_the_cookie() {
	let api = accounts(LoginMode::Required).await;
	let browser = api.signed_in("sam", SAM).await;
	let forged = api
		.call("POST", "/api/auth/logout", json!({}), As::Cookie(&browser))
		.await;
	assert_eq!(forged.status, StatusCode::FORBIDDEN);
	assert_eq!(
		api.ok("/api/me", As::Browser(&browser)).await["name"],
		"sam"
	);
	let out = api
		.call("POST", "/api/auth/logout", json!({}), As::Browser(&browser))
		.await;
	assert_eq!(out.status, StatusCode::NO_CONTENT);
	let cleared = out.headers[header::SET_COOKIE].to_str().unwrap();
	assert!(
		cleared.starts_with("__Host-violet_session=;") && cleared.contains("Max-Age=0"),
		"{cleared}"
	);
	assert_eq!(
		api.get("/api/me", As::Browser(&browser)).await.status,
		StatusCode::UNAUTHORIZED
	);
	let again = api
		.call("POST", "/api/auth/logout", json!({}), As::Browser(&browser))
		.await;
	assert_eq!(again.status, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn admins_add_users_with_a_password_and_see_who_has_what() {
	let api = accounts(LoginMode::Required).await;
	let alex = api.signed_in("alex", ALEX).await;
	let add = async |body: Value| {
		api.call("POST", "/api/users", body, As::Browser(&alex))
			.await
	};
	assert_eq!(
		add(json!({"name": "kim", "role": "member", "password": "kim"}))
			.await
			.status,
		StatusCode::BAD_REQUEST
	);
	let added = add(json!({"name": "kim", "role": "member", "password": "kim's passphrase"})).await;
	assert_eq!(added.status, StatusCode::OK, "{:?}", added.json());
	assert!(added.json()["entry"].is_string());
	let kim = api.signed_in("kim", "kim's passphrase").await;
	assert_eq!(api.ok("/api/me", As::Browser(&kim)).await["role"], "member");
	assert_eq!(
		add(json!({"name": "lee", "role": "member"})).await.status,
		StatusCode::OK
	);
	let listed = api.ok("/api/users", As::Browser(&alex)).await;
	let users: Vec<(Value, Value, Value)> = listed["users"]
		.as_array()
		.unwrap()
		.iter()
		.map(|user| {
			(
				user["name"].clone(),
				user["password"].clone(),
				user["passkeys"].clone(),
			)
		})
		.collect();
	assert_eq!(
		users,
		[
			(json!("alex"), json!(true), json!([])),
			(json!("sam"), json!(true), json!([])),
			(json!("kim"), json!(true), json!([])),
			(json!("lee"), json!(false), json!([])),
		]
	);
	assert!(!listed.to_string().contains("argon2"), "{listed}");
	let users = api.users_file();
	assert!(users["users"][3].get("password").is_none());
	assert!(users["users"][0].get("identities").is_none());
}

/// Collects what the server logs while a closure runs.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Write for Captured {
	fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
		self.0.lock().unwrap().extend_from_slice(bytes);
		Ok(bytes.len())
	}
	fn flush(&mut self) -> std::io::Result<()> {
		Ok(())
	}
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Captured {
	type Writer = Self;
	fn make_writer(&'a self) -> Self {
		self.clone()
	}
}

#[tokio::test]
async fn secrets_never_reach_the_logs() {
	let api = accounts(LoginMode::Required).await;
	api.add_passkey("alex", "phone", 1);
	let captured = Captured::default();
	let subscriber = tracing_subscriber::fmt()
		.with_max_level(tracing::Level::TRACE)
		.with_writer(captured.clone())
		.finish();
	let guard = tracing::subscriber::set_default(subscriber);
	let wrong = "a wrong passphrase";
	api.login("sam", wrong).await;
	let browser = api.signed_in("sam", SAM).await;
	api.call("POST", "/api/auth/logout", json!({}), As::Cookie(&browser))
		.await;
	let changed = api
		.call(
			"PUT",
			"/api/account/password",
			json!({"current": SAM, "password": "sam's newer passphrase"}),
			As::Browser(&browser),
		)
		.await;
	assert_eq!(changed.status, StatusCode::OK);
	let started = api
		.call(
			"POST",
			"/api/auth/passkey/start",
			json!({"name": "alex"}),
			As::Page,
		)
		.await
		.json();
	let ceremony = started["ceremony"].as_str().unwrap().to_string();
	api.call(
		"POST",
		"/api/auth/passkey/finish",
		json!({"ceremony": ceremony, "credential": {}}),
		As::Page,
	)
	.await;
	let token = api.token("sam", "phone", Scope::Read);
	api.get("/api/me", As::Token(&token)).await;
	drop(guard);
	let logs = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
	assert!(!logs.is_empty());
	for secret in [
		wrong,
		SAM,
		"sam's newer passphrase",
		&browser.cookie,
		&browser.csrf,
		&ceremony,
		&token,
	] {
		assert!(!logs.contains(secret), "{secret} in the logs:\n{logs}");
	}
}
