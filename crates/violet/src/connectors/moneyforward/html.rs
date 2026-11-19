//! What the Money Forward pages tell, read from their HTML as text.

use serde::Deserialize;
use time::{Date, Month};

const DATE_SEPARATORS: &str = "年月/-. ";

/// A slip a list page links to, with the text of its link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slip {
	pub id: String,
	pub label: String,
}

/// One entry of `gon.accounts` on the account selector.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
	pub form_action: String,
	#[serde(default)]
	pub active: bool,
	#[serde(default)]
	pub available: bool,
}

pub fn payslips(html: &str) -> Vec<Slip> {
	slips(
		html,
		"/my/reports/periods/",
		&["/payslip", "/payslips_v2.pdf"],
	)
}

pub fn bonus_slips(html: &str) -> Vec<Slip> {
	slips(
		html,
		"/my/reports/bonus_periods/",
		&["/bonus_slip", "/bonus_slips_v2.pdf"],
	)
}

pub fn withholding_slips(html: &str) -> Vec<Slip> {
	slips(
		html,
		"/my/reports/terms/",
		&["/withholding_slip.pdf", "/withholding_slip"],
	)
}

/// Adds the slips not known yet; a label with a date replaces one without.
pub fn merge(known: &mut Vec<Slip>, more: Vec<Slip>) {
	for slip in more {
		match known.iter_mut().find(|other| other.id == slip.id) {
			Some(other) => {
				let better = date(&other.label).is_none()
					&& (other.label.is_empty() || date(&slip.label).is_some());
				if better && !slip.label.is_empty() {
					other.label = slip.label;
				}
			}
			None => known.push(slip),
		}
	}
}

fn slips(html: &str, prefix: &str, suffixes: &[&str]) -> Vec<Slip> {
	let mut found = Vec::new();
	for (href, label) in links(html) {
		if let Some(id) = slip_id(&href, prefix, suffixes) {
			merge(&mut found, vec![Slip { id, label }]);
		}
	}
	found
}

fn slip_id(href: &str, prefix: &str, suffixes: &[&str]) -> Option<String> {
	let path = href.split(['?', '#']).next()?.trim();
	let path = match path.split_once("://") {
		Some((_, rest)) => &rest[rest.find('/')?..],
		None => path,
	};
	let middle = path.strip_prefix(prefix)?;
	suffixes
		.iter()
		.find_map(|suffix| middle.strip_suffix(suffix))
		.filter(|id| is_id(id))
		.map(str::to_owned)
}

pub fn is_id(id: &str) -> bool {
	!id.is_empty()
		&& id.len() <= 200
		&& id
			.chars()
			.all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '=' | '%'))
}

/// The `href` and the text of every link.
pub fn links(html: &str) -> Vec<(String, String)> {
	let mut found = Vec::new();
	let mut from = 0;
	while let Some((attributes, after)) = next_tag(html, "a", from) {
		from = after;
		let Some(href) = attribute(&attributes, "href") else {
			continue;
		};
		let end = find_ignoring_case(html, after, "</a").unwrap_or(html.len());
		found.push((href, text(&html[after..end])));
	}
	found
}

/// The token of `<meta name="csrf-token" content="...">`.
pub fn csrf(html: &str) -> Option<String> {
	let mut from = 0;
	while let Some((attributes, after)) = next_tag(html, "meta", from) {
		from = after;
		if attribute(&attributes, "name").as_deref() == Some("csrf-token") {
			return attribute(&attributes, "content").filter(|token| !token.is_empty());
		}
	}
	None
}

/// The accounts in the page's `gon.accounts=[...];` script.
pub fn accounts(html: &str) -> Option<Vec<Account>> {
	const NAME: &str = "gon.accounts";
	let at = html.find(NAME)?;
	let json = html[at + NAME.len()..].trim_start().strip_prefix('=')?;
	serde_json::Deserializer::from_str(json)
		.into_iter::<Vec<Account>>()
		.next()?
		.ok()
}

