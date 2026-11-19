//! A browser-like HTTP session for connector kinds: a cookie jar that can be saved, and
//! redirects followed by hand so the cookies set on the way are kept. Errors name the host and
//! the path only, never the query, a header or a cookie.

use std::fmt;
use std::time::Duration;

use anyhow::{anyhow, bail};
use reqwest::header::{CONTENT_TYPE, COOKIE, HeaderMap, LOCATION, SET_COOKIE, USER_AGENT};
use reqwest::{Method, Url};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

const TIMEOUT: Duration = Duration::from_secs(60);
const MAX_REDIRECTS: usize = 10;
const MAX_BODY_BYTES: usize = 100 * 1024 * 1024;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cookie {
	pub name: String,
	pub value: String,
	/// Lowercase, without a leading dot.
	pub domain: String,
	/// Sent to `domain` only, not to its subdomains.
	pub host_only: bool,
	pub path: String,
	pub secure: bool,
	/// Unix seconds; none for a cookie that lasts as long as the jar.
	pub expires: Option<i64>,
}

impl fmt::Debug for Cookie {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "Cookie({} for {}{})", self.name, self.domain, self.path)
	}
}

impl Cookie {
	fn matches(&self, url: &Url, now: i64) -> bool {
		let Some(host) = url.host_str() else {
			return false;
		};
		let host = host.to_ascii_lowercase();
		let domain = host == self.domain || (!self.host_only && is_subdomain(&host, &self.domain));
		domain
			&& path_matches(url.path(), &self.path)
			&& (!self.secure || url.scheme() == "https")
			&& self.expires.is_none_or(|expires| expires > now)
	}
}

fn is_subdomain(host: &str, domain: &str) -> bool {
	host.strip_suffix(domain)
		.is_some_and(|rest| rest.ends_with('.'))
		&& host.parse::<std::net::IpAddr>().is_err()
}

fn path_matches(request: &str, cookie: &str) -> bool {
	request == cookie
		|| request
			.strip_prefix(cookie)
			.is_some_and(|rest| cookie.ends_with('/') || rest.starts_with('/'))
}

fn default_path(url: &Url) -> String {
	match url.path().rfind('/') {
		Some(0) | None => "/".into(),
		Some(end) => url.path()[..end].into(),
	}
}

/// The date of an `Expires` attribute, in any of the forms sites send, as Unix seconds.
fn cookie_date(raw: &str) -> Option<i64> {
	const MONTHS: [&str; 12] = [
		"jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
	];
	let (mut clock, mut day, mut month, mut year) = (None, None, None, None);
	for token in raw.split(|c: char| !c.is_ascii_alphanumeric() && c != ':') {
		if token.is_empty() {
			continue;
		}
		let digits = token.bytes().all(|byte| byte.is_ascii_digit());
		if clock.is_none() && token.contains(':') {
			let parts: Vec<u8> = token
				.split(':')
				.filter_map(|part| part.parse().ok())
				.collect();
			if let [hour, minute, second] = parts[..] {
				clock = time::Time::from_hms(hour, minute, second).ok();
				continue;
			}
		}
		if day.is_none() && digits && token.len() <= 2 {
			day = token.parse::<u8>().ok();
		} else if month.is_none()
			&& let Some(position) = token.get(..3).and_then(|start| {
				MONTHS
					.iter()
					.position(|month| start.eq_ignore_ascii_case(month))
			}) {
			month = time::Month::try_from(position as u8 + 1).ok();
		} else if year.is_none() && digits && (token.len() == 2 || token.len() == 4) {
			year = token.parse::<i32>().ok().map(|year| match year {
				0..=69 => year + 2000,
				70..=99 => year + 1900,
				_ => year,
			});
		}
	}
	let date = time::Date::from_calendar_date(year?, month?, day?).ok()?;
	Some(
		time::PrimitiveDateTime::new(date, clock?)
			.assume_utc()
			.unix_timestamp(),
	)
}

/// Cookies kept between requests. Serialisable, so a kind can save the ones that keep it
/// signed in.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Jar {
	cookies: Vec<Cookie>,
}

impl Jar {
	pub fn new() -> Self {
		Self::default()
	}

	pub fn cookies(&self) -> &[Cookie] {
		&self.cookies
	}

