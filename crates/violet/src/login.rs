//! Signing in: password hashes, sessions and their cookie, the rate limit on password logins and
//! the passkey ceremonies in flight.

use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier};
use axum::http::{HeaderMap, HeaderValue};
use time::OffsetDateTime;
use tokio::sync::Semaphore;
use uuid::Uuid;
use violet_core::{Session, Sessions, hash};
use webauthn_rs::prelude::{PasskeyAuthentication, PasskeyRegistration, Url};
use webauthn_rs::{Webauthn, WebauthnBuilder};

use crate::auth::Network;
use crate::{Error, Result};

pub const COOKIE: &str = "__Host-violet_session";
pub const IDLE: i64 = 30 * 24 * 60 * 60;
pub const MAX_AGE: i64 = 180 * 24 * 60 * 60;
pub const TOUCH_EVERY: i64 = 60 * 60;
pub const MIN_PASSWORD: usize = 12;
pub const MAX_PASSWORD: usize = 1024;
pub const WRONG_PASSWORD: &str = "wrong name or password";
pub const WRONG_PASSKEY: &str = "wrong name or passkey";
const TOKEN_BYTES: usize = 32;
const HASHES_AT_ONCE: usize = 2;
const WINDOW: Duration = Duration::from_secs(15 * 60);
const NAME_LIMIT: usize = 5;
const ADDRESS_LIMIT: usize = 20;
const GLOBAL_LIMIT: usize = 100;
const CEREMONY_TTL: Duration = Duration::from_secs(5 * 60);
const MAX_CEREMONIES: usize = 1000;
const MAX_LABEL: usize = 100;