/// The first full date of a label as `YYYY-MM-DD`: `2026年(令和08)年10月23日支給分`,
/// `2026/10/23`, `令和8年10月23日`.
pub fn date(label: &str) -> Option<String> {
	let text = without_parentheses(&ascii_digits(label));
	let bytes = text.as_bytes();
	let mut runs = Vec::new();
	let mut at = 0;
	while at < bytes.len() {
		if bytes[at].is_ascii_digit() {
			let start = at;
			while at < bytes.len() && bytes[at].is_ascii_digit() {
				at += 1;
			}
			runs.push((start, at));
		} else {
			at += 1;
		}
	}
	let joined = |from: usize, to: usize| {
		let between = &text[from..to];
		!between.is_empty() && between.chars().all(|c| DATE_SEPARATORS.contains(c))
	};
	runs.windows(3).find_map(|runs| {
		let (year, month, day) = (runs[0], runs[1], runs[2]);
		if !joined(year.1, month.0) || !joined(month.1, day.0) {
			return None;
		}
		let digits = &text[year.0..year.1];
		let number: i32 = digits.parse().ok()?;
		let year = match digits.len() {
			4 => number,
			1 | 2 if text[..year.0].trim_end().ends_with("令和") => 2018 + number,
			_ => return None,
		};
		let month: u8 = text[month.0..month.1].parse().ok()?;
		let day: u8 = text[day.0..day.1].parse().ok()?;
		Date::from_calendar_date(year, Month::try_from(month).ok()?, day).ok()?;
		Some(format!("{year:04}-{month:02}-{day:02}"))
	})
}

/// The year and month of a label that names no day, as `YYYY-MM`: `2023年2月賞与`.
pub fn month(label: &str) -> Option<String> {
	let text = without_parentheses(&ascii_digits(label));
	text.match_indices('年').find_map(|(at, _)| {
		let year = text[..at].trim_end();
		let digits =
			&year[year.len() - year.bytes().rev().take_while(u8::is_ascii_digit).count()..];
		let year: i32 = (digits.len() == 4).then(|| digits.parse().ok())??;
		let rest = text[at + '年'.len_utf8()..].trim_start();
		let digits = &rest[..rest.bytes().take_while(u8::is_ascii_digit).count()];
		let month: u8 = digits.parse().ok()?;
		rest[digits.len()..]
			.trim_start()
			.starts_with('月')
			.then_some(())?;
		Month::try_from(month).ok()?;
		Some(format!("{year:04}-{month:02}"))
	})
}

/// Whether `text` is a date written `YYYY-MM-DD`.
pub fn is_iso_date(text: &str) -> bool {
	let bytes = text.as_bytes();
	bytes.len() == 10 && bytes[4] == b'-' && bytes[7] == b'-' && date(text).as_deref() == Some(text)
}

/// The year of the 令和 era a label names: 7 for `令和07年分`, 1 for `令和元年分`.
pub fn reiwa_year(label: &str) -> Option<u32> {
	let text = ascii_digits(label);
	let rest = text[text.find("令和")? + "令和".len()..].trim_start();
	if rest.starts_with('元') {
		return Some(1);
	}
	let digits = &rest[..rest.bytes().take_while(u8::is_ascii_digit).count()];
	digits.parse().ok().filter(|year| (1..=99).contains(year))
}

fn ascii_digits(text: &str) -> String {
	text.chars()
		.map(|c| match c {
			'０'..='９' => char::from_digit(c as u32 - '０' as u32, 10).unwrap_or(c),
			_ => c,
		})
		.collect()
}

fn without_parentheses(text: &str) -> String {
	let mut depth = 0_u32;
	let mut out = String::with_capacity(text.len());
	for c in text.chars() {
		match c {
			'(' | '（' => depth += 1,
			')' | '）' => depth = depth.saturating_sub(1),
			_ if depth == 0 => out.push(c),
			_ => {}
		}
	}
	out
}

fn attribute(attributes: &[(String, String)], name: &str) -> Option<String> {
	attributes
		.iter()
		.find(|(known, _)| known == name)
		.map(|(_, value)| value.clone())
}

