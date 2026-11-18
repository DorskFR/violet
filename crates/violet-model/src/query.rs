//! Reads what a search box says besides words: years, months, amounts and "the latest".

use serde::Serialize;
use violet_core::Query;

use crate::amount::parse_amount;
use crate::date::{MONTHS, days_in_month};
use crate::text::{fold_accents, fold_width};

const MIN_YEAR: u32 = 1900;
const MAX_YEAR: u32 = 2100;
const SHORT_MONTHS: [(&str, u32); 11] = [
	("jan", 1),
	("feb", 2),
	("mar", 3),
	("apr", 4),
	("jun", 6),
	("jul", 7),
	("aug", 8),
	("sep", 9),
	("oct", 10),
	("nov", 11),
	("dec", 12),
];
const LATEST: [&str; 8] = [
	"dernier",
	"derniere",
	"derniers",
	"dernieres",
	"latest",
	"last",
	"newest",
	"最新",
];

/// The filters read from `q`, as applied to the search.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Understood {
	#[serde(skip_serializing_if = "Option::is_none")]
	pub from: Option<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub to: Option<String>,
	/// 1 to 12, in any year.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub month: Option<u32>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub min: Option<i64>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub max: Option<i64>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub currency: Option<String>,
	/// Newest first.
	#[serde(skip_serializing_if = "std::ops::Not::not")]
	pub latest: bool,
	/// The parts of `q` read as these filters, as typed, in order.
	pub terms: Vec<UnderstoodTerm>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UnderstoodTerm {
	pub text: String,
	pub kind: TermKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TermKind {
	Date,
	Amount,
	Latest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DatePart {
	Year(u32),
	Month(u32),
	Both(u32, u32),
}

/// A word or a quoted phrase of the box.
#[derive(Debug)]
struct Token {
	text: String,
	quoted: bool,
	kind: Option<TermKind>,
	/// Bytes of `text` the filter took; the rest stays a word.
	taken: usize,
}

impl Token {
	fn free_word(&self) -> Option<&str> {
		(!self.quoted && self.kind.is_none()).then_some(self.text.as_str())
	}
	fn take(&mut self, kind: TermKind, taken: usize) {
		self.kind = Some(kind);
		self.taken = taken;
	}
}

/// Moves the years, months (`2023`, `mars`, `March`, `3月`, `2024年3月`), amounts (`133,90 €`,
/// `42 EUR`, `¥3,000`, `12.50`) and words for the latest (`dernier`, `latest`, `最新`) out of
/// `query.text` into its filters; quoted phrases stay words. A filter already set in `query` wins
/// and the words that would have set it are dropped. A year is the whole year, a month with a year
/// that month, a month alone that month in any year, several years the range they span; several
/// different months stay words. Several amounts in one currency are a range, else the first
/// counts. An amount without a currency (`133,90`) needs two decimals and is in hundredths.
pub fn understand(query: &mut Query) -> Understood {
	let Some(text) = query.text.take() else {
		return Understood::default();
	};
	let mut tokens = split(&text);
	let mut understood = Understood::default();
	let mut terms: Vec<(usize, UnderstoodTerm)> = Vec::new();

	for token in &mut tokens {
		if let Some(word) = token.free_word()
			&& LATEST.contains(&fold_accents(&fold_width(word)).as_str())
		{
			let taken = word.len();
			token.take(TermKind::Latest, taken);
			understood.latest = true;
		}
	}
	if understood.latest {
		query.newest = true;
	}

	let amounts = amounts(&mut tokens);
	if let Some((_, _, currency)) = amounts.first() {
		let min = amounts.iter().map(|(_, minor, _)| *minor).min();
		let max = amounts.iter().map(|(_, minor, _)| *minor).max();
		if query.amount_min.is_none() && query.amount_max.is_none() {
			(query.amount_min, query.amount_max) = (min, max);
			(understood.min, understood.max) = (min, max);
			if query.currency.is_none() {
				query.currency.clone_from(currency);
				understood.currency.clone_from(currency);
			}
			for (span, _, _) in &amounts {
				let text: Vec<&str> = span.iter().map(|&at| tokens[at].text.as_str()).collect();
				terms.push((span[0], term(text.join(" "), TermKind::Amount)));
			}
		}
	}

	let (from, to, month, dated) = dates(&mut tokens);
	if !dated.is_empty()
		&& query.date_from.is_none()
		&& query.date_to.is_none()
		&& query.month.is_none()
	{
		(query.date_from, query.date_to, query.month) = (from.clone(), to.clone(), month);
		(understood.from, understood.to, understood.month) = (from, to, month);
		for at in dated {
			let token = &tokens[at];
			terms.push((
				at,
				term(token.text[..token.taken].to_string(), TermKind::Date),
			));
		}
	}
	for (at, token) in tokens.iter().enumerate() {
		if token.kind == Some(TermKind::Latest) {
			terms.push((at, term(token.text.clone(), TermKind::Latest)));
		}
	}
	terms.sort_by_key(|(at, _)| *at);
	understood.terms = terms.into_iter().map(|(_, term)| term).collect();

	let rest: Vec<String> = tokens
		.iter()
		.filter_map(|token| match token.kind {
			_ if token.quoted => Some(format!("\"{}\"", token.text)),
			None => Some(token.text.clone()),
			Some(_) => {
				let rest = &token.text[token.taken..];
				(!rest.is_empty()).then(|| rest.to_string())
			}
		})
		.collect();
	let rest = rest.join(" ");
	query.text = (!rest.is_empty()).then_some(rest);
	understood
}

fn term(text: String, kind: TermKind) -> UnderstoodTerm {
	UnderstoodTerm { text, kind }
}

/// Marks the amounts among the words: the token indices of each, its minor units and currency.
/// When they are not all in one currency only the first is kept.
fn amounts(tokens: &mut [Token]) -> Vec<(Vec<usize>, i64, Option<String>)> {
	let no_digits = |word: &str| !word.chars().any(|c| c.is_ascii_digit());
	let mut found: Vec<(Vec<usize>, i64, Option<String>)> = Vec::new();
	for at in 0..tokens.len() {
		let Some(word) = tokens[at].free_word() else {
			continue;
		};
		if no_digits(word) {
			continue;
		}
		let mut last = at;
		while tokens
			.get(last + 1)
			.and_then(Token::free_word)
			.is_some_and(digit_group)
		{
			last += 1;
		}
		let number: Vec<&str> = tokens[at..=last]
			.iter()
			.map(|token| token.text.as_str())
			.collect();
		let number = number.join(" ");
		let span: Vec<usize> = (at..=last).collect();
		let next = tokens
			.get(last + 1)
			.and_then(Token::free_word)
			.filter(|next| no_digits(next));
		let previous = at
			.checked_sub(1)
			.and_then(|before| tokens[before].free_word())
			.filter(|previous| no_digits(previous));
		let amount = if let Some(amount) = parse_amount(&number, None) {
			Some((span, amount.minor, Some(amount.currency)))
		} else if let Some(amount) =
			next.and_then(|next| parse_amount(&format!("{number} {next}"), None))
		{
			Some((
				[span, vec![last + 1]].concat(),
				amount.minor,
				Some(amount.currency),
			))
		} else if let Some(amount) =
			previous.and_then(|previous| parse_amount(&format!("{previous} {number}"), None))
		{
			Some((
				[vec![at - 1], span].concat(),
				amount.minor,
				Some(amount.currency),
			))
		} else if bare_decimal(&number) {
			parse_amount(&number, Some("EUR")).map(|amount| (span, amount.minor, None))
		} else {
			None
		};
		let Some((span, minor, currency)) = amount else {
			continue;
		};
		if found
			.first()
			.is_some_and(|(_, _, first)| *first != currency)
		{
			continue;
		}
		for &part in &span {
			let taken = tokens[part].text.len();
			tokens[part].take(TermKind::Amount, taken);
		}
		found.push((span, minor.abs(), currency));
	}
	found
}

/// Marks the date words and reads them as `from`, `to` and an any-year month, with the indices
/// of the tokens taken.
fn dates(tokens: &mut [Token]) -> (Option<String>, Option<String>, Option<u32>, Vec<usize>) {
	let mut parts: Vec<(usize, DatePart, usize)> = Vec::new();
	for (at, token) in tokens.iter().enumerate() {
		if let Some(word) = token.free_word()
			&& let Some((part, taken)) = date_part(word)
		{
			parts.push((at, part, taken));
		}
	}
	let mut years: Vec<u32> = Vec::new();
	let mut months: Vec<u32> = Vec::new();
	for (_, part, _) in &parts {
		let (year, month) = match *part {
			DatePart::Year(year) => (Some(year), None),
			DatePart::Month(month) => (None, Some(month)),
			DatePart::Both(year, month) => (Some(year), Some(month)),
		};
		years.extend(year.filter(|year| !years.contains(year)));
		months.extend(month.filter(|month| !months.contains(month)));
	}
	let month = (months.len() == 1).then(|| months[0]);
	let mut taken = Vec::new();
	for (at, part, bytes) in parts {
		if month.is_some() || matches!(part, DatePart::Year(_)) {
			tokens[at].take(TermKind::Date, bytes);
			taken.push(at);
		}
	}
	let (first, last) = (years.iter().min().copied(), years.iter().max().copied());
	let (from, to, month) = match (first, last, month) {
		(Some(year), Some(last), Some(month)) if year == last => (
			Some(format!("{year:04}-{month:02}-01")),
			Some(format!(
				"{year:04}-{month:02}-{:02}",
				days_in_month(year as i32, month)
			)),
			None,
		),
		(Some(first), Some(last), month) => (
			Some(format!("{first:04}-01-01")),
			Some(format!("{last:04}-12-31")),
			month,
		),
		(_, _, month) => (None, None, month),
	};
	(from, to, month, taken)
}

/// Quoted phrases and whitespace-separated words, as the index reads them.
fn split(text: &str) -> Vec<Token> {
	let token = |text: &str, quoted: bool| Token {
		text: text.to_string(),
		quoted,
		kind: None,
		taken: 0,
	};
	let mut tokens = Vec::new();
	for (index, part) in text.split('"').enumerate() {
		if index % 2 == 1 {
			if !part.trim().is_empty() {
				tokens.push(token(part, true));
			}
		} else {
			tokens.extend(part.split_whitespace().map(|word| token(word, false)));
		}
	}
	tokens
}

/// `234` or `234,56`, as printed after the thousands in `1 234,56`.
fn digit_group(word: &str) -> bool {
	let word = fold_width(word);
	let digits = |text: &str| !text.is_empty() && text.chars().all(|c| c.is_ascii_digit());
	match word.split_once([',', '.']) {
		Some((group, decimals)) => group.len() == 3 && digits(group) && digits(decimals),
		None => word.len() == 3 && digits(&word),
	}
}

/// `133,90`, `1 234.50`: digits, then a comma or a dot and exactly two digits.
fn bare_decimal(word: &str) -> bool {
	let word = fold_width(word).replace(' ', "");
	let Some((whole, cents)) = word.rsplit_once([',', '.']) else {
		return false;
	};
	cents.len() == 2
		&& cents.chars().all(|c| c.is_ascii_digit())
		&& !whole.is_empty()
		&& whole
			.chars()
			.all(|c| c.is_ascii_digit() || c == '.' || c == ',')
		&& whole.starts_with(|c: char| c.is_ascii_digit())
}

/// A year, a month name or a Japanese `2024年3月` / `3月分` at the start of a word, with how many
/// bytes of the word it takes.
fn date_part(word: &str) -> Option<(DatePart, usize)> {
	let folded = fold_width(word);
	if folded.len() == 4 && folded.chars().all(|c| c.is_ascii_digit()) {
		let year: u32 = folded.parse().ok()?;
		return (MIN_YEAR..=MAX_YEAR)
			.contains(&year)
			.then_some((DatePart::Year(year), word.len()));
	}
	let name = fold_accents(&folded);
	if let Some(month) = MONTHS
		.iter()
		.chain(SHORT_MONTHS.iter())
		.find(|(known, _)| fold_accents(known) == name)
		.map(|(_, month)| *month)
	{
		return Some((DatePart::Month(month), word.len()));
	}
	japanese_date(word)
}

fn japanese_date(word: &str) -> Option<(DatePart, usize)> {
	let (year, after_year) = match number_before(word, '年', 4) {
		Some((year, rest)) if (MIN_YEAR..=MAX_YEAR).contains(&year) => (Some(year), rest),
		_ => (None, word),
	};
	let (month, rest) = match number_before(after_year, '月', 2) {
		Some((month, rest)) if (1..=12).contains(&month) => {
			(Some(month), rest.strip_prefix('分').unwrap_or(rest))
		}
		_ => (None, after_year),
	};
	let part = match (year, month) {
		(Some(year), Some(month)) => DatePart::Both(year, month),
		(Some(year), None) => DatePart::Year(year),
		(None, Some(month)) => DatePart::Month(month),
		(None, None) => return None,
	};
	Some((part, word.len() - rest.len()))
}

/// The number of at most `digits` digits that starts `text` and ends at `unit`, and what follows.
fn number_before(text: &str, unit: char, digits: usize) -> Option<(u32, &str)> {
	let end = text.find(unit)?;
	let value = fold_width(&text[..end]);
	if value.is_empty() || value.len() > digits || !value.chars().all(|c| c.is_ascii_digit()) {
		return None;
	}
	Some((value.parse().ok()?, &text[end + unit.len_utf8()..]))
}

#[cfg(test)]
mod tests {
	use super::*;

	fn read(text: &str) -> (Query, Understood) {
		let mut query = Query {
			text: Some(text.into()),
			..Query::default()
		};
		let understood = understand(&mut query);
		(query, understood)
	}

	fn rest(text: &str) -> Option<String> {
		read(text).0.text
	}

	fn dates(text: &str) -> (Option<String>, Option<String>, Option<u32>) {
		let (query, _) = read(text);
		(query.date_from, query.date_to, query.month)
	}

	fn range(from: &str, to: &str) -> (Option<String>, Option<String>, Option<u32>) {
		(Some(from.into()), Some(to.into()), None)
	}

	fn amount(text: &str) -> (Option<i64>, Option<i64>, Option<String>) {
		let (query, _) = read(text);
		(query.amount_min, query.amount_max, query.currency)
	}

	fn terms(text: &str) -> Vec<(String, TermKind)> {
		read(text)
			.1
			.terms
			.into_iter()
			.map(|term| (term.text, term.kind))
			.collect()
	}

	#[test]
	fn the_audit_queries() {
		assert_eq!(rest("facture EDF 2023").as_deref(), Some("facture EDF"));
		assert_eq!(dates("facture EDF 2023"), range("2023-01-01", "2023-12-31"));
		assert_eq!(terms("facture EDF 2023"), [("2023".into(), TermKind::Date)]);

		assert_eq!(rest("impots 2024").as_deref(), Some("impots"));
		assert_eq!(dates("impots 2024"), range("2024-01-01", "2024-12-31"));

		for unchanged in ["電気 請求書", "assurance voiture"] {
			let (query, understood) = read(unchanged);
			assert_eq!(query.text.as_deref(), Some(unchanged));
			assert_eq!(understood, Understood::default());
			assert!(!query.newest);
		}

		let (query, understood) = read("dernier bulletin de paie");
		assert_eq!(query.text.as_deref(), Some("bulletin de paie"));
		assert!(query.newest && understood.latest);
		assert_eq!(understood.terms, [term("dernier".into(), TermKind::Latest)]);

		let (query, understood) = read("133,90 €");
		assert_eq!(query.text, None);
		assert_eq!(
			amount("133,90 €"),
			(Some(13_390), Some(13_390), Some("EUR".into()))
		);
		assert_eq!(
			(
				understood.min,
				understood.max,
				understood.currency.as_deref()
			),
			(Some(13_390), Some(13_390), Some("EUR"))
		);
		assert_eq!(
			understood.terms,
			[term("133,90 €".into(), TermKind::Amount)]
		);
	}

	#[test]
	fn years_and_months() {
		assert_eq!(
			dates("payslip march 2014"),
			range("2014-03-01", "2014-03-31")
		);
		assert_eq!(
			dates("Février 2024 loyer"),
			range("2024-02-01", "2024-02-29")
		);
		assert_eq!(dates("fiche de paie mars"), (None, None, Some(3)));
		assert_eq!(rest("fiche de paie mars").as_deref(), Some("fiche de paie"));
		assert_eq!(dates("aout"), (None, None, Some(8)));
		assert_eq!(dates("Dec invoice"), (None, None, Some(12)));
		assert_eq!(dates("3月"), (None, None, Some(3)));
		assert_eq!(dates("１２月分"), (None, None, Some(12)));
		assert_eq!(dates("2022 2023"), range("2022-01-01", "2023-12-31"));
		assert_eq!(
			dates("mars 2022 2023"),
			(
				Some("2022-01-01".into()),
				Some("2023-12-31".into()),
				Some(3)
			)
		);
		assert_eq!(dates("２０２３"), range("2023-01-01", "2023-12-31"));

		assert_eq!(dates("2024年3月 電気"), range("2024-03-01", "2024-03-31"));
		assert_eq!(rest("2024年3月 電気").as_deref(), Some("電気"));
		assert_eq!(
			dates("2024年3月分電気料金"),
			range("2024-03-01", "2024-03-31")
		);
		assert_eq!(rest("2024年3月分電気料金").as_deref(), Some("電気料金"));
		assert_eq!(
			terms("2024年3月分電気料金"),
			[("2024年3月分".into(), TermKind::Date)]
		);
		assert_eq!(dates("2024年"), range("2024-01-01", "2024-12-31"));

		assert_eq!(dates("mars avril"), (None, None, None));
		assert_eq!(rest("mars avril 2024").as_deref(), Some("mars avril"));
		for literal in [
			"1234",
			"12345",
			"2101",
			"13月",
			"\"2023\" facture",
			"marsupial",
		] {
			assert_eq!(dates(literal), (None, None, None), "{literal}");
			assert_eq!(rest(literal).as_deref(), Some(literal), "{literal}");
		}
	}

	#[test]
	fn amounts() {
		let eur = |minor| (Some(minor), Some(minor), Some("EUR".to_string()));
		assert_eq!(amount("42 EUR"), eur(4_200));
		assert_eq!(amount("EUR 42"), eur(4_200));
		assert_eq!(amount("42€"), eur(4_200));
		assert_eq!(amount("€ 12.50"), eur(1_250));
		assert_eq!(amount("1 234,56 €"), eur(123_456));
		assert_eq!(amount("1 234,56"), (Some(123_456), Some(123_456), None));
		assert_eq!(
			amount("12 345 678 JPY"),
			(Some(12_345_678), Some(12_345_678), Some("JPY".into()))
		);
		assert_eq!(amount("1234,56 €"), eur(123_456));
		assert_eq!(
			amount("¥3,000"),
			(Some(3_000), Some(3_000), Some("JPY".into()))
		);
		assert_eq!(
			amount("3000円 電気"),
			(Some(3_000), Some(3_000), Some("JPY".into()))
		);
		assert_eq!(rest("3000円 電気").as_deref(), Some("電気"));
		assert_eq!(amount("133,90"), (Some(13_390), Some(13_390), None));
		assert_eq!(amount("133.90 facture"), (Some(13_390), Some(13_390), None));

		assert_eq!(
			amount("100 € 200 €"),
			(Some(10_000), Some(20_000), Some("EUR".into()))
		);
		assert_eq!(amount("100 € 5 USD"), eur(10_000));
		assert_eq!(rest("100 € 5 USD").as_deref(), Some("5 USD"));

		for literal in ["facture 42", "n° 123456", "EUR", "12.5", "2023 facture"] {
			assert_eq!(amount(literal), (None, None, None), "{literal}");
		}
		assert_eq!(rest("facture 42").as_deref(), Some("facture 42"));
	}

	#[test]
	fn explicit_filters_win_and_their_words_are_dropped() {
		let mut query = Query {
			text: Some("facture 2023 133,90 € dernier".into()),
			date_from: Some("2020-01-01".into()),
			amount_min: Some(1),
			..Query::default()
		};
		let understood = understand(&mut query);
		assert_eq!(query.text.as_deref(), Some("facture"));
		assert_eq!(query.date_from.as_deref(), Some("2020-01-01"));
		assert_eq!((query.date_to, query.month), (None, None));
		assert_eq!((query.amount_min, query.amount_max), (Some(1), None));
		assert_eq!(query.currency, None);
		assert_eq!(understood.from, None);
		assert_eq!(understood.min, None);
		assert_eq!(understood.terms, [term("dernier".into(), TermKind::Latest)]);

		let mut query = Query {
			text: Some("42 €".into()),
			currency: Some("JPY".into()),
			..Query::default()
		};
		let understood = understand(&mut query);
		assert_eq!(query.currency.as_deref(), Some("JPY"));
		assert_eq!((understood.min, understood.currency), (Some(4_200), None));
	}

	#[test]
	fn understood_serialises_only_what_was_read() {
		let (_, understood) = read("facture 2023");
		assert_eq!(
			serde_json::to_value(&understood).unwrap(),
			serde_json::json!({
				"from": "2023-01-01",
				"to": "2023-12-31",
				"terms": [{"text": "2023", "kind": "date"}],
			})
		);
		let (query, understood) = read("");
		assert_eq!(query.text, None);
		assert_eq!(
			serde_json::to_value(&understood).unwrap(),
			serde_json::json!({"terms": []})
		);
	}
}
