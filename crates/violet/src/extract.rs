//! Text out of files that are stored as they are: office documents (zip + XML), emails,
//! RTF, HTML and plain text (txt, md, csv, tsv, vCard, iCalendar, JSON).

use std::io::{Cursor, Read};

use quick_xml::Reader;
use quick_xml::events::{BytesRef, BytesStart, Event};
use violet_model::normalize_date;
use zip::ZipArchive;

use crate::{Error, Result};

const MAX_ENTRY_BYTES: u64 = 64 * 1024 * 1024;
const MAX_TEXT_CHARS: usize = 1_000_000;
const MAX_MIME_DEPTH: usize = 4;
const SNIFFED_BYTES: usize = 4096;
const MAIL_HEADERS: [&str; 8] = [
	"from",
	"to",
	"subject",
	"date",
	"received",
	"message-id",
	"return-path",
	"mime-version",
];
const MIN_MAIL_HEADERS: usize = 2;
/// RTF groups whose text is not part of the document.
const RTF_SKIPPED: [&str; 12] = [
	"fonttbl",
	"colortbl",
	"stylesheet",
	"info",
	"pict",
	"object",
	"listtable",
	"listoverridetable",
	"rsidtbl",
	"generator",
	"themedata",
	"datastore",
];
const MONTHS: [&str; 12] = [
	"jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Extracted {
	pub text: String,
	/// A title the file itself carries, such as an email subject.
	pub title: Option<String>,
	/// `YYYY-MM-DD` the file itself carries, such as an email date.
	pub date: Option<String>,
}

/// Lowercase extension of a file name, empty when it has none.
pub fn extension(name: &str) -> String {
	violet_core::naming::extension(name).unwrap_or_default()
}

pub fn is_office(ext: &str) -> bool {
	matches!(ext, "docx" | "xlsx" | "pptx" | "odt" | "ods" | "odp")
}

pub fn is_plain(ext: &str) -> bool {
	matches!(ext, "eml" | "txt" | "md")
}

/// Whether [`plain`] reads files with this extension.
pub fn is_text(ext: &str) -> bool {
	is_plain(ext)
		|| matches!(
			ext,
			"csv" | "tsv" | "vcf" | "ics" | "rtf" | "json" | "html" | "htm"
		)
}

/// The extension a file without one should have, judged from its first bytes: `eml` for a
/// message that starts with mail headers.
pub fn sniff(bytes: &[u8]) -> Option<&'static str> {
	let head = String::from_utf8_lossy(&bytes[..bytes.len().min(SNIFFED_BYTES)]);
	let head = head.strip_prefix("From ").map_or(head.as_ref(), |rest| {
		rest.split_once('\n').map_or("", |(_, after)| after)
	});
	let (headers, _) = message(head);
	let first_is_header = head
		.lines()
		.next()
		.and_then(|line| line.split_once(':'))
		.is_some_and(|(name, _)| {
			!name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
		});
	let known = MAIL_HEADERS
		.iter()
		.filter(|name| header(&headers, name).is_some())
		.count();
	(first_is_header && known >= MIN_MAIL_HEADERS).then_some("eml")
}

/// The text of a docx, xlsx, pptx, odt, ods or odp file.
pub fn office(ext: &str, bytes: &[u8]) -> Result<Extracted> {
	let mut archive = ZipArchive::new(Cursor::new(bytes))
		.map_err(|error| Error::Invalid(format!("not a readable {ext} file: {error}")))?;
	let text = match ext {
		"docx" => {
			let xml = required(&mut archive, "word/document.xml")?;
			xml_text(&xml, &WORD)
		}
		"pptx" => {
			let mut text = String::new();
			for name in numbered(&archive, "ppt/slides/slide") {
				text.push_str(&xml_text(&required(&mut archive, &name)?, &SLIDE));
				text.push('\n');
			}
			text
		}
		"xlsx" => {
			let shared = match entry(&mut archive, "xl/sharedStrings.xml")? {
				Some(xml) => shared_strings(&xml),
				None => Vec::new(),
			};
			let mut text = String::new();
			for name in numbered(&archive, "xl/worksheets/sheet") {
				text.push_str(&sheet_text(&required(&mut archive, &name)?, &shared));
				text.push('\n');
			}
			text
		}
		_ => xml_text(&required(&mut archive, "content.xml")?, &OPEN_DOCUMENT),
	};
	Ok(Extracted {
		text: tidy(&text),
		..Extracted::default()
	})
}