	/// The first cookie with this name, whatever its domain.
	pub fn get(&self, name: &str) -> Option<&Cookie> {
		self.cookies.iter().find(|cookie| cookie.name == name)
	}

	/// Adds a cookie, in place of the one with the same name, domain and path.
	pub fn insert(&mut self, cookie: Cookie) {
		self.cookies.retain(|known| {
			!(known.name == cookie.name
				&& known.domain == cookie.domain
				&& known.path == cookie.path)
		});
		self.cookies.push(cookie);
	}

	pub fn remove(&mut self, name: &str) {
		self.cookies.retain(|cookie| cookie.name != name);
	}

	/// A jar with only the cookies of these names, to save.
	pub fn only(&self, names: &[&str]) -> Self {
		Self {
			cookies: self
				.cookies
				.iter()
				.filter(|cookie| names.contains(&cookie.name.as_str()))
				.cloned()
				.collect(),
		}
	}

	pub fn extend(&mut self, other: Jar) {
		for cookie in other.cookies {
			self.insert(cookie);
		}
	}

	/// Takes one `Set-Cookie` header answered for `url`. A cookie for a domain the host is not
	/// part of is dropped; one that is expired removes the cookie it names.
	pub fn store(&mut self, url: &Url, header: &str, now: i64) {
		let Some(host) = url.host_str().map(str::to_ascii_lowercase) else {
			return;
		};
		let mut parts = header.split(';');
		let Some((name, value)) = parts.next().and_then(|pair| pair.split_once('=')) else {
			return;
		};
		let (name, value) = (name.trim(), value.trim());
		if name.is_empty() {
			return;
		}
		let value = value
			.strip_prefix('"')
			.and_then(|value| value.strip_suffix('"'))
			.unwrap_or(value);
		let mut cookie = Cookie {
			name: name.into(),
			value: value.into(),
			domain: host.clone(),
			host_only: true,
			path: default_path(url),
			secure: false,
			expires: None,
		};
		let mut max_age = None;
		for attribute in parts {
			let (key, value) = match attribute.split_once('=') {
				Some((key, value)) => (key.trim(), value.trim()),
				None => (attribute.trim(), ""),
			};
			match key.to_ascii_lowercase().as_str() {
				"domain" => {
					let domain = value.trim_start_matches('.').to_ascii_lowercase();
					if domain.is_empty() {
						continue;
					}
					let allowed =
						domain == host || (domain.contains('.') && is_subdomain(&host, &domain));
					if !allowed {
						return;
					}
					cookie.host_only = host.parse::<std::net::IpAddr>().is_ok();
					cookie.domain = domain;
				}
				"path" if value.starts_with('/') => cookie.path = value.into(),
				"secure" => cookie.secure = true,
				"max-age" => {
					if let Ok(seconds) = value.parse::<i64>() {
						max_age = Some(now.saturating_add(seconds.max(i64::MIN / 2)));
					}
				}
				"expires" => cookie.expires = cookie_date(value).or(cookie.expires),
				_ => {}
			}
		}
		if max_age.is_some() {
			cookie.expires = max_age;
		}
		if cookie.expires.is_some_and(|expires| expires <= now) {
			self.cookies.retain(|known| {
				!(known.name == cookie.name
					&& known.domain == cookie.domain
					&& known.path == cookie.path)
			});
			return;
		}
		self.insert(cookie);
	}

	/// The `Cookie` header for a request to `url`, longest paths first.
	pub fn header(&self, url: &Url, now: i64) -> Option<String> {
		let mut sent: Vec<&Cookie> = self
			.cookies
			.iter()
			.filter(|cookie| cookie.matches(url, now))
			.collect();
		if sent.is_empty() {
			return None;
		}
		sent.sort_by_key(|cookie| std::cmp::Reverse(cookie.path.len()));
		Some(
			sent.iter()
				.map(|cookie| format!("{}={}", cookie.name, cookie.value))
				.collect::<Vec<_>>()
				.join("; "),
		)
	}
}

/// `application/x-www-form-urlencoded` of these fields.
pub fn form_encode(fields: &[(&str, &str)]) -> String {
	let mut url = Url::parse("http://form.invalid/").expect("a constant URL");
	url.query_pairs_mut().extend_pairs(fields);
	url.query().unwrap_or_default().to_string()
}

