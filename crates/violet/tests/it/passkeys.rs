use axum::http::{StatusCode, header};
use p256::ecdsa::signature::Signer;
use p256::ecdsa::{Signature, SigningKey};
use serde_json::{Value, json};
use violet::auth::{LoginMode, Role};

use crate::accounts::{ALEX, SAM, accounts, base64url};
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

	/// `navigator.credentials.create(…)` then `toJSON()`, as Chrome shapes it.
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

	/// `navigator.credentials.get(…)` then `toJSON()`, as Chrome shapes it.
	fn get(&self, options: &Value, wire: &Wire) -> Value {
		let client = Self::client_data("webauthn.get", options, wire);
		let mut data = sha256(wire.rp_id.as_bytes());
		data.push(wire.flags);
		data.extend(0u32.to_be_bytes());
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

#[tokio::test]
async fn an_assertion_that_does_not_verify_is_refused() {
	let api = accounts(LoginMode::Required).await;
	api.add_user("kim", Role::Member);
	let alex = api.signed_in("alex", ALEX).await;
	let sam = api.signed_in("sam", SAM).await;
	let mut phone = Authenticator::new();
	let mut sams = Authenticator::new();
	let ok = |answer: Answer| assert_eq!(answer.status, StatusCode::OK, "{:?}", answer.json());
	ok(api
		.register(&alex, &mut phone, "phone", &Wire::default())
		.await);
	ok(api
		.register(&sam, &mut sams, "phone", &Wire::default())
		.await);

	let cases = [
		(
			&phone,
			Wire {
				origin: "https://evil.example",
				..Wire::default()
			},
			"another origin",
		),
		(
			&phone,
			Wire {
				origin: "http://violet.test:8080",
				..Wire::default()
			},
			"another port",
		),
		(
			&phone,
			Wire {
				rp_id: "evil.example",
				..Wire::default()
			},
			"another relying party id hash",
		),
		(
			&phone,
			Wire {
				tamper: true,
				..Wire::default()
			},
			"a tampered signature",
		),
		(
			&phone,
			Wire {
				flags: UP,
				..Wire::default()
			},
			"no user verification",
		),
		(&sams, Wire::default(), "the passkey of another user"),
	];
	for (authenticator, wire, case) in cases {
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
	let started = api
		.call(
			"POST",
			"/api/auth/passkey/start",
			json!({"name": "alex"}),
			As::Page,
		)
		.await;
	assert_eq!(started.status, StatusCode::UNAUTHORIZED, "no passkey left");
	assert_eq!(
		api.passkey_sign_in("sam", &sams, &Wire::default())
			.await
			.status,
		StatusCode::OK
	);
}