/// The text of a file [`is_text`] accepts.
pub fn plain(ext: &str, bytes: &[u8]) -> Extracted {
	let text = String::from_utf8_lossy(bytes);
	let text = text.trim_start_matches('\u{feff}');
	let text = match ext {
		"eml" => return email(bytes),
		"rtf" => rtf_text(text),
		"html" | "htm" => strip_tags(text),
		"vcf" | "ics" => unfold(text),
		_ => text.to_string(),
	};
	Extracted {
		text: tidy(&text),
		..Extracted::default()
	}
}

/// vCard and iCalendar lines continued on the next line by a leading space or tab joined
/// back.
fn unfold(text: &str) -> String {
	let mut out = String::with_capacity(text.len());
	for line in text.lines() {
		match line.strip_prefix([' ', '\t']) {
			Some(rest) if !out.is_empty() => {
				out.pop();
				out.push_str(rest);
			}
			_ => out.push_str(line),
		}
		out.push('\n');
	}
	out
}

/// The text of an RTF document: control words dropped, paragraphs and tabs kept, `\'hh` read
/// as Windows-1252 and `\uN` as Unicode, the font, colour and style tables, pictures and
/// other `{\*...}` destinations left out.
fn rtf_text(rtf: &str) -> String {
	struct Group {
		skipped: bool,
		/// Characters to drop after a `\uN`, set by `\ucN`.
		fallback: usize,
	}
	let chars: Vec<char> = rtf.chars().collect();
	let mut out = String::new();
	let mut groups = vec![Group {
		skipped: false,
		fallback: 1,
	}];
	let mut skip_fallback = 0;
	let mut at = 0;
	let emit = |out: &mut String, groups: &[Group], skip_fallback: &mut usize, c: char| {
		if *skip_fallback > 0 {
			*skip_fallback -= 1;
		} else if !groups.last().is_some_and(|group| group.skipped) {
			out.push(c);
		}
	};
	while at < chars.len() {
		let c = chars[at];
		at += 1;
		match c {
			'{' => {
				let parent = groups.last();
				let skipped = parent.is_some_and(|group| group.skipped)
					|| chars.get(at..at + 2) == Some(&['\\', '*'][..]);
				let fallback = parent.map_or(1, |group| group.fallback);
				groups.push(Group { skipped, fallback });
				skip_fallback = 0;
			}
			'}' => {
				if groups.len() > 1 {
					groups.pop();
				}
				skip_fallback = 0;
			}
			'\r' | '\n' => {}
			'\\' => {
				let Some(&next) = chars.get(at) else {
					break;
				};
				if !next.is_ascii_alphabetic() {
					at += 1;
					match next {
						'\'' => {
							let hex: String = chars.iter().skip(at).take(2).collect();
							at += 2;
							if let Ok(byte) = u8::from_str_radix(&hex, 16) {
								emit(&mut out, &groups, &mut skip_fallback, windows_1252(byte));
							}
						}
						'~' => emit(&mut out, &groups, &mut skip_fallback, '\u{a0}'),
						'_' => emit(&mut out, &groups, &mut skip_fallback, '-'),
						'\n' | '\r' => emit(&mut out, &groups, &mut skip_fallback, '\n'),
						other => emit(&mut out, &groups, &mut skip_fallback, other),
					}
					continue;
				}
				let start = at;
				while chars.get(at).is_some_and(char::is_ascii_alphabetic) {
					at += 1;
				}
				let word: String = chars[start..at].iter().collect();
				let number_start = at;
				if chars.get(at) == Some(&'-') {
					at += 1;
				}
				while chars.get(at).is_some_and(char::is_ascii_digit) {
					at += 1;
				}
				let number: Option<i64> = chars[number_start..at]
					.iter()
					.collect::<String>()
					.parse()
					.ok();
				if chars.get(at) == Some(&' ') {
					at += 1;
				}
				let group = groups.last_mut().expect("the outer group stays");
				match word.as_str() {
					"par" | "line" | "sect" | "page" | "row" => {
						emit(&mut out, &groups, &mut skip_fallback, '\n');
					}
					"tab" | "cell" => emit(&mut out, &groups, &mut skip_fallback, '\t'),
					"uc" => group.fallback = number.unwrap_or(1).max(0) as usize,
					"u" => {
						let code = number.unwrap_or(0);
						let code = if code < 0 { code + 65_536 } else { code };
						let fallback = group.fallback;
						if let Some(c) = u32::try_from(code).ok().and_then(char::from_u32) {
							skip_fallback = 0;
							emit(&mut out, &groups, &mut skip_fallback, c);
						}
						skip_fallback = fallback;
					}
					word if RTF_SKIPPED.contains(&word) => group.skipped = true,
					_ => {}
				}
			}
			c => emit(&mut out, &groups, &mut skip_fallback, c),
		}
	}
	out
}