#[derive(Clone)]
pub struct Request {
	method: Method,
	url: String,
	headers: Vec<(String, String)>,
	body: Option<Vec<u8>>,
}

impl Request {
	pub fn new(method: Method, url: &str) -> Self {
		Self {
			method,
			url: url.into(),
			headers: Vec::new(),
			body: None,
		}
	}

	pub fn get(url: &str) -> Self {
		Self::new(Method::GET, url)
	}

	pub fn post(url: &str) -> Self {
		Self::new(Method::POST, url)
	}

	/// A header sent with the request and with the redirects that stay on its host.
	pub fn header(mut self, name: &str, value: &str) -> Self {
		self.headers.push((name.into(), value.into()));
		self
	}

	pub fn body(mut self, content_type: &str, bytes: impl Into<Vec<u8>>) -> Self {
		self.headers
			.retain(|(name, _)| !name.eq_ignore_ascii_case(CONTENT_TYPE.as_str()));
		self.headers
			.push((CONTENT_TYPE.as_str().into(), content_type.into()));
		self.body = Some(bytes.into());
		self
	}

	pub fn form(self, fields: &[(&str, &str)]) -> Self {
		self.body("application/x-www-form-urlencoded", form_encode(fields))
	}

	pub fn json(self, value: &serde_json::Value) -> Self {
		self.body("application/json", value.to_string())
	}
}

pub struct Response {
	pub status: u16,
	/// Where the last redirect led.
	pub url: Url,
	pub headers: HeaderMap,
	pub body: Vec<u8>,
}

impl Response {
	pub fn header(&self, name: &str) -> Option<&str> {
		self.headers.get(name).and_then(|value| value.to_str().ok())
	}

	/// The body as UTF-8, bytes that are not replaced.
	pub fn text(&self) -> String {
		String::from_utf8_lossy(&self.body).into_owned()
	}

	pub fn json<T: serde::de::DeserializeOwned>(&self) -> anyhow::Result<T> {
		serde_json::from_slice(&self.body)
			.map_err(|_| anyhow!("{} did not answer the expected JSON", place(&self.url)))
	}

	/// The response when its status is 2xx, an error naming the status otherwise.
	pub fn success(self) -> anyhow::Result<Self> {
		if (200..300).contains(&self.status) {
			Ok(self)
		} else {
			Err(anyhow!("{} answered {}", place(&self.url), self.status))
		}
	}
}

fn place(url: &Url) -> String {
	format!("{}{}", url.host_str().unwrap_or_default(), url.path())
}

fn now() -> i64 {
	OffsetDateTime::now_utc().unix_timestamp()
}

pub struct Session {
	client: reqwest::Client,
	pub jar: Jar,
	user_agent: String,
}

impl Session {
	pub fn new(user_agent: &str) -> anyhow::Result<Self> {
		Self::with_jar(user_agent, Jar::new())
	}

	pub fn with_jar(user_agent: &str, jar: Jar) -> anyhow::Result<Self> {
		let client = reqwest::Client::builder()
			.redirect(reqwest::redirect::Policy::none())
			.timeout(TIMEOUT)
			.build()
			.map_err(|error| anyhow!("cannot set up HTTP: {}", error.without_url()))?;
		Ok(Self {
			client,
			jar,
			user_agent: user_agent.into(),
		})
	}

	pub async fn get(&mut self, url: &str) -> anyhow::Result<Response> {
		self.send(Request::get(url)).await
	}

	pub async fn post_form(
		&mut self,
		url: &str,
		fields: &[(&str, &str)],
	) -> anyhow::Result<Response> {
		self.send(Request::post(url).form(fields)).await
	}

	pub async fn post_json(
		&mut self,
		url: &str,
		value: &serde_json::Value,
	) -> anyhow::Result<Response> {
		self.send(Request::post(url).json(value)).await
	}