fn find_ignoring_case(text: &str, from: usize, needle: &str) -> Option<usize> {
	let (text, needle) = (text.as_bytes(), needle.as_bytes());
	let last = text.len().checked_sub(needle.len())?;
	(from..=last).find(|&at| text[at..at + needle.len()].eq_ignore_ascii_case(needle))
}

/// The attributes of the next `<name ...>` from `from` on, and where its content starts.
fn next_tag(html: &str, name: &str, mut from: usize) -> Option<(Vec<(String, String)>, usize)> {
	let open = format!("<{name}");
	loop {
		from = find_ignoring_case(html, from, &open)? + open.len();
		let rest = &html[from..];
		if rest.starts_with(|c: char| c.is_ascii_whitespace() || c == '>' || c == '/') {
			let (attributes, used) = attributes(rest);
			return Some((attributes, from + used));
		}
	}
}

/// The attributes of a tag, names in lowercase, and the length up to and with its `>`.
fn attributes(tag: &str) -> (Vec<(String, String)>, usize) {
	let bytes = tag.as_bytes();
	let space = |at: usize| bytes.get(at).is_some_and(u8::is_ascii_whitespace);
	let mut found = Vec::new();
	let mut at = 0;
	loop {
		while space(at) || bytes.get(at) == Some(&b'/') {
			at += 1;
		}
		match bytes.get(at) {
			None => return (found, at),
			Some(b'>') => return (found, at + 1),
			Some(_) => {}
		}
		let start = at;
		while at < bytes.len() && !space(at) && !matches!(bytes[at], b'=' | b'>' | b'/') {
			at += 1;
		}
		let name = tag[start..at].to_ascii_lowercase();
		while space(at) {
			at += 1;
		}
		let mut value = String::new();
		if bytes.get(at) == Some(&b'=') {
			at += 1;
			while space(at) {
				at += 1;
			}
			match bytes.get(at) {
				Some(&quote @ (b'"' | b'\'')) => {
					let start = at + 1;
					at = start;
					while at < bytes.len() && bytes[at] != quote {
						at += 1;
					}
					value = unescape(&tag[start..at]);
					at = (at + 1).min(bytes.len());
				}
				_ => {
					let start = at;
					while at < bytes.len() && !space(at) && bytes[at] != b'>' {
						at += 1;
					}
					value = unescape(&tag[start..at]);
				}
			}
		}
		if !name.is_empty() {
			found.push((name, value));
		}
	}
}

/// The text of a piece of HTML: tags dropped, entities decoded, whitespace folded.
fn text(fragment: &str) -> String {
	let mut out = String::new();
	let mut rest = fragment;
	while let Some(at) = rest.find('<') {
		out.push_str(&rest[..at]);
		out.push(' ');
		rest = rest[at..].find('>').map_or("", |end| &rest[at + end + 1..]);
	}
	out.push_str(rest);
	unescape(&out)
		.split_whitespace()
		.collect::<Vec<_>>()
		.join(" ")
}

fn unescape(text: &str) -> String {
	let mut out = String::with_capacity(text.len());
	let mut rest = text;
	while let Some(at) = rest.find('&') {
		out.push_str(&rest[..at]);
		rest = &rest[at + 1..];
		let entity = rest
			.split_once(';')
			.map(|(entity, _)| entity)
			.filter(|entity| entity.len() <= 8);
		match entity.and_then(|entity| Some((entity, decode(entity)?))) {
			Some((entity, decoded)) => {
				out.push(decoded);
				rest = &rest[entity.len() + 1..];
			}
			None => out.push('&'),
		}
	}
	out.push_str(rest);
	out
}

fn decode(entity: &str) -> Option<char> {
	Some(match entity {
		"amp" => '&',
		"lt" => '<',
		"gt" => '>',
		"quot" => '"',
		"apos" => '\'',
		"nbsp" => ' ',
		_ => {
			let code = entity.strip_prefix('#')?;
			let number = match code.strip_prefix(['x', 'X']) {
				Some(hex) => u32::from_str_radix(hex, 16).ok()?,
				None => code.parse().ok()?,
			};
			char::from_u32(number)?
		}
	})
}