fn windows_1252(byte: u8) -> char {
	const HIGH: [char; 32] = [
		'€', '\u{81}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{8d}', 'Ž',
		'\u{8f}', '\u{90}', '‘', '’', '“', '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ', '\u{9d}',
		'ž', 'Ÿ',
	];
	match byte {
		0x80..=0x9f => HIGH[usize::from(byte - 0x80)],
		byte => char::from(byte),
	}
}

/// The first `YYYY-MM-DD`, `YYYY_MM_DD`, `YYYY.MM.DD` or `YYYYMMDD` in a file name that is a
/// real date.
pub fn date_in_name(name: &str) -> Option<String> {
	let chars: Vec<char> = name.chars().collect();
	let digits = |from: usize, count: usize| -> Option<String> {
		let run = chars.get(from..from + count)?;
		run.iter()
			.all(char::is_ascii_digit)
			.then(|| run.iter().collect())
	};
	let separator = |at: usize| usize::from(matches!(chars.get(at), Some('-' | '_' | '.')));
	(0..chars.len()).find_map(|start| {
		if start > 0 && chars[start - 1].is_ascii_digit() {
			return None;
		}
		let year = digits(start, 4)?;
		let month_at = start + 4 + separator(start + 4);
		let month = digits(month_at, 2)?;
		let day_at = month_at + 2 + separator(month_at + 2);
		let day = digits(day_at, 2)?;
		if chars.get(day_at + 2).is_some_and(char::is_ascii_digit) {
			return None;
		}
		if !(1900..=2100).contains(&year.parse::<u32>().ok()?) {
			return None;
		}
		normalize_date(&format!("{year}-{month}-{day}"))
	})
}

type Archive<'a> = ZipArchive<Cursor<&'a [u8]>>;

fn entry(archive: &mut Archive<'_>, name: &str) -> Result<Option<Vec<u8>>> {
	let file = match archive.by_name(name) {
		Ok(file) => file,
		Err(zip::result::ZipError::FileNotFound) => return Ok(None),
		Err(error) => return Err(Error::Invalid(format!("{name}: {error}"))),
	};
	let mut out = Vec::new();
	file.take(MAX_ENTRY_BYTES + 1)
		.read_to_end(&mut out)
		.map_err(|error| Error::Invalid(format!("{name}: {error}")))?;
	if out.len() as u64 > MAX_ENTRY_BYTES {
		return Err(Error::Invalid(format!("{name} is too large")));
	}
	Ok(Some(out))
}

fn required(archive: &mut Archive<'_>, name: &str) -> Result<Vec<u8>> {
	entry(archive, name)?.ok_or_else(|| Error::Invalid(format!("{name} is missing")))
}

/// `<prefix><n>.xml` entries in numeric order.
fn numbered(archive: &Archive<'_>, prefix: &str) -> Vec<String> {
	let mut names: Vec<(u32, String)> = archive
		.file_names()
		.filter_map(|name| {
			let number = name.strip_prefix(prefix)?.strip_suffix(".xml")?;
			Some((number.parse().ok()?, name.to_string()))
		})
		.collect();
	names.sort();
	names.into_iter().map(|(_, name)| name).collect()
}

struct Rules {
	/// Text is kept only inside these elements.
	text_in: &'static [&'static [u8]],
	line_after: &'static [&'static [u8]],
	tab: &'static [&'static [u8]],
	line_break: &'static [&'static [u8]],
	space: &'static [&'static [u8]],
}

const WORD: Rules = Rules {
	text_in: &[b"t"],
	line_after: &[b"p"],
	tab: &[b"tab"],
	line_break: &[b"br", b"cr"],
	space: &[],
};
const SLIDE: Rules = Rules {
	text_in: &[b"t"],
	line_after: &[b"p"],
	tab: &[],
	line_break: &[b"br"],
	space: &[],
};
const OPEN_DOCUMENT: Rules = Rules {
	text_in: &[b"p", b"h"],
	line_after: &[b"p", b"h"],
	tab: &[b"tab"],
	line_break: &[b"line-break"],
	space: &[b"s"],
};

fn reference(reference: &BytesRef<'_>) -> Option<char> {
	if let Ok(Some(resolved)) = reference.resolve_char_ref() {
		return Some(resolved);
	}
	match &**reference {
		b"lt" => Some('<'),
		b"gt" => Some('>'),
		b"amp" => Some('&'),
		b"quot" => Some('"'),
		b"apos" => Some('\''),
		_ => None,
	}
}

