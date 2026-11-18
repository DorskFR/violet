//! A small IMAP4rev1 client: just what the mail sync needs, with literals read and written by
//! the byte count both ways.

use std::collections::HashSet;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::{Context, anyhow, bail};
use time::Date;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio_rustls::TlsConnector;
use tokio_rustls::rustls::pki_types::ServerName;
use tokio_rustls::rustls::{self, ClientConfig, RootCertStore};

use super::config::{Account, Security};

const MAX_LINE: u64 = 1024 * 1024;
const MAX_LITERAL: usize = 200 * 1024 * 1024;
const READ_TIMEOUT: Duration = Duration::from_secs(120);
const UIDS_PER_COMMAND: usize = 200;

pub trait Stream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Stream for T {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
	Atom(String),
	/// A quoted string or a literal.
	Bytes(Vec<u8>),
	List(Vec<Value>),
	Nil,
}

impl Value {
	fn atom(&self) -> Option<&str> {
		match self {
			Self::Atom(atom) => Some(atom),
			_ => None,
		}
	}
	fn number(&self) -> Option<u32> {
		self.atom()?.parse().ok()
	}
	fn text(&self) -> Option<String> {
		match self {
			Self::Atom(atom) => Some(atom.clone()),
			Self::Bytes(bytes) => Some(String::from_utf8_lossy(bytes).into_owned()),
			_ => None,
		}
	}
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Response {
	/// `OK`, `NO`, `BAD`, `BYE` or `PREAUTH`; `tag` is `None` when untagged.
	Status {
		tag: Option<String>,
		status: String,
		code: Option<String>,
		text: String,
	},
	/// Untagged data after `* `.
	Data(Vec<Value>),
	Continue,
}

/// Reads the bytes of one response: its lines, and every literal they announce.
pub async fn read_response<R: AsyncBufReadExt + Unpin>(reader: &mut R) -> anyhow::Result<Vec<u8>> {
	let mut out = Vec::new();
	loop {
		let mut line = Vec::new();
		let read = tokio::time::timeout(
			READ_TIMEOUT,
			(&mut *reader).take(MAX_LINE).read_until(b'\n', &mut line),
		)
		.await
		.context("the server stopped answering")??;
		if read == 0 {
			bail!("the server closed the connection");
		}
		if !line.ends_with(b"\n") {
			bail!("the server sent a line longer than {MAX_LINE} bytes");
		}
		out.extend_from_slice(&line);
		let Some(length) = literal_length(&line) else {
			return Ok(out);
		};
		if length > MAX_LITERAL {
			bail!("the server sent a literal of {length} bytes");
		}
		let start = out.len();
		out.resize(start + length, 0);
		tokio::time::timeout(READ_TIMEOUT, reader.read_exact(&mut out[start..]))
			.await
			.context("the server stopped answering")??;
	}
}

/// `n` when a line ends with `{n}` (or the non-synchronising `{n+}`).
fn literal_length(line: &[u8]) -> Option<usize> {
	let line = line.strip_suffix(b"\n")?;
	let line = line.strip_suffix(b"\r").unwrap_or(line);
	let line = line.strip_suffix(b"}")?;
	let open = line.iter().rposition(|byte| *byte == b'{')?;
	let digits = &line[open + 1..];
	let digits = digits.strip_suffix(b"+").unwrap_or(digits);
	if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
		return None;
	}
	std::str::from_utf8(digits).ok()?.parse().ok()
}

struct Parser<'a> {
	bytes: &'a [u8],
	at: usize,
}

