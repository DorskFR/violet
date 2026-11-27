use axum::http::{StatusCode, header};
use p256::ecdsa::signature::Signer;
use p256::ecdsa::{Signature, SigningKey};
use serde_json::{Value, json};
use violet::auth::{LoginMode, Role, Scope, USERS_FILE, password_credential};
use violet::login;

use crate::accounts::{ALEX, SAM, accounts, api, base64url};
use crate::users::{Answer, Api, As, Browser, ORIGIN};

const RP_ID: &str = "violet.test";
const UP: u8 = 0x01;
const UV: u8 = 0x04;
const AT: u8 = 0x40;
const ES256: i64 = -7;

fn sha256(bytes: &[u8]) -> Vec<u8> {
	let hex = violet_core::hash::sha256(bytes);
	(0..hex.len())
		.step_by(2)
		.map(|at| u8::from_str_radix(&hex[at..at + 2], 16).unwrap())
		.collect()
}

fn random<const N: usize>() -> [u8; N] {
	let mut bytes = [0u8; N];
	getrandom::fill(&mut bytes).unwrap();
	bytes
}

/// CBOR, as much of it as an authenticator needs.
mod cbor {
	pub fn head(major: u8, n: usize) -> Vec<u8> {
		let major = major << 5;
		match n {
			0..24 => vec![major | n as u8],
			24..256 => vec![major | 24, n as u8],
			256..65536 => vec![major | 25, (n >> 8) as u8, n as u8],
			_ => {
				let mut out = vec![major | 26];
				out.extend((n as u32).to_be_bytes());
				out
			}
		}
	}

	pub fn int(n: i64) -> Vec<u8> {
		if n >= 0 {
			head(0, n as usize)
		} else {
			head(1, (-1 - n) as usize)
		}
	}

	pub fn bytes(bytes: &[u8]) -> Vec<u8> {
		let mut out = head(2, bytes.len());
		out.extend_from_slice(bytes);
		out
	}

	pub fn text(text: &str) -> Vec<u8> {
		let mut out = head(3, text.len());
		out.extend_from_slice(text.as_bytes());
		out
	}

	pub fn map(entries: &[(Vec<u8>, Vec<u8>)]) -> Vec<u8> {
		let mut out = head(5, entries.len());
		for (key, value) in entries {
			out.extend_from_slice(key);
			out.extend_from_slice(value);
		}
		out
	}
}

/// An ASN.1 DER `SEQUENCE { INTEGER r, INTEGER s }`, as WebAuthn wants ES256 signatures.
fn der(signature: &Signature) -> Vec<u8> {
	let raw = signature.to_bytes();
	let integer = |half: &[u8]| {
		let mut value: Vec<u8> = half.iter().copied().skip_while(|byte| *byte == 0).collect();
		if value.first().is_none_or(|byte| byte & 0x80 != 0) {
			value.insert(0, 0);
		}
		let mut out = vec![0x02, value.len() as u8];
		out.extend(value);
		out
	};
	let body = [integer(&raw[..32]), integer(&raw[32..])].concat();
	let mut out = vec![0x30, body.len() as u8];
	out.extend(body);
	out
}

/// How an authenticator and its browser answer a ceremony, right or wrong in one way.
#[derive(Clone)]
struct Wire {
	origin: &'static str,
	rp_id: &'static str,
	flags: u8,
	tamper: bool,
}

impl Default for Wire {
	fn default() -> Self {
		Self {
			origin: ORIGIN,
			rp_id: RP_ID,
			flags: UP | UV,
			tamper: false,
		}
	}
}

/// A platform authenticator in software: one ES256 key pair and its credential id.
struct Authenticator {
	key: SigningKey,
	id: [u8; 16],
	user_handle: Option<String>,
	counter: std::cell::Cell<u32>,
}

impl Authenticator {
	fn new() -> Self {
		let key = loop {
			if let Ok(key) = SigningKey::from_slice(&random::<32>()) {
				break key;
			}
		};
		Self {
			key,
			id: random(),
			user_handle: None,
			counter: std::cell::Cell::new(0),
		}
	}

	fn point(&self) -> (Vec<u8>, Vec<u8>) {
		let point = self.key.verifying_key().to_sec1_point(false);
		let bytes = point.as_bytes();
		assert_eq!(bytes.len(), 65);
		(bytes[1..33].to_vec(), bytes[33..].to_vec())
	}

	fn client_data(kind: &str, options: &Value, wire: &Wire) -> Vec<u8> {
		json!({
			"type": kind,
			"challenge": options["publicKey"]["challenge"],
			"origin": wire.origin,
			"crossOrigin": false,
		})
		.to_string()
		.into_bytes()
	}