/// Walks the XML, handing each event to `visit` until the end or the first syntax error.
fn walk(xml: &[u8], mut visit: impl FnMut(&Event<'_>)) {
	let mut reader = Reader::from_reader(xml);
	let mut buffer = Vec::new();
	loop {
		match reader.read_event_into(&mut buffer) {
			Ok(Event::Eof) | Err(_) => break,
			Ok(event) => visit(&event),
		}
		buffer.clear();
	}
}

fn push_text(out: &mut String, event: &Event<'_>) {
	match event {
		Event::Text(text) => {
			if let Ok(text) = text.decode() {
				out.push_str(&text);
			}
		}
		Event::CData(data) => {
			if let Ok(text) = data.decode() {
				out.push_str(&text);
			}
		}
		Event::GeneralRef(entity) => out.extend(reference(entity)),
		_ => {}
	}
}

fn xml_text(xml: &[u8], rules: &Rules) -> String {
	let mut out = String::new();
	let mut inside = 0usize;
	let has = |names: &[&[u8]], name: &[u8]| names.contains(&name);
	let mark = |out: &mut String, name: &[u8]| {
		if has(rules.tab, name) {
			out.push('\t');
		} else if has(rules.line_break, name) {
			out.push('\n');
		} else if has(rules.space, name) {
			out.push(' ');
		}
	};
	walk(xml, |event| match event {
		Event::Start(start) => {
			let name = start.local_name();
			if has(rules.text_in, name.as_ref()) {
				inside += 1;
			}
			mark(&mut out, name.as_ref());
		}
		Event::Empty(empty) => mark(&mut out, empty.local_name().as_ref()),
		Event::End(end) => {
			let name = end.local_name();
			if has(rules.text_in, name.as_ref()) {
				inside = inside.saturating_sub(1);
			}
			if has(rules.line_after, name.as_ref()) {
				out.push('\n');
			}
		}
		other if inside > 0 => push_text(&mut out, other),
		_ => {}
	});
	out
}

fn shared_strings(xml: &[u8]) -> Vec<String> {
	let mut strings = Vec::new();
	let mut current = String::new();
	let (mut in_text, mut phonetic) = (false, 0usize);
	walk(xml, |event| match event {
		Event::Start(start) => match start.local_name().as_ref() {
			b"si" => current.clear(),
			b"rPh" => phonetic += 1,
			b"t" => in_text = true,
			_ => {}
		},
		Event::End(end) => match end.local_name().as_ref() {
			b"si" => strings.push(std::mem::take(&mut current)),
			b"rPh" => phonetic = phonetic.saturating_sub(1),
			b"t" => in_text = false,
			_ => {}
		},
		Event::Empty(empty) if empty.local_name().as_ref() == b"si" => strings.push(String::new()),
		other if in_text && phonetic == 0 => push_text(&mut current, other),
		_ => {}
	});
	strings
}

fn attribute(element: &BytesStart<'_>, name: &[u8]) -> Option<Vec<u8>> {
	element
		.attributes()
		.flatten()
		.find(|attribute| attribute.key.as_ref() == name)
		.map(|attribute| attribute.value.into_owned())
}

fn sheet_text(xml: &[u8], shared: &[String]) -> String {
	let mut out = String::new();
	let mut row: Vec<String> = Vec::new();
	let mut value = String::new();
	let (mut is_shared, mut in_value) = (false, false);
	walk(xml, |event| match event {
		Event::Start(start) => match start.local_name().as_ref() {
			b"row" => row.clear(),
			b"c" => {
				value.clear();
				is_shared = attribute(start, b"t").as_deref() == Some(b"s");
			}
			b"v" | b"t" => in_value = true,
			_ => {}
		},
		Event::End(end) => match end.local_name().as_ref() {
			b"v" | b"t" => in_value = false,
			b"c" => {
				let cell = if is_shared {
					value
						.trim()
						.parse::<usize>()
						.ok()
						.and_then(|index| shared.get(index))
						.cloned()
						.unwrap_or_default()
				} else {
					std::mem::take(&mut value)
				};
				row.push(cell);
			}
			b"row" => {
				if row.iter().any(|cell| !cell.trim().is_empty()) {
					out.push_str(row.join("\t").trim_end());
					out.push('\n');
				}
				row.clear();
			}
			_ => {}
		},
		other if in_value => push_text(&mut value, other),
		_ => {}
	});
	out
}

/// Trailing spaces removed, runs of blank lines collapsed, capped in length.
fn tidy(text: &str) -> String {
	let mut out = String::new();
	let mut blank = 0;
	for line in text.lines() {
		let line = line.trim_end();
		blank = if line.is_empty() { blank + 1 } else { 0 };
		if blank > 1 {
			continue;
		}
		out.push_str(line);
		out.push('\n');
	}
	let trimmed = out.trim();
	match trimmed.char_indices().nth(MAX_TEXT_CHARS) {
		Some((cut, _)) => trimmed[..cut].to_string(),
		None => trimmed.to_string(),
	}
}

type Headers = Vec<(String, String)>;

fn header<'a>(headers: &'a Headers, name: &str) -> Option<&'a str> {
	headers
		.iter()
		.find(|(known, _)| known == name)
		.map(|(_, value)| value.as_str())
}