	/// Sends the request and follows its redirects, at most ten: after a 301, 302 or 303 a POST
	/// goes on as a GET without its body, a 307 or 308 repeats the request as it was. Any status
	/// is an answer; see [`Response::success`].
	pub async fn send(&mut self, request: Request) -> anyhow::Result<Response> {
		let Request {
			mut method,
			url,
			mut headers,
			mut body,
		} = request;
		let mut url = Url::parse(&url).map_err(|_| anyhow!("not a URL"))?;
		for _ in 0..=MAX_REDIRECTS {
			if !matches!(url.scheme(), "http" | "https") {
				bail!("{} is not an HTTP address", place(&url));
			}
			let mut builder = self
				.client
				.request(method.clone(), url.clone())
				.header(USER_AGENT, &self.user_agent);
			for (name, value) in &headers {
				builder = builder.header(name, value);
			}
			if let Some(cookies) = self.jar.header(&url, now()) {
				builder = builder.header(COOKIE, cookies);
			}
			if let Some(body) = &body {
				builder = builder.body(body.clone());
			}
			let mut response = builder
				.send()
				.await
				.map_err(|error| anyhow!("{method} {}: {}", place(&url), error.without_url()))?;
			let at = now();
			for cookie in response.headers().get_all(SET_COOKIE) {
				if let Ok(cookie) = std::str::from_utf8(cookie.as_bytes()) {
					self.jar.store(&url, cookie, at);
				}
			}
			let status = response.status().as_u16();
			let location = response
				.headers()
				.get(LOCATION)
				.and_then(|value| std::str::from_utf8(value.as_bytes()).ok());
			if let (301 | 302 | 303 | 307 | 308, Some(location)) = (status, location) {
				let next = url
					.join(location)
					.map_err(|_| anyhow!("{} redirects to no address", place(&url)))?;
				let to_get = match status {
					301 | 302 => method == Method::POST,
					303 => method != Method::HEAD,
					_ => false,
				};
				if to_get {
					method = Method::GET;
					body = None;
					headers.retain(|(name, _)| !name.eq_ignore_ascii_case(CONTENT_TYPE.as_str()));
				}
				if next.host_str() != url.host_str() {
					headers.retain(|(name, _)| name.eq_ignore_ascii_case(CONTENT_TYPE.as_str()));
				}
				url = next;
				continue;
			}
			let mut bytes = Vec::new();
			while let Some(chunk) = response
				.chunk()
				.await
				.map_err(|error| anyhow!("{method} {}: {}", place(&url), error.without_url()))?
			{
				if bytes.len() + chunk.len() > MAX_BODY_BYTES {
					bail!(
						"{} answered more than {} MB",
						place(&url),
						MAX_BODY_BYTES / 1024 / 1024
					);
				}
				bytes.extend_from_slice(&chunk);
			}
			return Ok(Response {
				status,
				url,
				headers: response.headers().clone(),
				body: bytes,
			});
		}
		bail!("{} redirects without end", place(&url))
	}
}

#[cfg(test)]
mod tests {
	use axum::Router;
	use axum::http::{HeaderMap as AxumHeaders, StatusCode, header};
	use axum::response::IntoResponse;
	use axum::routing::{get, post};

	use super::*;

	fn url(text: &str) -> Url {
		Url::parse(text).unwrap()
	}

	#[test]
	fn cookie_dates_in_the_forms_sites_send() {
		let expected = Some(1_445_412_480);
		for raw in [
			"Wed, 21 Oct 2015 07:28:00 GMT",
			"Wed, 21-Oct-2015 07:28:00 GMT",
			"Wednesday, 21-Oct-15 07:28:00 GMT",
			"Wed Oct 21 07:28:00 2015",
		] {
			assert_eq!(cookie_date(raw), expected, "{raw}");
		}
		assert_eq!(cookie_date("Thu, 01 Jan 1970 00:00:00 GMT"), Some(0));
		for raw in ["", "soon", "21 Oct 2015", "Wed, 32 Oct 2015 07:28:00 GMT"] {
			assert_eq!(cookie_date(raw), None, "{raw}");
		}
	}