	/// A browser creating a passkey, then `toJSON()`, as Chrome shapes it.
	fn create(&mut self, options: &Value, wire: &Wire) -> Value {
		self.user_handle = options["publicKey"]["user"]["id"]
			.as_str()
			.map(str::to_string);
		let (x, y) = self.point();
		let cose = cbor::map(&[
			(cbor::int(1), cbor::int(2)),
			(cbor::int(3), cbor::int(ES256)),
			(cbor::int(-1), cbor::int(1)),
			(cbor::int(-2), cbor::bytes(&x)),
			(cbor::int(-3), cbor::bytes(&y)),
		]);
		let mut data = sha256(wire.rp_id.as_bytes());
		data.push(wire.flags | AT);
		data.extend(0u32.to_be_bytes());
		data.extend([0u8; 16]);
		data.extend((self.id.len() as u16).to_be_bytes());
		data.extend(self.id);
		data.extend(cose);
		let attestation = cbor::map(&[
			(cbor::text("fmt"), cbor::text("none")),
			(cbor::text("attStmt"), cbor::map(&[])),
			(cbor::text("authData"), cbor::bytes(&data)),
		]);
		let spki = [
			&[
				0x30, 0x59, 0x30, 0x13, 0x06, 0x07, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01, 0x06,
				0x08, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07, 0x03, 0x42, 0x00, 0x04,
			][..],
			&x,
			&y,
		]
		.concat();
		json!({
			"id": base64url(&self.id),
			"rawId": base64url(&self.id),
			"type": "public-key",
			"authenticatorAttachment": "platform",
			"clientExtensionResults": {"credProps": {"rk": true}},
			"response": {
				"clientDataJSON": base64url(&Self::client_data("webauthn.create", options, wire)),
				"attestationObject": base64url(&attestation),
				"transports": ["hybrid", "internal"],
				"publicKey": base64url(&spki),
				"publicKeyAlgorithm": ES256,
				"authenticatorData": base64url(&data),
			},
		})
	}

	/// A browser signing in with the passkey, then `toJSON()`, as Chrome shapes it.
	fn get(&self, options: &Value, wire: &Wire) -> Value {
		let client = Self::client_data("webauthn.get", options, wire);
		let mut data = sha256(wire.rp_id.as_bytes());
		data.push(wire.flags);
		data.extend(self.counter.get().to_be_bytes());
		let signed = [data.clone(), sha256(&client)].concat();
		let signature: Signature = self.key.sign(&signed);
		let mut signature = der(&signature);
		if wire.tamper {
			*signature.last_mut().unwrap() ^= 0x01;
		}
		json!({
			"id": base64url(&self.id),
			"rawId": base64url(&self.id),
			"type": "public-key",
			"authenticatorAttachment": "platform",
			"clientExtensionResults": {},
			"response": {
				"clientDataJSON": base64url(&client),
				"authenticatorData": base64url(&data),
				"signature": base64url(&signature),
				"userHandle": self.user_handle,
			},
		})
	}
}

impl Api {
	async fn register(
		&self,
		browser: &Browser,
		authenticator: &mut Authenticator,
		name: &str,
		wire: &Wire,
	) -> Answer {
		let started = self
			.call(
				"POST",
				"/api/account/passkeys/start",
				json!({"name": name}),
				As::Browser(browser),
			)
			.await;
		assert_eq!(started.status, StatusCode::OK, "{:?}", started.json());
		let started = started.json();
		let credential = authenticator.create(&started["options"], wire);
		self.call(
			"POST",
			"/api/account/passkeys/finish",
			json!({"ceremony": started["ceremony"], "credential": credential}),
			As::Browser(browser),
		)
		.await
	}

	async fn passkey_sign_in(
		&self,
		name: &str,
		authenticator: &Authenticator,
		wire: &Wire,
	) -> Answer {
		let started = self
			.call(
				"POST",
				"/api/auth/passkey/start",
				json!({"name": name}),
				As::Page,
			)
			.await;
		assert_eq!(started.status, StatusCode::OK, "{:?}", started.json());
		let started = started.json();
		let credential = authenticator.get(&started["options"], wire);
		self.call(
			"POST",
			"/api/auth/passkey/finish",
			json!({"ceremony": started["ceremony"], "credential": credential}),
			As::Page,
		)
		.await
	}

	fn setup_token(&self) -> String {
		self.harness.app.auth.setup_token().unwrap()
	}