impl Parser<'_> {
	fn peek(&self) -> Option<u8> {
		self.bytes.get(self.at).copied()
	}

	fn spaces(&mut self) {
		while self.peek() == Some(b' ') {
			self.at += 1;
		}
	}

	fn values(&mut self, close: Option<u8>) -> anyhow::Result<Vec<Value>> {
		let mut values = Vec::new();
		loop {
			self.spaces();
			match self.peek() {
				None | Some(b'\r' | b'\n') if close.is_none() => return Ok(values),
				None | Some(b'\r' | b'\n') => bail!("a list is not closed"),
				Some(byte) if Some(byte) == close => {
					self.at += 1;
					return Ok(values);
				}
				Some(b')') => bail!("unexpected ')'"),
				Some(_) => values.push(self.value()?),
			}
		}
	}

	fn value(&mut self) -> anyhow::Result<Value> {
		match self.peek() {
			Some(b'(') => {
				self.at += 1;
				Ok(Value::List(self.values(Some(b')'))?))
			}
			Some(b'"') => self.quoted(),
			Some(b'{') => self.literal(),
			_ => {
				let atom = self.atom()?;
				Ok(if atom.eq_ignore_ascii_case("NIL") {
					Value::Nil
				} else {
					Value::Atom(atom)
				})
			}
		}
	}

	fn quoted(&mut self) -> anyhow::Result<Value> {
		self.at += 1;
		let mut out = Vec::new();
		loop {
			match self.peek() {
				None | Some(b'\r' | b'\n') => bail!("a quoted string is not closed"),
				Some(b'"') => {
					self.at += 1;
					return Ok(Value::Bytes(out));
				}
				Some(b'\\') => {
					self.at += 1;
					out.push(
						self.peek()
							.ok_or_else(|| anyhow!("a quoted string is cut"))?,
					);
					self.at += 1;
				}
				Some(byte) => {
					out.push(byte);
					self.at += 1;
				}
			}
		}
	}

	fn literal(&mut self) -> anyhow::Result<Value> {
		let close = self.bytes[self.at..]
			.iter()
			.position(|byte| *byte == b'}')
			.ok_or_else(|| anyhow!("a literal has no length"))?;
		let digits = &self.bytes[self.at + 1..self.at + close];
		let digits = digits.strip_suffix(b"+").unwrap_or(digits);
		let length: usize = std::str::from_utf8(digits)?.parse()?;
		self.at += close + 1;
		if self.peek() == Some(b'\r') {
			self.at += 1;
		}
		if self.peek() != Some(b'\n') {
			bail!("a literal length is not followed by a line break");
		}
		self.at += 1;
		let end = self.at + length;
		let bytes = self
			.bytes
			.get(self.at..end)
			.ok_or_else(|| anyhow!("a literal is cut"))?;
		self.at = end;
		Ok(Value::Bytes(bytes.to_vec()))
	}

	/// An atom; a `[` runs to its `]`, so `BODY[]` and `[UIDVALIDITY 3]` are one atom.
	fn atom(&mut self) -> anyhow::Result<String> {
		let start = self.at;
		let mut depth = 0usize;
		while let Some(byte) = self.peek() {
			match byte {
				b'[' => depth += 1,
				b']' if depth > 0 => depth -= 1,
				b' ' | b'(' | b')' | b'\r' | b'\n' if depth == 0 => break,
				b'\r' | b'\n' => bail!("a '[' is not closed"),
				_ => {}
			}
			self.at += 1;
		}
		if self.at == start {
			bail!("expected a value at byte {start}");
		}
		Ok(String::from_utf8_lossy(&self.bytes[start..self.at]).into_owned())
	}

	fn rest(&mut self) -> String {
		self.spaces();
		let rest = &self.bytes[self.at..];
		let end = rest
			.iter()
			.position(|byte| matches!(byte, b'\r' | b'\n'))
			.unwrap_or(rest.len());
		self.at = self.bytes.len();
		String::from_utf8_lossy(&rest[..end]).into_owned()
	}
}

const STATUSES: [&str; 5] = ["OK", "NO", "BAD", "BYE", "PREAUTH"];