#[cfg(test)]
mod tests {
	use super::*;

	fn slip(id: &str, label: &str) -> Slip {
		Slip {
			id: id.into(),
			label: label.into(),
		}
	}

	#[test]
	fn dates_in_the_forms_the_labels_take() {
		for (label, expected) in [
			(
				"2026年(令和08)年10月23日支給分 給与明細",
				Some("2026-10-23"),
			),
			("2026年（令和０８）年１０月２３日支給分", Some("2026-10-23")),
			("2026/10/23", Some("2026-10-23")),
			("2026/1/5 令和07年分", Some("2026-01-05")),
			("2026-10-23 支給", Some("2026-10-23")),
			("2026年 10月 23日", Some("2026-10-23")),
			("令和8年10月23日支給分", Some("2026-10-23")),
			("令和 08年2月29日", None),
			("令和10年2月29日", Some("2028-02-29")),
			("賞与 2023年02月24日", Some("2023-02-24")),
			("No. 12 2024/06/28", Some("2024-06-28")),
			("2026年10月分", None),
			("2026/13/01", None),
			("2026/02/30", None),
			("8年10月23日", None),
			("20261023", None),
			("表示", None),
			("", None),
		] {
			assert_eq!(date(label).as_deref(), expected, "{label}");
		}
	}

	#[test]
	fn the_month_of_a_label_without_a_day() {
		for (label, expected) in [
			("2023年2月賞与", Some("2023-02")),
			("２０２３年１２月 賞与明細", Some("2023-12")),
			("2023年13月賞与", None),
			("23年2月", None),
			("賞与明細", None),
		] {
			assert_eq!(month(label).as_deref(), expected, "{label}");
		}
	}

	#[test]
	fn only_dates_written_in_full_are_iso() {
		assert!(is_iso_date("2024-02-29"));
		for text in [
			"2024-2-29",
			"2024/02/29",
			"2023-02-29",
			"2024-02-29 ",
			"soon",
		] {
			assert!(!is_iso_date(text), "{text}");
		}
	}

	#[test]
	fn the_era_year_of_a_label() {
		for (label, expected) in [
			("2026/01/05 令和07年分", Some(7)),
			("令和７年分 源泉徴収票", Some(7)),
			("令和元年分", Some(1)),
			("令和 3 年分", Some(3)),
			("2026年(令和08)年10月23日支給分", Some(8)),
			("平成30年分", None),
			("令和年分", None),
			("2026/01/05", None),
		] {
			assert_eq!(reiwa_year(label), expected, "{label}");
		}
	}

	#[test]
	fn links_whatever_the_order_of_attributes_the_quotes_and_the_entities() {
		let html = r#"
			<nav><A class="item" HREF='/a?x=1&amp;y=2' data-x="1>2">
				<span class="year">2026年</span><span>10月&nbsp;23日</span> <b>給与&#26126;&#x7d30;</b>
			</A>
			<a href=/plain>plain &amp; simple</a><abbr title="x">not a link</abbr>
			<a name="anchor">no address</a>
			<a
				data-turbo="false"
				href="/last"
			>R&D &unknown; 5 &lt; 6</a></nav>"#;
		assert_eq!(
			links(html),
			[
				(
					"/a?x=1&y=2".to_owned(),
					"2026年 10月 23日 給与明細".to_owned()
				),
				("/plain".to_owned(), "plain & simple".to_owned()),
				("/last".to_owned(), "R&D &unknown; 5 < 6".to_owned()),
			]
		);
		assert_eq!(
			links("<a href=\"/open\">never closed"),
			[("/open".to_owned(), "never closed".to_owned())]
		);
		assert_eq!(
			links("<a href=\"/cut"),
			[("/cut".to_owned(), String::new())]
		);
	}