	async fn set_up(&self, name: &str, authenticator: &mut Authenticator) -> Answer {
		let token = self.setup_token();
		let started = self
			.call(
				"POST",
				"/api/auth/setup/start",
				json!({"name": name, "token": token}),
				As::Page,
			)
			.await;
		if started.status != StatusCode::OK {
			return started;
		}
		let started = started.json();
		let credential = authenticator.create(&started["options"], &Wire::default());
		self.call(
			"POST",
			"/api/auth/setup/finish",
			json!({"ceremony": started["ceremony"], "credential": credential, "token": token}),
			As::Page,
		)
		.await
	}

	/// A sign-in where the authenticator names the user.
	async fn discover(&self, authenticator: &Authenticator) -> Answer {
		let started = self
			.call("POST", "/api/auth/passkey/start", json!({}), As::Page)
			.await;
		assert_eq!(started.status, StatusCode::OK, "{:?}", started.json());
		let started = started.json();
		let credential = authenticator.get(&started["options"], &Wire::default());
		self.call(
			"POST",
			"/api/auth/passkey/finish",
			json!({"ceremony": started["ceremony"], "credential": credential}),
			As::Page,
		)
		.await
	}

	fn stored_passkeys(&self, user: usize) -> usize {
		self.users_file()["users"][user]["passkeys"]
			.as_array()
			.map_or(0, Vec::len)
	}
}

#[tokio::test]
async fn a_passkey_registers_then_signs_in_to_a_working_session() {
	let api = accounts(LoginMode::Required).await;
	let alex = api.signed_in("alex", ALEX).await;
	let mut phone = Authenticator::new();

	for (wire, case) in [
		(
			Wire {
				origin: "https://evil.example",
				..Wire::default()
			},
			"another origin",
		),
		(
			Wire {
				rp_id: "evil.example",
				..Wire::default()
			},
			"another relying party",
		),
		(
			Wire {
				flags: UP,
				..Wire::default()
			},
			"no user verification",
		),
	] {
		let refused = api.register(&alex, &mut phone, "phone", &wire).await;
		assert_eq!(
			refused.status,
			StatusCode::BAD_REQUEST,
			"{case}: {:?}",
			refused.json()
		);
		assert_eq!(
			refused.json(),
			json!({"error": "the passkey could not be registered"})
		);
	}
	assert_eq!(api.stored_passkeys(0), 0);

	let registered = api
		.register(&alex, &mut phone, "phone", &Wire::default())
		.await;
	assert_eq!(registered.status, StatusCode::OK, "{:?}", registered.json());
	let answer = registered.json();
	assert!(answer["entry"].is_string());
	assert_eq!(answer["passkey"]["name"], "phone");
	assert!(
		answer["passkey"]["created"]
			.as_str()
			.unwrap()
			.ends_with('Z')
	);
	assert_eq!(
		api.harness.last_entry().summary,
		"Added passkey phone of alex"
	);
	let stored = &api.users_file()["users"][0]["passkeys"][0];
	assert_eq!(
		stored["credential"]["cred"]["cred_id"],
		base64url(&phone.id)
	);
	assert_eq!(stored["credential"]["cred"]["user_verified"], true);
	assert_eq!(
		api.ok("/api/me", As::Browser(&alex)).await["passkeys"],
		json!(["phone"]),
		"adding a passkey keeps the password session"
	);
	let again = api
		.register(&alex, &mut phone, "again", &Wire::default())
		.await;
	assert_eq!(
		again.status,
		StatusCode::BAD_REQUEST,
		"the same credential is excluded"
	);

	let before = std::fs::read(api.harness.root().join(violet::auth::USERS_FILE)).unwrap();
	let signed_in = api.passkey_sign_in("alex", &phone, &Wire::default()).await;
	assert_eq!(signed_in.status, StatusCode::OK, "{:?}", signed_in.json());
	let me = signed_in.json();
	assert_eq!(
		(&me["name"], &me["via"], &me["passkeys"], &me["password"]),
		(
			&json!("alex"),
			&json!("session"),
			&json!(["phone"]),
			&json!(true)
		)
	);
	let cookie = signed_in.headers[header::SET_COOKIE].to_str().unwrap();
	assert!(
		cookie.contains("HttpOnly") && cookie.contains("Secure"),
		"{cookie}"
	);
	let by_passkey = signed_in.browser();
	assert_ne!(by_passkey.cookie, alex.cookie);
	assert_eq!(
		api.ok("/api/me", As::Browser(&by_passkey)).await["name"],
		"alex"
	);
	let edited = api
		.call(
			"PATCH",
			&format!("/api/documents/{}", api.id("Electricity Bill")),
			json!({"title": "Power"}),
			As::Browser(&by_passkey),
		)
		.await;
	assert_eq!(edited.status, StatusCode::OK, "{:?}", edited.json());
	assert_eq!(api.harness.last_entry().actor, "user:alex");
	let sessions = api
		.ok("/api/account/sessions", As::Browser(&by_passkey))
		.await;
	let current = sessions
		.as_array()
		.unwrap()
		.iter()
		.find(|session| session["current"] == true)
		.unwrap();
	assert_eq!(current["credential"], "passkey");
	let after = std::fs::read(api.harness.root().join(violet::auth::USERS_FILE)).unwrap();
	assert_eq!(before, after, "a sign-in never writes the users file");
	let second = api.passkey_sign_in("ALEX", &phone, &Wire::default()).await;
	assert_eq!(second.status, StatusCode::OK, "{:?}", second.json());

	let password_changed = api
		.call(
			"PUT",
			"/api/account/password",
			json!({"current": ALEX, "password": "alex's newer passphrase"}),
			As::Browser(&alex),
		)
		.await;
	assert_eq!(password_changed.status, StatusCode::OK);
	assert_eq!(
		api.ok("/api/me", As::Browser(&by_passkey)).await["name"],
		"alex",
		"a passkey session outlives a password change"
	);
}