/// Parses the bytes `read_response` returned.
pub fn parse_response(bytes: &[u8]) -> anyhow::Result<Response> {
	let mut parser = Parser { bytes, at: 0 };
	if bytes.starts_with(b"+") {
		return Ok(Response::Continue);
	}
	let tag = parser.atom()?;
	parser.spaces();
	let save = parser.at;
	let word = parser.atom().unwrap_or_default();
	if STATUSES
		.iter()
		.any(|status| word.eq_ignore_ascii_case(status))
	{
		let text = parser.rest();
		let (code, text) = match text.strip_prefix('[').and_then(|rest| rest.split_once(']')) {
			Some((code, text)) => (Some(code.to_string()), text.trim_start().to_string()),
			None => (None, text),
		};
		return Ok(Response::Status {
			tag: (tag != "*").then_some(tag),
			status: word.to_ascii_uppercase(),
			code,
			text,
		});
	}
	if tag != "*" {
		bail!("a tagged response without a status");
	}
	parser.at = save;
	Ok(Response::Data(parser.values(None)?))
}

/// A part of a command: written as it is, or as an IMAP string (quoted, or a literal when
/// it cannot be quoted).
#[derive(Debug, Clone, Copy)]
pub enum Arg<'a> {
	Raw(&'a str),
	Str(&'a [u8]),
}

fn quotable(bytes: &[u8]) -> bool {
	bytes.len() < 1000 && bytes.iter().all(|byte| (0x20..0x7f).contains(byte))
}

fn quote(bytes: &[u8]) -> Vec<u8> {
	let mut out = Vec::with_capacity(bytes.len() + 2);
	out.push(b'"');
	for byte in bytes {
		if matches!(byte, b'"' | b'\\') {
			out.push(b'\\');
		}
		out.push(*byte);
	}
	out.push(b'"');
	out
}

/// A mailbox name in modified UTF-7 (RFC 3501 5.1.3).
pub fn mailbox_name(name: &str) -> String {
	const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+,";
	fn flush(out: &mut String, pending: &mut Vec<u16>) {
		if pending.is_empty() {
			return;
		}
		let bytes: Vec<u8> = pending.iter().flat_map(|unit| unit.to_be_bytes()).collect();
		out.push('&');
		for chunk in bytes.chunks(3) {
			let n = chunk
				.iter()
				.enumerate()
				.fold(0u32, |n, (i, byte)| n | u32::from(*byte) << (16 - 8 * i));
			for i in 0..=chunk.len() {
				out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
			}
		}
		out.push('-');
		pending.clear();
	}
	let mut out = String::new();
	let mut pending = Vec::new();
	for c in name.chars() {
		if (' '..='~').contains(&c) {
			flush(&mut out, &mut pending);
			if c == '&' {
				out.push_str("&-");
			} else {
				out.push(c);
			}
		} else {
			pending.extend(c.encode_utf16(&mut [0; 2]).iter());
		}
	}
	flush(&mut out, &mut pending);
	out
}

const MONTHS: [&str; 12] = [
	"Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// `d-Mon-yyyy`, the date form of SEARCH.
pub fn search_date(date: Date) -> String {
	format!(
		"{}-{}-{}",
		date.day(),
		MONTHS[usize::from(u8::from(date.month())) - 1],
		date.year()
	)
}

/// An INTERNALDATE (`17-Jul-1996 02:44:25 -0700`) in RFC 3339.
pub fn internal_date(value: &str) -> Option<String> {
	let format = time::macros::format_description!(
		"[day padding:none]-[month repr:short]-[year] [hour]:[minute]:[second] [offset_hour sign:mandatory][offset_minute]"
	);
	let parsed = time::OffsetDateTime::parse(value.trim(), &format).ok()?;
	parsed
		.format(&time::format_description::well_known::Rfc3339)
		.ok()
}

/// Which mails a SEARCH asks for.
#[derive(Debug, Clone, Default)]
pub struct Criteria {
	pub since: Option<Date>,
	pub from: Option<String>,
	pub to: Option<String>,
	pub subject: Option<String>,
	/// Leave out mails with this keyword.
	pub without_keyword: Option<String>,
	pub unseen: bool,
	pub unflagged: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Meta {
	pub uid: u32,
	pub size: u64,
	/// RFC 3339.
	pub received: Option<String>,
}

pub struct Session {
	io: BufReader<Box<dyn Stream>>,
	next_tag: u32,
	capabilities: HashSet<String>,
}

fn tls() -> anyhow::Result<TlsConnector> {
	static CONFIG: OnceLock<Arc<ClientConfig>> = OnceLock::new();
	if let Some(config) = CONFIG.get() {
		return Ok(TlsConnector::from(config.clone()));
	}
	let mut roots = RootCertStore::empty();
	let loaded = rustls_native_certs::load_native_certs();
	for error in &loaded.errors {
		tracing::warn!(%error, "a system certificate could not be loaded");
	}
	let (added, _) = roots.add_parsable_certificates(loaded.certs);
	if added == 0 {
		bail!("no system root certificate could be loaded");
	}
	let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
	let config = ClientConfig::builder_with_provider(provider)
		.with_safe_default_protocol_versions()?
		.with_root_certificates(roots)
		.with_no_client_auth();
	Ok(TlsConnector::from(
		CONFIG.get_or_init(|| Arc::new(config)).clone(),
	))
}

async fn wrap_tls(stream: Box<dyn Stream>, host: &str) -> anyhow::Result<Box<dyn Stream>> {
	let name =
		ServerName::try_from(host.to_string()).map_err(|_| anyhow!("{host} is not a host name"))?;
	let stream = tokio::time::timeout(READ_TIMEOUT, tls()?.connect(name, stream))
		.await
		.context("the TLS handshake timed out")?
		.context("TLS handshake")?;
	Ok(Box::new(stream))
}

impl Session {
	/// Connects with the account's security and logs in.
	pub async fn connect(account: &Account, password: &str) -> anyhow::Result<Self> {
		let address = (account.host.as_str(), account.port());
		let tcp = tokio::time::timeout(READ_TIMEOUT, tokio::net::TcpStream::connect(address))
			.await
			.context("connecting timed out")?
			.with_context(|| format!("cannot connect to {}:{}", account.host, account.port()))?;
		let stream: Box<dyn Stream> = match account.security {
			Security::Tls => wrap_tls(Box::new(tcp), &account.host).await?,
			Security::Starttls | Security::Plain => Box::new(tcp),
		};
		let mut session = Self::start(stream).await?;
		if account.security == Security::Starttls {
			session = session.starttls(&account.host).await?;
		}
		session.login(&account.username, password).await?;
		Ok(session)
	}

	/// Reads the greeting on an open stream.
	pub async fn start(stream: Box<dyn Stream>) -> anyhow::Result<Self> {
		let mut session = Self {
			io: BufReader::new(stream),
			next_tag: 1,
			capabilities: HashSet::new(),
		};
		match parse_response(&read_response(&mut session.io).await?)? {
			Response::Status {
				tag: None,
				status,
				text,
				..
			} if status == "OK" || status == "PREAUTH" => {
				tracing::debug!(greeting = text, "imap");
			}
			other => bail!("unexpected greeting: {other:?}"),
		}
		Ok(session)
	}

	pub async fn starttls(mut self, host: &str) -> anyhow::Result<Self> {
		self.command(&[Arg::Raw("STARTTLS")]).await?;
		let stream = self.io.into_inner();
		Ok(Self {
			io: BufReader::new(wrap_tls(stream, host).await?),
			next_tag: self.next_tag,
			capabilities: HashSet::new(),
		})
	}

	pub async fn login(&mut self, username: &str, password: &str) -> anyhow::Result<()> {
		self.command(&[
			Arg::Raw("LOGIN "),
			Arg::Str(username.as_bytes()),
			Arg::Raw(" "),
			Arg::Str(password.as_bytes()),
		])
		.await
		.map_err(|error| anyhow!("login refused: {error}"))?;
		let data = self.command(&[Arg::Raw("CAPABILITY")]).await?;
		self.capabilities = data
			.iter()
			.filter(|values| {
				values
					.first()
					.and_then(Value::atom)
					.is_some_and(|word| word.eq_ignore_ascii_case("CAPABILITY"))
			})
			.flat_map(|values| values.iter().skip(1).filter_map(Value::atom))
			.map(str::to_ascii_uppercase)
			.collect();
		Ok(())
	}

	pub fn has(&self, capability: &str) -> bool {
		self.capabilities.contains(capability)
	}

	/// Selects a folder and returns its UIDVALIDITY.
	pub async fn select(&mut self, folder: &str) -> anyhow::Result<u32> {
		let name = mailbox_name(folder);
		let mut uidvalidity = None;
		let tag = self
			.send(&[Arg::Raw("SELECT "), Arg::Str(name.as_bytes())])
			.await?;
		self.finish(&tag, |response| {
			if let Response::Status {
				code: Some(code), ..
			} = response
				&& let Some(value) = code.strip_prefix("UIDVALIDITY ")
			{
				uidvalidity = value.trim().parse().ok();
			}
		})
		.await
		.map_err(|error| anyhow!("cannot open the folder {folder}: {error}"))?;
		uidvalidity.ok_or_else(|| anyhow!("the server gave no UIDVALIDITY for {folder}"))
	}

	pub async fn search(&mut self, criteria: &Criteria) -> anyhow::Result<Vec<u32>> {
		let since = criteria.since.map(search_date);
		let texts = [
			("FROM", &criteria.from),
			("TO", &criteria.to),
			("SUBJECT", &criteria.subject),
		];
		let mut args = vec![Arg::Raw("UID SEARCH")];
		if texts
			.iter()
			.any(|(_, text)| text.as_ref().is_some_and(|text| !text.is_ascii()))
		{
			args.push(Arg::Raw(" CHARSET UTF-8"));
		}
		args.push(Arg::Raw(" UNDELETED"));
		if let Some(since) = &since {
			args.extend([Arg::Raw(" SINCE "), Arg::Raw(since)]);
		}
		let keys = [" FROM ", " TO ", " SUBJECT "];
		for ((_, text), key) in texts.iter().zip(keys) {
			if let Some(text) = text {
				args.extend([Arg::Raw(key), Arg::Str(text.as_bytes())]);
			}
		}
		if let Some(keyword) = &criteria.without_keyword {
			args.extend([Arg::Raw(" UNKEYWORD "), Arg::Raw(keyword)]);
		}
		if criteria.unseen {
			args.push(Arg::Raw(" UNSEEN"));
		}
		if criteria.unflagged {
			args.push(Arg::Raw(" UNFLAGGED"));
		}
		let data = self.command(&args).await?;
		let mut uids: Vec<u32> = data
			.iter()
			.filter(|values| {
				values
					.first()
					.and_then(Value::atom)
					.is_some_and(|word| word.eq_ignore_ascii_case("SEARCH"))
			})
			.flat_map(|values| values.iter().skip(1).filter_map(Value::number))
			.collect();
		uids.sort_unstable();
		uids.dedup();
		Ok(uids)
	}

	/// The FETCH items of every untagged FETCH in `data` that carries a UID, by UID.
	fn fetched(data: &[Vec<Value>]) -> Vec<(u32, Vec<(String, Value)>)> {
		let mut out = Vec::new();
		for values in data {
			let [_, word, Value::List(items)] = values.as_slice() else {
				continue;
			};
			if !word
				.atom()
				.is_some_and(|word| word.eq_ignore_ascii_case("FETCH"))
			{
				continue;
			}
			let pairs: Vec<(String, Value)> = items
				.chunks(2)
				.filter_map(|pair| match pair {
					[key, value] => Some((key.atom()?.to_ascii_uppercase(), value.clone())),
					_ => None,
				})
				.collect();
			let uid = pairs
				.iter()
				.find(|(key, _)| key == "UID")
				.and_then(|(_, value)| value.number());
			if let Some(uid) = uid {
				out.push((uid, pairs));
			}
		}
		out
	}

	/// Size and arrival time of each mail.
	pub async fn fetch_meta(&mut self, uids: &[u32]) -> anyhow::Result<Vec<Meta>> {
		let mut metas = Vec::new();
		for chunk in uids.chunks(UIDS_PER_COMMAND) {
			let set = chunk
				.iter()
				.map(u32::to_string)
				.collect::<Vec<_>>()
				.join(",");
			let data = self
				.command(&[
					Arg::Raw("UID FETCH "),
					Arg::Raw(&set),
					Arg::Raw(" (UID RFC822.SIZE INTERNALDATE)"),
				])
				.await?;
			for (uid, pairs) in Self::fetched(&data) {
				if !chunk.contains(&uid) {
					continue;
				}
				let item = |name: &str| {
					pairs
						.iter()
						.find(|(key, _)| key == name)
						.map(|(_, value)| value)
				};
				metas.push(Meta {
					uid,
					size: item("RFC822.SIZE")
						.and_then(Value::text)
						.and_then(|size| size.parse().ok())
						.unwrap_or(0),
					received: item("INTERNALDATE")
						.and_then(Value::text)
						.and_then(|date| internal_date(&date)),
				});
			}
		}
		metas.sort_by_key(|meta| meta.uid);
		metas.dedup_by_key(|meta| meta.uid);
		Ok(metas)
	}

	/// The raw message, without marking it seen.
	pub async fn fetch(&mut self, uid: u32) -> anyhow::Result<Vec<u8>> {
		let uid_text = uid.to_string();
		let data = self
			.command(&[
				Arg::Raw("UID FETCH "),
				Arg::Raw(&uid_text),
				Arg::Raw(" (UID BODY.PEEK[])"),
			])
			.await?;
		for (fetched, pairs) in Self::fetched(&data) {
			if fetched != uid {
				continue;
			}
			if let Some((_, Value::Bytes(bytes))) =
				pairs.into_iter().find(|(key, _)| key == "BODY[]")
			{
				return Ok(bytes);
			}
		}
		bail!("the server sent no body for UID {uid}")
	}

	/// Adds a flag or keyword to one mail.
	pub async fn store(&mut self, uid: u32, flag: &str) -> anyhow::Result<()> {
		let uid_text = uid.to_string();
		self.command(&[
			Arg::Raw("UID STORE "),
			Arg::Raw(&uid_text),
			Arg::Raw(" +FLAGS.SILENT ("),
			Arg::Raw(flag),
			Arg::Raw(")"),
		])
		.await?;
		Ok(())
	}

	/// Moves one mail. Without MOVE it is copied and marked `\Deleted`, and only expunged by
	/// UID when the server has UIDPLUS, so no other mail is ever expunged.
	pub async fn move_to(&mut self, uid: u32, folder: &str) -> anyhow::Result<()> {
		let uid_text = uid.to_string();
		let name = mailbox_name(folder);
		if self.has("MOVE") {
			self.command(&[
				Arg::Raw("UID MOVE "),
				Arg::Raw(&uid_text),
				Arg::Raw(" "),
				Arg::Str(name.as_bytes()),
			])
			.await?;
			return Ok(());
		}
		self.command(&[
			Arg::Raw("UID COPY "),
			Arg::Raw(&uid_text),
			Arg::Raw(" "),
			Arg::Str(name.as_bytes()),
		])
		.await?;
		self.store(uid, "\\Deleted").await?;
		if self.has("UIDPLUS") {
			self.command(&[Arg::Raw("UID EXPUNGE "), Arg::Raw(&uid_text)])
				.await?;
		}
		Ok(())
	}

	pub async fn logout(mut self) {
		if let Err(error) = self.command(&[Arg::Raw("LOGOUT")]).await {
			tracing::debug!(%error, "imap logout");
		}
	}

	async fn write(&mut self, bytes: &[u8]) -> anyhow::Result<()> {
		let stream = self.io.get_mut();
		stream.write_all(bytes).await?;
		stream.flush().await?;
		Ok(())
	}

	/// Sends a command, waiting for the server's go-ahead before each literal. Returns its tag.
	async fn send(&mut self, args: &[Arg<'_>]) -> anyhow::Result<String> {
		let tag = format!("v{}", self.next_tag);
		self.next_tag += 1;
		let mut buffer = format!("{tag} ").into_bytes();
		for arg in args {
			match arg {
				Arg::Raw(text) => buffer.extend_from_slice(text.as_bytes()),
				Arg::Str(bytes) if quotable(bytes) => buffer.extend(quote(bytes)),
				Arg::Str(bytes) => {
					buffer.extend_from_slice(format!("{{{}}}\r\n", bytes.len()).as_bytes());
					self.write(&buffer).await?;
					loop {
						match parse_response(&read_response(&mut self.io).await?)? {
							Response::Continue => break,
							Response::Status {
								tag: Some(answered),
								status,
								text,
								..
							} if answered == tag => {
								bail!("{status} {text}");
							}
							Response::Status {
								tag: None,
								status,
								text,
								..
							} if status == "BYE" => {
								bail!("the server said goodbye: {text}");
							}
							_ => {}
						}
					}
					buffer = bytes.to_vec();
				}
			}
		}
		buffer.extend_from_slice(b"\r\n");
		self.write(&buffer).await?;
		Ok(tag)
	}

	/// Reads responses up to the tagged one for `tag`, passing each untagged one to `seen`.
	async fn finish(&mut self, tag: &str, mut seen: impl FnMut(&Response)) -> anyhow::Result<()> {
		loop {
			let response = parse_response(&read_response(&mut self.io).await?)?;
			match &response {
				Response::Status {
					tag: Some(answered),
					status,
					text,
					..
				} if answered == tag => {
					if status == "OK" {
						return Ok(());
					}
					bail!("{status} {text}");
				}
				_ => seen(&response),
			}
		}
	}

	/// Runs a command and returns its untagged data.
	async fn command(&mut self, args: &[Arg<'_>]) -> anyhow::Result<Vec<Vec<Value>>> {
		let tag = self.send(args).await?;
		let mut data = Vec::new();
		self.finish(&tag, |response| {
			if let Response::Data(values) = response {
				data.push(values.clone());
			}
		})
		.await?;
		Ok(data)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn parse(text: &[u8]) -> Response {
		parse_response(text).unwrap()
	}

	#[test]
	fn literal_lengths() {
		assert_eq!(literal_length(b"* 1 FETCH (BODY[] {12}\r\n"), Some(12));
		assert_eq!(literal_length(b"A1 LOGIN {3+}\r\n"), Some(3));
		assert_eq!(literal_length(b"* OK {x}\r\n"), None);
		assert_eq!(literal_length(b"* OK done\r\n"), None);
		assert_eq!(literal_length(b"* OK {}\r\n"), None);
	}

	#[test]
	fn statuses_and_codes() {
		assert_eq!(
			parse(b"* OK [UIDVALIDITY 3857529045] UIDs valid\r\n"),
			Response::Status {
				tag: None,
				status: "OK".into(),
				code: Some("UIDVALIDITY 3857529045".into()),
				text: "UIDs valid".into(),
			}
		);
		assert_eq!(
			parse(b"v3 no [TRYCREATE] Don't \"go (there\r\n"),
			Response::Status {
				tag: Some("v3".into()),
				status: "NO".into(),
				code: Some("TRYCREATE".into()),
				text: "Don't \"go (there".into(),
			}
		);
		assert_eq!(parse(b"+ go ahead\r\n"), Response::Continue);
		assert!(parse_response(b"v1 FETCH\r\n").is_err());
	}

	#[test]
	fn fetch_with_literal_and_quoted_values() {
		let raw = b"* 2 FETCH (UID 7 INTERNALDATE \" 7-Jul-2026 02:44:25 -0700\" BODY[] {13}\r\nhi)\r\n{3}\r\n\"x\" ENVELOPE NIL FLAGS (\\Seen $Junk))\r\n";
		let Response::Data(values) = parse(raw) else {
			panic!("not data");
		};
		assert_eq!(values[0], Value::Atom("2".into()));
		let Value::List(items) = &values[2] else {
			panic!("no list");
		};
		assert_eq!(items[1], Value::Atom("7".into()));
		assert_eq!(items[4], Value::Atom("BODY[]".into()));
		assert_eq!(items[5], Value::Bytes(b"hi)\r\n{3}\r\n\"x\"".to_vec()));
		assert_eq!(items[7], Value::Nil);
		assert_eq!(
			items[9],
			Value::List(vec![
				Value::Atom("\\Seen".into()),
				Value::Atom("$Junk".into())
			])
		);
		assert_eq!(
			internal_date(&items[3].text().unwrap()).as_deref(),
			Some("2026-07-07T02:44:25-07:00")
		);
	}

	#[test]
	fn brackets_stay_in_one_atom() {
		let Response::Data(values) =
			parse(b"* 1 FETCH (UID 1 BODY[HEADER.FIELDS (DATE FROM)] {0}\r\n)\r\n")
		else {
			panic!("not data");
		};
		let Value::List(items) = &values[2] else {
			panic!("no list");
		};
		assert_eq!(
			items[2],
			Value::Atom("BODY[HEADER.FIELDS (DATE FROM)]".into())
		);
		assert_eq!(items[3], Value::Bytes(Vec::new()));
		assert!(parse_response(b"* 1 FETCH (UID 1\r\n").is_err());
		assert!(parse_response(b"* 1 FETCH (BODY[] {9}\r\nab)\r\n").is_err());
	}

	#[tokio::test]
	async fn responses_are_read_through_their_literals() {
		let wire: &[u8] = b"* 1 FETCH (BODY[] {8}\r\nv1 OK\r\n)\r\nv1 OK done\r\n";
		let mut reader = BufReader::new(wire);
		let first = read_response(&mut reader).await.unwrap();
		assert_eq!(first, b"* 1 FETCH (BODY[] {8}\r\nv1 OK\r\n)\r\n");
		let second = read_response(&mut reader).await.unwrap();
		assert_eq!(second, b"v1 OK done\r\n");
		assert!(read_response(&mut reader).await.is_err());
	}

	#[test]
	fn strings_quote_or_need_a_literal() {
		assert!(quotable(b"me@example.com"));
		assert_eq!(quote(br#"a"b\c"#), br#""a\"b\\c""#.to_vec());
		assert!(!quotable("パス".as_bytes()));
		assert!(!quotable(b"two\r\nlines"));
	}

	#[test]
	fn mailbox_names_use_modified_utf7() {
		assert_eq!(mailbox_name("INBOX/Invoices"), "INBOX/Invoices");
		assert_eq!(mailbox_name("Tom & Jerry"), "Tom &- Jerry");
		assert_eq!(
			mailbox_name("~peter/mail/台北/日本語"),
			"~peter/mail/&U,BTFw-/&ZeVnLIqe-"
		);
		assert_eq!(mailbox_name("請求書"), "&istsQmb4-");
	}

	#[test]
	fn search_dates() {
		let date = time::macros::date!(2026 - 03 - 07);
		assert_eq!(search_date(date), "7-Mar-2026");
	}

	#[test]
	fn tls_config_loads_system_roots() {
		match tls() {
			Ok(_) => assert!(tls().is_ok()),
			Err(error) => assert!(error.to_string().contains("root certificate")),
		}
	}
}