	#[test]
	fn cookies_go_back_to_their_domain_and_path_only() {
		let mut jar = Jar::new();
		let from = url("https://app.example.com/account/login");
		jar.store(&from, "host=1", 100);
		jar.store(
			&from,
			"wide=2; Domain=.Example.com; Path=/; Secure; HttpOnly",
			100,
		);
		jar.store(&from, "deep=3; Path=/account/pay; SameSite=Lax", 100);
		jar.store(&from, "quoted=\"4\"; Path=/", 100);
		jar.store(&from, "other=5; Domain=example.org", 100);
		jar.store(&from, "tld=6; Domain=com", 100);
		jar.store(&from, "sub=7; Domain=deeper.app.example.com", 100);
		jar.store(&from, "=8", 100);
		jar.store(&from, "nonsense", 100);
		let names: Vec<&str> = jar
			.cookies()
			.iter()
			.map(|cookie| cookie.name.as_str())
			.collect();
		assert_eq!(names, ["host", "wide", "deep", "quoted"]);
		assert_eq!(jar.get("host").unwrap().path, "/account");
		assert_eq!(jar.get("wide").unwrap().domain, "example.com");

		let header = |address: &str| jar.header(&url(address), 100);
		assert_eq!(
			header("https://app.example.com/account/pay/2026").as_deref(),
			Some("deep=3; host=1; wide=2; quoted=4")
		);
		assert_eq!(
			header("https://app.example.com/accounts").as_deref(),
			Some("wide=2; quoted=4")
		);
		assert_eq!(
			header("http://app.example.com/account").as_deref(),
			Some("host=1; quoted=4"),
			"a secure cookie stays off plain HTTP"
		);
		assert_eq!(header("https://id.example.com/").as_deref(), Some("wide=2"));
		assert_eq!(header("https://badexample.com/"), None);
		assert_eq!(header("https://example.org/"), None);
	}

	#[test]
	fn cookies_expire_and_are_replaced_or_removed() {
		let mut jar = Jar::new();
		let from = url("https://example.com/");
		jar.store(&from, "a=1; Max-Age=60", 100);
		jar.store(
			&from,
			"b=1; Expires=Wed, 21 Oct 2015 07:28:00 GMT; Max-Age=60",
			100,
		);
		jar.store(&from, "c=1; Expires=Thu, 01 Jan 1970 00:10:00 GMT", 100);
		jar.store(&from, "d=1; Expires=Thu, 01 Jan 1970 00:00:01 GMT", 100);
		assert_eq!(jar.get("a").unwrap().expires, Some(160));
		assert_eq!(jar.get("b").unwrap().expires, Some(160), "Max-Age wins");
		assert_eq!(jar.get("c").unwrap().expires, Some(600));
		assert!(jar.get("d").is_none());
		assert_eq!(jar.header(&from, 159).as_deref(), Some("a=1; b=1; c=1"));
		assert_eq!(jar.header(&from, 160).as_deref(), Some("c=1"));

		jar.store(&from, "c=2", 100);
		assert_eq!(jar.cookies().len(), 3);
		assert_eq!(jar.get("c").unwrap().value, "2");
		jar.store(&from, "c=; Max-Age=0", 100);
		assert!(jar.get("c").is_none());
	}

	#[test]
	fn an_address_as_host_takes_no_subdomain_cookies() {
		let mut jar = Jar::new();
		let from = url("http://127.0.0.1:8080/");
		jar.store(&from, "a=1; Domain=0.0.1", 100);
		jar.store(&from, "b=1; Domain=127.0.0.1", 100);
		assert_eq!(jar.header(&from, 100).as_deref(), Some("b=1"));
		assert!(jar.get("b").unwrap().host_only);
	}

	#[test]
	fn a_jar_is_saved_whole_or_in_part_and_hides_its_values() {
		let mut jar = Jar::new();
		let from = url("https://example.com/");
		jar.store(&from, "device=secret-token; Max-Age=31536000; Secure", 100);
		jar.store(&from, "session=short", 100);
		let kept = jar.only(&["device"]);
		let saved = serde_json::to_value(&kept).unwrap();
		assert_eq!(saved[0]["name"], "device");
		assert_eq!(saved.as_array().unwrap().len(), 1);
		let restored: Jar = serde_json::from_value(saved).unwrap();
		assert_eq!(restored, kept);
		let mut fresh = Jar::new();
		fresh.extend(restored);
		assert_eq!(
			fresh.header(&from, 200).as_deref(),
			Some("device=secret-token")
		);
		assert!(!format!("{jar:?}").contains("secret-token"));
		fresh.remove("device");
		assert!(fresh.cookies().is_empty());
	}

	#[test]
	fn forms_are_percent_encoded() {
		assert_eq!(
			form_encode(&[("user", "a b@c.d"), ("pass", "p&=é")]),
			"user=a+b%40c.d&pass=p%26%3D%C3%A9"
		);
	}