/// Splits a message or MIME part into unfolded headers (names in lowercase) and body.
fn message(raw: &str) -> (Headers, &str) {
	let (head, body) = match (raw.find("\r\n\r\n"), raw.find("\n\n")) {
		(Some(crlf), Some(lf)) if crlf < lf => (&raw[..crlf], &raw[crlf + 4..]),
		(_, Some(lf)) => (&raw[..lf], &raw[lf + 2..]),
		(Some(crlf), None) => (&raw[..crlf], &raw[crlf + 4..]),
		(None, None) => (raw, ""),
	};
	let mut headers: Headers = Vec::new();
	for line in head.lines() {
		if line.starts_with([' ', '\t']) {
			if let Some((_, value)) = headers.last_mut() {
				value.push(' ');
				value.push_str(line.trim());
			}
		} else if let Some((name, value)) = line.split_once(':') {
			headers.push((name.trim().to_lowercase(), value.trim().to_string()));
		}
	}
	(headers, body)
}

fn email(bytes: &[u8]) -> Extracted {
	let raw = String::from_utf8_lossy(bytes);
	let (headers, body) = message(&raw);
	let mut text = String::new();
	for (label, name) in [
		("Subject", "subject"),
		("From", "from"),
		("To", "to"),
		("Date", "date"),
	] {
		if let Some(value) = header(&headers, name) {
			text.push_str(&format!("{label}: {}\n", encoded_words(value)));
		}
	}
	text.push('\n');
	text.push_str(&part_text(&headers, body, 0).unwrap_or_default());
	Extracted {
		text: tidy(&text),
		title: header(&headers, "subject")
			.map(encoded_words)
			.filter(|subject| !subject.trim().is_empty()),
		date: header(&headers, "date").and_then(mail_date),
	}
}

fn parameter(value: &str, name: &str) -> Option<String> {
	value.split(';').skip(1).find_map(|part| {
		let (key, value) = part.split_once('=')?;
		(key.trim().eq_ignore_ascii_case(name)).then(|| value.trim().trim_matches('"').to_string())
	})
}

/// The readable text of a MIME part: its first `text/plain` part, else its first `text/html`
/// part without the tags.
fn part_text(headers: &Headers, body: &str, depth: usize) -> Option<String> {
	let content_type = header(headers, "content-type").unwrap_or("text/plain");
	let kind = content_type
		.split(';')
		.next()
		.unwrap_or_default()
		.trim()
		.to_lowercase();
	if kind.starts_with("multipart/") {
		let boundary = parameter(content_type, "boundary")?;
		if depth >= MAX_MIME_DEPTH || boundary.is_empty() {
			return None;
		}
		let delimiter = format!("--{boundary}");
		let parts: Vec<(Headers, &str)> = body
			.split(delimiter.as_str())
			.skip(1)
			.take_while(|part| !part.starts_with("--"))
			.map(|part| message(part.trim_start_matches(['\r', '\n'])))
			.collect();
		let is_html = |headers: &Headers| {
			header(headers, "content-type").is_some_and(|kind| kind.to_lowercase().contains("html"))
		};
		let (html, other): (Vec<_>, Vec<_>) =
			parts.iter().partition(|(headers, _)| is_html(headers));
		return other
			.into_iter()
			.chain(html)
			.find_map(|(headers, body)| part_text(headers, body, depth + 1));
	}
	if !kind.starts_with("text/") {
		return None;
	}
	let encoding = header(headers, "content-transfer-encoding")
		.unwrap_or_default()
		.trim()
		.to_lowercase();
	let decoded = match encoding.as_str() {
		"base64" => String::from_utf8_lossy(&base64(body)).into_owned(),
		"quoted-printable" => String::from_utf8_lossy(&quoted_printable(body, false)).into_owned(),
		_ => body.to_string(),
	};
	Some(if kind == "text/html" {
		strip_tags(&decoded)
	} else {
		decoded
	})
}