/// alex and sam, each with a password and a passkey called phone.
async fn with_phones() -> (Api, Browser, Authenticator, Authenticator) {
	let api = accounts(LoginMode::Required).await;
	let alex = api.signed_in("alex", ALEX).await;
	let sam = api.signed_in("sam", SAM).await;
	let mut phone = Authenticator::new();
	let mut sams = Authenticator::new();
	for (browser, authenticator) in [(&alex, &mut phone), (&sam, &mut sams)] {
		let answer = api
			.register(browser, authenticator, "phone", &Wire::default())
			.await;
		assert_eq!(answer.status, StatusCode::OK, "{:?}", answer.json());
	}
	(api, alex, phone, sams)
}

#[tokio::test]
async fn an_assertion_that_does_not_verify_is_refused() {
	let cases = [
		(
			false,
			Wire {
				origin: "https://evil.example",
				..Wire::default()
			},
			"another origin",
		),
		(
			false,
			Wire {
				origin: "https://violet.test:8080",
				..Wire::default()
			},
			"another port",
		),
		(
			false,
			Wire {
				rp_id: "evil.example",
				..Wire::default()
			},
			"another relying party id hash",
		),
		(
			false,
			Wire {
				tamper: true,
				..Wire::default()
			},
			"a tampered signature",
		),
		(
			false,
			Wire {
				flags: UP,
				..Wire::default()
			},
			"no user verification",
		),
		(true, Wire::default(), "the passkey of another user"),
	];
	for (sams_key, wire, case) in cases {
		let (api, _, phone, sams) = with_phones().await;
		let authenticator = if sams_key { &sams } else { &phone };
		let refused = api.passkey_sign_in("alex", authenticator, &wire).await;
		assert_eq!(
			refused.status,
			StatusCode::UNAUTHORIZED,
			"{case}: {:?}",
			refused.json()
		);
		assert_eq!(
			refused.json(),
			json!({"error": "wrong name or passkey"}),
			"{case}"
		);
		assert!(refused.cookie().is_none(), "{case}");
	}
	let (api, alex, phone, sams) = with_phones().await;
	let unknown = api
		.passkey_sign_in("alex", &Authenticator::new(), &Wire::default())
		.await;
	assert_eq!(
		unknown.status,
		StatusCode::UNAUTHORIZED,
		"an unregistered key"
	);

	let started = api
		.call(
			"POST",
			"/api/auth/passkey/start",
			json!({"name": "alex"}),
			As::Page,
		)
		.await
		.json();
	let credential = phone.get(&started["options"], &Wire::default());
	let finish = async || {
		api.call(
			"POST",
			"/api/auth/passkey/finish",
			json!({"ceremony": started["ceremony"], "credential": credential}),
			As::Page,
		)
		.await
	};
	assert_eq!(finish().await.status, StatusCode::OK);
	assert_eq!(
		finish().await.status,
		StatusCode::UNAUTHORIZED,
		"a replayed assertion"
	);

	let removed = api
		.call(
			"DELETE",
			"/api/account/passkeys/phone",
			json!({}),
			As::Browser(&alex),
		)
		.await;
	assert_eq!(removed.status, StatusCode::OK);
	assert_eq!(
		api.passkey_sign_in("alex", &phone, &Wire::default())
			.await
			.status,
		StatusCode::UNAUTHORIZED,
		"no passkey left"
	);
	assert_eq!(
		api.passkey_sign_in("sam", &sams, &Wire::default())
			.await
			.status,
		StatusCode::OK
	);
}