	async fn echo(method: axum::http::Method, headers: AxumHeaders, body: String) -> String {
		let header = |name| {
			headers
				.get(name)
				.and_then(|value: &axum::http::HeaderValue| value.to_str().ok())
				.unwrap_or("-")
				.to_string()
		};
		format!(
			"{method}|{}|{}|{}|{body}",
			header(header::COOKIE),
			header(header::CONTENT_TYPE),
			header(header::HeaderName::from_static("x-csrf")),
		)
	}

	async fn site() -> String {
		let to = |status: StatusCode, location: &'static str, cookie: &'static str| {
			move || async move {
				(
					status,
					[(header::LOCATION, location), (header::SET_COOKIE, cookie)],
				)
					.into_response()
			}
		};
		let router = Router::new()
			.route("/echo", get(echo).post(echo))
			.route("/app/echo", get(echo))
			.route(
				"/login",
				post(to(StatusCode::FOUND, "/hop", "sid=abc; Path=/; HttpOnly")),
			)
			.route(
				"/hop",
				get(to(StatusCode::SEE_OTHER, "app/echo", "hop=1; Path=/app")),
			)
			.route(
				"/keep",
				post(to(StatusCode::TEMPORARY_REDIRECT, "/echo", "k=1")),
			)
			.route(
				"/moved",
				post(to(StatusCode::MOVED_PERMANENTLY, "/echo", "m=1")),
			)
			.route("/loop", get(to(StatusCode::FOUND, "/loop", "l=1")))
			.route(
				"/missing",
				get(|| async { (StatusCode::NOT_FOUND, "nothing here") }),
			);
		let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
		let address = listener.local_addr().unwrap();
		tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
		format!("http://{address}")
	}

	#[tokio::test]
	async fn a_post_redirected_becomes_a_get_and_keeps_the_cookies_set_on_the_way() {
		let base = site().await;
		let mut session = Session::new("violet-test").unwrap();
		let response = session
			.send(
				Request::post(&format!("{base}/login"))
					.header("X-CSRF", "t0ken")
					.form(&[("user", "sam"), ("password", "pw")]),
			)
			.await
			.unwrap();
		assert_eq!(response.status, 200);
		assert_eq!(response.url.path(), "/app/echo");
		assert_eq!(response.text(), "GET|hop=1; sid=abc|-|t0ken|");
		assert!(
			response
				.header("content-type")
				.unwrap()
				.starts_with("text/plain")
		);

		let again = session.get(&format!("{base}/echo")).await.unwrap();
		assert_eq!(again.text(), "GET|sid=abc|-|-|");
	}

	#[tokio::test]
	async fn a_307_repeats_the_post_and_a_301_does_not() {
		let base = site().await;
		let mut session = Session::new("violet-test").unwrap();
		let kept = session
			.post_form(&format!("{base}/keep"), &[("a", "1")])
			.await
			.unwrap();
		assert_eq!(
			kept.text(),
			"POST|k=1|application/x-www-form-urlencoded|-|a=1"
		);
		let moved = session
			.post_json(&format!("{base}/moved"), &serde_json::json!({"a": 1}))
			.await
			.unwrap();
		assert_eq!(moved.text(), "GET|k=1; m=1|-|-|");
	}

	#[tokio::test]
	async fn failures_name_the_place_and_nothing_secret() {
		let base = site().await;
		let mut session = Session::new("violet-test").unwrap();
		let looping = session
			.get(&format!("{base}/loop?token=s3cret"))
			.await
			.err()
			.unwrap()
			.to_string();
		assert!(looping.contains("/loop redirects without end"), "{looping}");
		let missing = session
			.get(&format!("{base}/missing?token=s3cret"))
			.await
			.unwrap();
		assert_eq!(
			(missing.status, missing.text().as_str()),
			(404, "nothing here")
		);
		let error = missing.success().err().unwrap().to_string();
		assert!(error.ends_with("/missing answered 404"), "{error}");
		let refused = session
			.get("http://127.0.0.1:1/private?token=s3cret")
			.await
			.err()
			.unwrap()
			.to_string();
		assert!(refused.starts_with("GET 127.0.0.1/private: "), "{refused}");
		for error in [looping, error, refused] {
			assert!(!error.contains("s3cret"), "{error}");
		}
		assert!(session.get("ftp://example.com/x").await.is_err());
		assert!(session.get("not a url").await.is_err());
	}
}