fn strip_tags(html: &str) -> String {
	let mut out = String::new();
	let mut in_tag = false;
	for c in html.chars() {
		match c {
			'<' => in_tag = true,
			'>' if in_tag => {
				in_tag = false;
				out.push(' ');
			}
			c if !in_tag => out.push(c),
			_ => {}
		}
	}
	out.replace("&nbsp;", " ")
		.replace("&lt;", "<")
		.replace("&gt;", ">")
		.replace("&quot;", "\"")
		.replace("&amp;", "&")
}

fn base64(text: &str) -> Vec<u8> {
	let mut out = Vec::new();
	let (mut bits, mut count) = (0u32, 0u8);
	for byte in text.bytes() {
		let value = match byte {
			b'A'..=b'Z' => byte - b'A',
			b'a'..=b'z' => byte - b'a' + 26,
			b'0'..=b'9' => byte - b'0' + 52,
			b'+' | b'-' => 62,
			b'/' | b'_' => 63,
			b'=' => break,
			_ => continue,
		};
		bits = (bits << 6) | u32::from(value);
		count += 6;
		if count >= 8 {
			count -= 8;
			out.push((bits >> count) as u8);
			bits &= (1 << count) - 1;
		}
	}
	out
}

fn quoted_printable(text: &str, underscore_is_space: bool) -> Vec<u8> {
	let bytes = text.as_bytes();
	let mut out = Vec::with_capacity(bytes.len());
	let mut at = 0;
	while at < bytes.len() {
		match bytes[at] {
			b'=' if bytes[at + 1..].starts_with(b"\r\n") => at += 3,
			b'=' if bytes[at + 1..].starts_with(b"\n") => at += 2,
			b'=' => {
				let hex = bytes
					.get(at + 1..at + 3)
					.and_then(|pair| std::str::from_utf8(pair).ok())
					.and_then(|pair| u8::from_str_radix(pair, 16).ok());
				match hex {
					Some(byte) => {
						out.push(byte);
						at += 3;
					}
					None => {
						out.push(b'=');
						at += 1;
					}
				}
			}
			b'_' if underscore_is_space => {
				out.push(b' ');
				at += 1;
			}
			byte => {
				out.push(byte);
				at += 1;
			}
		}
	}
	out
}

/// Decodes RFC 2047 `=?utf-8?B?...?=` words; words in another charset are left as they are.
fn encoded_words(value: &str) -> String {
	let mut out = String::new();
	let mut rest = value;
	let mut after_word = false;
	while let Some(start) = rest.find("=?") {
		let decoded = (|| {
			let mut fields = rest[start + 2..].splitn(3, '?');
			let (charset, encoding, tail) = (fields.next()?, fields.next()?, fields.next()?);
			let end = tail.find("?=")?;
			let charset = charset.to_lowercase();
			if charset != "utf-8" && charset != "us-ascii" {
				return None;
			}
			let bytes = match encoding {
				"B" | "b" => base64(&tail[..end]),
				"Q" | "q" => quoted_printable(&tail[..end], true),
				_ => return None,
			};
			let consumed = start + 2 + charset.len() + 1 + encoding.len() + 1 + end + 2;
			Some((String::from_utf8_lossy(&bytes).into_owned(), consumed))
		})();
		let between = &rest[..start];
		match decoded {
			Some((word, consumed)) => {
				if !(after_word && between.trim().is_empty()) {
					out.push_str(between);
				}
				out.push_str(&word);
				rest = &rest[consumed..];
				after_word = true;
			}
			None => {
				out.push_str(&rest[..start + 2]);
				rest = &rest[start + 2..];
				after_word = false;
			}
		}
	}
	out.push_str(rest);
	out
}

/// `Thu, 14 Mar 2024 10:00:00 +0900` to `2024-03-14`.
fn mail_date(value: &str) -> Option<String> {
	let tokens: Vec<&str> = value
		.split(|c: char| c.is_whitespace() || c == ',')
		.filter(|token| !token.is_empty())
		.collect();
	tokens.windows(3).find_map(|window| {
		let month = MONTHS
			.iter()
			.position(|month| window[1].to_lowercase().starts_with(month))?;
		let day: u32 = window[0].parse().ok()?;
		let year: u32 = window[2].parse().ok()?;
		normalize_date(&format!("{year:04}-{:02}-{day:02}", month + 1))
	})
}

#[cfg(test)]
mod tests {
	use std::io::Write;

	use zip::write::SimpleFileOptions;

	use super::*;