#[tokio::test]
async fn the_first_admin_signs_up_with_a_name_and_a_passkey() {
	let api = api(LoginMode::Required).await;
	let token = api.setup_token();
	assert_eq!(
		std::fs::read_to_string(api.harness.root().join(".setup-token"))
			.unwrap()
			.trim(),
		token
	);
	let setup =
		async |body: Value, who: As<'_>| api.call("POST", "/api/auth/setup/start", body, who).await;
	let refused = setup(json!({"name": "alex", "token": token}), As::Nobody).await;
	assert_eq!(refused.status, StatusCode::FORBIDDEN, "no Origin");
	let bad = setup(json!({"name": "a b", "token": token}), As::Page).await;
	assert_eq!(bad.status, StatusCode::BAD_REQUEST);
	let password = setup(
		json!({"name": "alex", "token": token, "password": ALEX}),
		As::Page,
	)
	.await;
	assert_eq!(password.status, StatusCode::BAD_REQUEST, "no password here");
	let tokenless = setup(json!({"name": "alex"}), As::Page).await;
	assert_eq!(
		tokenless.status,
		StatusCode::BAD_REQUEST,
		"the token is needed"
	);
	for wrong in ["", "   ", "not the token"] {
		let refused = setup(json!({"name": "alex", "token": wrong}), As::Page).await;
		assert_eq!(refused.status, StatusCode::FORBIDDEN, "{wrong:?}");
		assert_eq!(refused.json(), json!({"error": "the setup token is wrong"}));
	}

	let started = setup(json!({"name": "alex", "token": token}), As::Page)
		.await
		.json();
	let options = &started["options"]["publicKey"];
	assert_eq!(options["user"]["name"], "alex");
	assert_eq!(
		options["authenticatorSelection"]["residentKey"],
		"preferred"
	);
	assert_eq!(
		options["authenticatorSelection"]["userVerification"],
		"required"
	);
	let mut phone = Authenticator::new();
	let tampered = phone.create(
		&started["options"],
		&Wire {
			origin: "https://evil.example",
			..Wire::default()
		},
	);
	let refused = api
		.call(
			"POST",
			"/api/auth/setup/finish",
			json!({"ceremony": started["ceremony"], "credential": tampered, "token": token}),
			As::Page,
		)
		.await;
	assert_eq!(refused.status, StatusCode::BAD_REQUEST);
	assert!(!api.harness.root().join(USERS_FILE).exists());
	let started = setup(json!({"name": "alex", "token": token}), As::Page)
		.await
		.json();
	let credential = phone.create(&started["options"], &Wire::default());
	let without = api
		.call(
			"POST",
			"/api/auth/setup/finish",
			json!({"ceremony": started["ceremony"], "credential": credential, "token": "other"}),
			As::Page,
		)
		.await;
	assert_eq!(
		without.status,
		StatusCode::FORBIDDEN,
		"the finish needs the token too"
	);
	assert!(!api.harness.root().join(USERS_FILE).exists());

	let done = api.set_up("alex", &mut phone).await;
	assert_eq!(done.status, StatusCode::OK, "{:?}", done.json());
	let cookie = done.headers[header::SET_COOKIE]
		.to_str()
		.unwrap()
		.to_string();
	for part in ["HttpOnly", "Secure", "SameSite=Lax", "Path=/"] {
		assert!(cookie.contains(part), "{cookie}");
	}
	let me = done.json();
	assert_eq!(
		(
			&me["name"],
			&me["role"],
			&me["via"],
			&me["password"],
			&me["passkeys"]
		),
		(
			&json!("alex"),
			&json!("admin"),
			&json!("session"),
			&json!(false),
			&json!(["unknown browser"])
		)
	);
	let browser = done.browser();
	assert!(
		!api.harness.root().join(".setup-token").exists(),
		"a used setup token is gone"
	);
	let entry = api.harness.last_entry();
	assert_eq!(
		(entry.actor.as_str(), entry.summary.as_str()),
		("user", "Set up the admin alex")
	);
	let stored = &api.users_file()["users"][0];
	assert_eq!(stored["role"], "admin");
	assert!(stored.get("password").is_none(), "{stored}");
	assert_eq!(
		stored["passkeys"][0]["credential"]["cred"]["cred_id"],
		base64url(&phone.id)
	);
	let sessions = api.ok("/api/account/sessions", As::Browser(&browser)).await;
	assert_eq!(sessions[0]["credential"], "passkey");

	let state = api.ok("/api/auth", As::Nobody).await;
	assert_eq!(
		(&state["setup"], &state["user"]),
		(&json!(false), &Value::Null)
	);
	assert_eq!(
		api.get("/api/me", As::Nobody).await.status,
		StatusCode::UNAUTHORIZED,
		"signing in is required once someone can"
	);
	let again = api.set_up("sam", &mut Authenticator::new()).await;
	assert_eq!(
		again.status,
		StatusCode::FORBIDDEN,
		"nobody is the implicit admin any more"
	);

	let by_name = api.passkey_sign_in("alex", &phone, &Wire::default()).await;
	assert_eq!(by_name.status, StatusCode::OK, "{:?}", by_name.json());
	let discovered = api.discover(&phone).await;
	assert_eq!(discovered.status, StatusCode::OK, "{:?}", discovered.json());
	assert_eq!(discovered.json()["name"], "alex");
	assert_eq!(
		api.discover(&Authenticator::new()).await.status,
		StatusCode::UNAUTHORIZED,
		"a passkey nobody registered"
	);
}