	#[test]
	fn the_form_token_of_a_page() {
		let html = r#"<head><meta charset="utf-8"><meta name="csrf-param" content="authenticity_token" />
			<meta content="tok+en/==&amp;" name="csrf-token" /></head>"#;
		assert_eq!(csrf(html).as_deref(), Some("tok+en/==&"));
		assert_eq!(csrf("<meta name=\"csrf-token\" content=\"\">"), None);
		assert_eq!(csrf("<p>csrf-token</p>"), None);
	}

	#[test]
	fn the_accounts_of_the_selector() {
		let html = r#"<script>window.gon={};gon.locale="ja";gon.accounts=[{"identifier":"a1","email":"first@example.com","signInMethod":"email","formAction":"/oauth/authorize?client_id=x&select=a1","active":false,"available":true},{"identifier":"a2","formAction":"/oauth/authorize?select=a2","active":true,"available":true}];gon.other={"a":[1]};</script>"#;
		let accounts = accounts(html).unwrap();
		assert_eq!(accounts.len(), 2);
		assert_eq!(
			accounts[0].form_action,
			"/oauth/authorize?client_id=x&select=a1"
		);
		assert!(accounts[1].active && accounts[1].available);
		assert!(super::accounts("gon.accounts = [];").unwrap().is_empty());
		assert!(super::accounts("gon.accounts=null;").is_none());
		assert!(super::accounts("<p>nothing</p>").is_none());
	}

	#[test]
	fn payslips_are_the_period_links_once_each_with_their_best_label() {
		let html = r#"
			<ul class="sidebar">
				<li><a href="/my/reports/periods/QUJD-1_x/payslips_v2.pdf?disposition=inline">PDF</a></li>
				<li><a class="is-active" href="/my/reports/periods/QUJD-1_x/payslip"><span>2026年(令和08)年10月23日支給分</span> <span>給与明細</span></a></li>
				<li><a href="https://payroll.moneyforward.com/my/reports/periods/REVG/payslip#top">2026/09/25</a></li>
				<li><a href="/my/reports/periods/REVG/payslip">again</a></li>
				<li><a href="/my/reports/periods/R0hJ/payslip"></a></li>
				<li><a href="/my/reports/periods/bad id/payslip">2026/08/25</a></li>
				<li><a href="/my/reports/periods/a/b/payslip">2026/08/25</a></li>
				<li><a href="/my/reports/periods//payslip">2026/08/25</a></li>
				<li><a href="/my/reports/bonus_periods/Qk9O/bonus_slip">2026/06/30</a></li>
				<li><a href="/my/reports/payslips">一覧</a></li>
			</ul>"#;
		assert_eq!(
			payslips(html),
			[
				slip("QUJD-1_x", "2026年(令和08)年10月23日支給分 給与明細"),
				slip("REVG", "2026/09/25"),
				slip("R0hJ", ""),
			]
		);
		assert_eq!(bonus_slips(html), [slip("Qk9O", "2026/06/30")]);
		assert!(withholding_slips(html).is_empty());
	}

	#[test]
	fn withholding_slips_are_the_term_links() {
		let html = r#"<table><tr><td><a target="_blank" href="/my/reports/terms/VEVSTQ==/withholding_slip.pdf?disposition=inline">2026/01/05 令和07年分</a></td></tr>
			<tr><td><a href="/my/reports/terms/T0xE/withholding_slip.pdf?disposition=inline">2025/01/06 令和06年分</a></td></tr></table>"#;
		assert_eq!(
			withholding_slips(html),
			[
				slip("VEVSTQ==", "2026/01/05 令和07年分"),
				slip("T0xE", "2025/01/06 令和06年分"),
			]
		);
	}

	#[test]
	fn merging_keeps_the_order_and_takes_a_dated_label_over_another() {
		let mut known = vec![slip("a", "PDF"), slip("b", "2026/09/25"), slip("c", "")];
		merge(
			&mut known,
			vec![
				slip("a", "2026/10/23"),
				slip("b", "2020/01/01"),
				slip("c", "表示"),
				slip("d", ""),
			],
		);
		assert_eq!(
			known,
			[
				slip("a", "2026/10/23"),
				slip("b", "2026/09/25"),
				slip("c", "表示"),
				slip("d", ""),
			]
		);
	}
}