	fn archive(entries: &[(&str, &str)]) -> Vec<u8> {
		let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
		for (name, content) in entries {
			writer
				.start_file(*name, SimpleFileOptions::default())
				.unwrap();
			writer.write_all(content.as_bytes()).unwrap();
		}
		writer.finish().unwrap().into_inner()
	}

	#[test]
	fn extension_is_lowercase_and_ignores_dotfiles() {
		assert_eq!(extension("Scan 001.PDF"), "pdf");
		assert_eq!(extension("archive.tar.gz"), "gz");
		assert_eq!(extension("README"), "");
		assert_eq!(extension(".hidden"), "");
		assert_eq!(extension("Re. Your order"), "");
		assert_eq!(extension("data.json.txt"), "json.txt");
	}

	#[test]
	fn docx_paragraphs_tabs_and_entities() {
		let xml = r#"<w:document xmlns:w="w"><w:body>
			<w:p><w:r><w:t>Invoice &amp; receipt</w:t></w:r><w:r><w:tab/><w:t xml:space="preserve"> No. 12</w:t></w:r></w:p>
			<w:p><w:r><w:t>合計 &#165;1,200</w:t><w:br/><w:t>Thanks</w:t></w:r></w:p>
		</w:body></w:document>"#;
		let bytes = archive(&[("word/document.xml", xml)]);
		assert_eq!(
			office("docx", &bytes).unwrap().text,
			"Invoice & receipt\t No. 12\n合計 ¥1,200\nThanks"
		);
	}

	#[test]
	fn pptx_slides_in_numeric_order() {
		let slide = |text: &str| {
			format!(r#"<p:sld xmlns:a="a"><a:p><a:r><a:t>{text}</a:t></a:r></a:p></p:sld>"#)
		};
		let bytes = archive(&[
			("ppt/slides/slide10.xml", &slide("ten")),
			("ppt/slides/slide2.xml", &slide("two")),
			("ppt/slides/slide1.xml", &slide("one")),
		]);
		assert_eq!(office("pptx", &bytes).unwrap().text, "one\n\ntwo\n\nten");
	}

	#[test]
	fn xlsx_resolves_shared_strings_and_skips_phonetic_runs() {
		let shared = r#"<sst><si><t>品名</t><rPh><t>ヒンメイ</t></rPh></si><si><r><t>Pri</t></r><r><t>ce</t></r></si></sst>"#;
		let sheet = r#"<worksheet><sheetData>
			<row><c t="s"><v>0</v></c><c t="s"><v>1</v></c></row>
			<row><c t="inlineStr"><is><t>Pen</t></is></c><c><v>120</v></c></row>
			<row><c/></row>
		</sheetData></worksheet>"#;
		let bytes = archive(&[
			("xl/sharedStrings.xml", shared),
			("xl/worksheets/sheet1.xml", sheet),
		]);
		assert_eq!(
			office("xlsx", &bytes).unwrap().text,
			"品名\tPrice\nPen\t120"
		);
	}

	#[test]
	fn open_document_text() {
		let xml = r#"<office:document-content xmlns:text="t"><office:body>
			<text:h>Title</text:h>
			<text:p>One<text:s/>two<text:tab/>three<text:line-break/><text:span>four</text:span></text:p>
		</office:body></office:document-content>"#;
		let bytes = archive(&[("content.xml", xml)]);
		assert_eq!(
			office("odt", &bytes).unwrap().text,
			"Title\nOne two\tthree\nfour"
		);
	}

	#[test]
	fn broken_office_files_are_invalid() {
		assert!(matches!(
			office("docx", b"not a zip"),
			Err(Error::Invalid(_))
		));
		let bytes = archive(&[("other.xml", "<a/>")]);
		assert!(matches!(office("docx", &bytes), Err(Error::Invalid(_))));
	}

	#[test]
	fn plain_text_is_kept() {
		let text = plain("txt", "\u{feff}line one  \n\n\n\nline two\n".as_bytes());
		assert_eq!(text.text, "line one\n\nline two");
		assert_eq!(text.title, None);
	}

	#[test]
	fn email_headers_and_plain_body() {
		let raw = "From: Acme <billing@acme.test>\r\nSubject: =?UTF-8?B?6KuL5rGC5pu4?=\r\n =?utf-8?Q?_March?=\r\nDate: Thu, 14 Mar 2024 10:00:00 +0900\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: quoted-printable\r\n\r\nTotal: =C2=A51,200=\r\n due soon\r\n";
		let extracted = plain("eml", raw.as_bytes());
		assert_eq!(extracted.title.as_deref(), Some("請求書 March"));
		assert_eq!(extracted.date.as_deref(), Some("2024-03-14"));
		assert_eq!(
			extracted.text,
			"Subject: 請求書 March\nFrom: Acme <billing@acme.test>\nDate: Thu, 14 Mar 2024 10:00:00 +0900\n\nTotal: ¥1,200 due soon"
		);
	}

	#[test]
	fn email_multipart_prefers_plain_text() {
		let raw = "Subject: Hi\nContent-Type: multipart/alternative; boundary=\"XX\"\n\npreamble\n--XX\nContent-Type: text/html\n\n<p>Hello <b>html</b></p>\n--XX\nContent-Type: text/plain\nContent-Transfer-Encoding: base64\n\nSGVsbG8gcGxhaW4=\n--XX--\n";
		assert_eq!(
			plain("eml", raw.as_bytes()).text,
			"Subject: Hi\n\nHello plain"
		);
		let html_only = "Subject: Hi\nContent-Type: text/html\n\n<p>Hello &amp; <b>bye</b></p>\n";
		assert_eq!(
			plain("eml", html_only.as_bytes()).text,
			"Subject: Hi\n\n Hello &  bye"
		);
	}

	#[test]
	fn rtf_control_words_are_stripped() {
		let rtf = r"{\rtf1\ansi\ansicpg1252\deff0{\fonttbl{\f0\fswiss Helvetica;}}{\colortbl;\red0\green0\blue0;}
{\*\generator Riched20;}\viewkind4\uc1\pard\f0\fs20 Facture \'e9lectricit\'e9\par
Total\tab 12,50 \'80\line
\b Merci\b0  \{ok\} \u35531?\u27714?\par
{\pict\pngblip 89504e47}}";
		assert_eq!(
			plain("rtf", rtf.as_bytes()).text,
			"Facture électricité\nTotal\t12,50 €\nMerci {ok} 請求"
		);
	}