pub fn hex(bytes: &[u8]) -> String {
	bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// 256 bits from the OS, in hex.
pub fn random_hex() -> String {
	let mut bytes = [0u8; TOKEN_BYTES];
	getrandom::fill(&mut bytes).unwrap_or_else(|error| panic!("the OS has no randomness: {error}"));
	hex(&bytes)
}

/// Whether `value` looks like a token of [`random_hex`].
pub fn is_token(value: &str) -> bool {
	value.len() == TOKEN_BYTES * 2
		&& value
			.bytes()
			.all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Compares two secrets in a time that depends only on their lengths.
pub fn equal(a: &[u8], b: &[u8]) -> bool {
	if a.len() != b.len() {
		return false;
	}
	let difference = a
		.iter()
		.zip(b)
		.fold(0u8, |difference, (x, y)| difference | (x ^ y));
	std::hint::black_box(difference) == 0
}

pub fn is_argon2id(phc: &str) -> bool {
	PasswordHash::new(phc).is_ok_and(|hash| hash.algorithm.as_str() == "argon2id")
}

/// 12 to 1024 bytes, not the user name.
pub fn check_password(name: &str, password: &str) -> Result<()> {
	if !(MIN_PASSWORD..=MAX_PASSWORD).contains(&password.len()) {
		return Err(Error::Invalid(format!(
			"a password must be {MIN_PASSWORD} to {MAX_PASSWORD} bytes long"
		)));
	}
	if password.trim().eq_ignore_ascii_case(name.trim()) {
		return Err(Error::Invalid(
			"the password cannot be the user name".into(),
		));
	}
	Ok(())
}

/// An argon2id hash with the current parameters, on the calling thread.
pub fn hash_now(password: &str) -> Result<String> {
	Argon2::default()
		.hash_password(password.as_bytes())
		.map(|hash| hash.to_string())
		.map_err(|error| Error::Internal(format!("argon2: {error}")))
}

fn verify_now(phc: &str, password: &str) -> bool {
	Argon2::default()
		.verify_password(password.as_bytes(), phc)
		.is_ok()
}

/// The WebAuthn relying party of `url`: its host is the id and its origin the only one
/// accepted.
pub fn relying_party(url: &Url) -> Result<Webauthn> {
	let bad = |why: &str| Error::Invalid(format!("VIOLET_PUBLIC_URL {url}: {why}"));
	if !matches!(url.scheme(), "https" | "http") {
		return Err(bad("must be http or https"));
	}
	if url.path() != "/" || url.query().is_some() || url.fragment().is_some() {
		return Err(bad("must be an origin, without a path"));
	}
	let Some(host) = url.domain() else {
		return Err(bad("must name a host"));
	};
	WebauthnBuilder::new(host, url)
		.and_then(|builder| builder.rp_name("violet").build())
		.map_err(|error| bad(&error.to_string()))
}

/// The WebAuthn user id of a user: stable, and the same for every passkey of theirs.
pub fn user_handle(name: &str) -> Uuid {
	let digest = hash::sha256(format!("violet-user:{}", name.to_lowercase()).as_bytes());
	Uuid::parse_str(&digest[..32]).unwrap_or_else(|_| Uuid::nil())
}

pub fn session_alive(session: &Session, now: i64) -> bool {
	now < session.expires && now - session.last_seen < IDLE
}

/// Opens a session and returns its token. `csrf` keeps the CSRF token of a session this one
/// replaces.
pub fn open_session(
	sessions: &Sessions,
	user: &str,
	credential: String,
	label: String,
	csrf: Option<String>,
) -> Result<(String, Session)> {
	let now = OffsetDateTime::now_utc().unix_timestamp();
	sessions.prune(now, now - IDLE)?;
	let token = random_hex();
	let session = Session {
		hash: hash::sha256(token.as_bytes()),
		user: user.to_string(),
		credential,
		csrf: csrf.unwrap_or_else(random_hex),
		created: now,
		last_seen: now,
		expires: now + MAX_AGE,
		label,
	};
	sessions.insert(&session)?;
	Ok((token, session))
}

pub fn set_cookie(token: &str) -> HeaderValue {
	HeaderValue::from_str(&format!(
		"{COOKIE}={token}; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age={MAX_AGE}"
	))
	.unwrap_or_else(|_| clear_cookie())
}

pub fn clear_cookie() -> HeaderValue {
	HeaderValue::from_static(
		"__Host-violet_session=; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age=0",
	)
}

/// "Firefox on Linux" and the like, from a User-Agent.
pub fn label(user_agent: Option<&str>) -> String {
	let agent = user_agent.unwrap_or_default();
	let browser = [
		("Edg/", "Edge"),
		("OPR/", "Opera"),
		("Firefox/", "Firefox"),
		("Chrome/", "Chrome"),
		("Safari/", "Safari"),
		("curl/", "curl"),
	]
	.into_iter()
	.find(|(marker, _)| agent.contains(marker))
	.map(|(_, name)| name);
	let system = [
		("Android", "Android"),
		("iPhone", "iOS"),
		("iPad", "iPadOS"),
		("Windows", "Windows"),
		("Mac OS X", "macOS"),
		("CrOS", "ChromeOS"),
		("Linux", "Linux"),
	]
	.into_iter()
	.find(|(marker, _)| agent.contains(marker))
	.map(|(_, name)| name);
	let label = match (browser, system) {
		(Some(browser), Some(system)) => format!("{browser} on {system}"),
		(Some(name), None) | (None, Some(name)) => name.to_string(),
		(None, None) if agent.trim().is_empty() => "unknown browser".to_string(),
		(None, None) => agent.chars().filter(|c| !c.is_control()).collect(),
	};
	label.chars().take(MAX_LABEL).collect()
}

/// The client a request comes from: the last `X-Forwarded-For` hop when the peer is a trusted
/// proxy, else the peer.
pub fn client(headers: &HeaderMap, peer: Option<IpAddr>, trusted: &[Network]) -> Option<IpAddr> {
	let peer = peer?;
	if !trusted.iter().any(|network| network.contains(peer)) {
		return Some(peer);
	}
	let forwarded = headers
		.get_all("x-forwarded-for")
		.iter()
		.filter_map(|value| value.to_str().ok())
		.flat_map(|value| value.split(','))
		.rfind(|hop| !hop.trim().is_empty())
		.and_then(|hop| hop.trim().parse::<IpAddr>().ok());
	Some(forwarded.unwrap_or(peer).to_canonical())
}

#[derive(Default)]
struct Counter {
	failures: VecDeque<Instant>,
	blocked_until: Option<Instant>,
}

impl Counter {
	fn blocked(&self, now: Instant) -> Option<Duration> {
		self.blocked_until
			.and_then(|until| until.checked_duration_since(now))
			.filter(|left| !left.is_zero())
	}

	fn forget(&mut self, now: Instant) {
		while self
			.failures
			.front()
			.is_some_and(|at| now.duration_since(*at) >= WINDOW)
		{
			self.failures.pop_front();
		}
		if self.blocked(now).is_none() {
			self.blocked_until = None;
		}
	}

	fn fail(&mut self, now: Instant, limit: usize) {
		self.forget(now);
		self.failures.push_back(now);
		if self.failures.len() >= limit {
			self.failures.clear();
			self.blocked_until = Some(now + WINDOW);
		}
	}

	fn idle(&self) -> bool {
		self.failures.is_empty() && self.blocked_until.is_none()
	}
}

#[derive(Default)]
struct Limits {
	names: HashMap<String, Counter>,
	addresses: HashMap<IpAddr, Counter>,
	global: Counter,
}

/// A passkey ceremony between its start and its finish.
pub enum Ceremony {
	Register {
		user: String,
		/// The session that started it, which alone may finish it.
		session: String,
		name: String,
		state: PasskeyRegistration,
	},
	SignIn {
		user: String,
		state: PasskeyAuthentication,
	},
}

pub struct Login {
	hashing: Semaphore,
	limits: Mutex<Limits>,
	ceremonies: Mutex<HashMap<String, (Instant, Ceremony)>>,
	dummy: OnceLock<String>,
}

impl Default for Login {
	fn default() -> Self {
		Self {
			hashing: Semaphore::new(HASHES_AT_ONCE),
			limits: Mutex::default(),
			ceremonies: Mutex::default(),
			dummy: OnceLock::new(),
		}
	}
}

impl Login {
	fn limits(&self) -> MutexGuard<'_, Limits> {
		self.limits.lock().unwrap_or_else(PoisonError::into_inner)
	}

	async fn hashing<T: Send + 'static>(
		&self,
		work: impl FnOnce() -> T + Send + 'static,
	) -> Result<T> {
		let _permit = self
			.hashing
			.acquire()
			.await
			.map_err(|error| Error::Internal(error.to_string()))?;
		Ok(tokio::task::spawn_blocking(work).await?)
	}

	pub async fn hash(&self, password: String) -> Result<String> {
		self.hashing(move || hash_now(&password)).await?
	}

	/// Whether `password` matches `phc`. Without a hash it checks against a dummy one, so
	/// that an unknown name takes as long as a wrong password.
	pub async fn verify(&self, phc: Option<String>, password: String) -> Result<bool> {
		let known = phc.is_some();
		let phc = match phc {
			Some(phc) => phc,
			None => match self.dummy.get() {
				Some(dummy) => dummy.clone(),
				None => {
					let dummy = self.hash(random_hex()).await?;
					self.dummy.get_or_init(|| dummy).clone()
				}
			},
		};
		let matches = self.hashing(move || verify_now(&phc, &password)).await?;
		Ok(known && matches)
	}

	/// How long a password login for `name` from `address` must wait, if it must.
	pub fn limited(&self, name: &str, address: Option<IpAddr>) -> Option<Duration> {
		self.limited_at(Instant::now(), name, address)
	}

	fn limited_at(&self, now: Instant, name: &str, address: Option<IpAddr>) -> Option<Duration> {
		let limits = self.limits();
		let by_name = limits
			.names
			.get(&name.trim().to_lowercase())
			.and_then(|counter| counter.blocked(now));
		let by_address = address
			.and_then(|address| limits.addresses.get(&address))
			.and_then(|counter| counter.blocked(now));
		[by_name, by_address, limits.global.blocked(now)]
			.into_iter()
			.flatten()
			.max()
	}

	pub fn failed(&self, name: &str, address: Option<IpAddr>) {
		self.failed_at(Instant::now(), name, address);
	}

	fn failed_at(&self, now: Instant, name: &str, address: Option<IpAddr>) {
		let mut limits = self.limits();
		let limits = &mut *limits;
		limits.global.fail(now, GLOBAL_LIMIT);
		limits
			.names
			.entry(name.trim().to_lowercase())
			.or_default()
			.fail(now, NAME_LIMIT);
		if let Some(address) = address {
			limits
				.addresses
				.entry(address)
				.or_default()
				.fail(now, ADDRESS_LIMIT);
		}
		for counter in limits.names.values_mut() {
			counter.forget(now);
		}
		for counter in limits.addresses.values_mut() {
			counter.forget(now);
		}
		limits.names.retain(|_, counter| !counter.idle());
		limits.addresses.retain(|_, counter| !counter.idle());
	}

	/// Keeps a ceremony for five minutes and returns the id that finishes it.
	pub fn start(&self, ceremony: Ceremony) -> Result<String> {
		let now = Instant::now();
		let mut ceremonies = self
			.ceremonies
			.lock()
			.unwrap_or_else(PoisonError::into_inner);
		ceremonies.retain(|_, (at, _)| now.duration_since(*at) < CEREMONY_TTL);
		if ceremonies.len() >= MAX_CEREMONIES {
			return Err(Error::TooMany(CEREMONY_TTL.as_secs()));
		}
		let id = random_hex();
		ceremonies.insert(hash::sha256(id.as_bytes()), (now, ceremony));
		Ok(id)
	}

	/// The ceremony of `id`, once, while it is fresh.
	pub fn finish(&self, id: &str) -> Option<Ceremony> {
		let mut ceremonies = self
			.ceremonies
			.lock()
			.unwrap_or_else(PoisonError::into_inner);
		let (at, ceremony) = ceremonies.remove(&hash::sha256(id.as_bytes()))?;
		(at.elapsed() < CEREMONY_TTL).then_some(ceremony)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn tokens_are_random_hex_and_compare_whole() {
		let (a, b) = (random_hex(), random_hex());
		assert!(is_token(&a) && is_token(&b));
		assert_ne!(a, b);
		assert!(!is_token("abc"));
		assert!(!is_token(&a.to_uppercase()));
		assert!(equal(a.as_bytes(), a.clone().as_bytes()));
		assert!(!equal(a.as_bytes(), b.as_bytes()));
		assert!(!equal(b"abc", b"abcd"));
		assert!(equal(b"", b""));
	}

	#[test]
	fn passwords_follow_the_rules_and_hash_with_owasp_parameters() {
		assert!(check_password("sam", "short").is_err());
		assert!(check_password("sam", &"x".repeat(1025)).is_err());
		assert!(check_password("sam-the-user", "SAM-THE-USER").is_err());
		assert!(check_password("sam", &"x".repeat(12)).is_ok());
		assert!(check_password("sam", &"é".repeat(512)).is_ok());
		let phc = hash_now("correct horse battery").unwrap();
		assert!(phc.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"), "{phc}");
		assert!(is_argon2id(&phc));
		assert!(verify_now(&phc, "correct horse battery"));
		assert!(!verify_now(&phc, "correct horse batterY"));
		assert_ne!(phc, hash_now("correct horse battery").unwrap(), "salted");

		let cheap = Argon2::new(
			argon2::Algorithm::Argon2id,
			argon2::Version::V0x13,
			argon2::Params::new(8, 1, 1, None).unwrap(),
		)
		.hash_password(b"correct horse battery")
		.unwrap()
		.to_string();
		assert!(cheap.contains("m=8,t=1,p=1"), "{cheap}");
		assert!(verify_now(&cheap, "correct horse battery"));

		let argon2i = Argon2::new(
			argon2::Algorithm::Argon2i,
			argon2::Version::V0x13,
			argon2::Params::new(8, 1, 1, None).unwrap(),
		)
		.hash_password(b"x")
		.unwrap()
		.to_string();
		assert!(!is_argon2id(&argon2i));
		assert!(!is_argon2id("plain text"));
	}

	#[tokio::test]
	async fn an_unknown_name_still_runs_a_hash() {
		let login = Login::default();
		assert!(!login.verify(None, "whatever it is".into()).await.unwrap());
		assert!(login.dummy.get().is_some());
		let phc = login.hash("correct horse battery".into()).await.unwrap();
		assert!(
			login
				.verify(Some(phc.clone()), "correct horse battery".into())
				.await
				.unwrap()
		);
		assert!(!login.verify(Some(phc), "wrong".into()).await.unwrap());
	}

	#[test]
	fn failures_block_a_name_an_address_and_everyone_for_fifteen_minutes() {
		let login = Login::default();
		let start = Instant::now();
		let home: IpAddr = "10.0.0.1".parse().unwrap();
		let other: IpAddr = "10.0.0.2".parse().unwrap();
		for _ in 0..4 {
			login.failed_at(start, "Sam", Some(home));
		}
		assert_eq!(login.limited_at(start, "sam", Some(home)), None);
		login.failed_at(start, "sam", Some(home));
		let wait = login.limited_at(start, "SAM", Some(other)).unwrap();
		assert_eq!(wait, WINDOW);
		assert_eq!(login.limited_at(start, "alex", Some(home)), None);
		let later = start + WINDOW - Duration::from_secs(1);
		assert_eq!(
			login.limited_at(later, "sam", None),
			Some(Duration::from_secs(1))
		);
		assert_eq!(login.limited_at(start + WINDOW, "sam", None), None);

		let login = Login::default();
		let old = start;
		for _ in 0..4 {
			login.failed_at(old, "sam", None);
		}
		let fresh = start + WINDOW;
		login.failed_at(fresh, "sam", None);
		assert_eq!(
			login.limited_at(fresh, "sam", None),
			None,
			"failures older than the window are forgotten"
		);

		let login = Login::default();
		for at in 0..20 {
			login.failed_at(start, &format!("user{at}"), Some(home));
		}
		assert_eq!(login.limited_at(start, "kim", Some(home)), Some(WINDOW));
		assert_eq!(login.limited_at(start, "kim", Some(other)), None);
		assert_eq!(login.limited_at(start, "kim", None), None);

		let login = Login::default();
		for at in 0..100u32 {
			let address = IpAddr::from(at.to_be_bytes());
			login.failed_at(start, &format!("user{at}"), Some(address));
		}
		assert_eq!(login.limited_at(start, "kim", Some(other)), Some(WINDOW));
		assert!(login.limits().names.len() <= 100);
	}

	#[test]
	fn the_client_is_the_last_forwarded_hop_only_behind_a_trusted_proxy() {
		let trusted = ["10.0.0.0/8".parse().unwrap()];
		let mut headers = HeaderMap::new();
		headers.insert("x-forwarded-for", "1.1.1.1, 203.0.113.7".parse().unwrap());
		let proxy: IpAddr = "10.1.2.3".parse().unwrap();
		let stranger: IpAddr = "192.168.1.9".parse().unwrap();
		assert_eq!(
			client(&headers, Some(proxy), &trusted),
			Some("203.0.113.7".parse().unwrap())
		);
		assert_eq!(client(&headers, Some(stranger), &trusted), Some(stranger));
		assert_eq!(client(&headers, Some(proxy), &[]), Some(proxy));
		assert_eq!(
			client(&HeaderMap::new(), Some(proxy), &trusted),
			Some(proxy)
		);
		assert_eq!(client(&headers, None, &trusted), None);
	}

	#[test]
	fn the_cookie_is_host_only_http_only_secure_and_lax() {
		let token = random_hex();
		let cookie = set_cookie(&token);
		let cookie = cookie.to_str().unwrap();
		assert!(cookie.starts_with(&format!("__Host-violet_session={token};")));
		for part in [
			"Path=/",
			"HttpOnly",
			"Secure",
			"SameSite=Lax",
			"Max-Age=15552000",
		] {
			assert!(cookie.split("; ").any(|p| p == part), "{part} in {cookie}");
		}
		assert!(!cookie.contains("Domain"));
		let cleared = clear_cookie();
		let cleared = cleared.to_str().unwrap();
		assert!(cleared.starts_with("__Host-violet_session=;") && cleared.contains("Max-Age=0"));
	}

	#[test]
	fn labels_name_the_browser_and_system() {
		let firefox = "Mozilla/5.0 (X11; Linux x86_64; rv:130.0) Gecko/20100101 Firefox/130.0";
		assert_eq!(label(Some(firefox)), "Firefox on Linux");
		let chrome = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0 Safari/537.36";
		assert_eq!(label(Some(chrome)), "Chrome on Windows");
		let safari = "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Mobile/15E148 Safari/604.1";
		assert_eq!(label(Some(safari)), "Safari on iOS");
		assert_eq!(label(None), "unknown browser");
		assert_eq!(label(Some(&"x".repeat(500))).len(), MAX_LABEL);
	}

	#[test]
	fn the_relying_party_is_the_origin_of_the_public_url() {
		for good in ["https://violet.dorsk.dev", "http://localhost:8080/"] {
			assert!(relying_party(&Url::parse(good).unwrap()).is_ok(), "{good}");
		}
		for bad in [
			"https://violet.dorsk.dev/app",
			"https://10.0.0.1",
			"ftp://violet.dorsk.dev",
			"https://violet.dorsk.dev/?x=1",
		] {
			assert!(relying_party(&Url::parse(bad).unwrap()).is_err(), "{bad}");
		}
		assert_eq!(user_handle("Sam"), user_handle("sam"));
		assert_ne!(user_handle("sam"), user_handle("alex"));
	}

	#[test]
	fn a_ceremony_finishes_once() {
		let login = Login::default();
		let webauthn = relying_party(&Url::parse("https://violet.example").unwrap()).unwrap();
		let (_, state) = webauthn
			.start_passkey_registration(user_handle("sam"), "sam", "sam", None)
			.unwrap();
		let id = login
			.start(Ceremony::Register {
				user: "sam".into(),
				session: "s".into(),
				name: "phone".into(),
				state,
			})
			.unwrap();
		assert!(is_token(&id));
		assert!(login.finish("not it").is_none());
		assert!(matches!(login.finish(&id), Some(Ceremony::Register { .. })));
		assert!(login.finish(&id).is_none());
	}
}