#[tokio::test]
async fn setting_up_claims_an_account_without_a_credential_once() {
	let api = api(LoginMode::Optional).await;
	api.add_user("alex", Role::Member);
	let token = api.token("alex", "cli", Scope::Admin);
	let setup_token = api.setup_token();
	let by_token = api
		.call(
			"POST",
			"/api/auth/setup/start",
			json!({"name": "alex", "token": setup_token}),
			As::Token(&token),
		)
		.await;
	assert_eq!(
		by_token.status,
		StatusCode::FORBIDDEN,
		"{:?}",
		by_token.json()
	);

	let start = async |name: &str| {
		api.call(
			"POST",
			"/api/auth/setup/start",
			json!({"name": name, "token": setup_token}),
			As::Page,
		)
		.await
		.json()
	};
	let (first, second) = (start("alex").await, start("lee").await);
	let (mut a, mut b) = (Authenticator::new(), Authenticator::new());
	let finish = async |started: &Value, authenticator: &mut Authenticator| {
		let credential = authenticator.create(&started["options"], &Wire::default());
		api.call(
			"POST",
			"/api/auth/setup/finish",
			json!({"ceremony": started["ceremony"], "credential": credential, "token": setup_token}),
			As::Page,
		)
		.await
	};
	let claimed = finish(&first, &mut a).await;
	assert_eq!(claimed.status, StatusCode::OK, "{:?}", claimed.json());
	let stored = &api.users_file()["users"];
	assert_eq!(stored.as_array().unwrap().len(), 1);
	assert_eq!(stored[0]["role"], "admin");
	assert_eq!(
		stored[0]["tokens"][0]["name"], "cli",
		"the account keeps its tokens"
	);
	let late = finish(&second, &mut b).await;
	assert_eq!(late.status, StatusCode::CONFLICT, "set up meanwhile");
	assert_eq!(api.users_file()["users"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn a_password_account_adds_a_passkey_without_naming_it() {
	let api = accounts(LoginMode::Required).await;
	let alex = api.signed_in("alex", ALEX).await;
	let (mut phone, mut laptop) = (Authenticator::new(), Authenticator::new());
	for authenticator in [&mut phone, &mut laptop] {
		let started = api
			.call(
				"POST",
				"/api/account/passkeys/start",
				json!({}),
				As::Browser(&alex),
			)
			.await;
		assert_eq!(started.status, StatusCode::OK, "{:?}", started.json());
		let started = started.json();
		assert_eq!(
			started["options"]["publicKey"]["authenticatorSelection"]["residentKey"],
			"preferred"
		);
		let credential = authenticator.create(&started["options"], &Wire::default());
		let finished = api
			.call(
				"POST",
				"/api/account/passkeys/finish",
				json!({"ceremony": started["ceremony"], "credential": credential}),
				As::Browser(&alex),
			)
			.await;
		assert_eq!(finished.status, StatusCode::OK, "{:?}", finished.json());
	}
	let me = api.ok("/api/me", As::Browser(&alex)).await;
	assert_eq!(
		(&me["password"], &me["passkeys"]),
		(
			&json!(true),
			&json!(["unknown browser", "unknown browser 2"])
		)
	);
	assert_eq!(
		api.login("alex", ALEX).await.status,
		StatusCode::OK,
		"the password still works"
	);
	let discovered = api.discover(&laptop).await;
	assert_eq!(discovered.json()["name"], "alex");
}

#[tokio::test]
async fn a_sign_in_writes_the_counter_back_and_refuses_a_cloned_key() {
	let (api, _, phone, _) = with_phones().await;
	phone.counter.set(7);
	let signed_in = api.passkey_sign_in("alex", &phone, &Wire::default()).await;
	assert_eq!(signed_in.status, StatusCode::OK, "{:?}", signed_in.json());
	assert_eq!(
		api.users_file()["users"][0]["passkeys"][0]["credential"]["cred"]["counter"],
		7
	);
	let entry = api.harness.last_entry();
	assert_eq!(
		(
			entry.actor.as_str(),
			entry.on_behalf_of.as_deref(),
			entry.summary.as_str()
		),
		(
			"agent",
			Some("alex"),
			"Updated the sign-in counter of passkey phone of alex"
		)
	);
	phone.counter.set(8);
	let discovered = api.discover(&phone).await;
	assert_eq!(discovered.status, StatusCode::OK, "{:?}", discovered.json());
	assert_eq!(
		api.users_file()["users"][0]["passkeys"][0]["credential"]["cred"]["counter"],
		8
	);
	for (counter, case) in [(8, "the same counter"), (3, "a counter that went back")] {
		phone.counter.set(counter);
		let cloned = api.passkey_sign_in("alex", &phone, &Wire::default()).await;
		assert_eq!(cloned.status, StatusCode::UNAUTHORIZED, "{case}");
	}
	phone.counter.set(9);
	assert_eq!(
		api.passkey_sign_in("alex", &phone, &Wire::default())
			.await
			.status,
		StatusCode::OK
	);
}

#[tokio::test]
async fn a_passkey_keeps_the_name_settled_when_its_registration_started() {
	let api = accounts(LoginMode::Required).await;
	let alex = api.signed_in("alex", ALEX).await;
	let mut phone = Authenticator::new();
	let started = api
		.call(
			"POST",
			"/api/account/passkeys/start",
			json!({"name": "phone"}),
			As::Browser(&alex),
		)
		.await
		.json();
	let credential = phone.create(&started["options"], &Wire::default());
	let finish = async |body: Value| {
		api.call(
			"POST",
			"/api/account/passkeys/finish",
			body,
			As::Browser(&alex),
		)
		.await
	};
	let renamed = finish(json!({
		"ceremony": started["ceremony"], "credential": credential, "name": "laptop"
	}))
	.await;
	assert_eq!(
		renamed.status,
		StatusCode::BAD_REQUEST,
		"{:?}",
		renamed.json()
	);
	let finished = finish(json!({"ceremony": started["ceremony"], "credential": credential})).await;
	assert_eq!(finished.status, StatusCode::OK, "{:?}", finished.json());
	assert_eq!(finished.json()["passkey"]["name"], "phone");
	assert_eq!(
		api.ok("/api/me", As::Browser(&alex)).await["passkeys"],
		json!(["phone"])
	);
}

#[tokio::test]
async fn a_cloned_key_counts_against_the_name_like_a_wrong_password() {
	let (api, _, phone, _) = with_phones().await;
	phone.counter.set(8);
	let signed_in = api.passkey_sign_in("alex", &phone, &Wire::default()).await;
	assert_eq!(signed_in.status, StatusCode::OK, "{:?}", signed_in.json());
	phone.counter.set(3);
	for _ in 0..5 {
		let cloned = api.passkey_sign_in("alex", &phone, &Wire::default()).await;
		assert_eq!(cloned.status, StatusCode::UNAUTHORIZED);
	}
	let blocked = api
		.call(
			"POST",
			"/api/auth/passkey/start",
			json!({"name": "alex"}),
			As::Page,
		)
		.await;
	assert_eq!(blocked.status, StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn refused_passkeys_count_against_the_name_like_wrong_passwords() {
	let (api, _, _, sams) = with_phones().await;
	for _ in 0..5 {
		let refused = api.passkey_sign_in("alex", &sams, &Wire::default()).await;
		assert_eq!(refused.status, StatusCode::UNAUTHORIZED);
	}
	let blocked = api
		.call(
			"POST",
			"/api/auth/passkey/start",
			json!({"name": "Alex"}),
			As::Page,
		)
		.await;
	assert_eq!(blocked.status, StatusCode::TOO_MANY_REQUESTS);
	assert_eq!(
		api.login("alex", ALEX).await.status,
		StatusCode::TOO_MANY_REQUESTS,
		"the password waits as well"
	);
	assert_eq!(
		api.passkey_sign_in("sam", &sams, &Wire::default())
			.await
			.status,
		StatusCode::OK,
		"other names still sign in"
	);
}

#[tokio::test]
async fn adding_a_passkey_needs_a_recent_sign_in() {
	let api = accounts(LoginMode::Required).await;
	let sessions = api.harness.app.auth.sessions().unwrap();
	let password = api.users_file()["users"][0]["password"]
		.as_str()
		.unwrap()
		.to_string();
	let now = time::OffsetDateTime::now_utc().unix_timestamp();
	let session = |signed_in: i64| {
		let (cookie, session) = login::open_session(
			sessions,
			"alex",
			password_credential(&password),
			"test".into(),
			None,
			Some(signed_in),
		)
		.unwrap();
		Browser {
			cookie,
			csrf: session.csrf,
		}
	};
	let start = async |browser: &Browser| {
		api.call(
			"POST",
			"/api/account/passkeys/start",
			json!({}),
			As::Browser(browser),
		)
		.await
	};
	let stale = start(&session(now - 11 * 60)).await;
	assert_eq!(stale.status, StatusCode::FORBIDDEN, "{:?}", stale.json());
	assert!(
		stale.json()["error"]
			.as_str()
			.unwrap()
			.starts_with("sign in again")
	);
	assert_eq!(start(&session(now - 9 * 60)).await.status, StatusCode::OK);

	let old = session(now - 3600);
	let changed = api
		.call(
			"PUT",
			"/api/account/password",
			json!({"current": ALEX, "password": "alex's newer passphrase"}),
			As::Browser(&old),
		)
		.await;
	assert_eq!(changed.status, StatusCode::OK);
	let renewed = Browser {
		cookie: changed.cookie().unwrap(),
		csrf: old.csrf.clone(),
	};
	assert_eq!(
		start(&renewed).await.status,
		StatusCode::OK,
		"typing the current password is a sign-in"
	);
}

#[tokio::test]
async fn autofill_offers_any_passkey_and_signs_in() {
	let (api, _, phone, _) = with_phones().await;
	let started = api
		.call("POST", "/api/auth/passkey/autofill", json!({}), As::Page)
		.await;
	assert_eq!(started.status, StatusCode::OK, "{:?}", started.json());
	let started = started.json();
	assert_eq!(started["options"]["mediation"], "conditional");
	assert!(
		started["options"]["publicKey"]
			.get("allowCredentials")
			.is_none_or(|ids| ids.as_array().is_some_and(Vec::is_empty))
	);
	let modal = api
		.call("POST", "/api/auth/passkey/start", json!({}), As::Page)
		.await
		.json();
	assert!(modal["options"].get("mediation").is_none_or(Value::is_null));
	let credential = phone.get(&started["options"], &Wire::default());
	let signed_in = api
		.call(
			"POST",
			"/api/auth/passkey/finish",
			json!({"ceremony": started["ceremony"], "credential": credential}),
			As::Page,
		)
		.await;
	assert_eq!(signed_in.status, StatusCode::OK, "{:?}", signed_in.json());
	assert_eq!(signed_in.json()["name"], "alex");
}

#[tokio::test]
async fn removing_a_passkey_ends_its_sessions() {
	let (api, alex, phone, _) = with_phones().await;
	let by_passkey = api
		.passkey_sign_in("alex", &phone, &Wire::default())
		.await
		.browser();
	let sessions = api.harness.app.auth.sessions().unwrap();
	assert_eq!(sessions.list("alex").unwrap().len(), 2);
	let removed = api
		.call(
			"DELETE",
			"/api/account/passkeys/phone",
			json!({}),
			As::Browser(&alex),
		)
		.await;
	assert_eq!(removed.status, StatusCode::OK, "{:?}", removed.json());
	assert_eq!(
		api.harness.last_entry().summary,
		"Removed passkey phone of alex"
	);
	let left = sessions.list("alex").unwrap();
	assert_eq!(left.len(), 1);
	assert!(left[0].credential.starts_with("password:"));
	assert_eq!(
		api.get("/api/me", As::Browser(&by_passkey)).await.status,
		StatusCode::UNAUTHORIZED
	);
	assert_eq!(
		api.call(
			"DELETE",
			"/api/account/passkeys/phone",
			json!({}),
			As::Browser(&alex),
		)
		.await
		.status,
		StatusCode::NOT_FOUND
	);
}