	#[test]
	fn csv_vcard_and_calendar_are_read_as_text() {
		assert_eq!(
			plain("csv", "date,amount\n2024-03-14,1200\n".as_bytes()).text,
			"date,amount\n2024-03-14,1200"
		);
		let card = "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Sakura\r\n  Clinic\r\nTEL:03-1234-5678\r\nEND:VCARD\r\n";
		assert_eq!(
			plain("vcf", card.as_bytes()).text,
			"BEGIN:VCARD\nVERSION:3.0\nFN:Sakura Clinic\nTEL:03-1234-5678\nEND:VCARD"
		);
		let event =
			"BEGIN:VEVENT\nSUMMARY:Dentist appoint\n ment\nDTSTART:20240314T100000\nEND:VEVENT\n";
		assert!(
			plain("ics", event.as_bytes())
				.text
				.contains("SUMMARY:Dentist appointment\n")
		);
		assert!(is_text("json") && is_text("tsv") && !is_text("doc"));
		assert!(!is_plain("csv"));
		assert_eq!(
			plain("html", b"<p>Total&nbsp;<b>1,200</b> yen</p>").text,
			"Total  1,200  yen"
		);
	}

	#[test]
	fn files_without_extension_that_start_with_mail_headers_are_mail() {
		let mail = "Return-Path: <a@b.test>\r\nReceived: from mx\r\n\tby host\r\nFrom: A <a@b.test>\r\nSubject: Hi\r\n\r\nBody\r\n";
		assert_eq!(sniff(mail.as_bytes()), Some("eml"));
		let mbox = "From a@b.test Thu Mar 14 10:00:00 2024\nFrom: a@b.test\nDate: Thu, 14 Mar 2024 10:00:00 +0900\n\nBody\n";
		assert_eq!(sniff(mbox.as_bytes()), Some("eml"));
		assert_eq!(sniff(b"Subject: only one header\n\nbody"), None);
		assert_eq!(sniff(b"Notes from: the meeting\nDate: soon\n"), None);
		assert_eq!(sniff(&[0, 1, 2, 3]), None);
	}

	#[test]
	fn dates_in_file_names() {
		assert_eq!(
			date_in_name("scan 2024-03-14 receipt").as_deref(),
			Some("2024-03-14")
		);
		assert_eq!(
			date_in_name("IMG_20240314_101500").as_deref(),
			Some("2024-03-14")
		);
		assert_eq!(date_in_name("2024.03.14").as_deref(), Some("2024-03-14"));
		assert_eq!(date_in_name("invoice 123456789012"), None);
		assert_eq!(date_in_name("2024-13-40 nothing"), None);
		assert_eq!(date_in_name("report"), None);
	}
}
