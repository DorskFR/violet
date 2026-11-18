use crate::text::{fold_width, kanji_digit};

/// A money amount in minor units of `currency`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Amount {
	pub minor: i64,
	/// ISO 4217.
	pub currency: String,
}

const SCALE: i128 = 1_000_000;
const ZERO_DECIMALS: [&str; 10] = [
	"JPY", "KRW", "VND", "CLP", "ISK", "HUF", "TWD", "XAF", "XOF", "XPF",
];
const THREE_DECIMALS: [&str; 5] = ["BHD", "KWD", "OMR", "JOD", "TND"];
/// Codes read as a currency when printed next to an amount or in the text of a document.
const CODES: [&str; 32] = [
	"JPY", "USD", "EUR", "GBP", "CHF", "CAD", "AUD", "NZD", "SGD", "HKD", "CNY", "KRW", "TWD",
	"THB", "VND", "SEK", "NOK", "DKK", "INR", "PHP", "XPF", "XOF", "XAF", "MYR", "IDR", "BRL",
	"MXN", "ZAR", "PLN", "CZK", "HUF", "ISK",
];
/// Active ISO 4217 currency codes.
const ISO_4217: &[&str] = &[
	"AED", "AFN", "ALL", "AMD", "AOA", "ARS", "AUD", "AWG", "AZN", "BAM", "BBD", "BDT", "BGN",
	"BHD", "BIF", "BMD", "BND", "BOB", "BRL", "BSD", "BTN", "BWP", "BYN", "BZD", "CAD", "CDF",
	"CHF", "CLP", "CNY", "COP", "CRC", "CUP", "CVE", "CZK", "DJF", "DKK", "DOP", "DZD", "EGP",
	"ERN", "ETB", "EUR", "FJD", "FKP", "GBP", "GEL", "GHS", "GIP", "GMD", "GNF", "GTQ", "GYD",
	"HKD", "HNL", "HTG", "HUF", "IDR", "ILS", "INR", "IQD", "IRR", "ISK", "JMD", "JOD", "JPY",
	"KES", "KGS", "KHR", "KMF", "KPW", "KRW", "KWD", "KYD", "KZT", "LAK", "LBP", "LKR", "LRD",
	"LSL", "LYD", "MAD", "MDL", "MGA", "MKD", "MMK", "MNT", "MOP", "MRU", "MUR", "MVR", "MWK",
	"MXN", "MYR", "MZN", "NAD", "NGN", "NIO", "NOK", "NPR", "NZD", "OMR", "PAB", "PEN", "PGK",
	"PHP", "PKR", "PLN", "PYG", "QAR", "RON", "RSD", "RUB", "RWF", "SAR", "SBD", "SCR", "SDG",
	"SEK", "SGD", "SHP", "SLE", "SOS", "SRD", "SSP", "STN", "SYP", "SZL", "THB", "TJS", "TMT",
	"TND", "TOP", "TRY", "TTD", "TWD", "TZS", "UAH", "UGX", "USD", "UYU", "UZS", "VES", "VND",
	"VUV", "WST", "XAF", "XCD", "XOF", "XPF", "YER", "ZAR", "ZMW",
];
/// Other names of a currency, lowercase, with the code they stand for.
const ALIASES: [(&str, &str); 22] = [
	("円", "JPY"),
	("¥", "JPY"),
	("yen", "JPY"),
	("日本円", "JPY"),
	("$", "USD"),
	("us$", "USD"),
	("€", "EUR"),
	("euro", "EUR"),
	("euros", "EUR"),
	("£", "GBP"),
	("cfp", "XPF"),
	("f cfp", "XPF"),
	("fcfp", "XPF"),
	("f.cfp", "XPF"),
	("francs cfp", "XPF"),
	("franc cfp", "XPF"),
	("xpf", "XPF"),
	("rmb", "CNY"),
	("元", "CNY"),
	("₩", "KRW"),
	("sfr", "CHF"),
	("₹", "INR"),
];
const DOLLARS: [&str; 6] = ["USD", "CAD", "AUD", "NZD", "SGD", "HKD"];
const COUNTERS: &str = "年月日時分秒%個枚点回件名人歳台本冊号番階泊度";
const NEGATIVE: [char; 5] = ['-', '\u{2212}', '\u{2013}', '▲', '△'];

pub(crate) fn decimals(currency: &str) -> u32 {
	if ZERO_DECIMALS.contains(&currency) {
		0
	} else if THREE_DECIMALS.contains(&currency) {
		3
	} else {
		2
	}
}

/// An ISO 4217 code, or a name or sign of one (`円`, `¥`, `yen`, `€`, `CFP`), as the upper-case
/// code. `None` for anything else, units such as `KWH` included.
pub(crate) fn normalize_currency(raw: &str) -> Option<String> {
	let folded = fold_width(raw);
	let text = folded.trim().to_lowercase();
	if let Some((_, code)) = ALIASES.iter().find(|(alias, _)| *alias == text) {
		return Some((*code).into());
	}
	let code = text.to_ascii_uppercase();
	ISO_4217.contains(&code.as_str()).then_some(code)
}

/// The currencies a document's text names, by code, alias or sign. A `$` alone counts as USD
/// only when no other dollar is named.
pub(crate) fn currencies_in(text: &str) -> Vec<String> {
	let folded = fold_width(text);
	let lower = folded.to_lowercase();
	let words: Vec<&str> = folded
		.split(|c: char| !c.is_ascii_alphabetic())
		.filter(|word| !word.is_empty())
		.collect();
	let mut found: Vec<String> = Vec::new();
	let add = |found: &mut Vec<String>, code: &str| {
		if !found.iter().any(|known| known == code) {
			found.push(code.into());
		}
	};
	for word in &words {
		if word.len() == 3 && word.chars().all(|c| c.is_ascii_uppercase()) && CODES.contains(word) {
			add(&mut found, word);
		}
		match word.to_ascii_lowercase().as_str() {
			"cfp" | "fcfp" => add(&mut found, "XPF"),
			"yen" => add(&mut found, "JPY"),
			"euro" | "euros" => add(&mut found, "EUR"),
			"dollar" | "dollars" => add(&mut found, "USD"),
			_ => {}
		}
	}
	if lower.contains('円') || lower.contains('¥') {
		add(&mut found, "JPY");
	}
	if lower.contains('€') {
		add(&mut found, "EUR");
	}
	if lower.contains('£') {
		add(&mut found, "GBP");
	}
	if lower.contains('$') && !found.iter().any(|code| DOLLARS.contains(&code.as_str())) {
		add(&mut found, "USD");
	}
	found
}

/// Whether a currency is consistent with the currencies a document names: one of them, or a
/// dollar where a `$` is printed.
pub(crate) fn currency_fits(currency: &str, named: &[String], text: &str) -> bool {
	named.iter().any(|code| code == currency)
		|| (DOLLARS.contains(&currency) && text.contains('$'))
		|| (currency == "CNY" && text.contains('¥'))
}

/// Reads a printed money amount: `¥12,340`, `12,340円`, `￥１２，３４０`, `1万2千円`,
/// `一万二千三百四十円`, `▲1,200円`, `$1,234.56`, `1.234,56 €`, `1 234,56 EUR`.
/// `currency_hint` decides when the text carries no currency or an ambiguous symbol.
/// `None` when the text is not exactly one unambiguous amount.
pub fn parse_amount(value: &str, currency_hint: Option<&str>) -> Option<Amount> {
	let folded = fold_width(value);
	let chars: Vec<char> = folded.trim().chars().collect();
	let (start, end) = single_run(&chars)?;
	let before: String = chars[..start].iter().collect();
	let after: String = chars[end..].iter().collect();
	if after.trim_start().starts_with(|c| COUNTERS.contains(c)) {
		return None;
	}
	let hint = currency_hint.and_then(normalize_currency);
	let currency = match detect_currency(&before, &after, hint.as_deref()) {
		Some(currency) => currency,
		None => {
			let decoration =
				|c: char| c.is_whitespace() || NEGATIVE.contains(&c) || "().".contains(c);
			if !before.chars().chain(after.chars()).all(decoration) {
				return None;
			}
			hint?
		}
	};
	let magnitude = run_value(&chars[start..end], decimals(&currency) == 0)?;
	let negative = before.contains(NEGATIVE)
		|| (before.trim_start().starts_with('(') && after.trim_end().ends_with(')'));
	let scaled = magnitude * 10i128.pow(decimals(&currency));
	if scaled % SCALE != 0 {
		return None;
	}
	let minor = i64::try_from(scaled / SCALE).ok()?;
	Some(Amount {
		minor: if negative { -minor } else { minor },
		currency,
	})
}

fn is_unit(c: char) -> bool {
	small_unit(c).is_some() || big_unit(c).is_some()
}

fn small_unit(c: char) -> Option<i128> {
	match c {
		'十' => Some(10),
		'百' => Some(100),
		'千' => Some(1000),
		_ => None,
	}
}

fn big_unit(c: char) -> Option<i128> {
	match c {
		'万' => Some(10_000),
		'億' => Some(100_000_000),
		'兆' => Some(1_000_000_000_000),
		_ => None,
	}
}

fn is_numeral(c: char) -> bool {
	c.is_ascii_digit() || kanji_digit(c).is_some() || is_unit(c)
}

fn is_space(c: char) -> bool {
	matches!(c, ' ' | '\u{a0}' | '\u{202f}' | '\u{2009}')
}

/// Whether `chars[at]` is a separator inside one number, e.g. the comma of `12,340`.
fn joins_digits(chars: &[char], at: usize) -> bool {
	let digit = |i: usize| chars.get(i).is_some_and(char::is_ascii_digit);
	if at == 0 || !digit(at - 1) || !digit(at + 1) {
		return false;
	}
	match chars[at] {
		',' | '.' => true,
		c if is_space(c) => digit(at + 2) && digit(at + 3) && !digit(at + 4),
		_ => false,
	}
}

/// The bounds of the only number in `chars`; `None` when there are none or several.
fn single_run(chars: &[char]) -> Option<(usize, usize)> {
	let mut found = None;
	let mut i = 0;
	while i < chars.len() {
		if !is_numeral(chars[i]) {
			i += 1;
			continue;
		}
		let start = i;
		while i < chars.len() && (is_numeral(chars[i]) || joins_digits(chars, i)) {
			i += 1;
		}
		if found.replace((start, i)).is_some() {
			return None;
		}
	}
	found
}

/// The value of a number run, scaled by [`SCALE`].
fn run_value(run: &[char], whole_units: bool) -> Option<i128> {
	let mut total = 0i128;
	let mut section = 0i128;
	let mut current: Option<i128> = None;
	let mut i = 0;
	while i < run.len() {
		let c = run[i];
		if let Some(unit) = small_unit(c) {
			section += current.take().unwrap_or(SCALE) * unit;
			i += 1;
		} else if let Some(unit) = big_unit(c) {
			let group = section + current.take().unwrap_or(0);
			if group == 0 {
				return None;
			}
			total = total.checked_add(group.checked_mul(unit)?)?;
			section = 0;
			i += 1;
		} else {
			if current.is_some() {
				return None;
			}
			let kanji = kanji_digit(c).is_some();
			let start = i;
			while i < run.len() && !is_unit(run[i]) && kanji_digit(run[i]).is_some() == kanji {
				i += 1;
			}
			let token = &run[start..i];
			current = Some(if kanji {
				token.iter().try_fold(0i128, |acc, &d| {
					acc.checked_mul(10)?
						.checked_add(i128::from(kanji_digit(d)?))
				})? * SCALE
			} else {
				arabic(token, whole_units)?
			});
		}
	}
	Some(total + section + current.unwrap_or(0))
}

/// Western digits with thousands and decimal separators in either convention. In a currency
/// without minor units, `206.000` is two hundred and six thousand.
fn arabic(token: &[char], whole_units: bool) -> Option<i128> {
	let text: String = token.iter().filter(|c| !is_space(**c)).collect();
	let commas = text.matches(',').count();
	let dots = text.matches('.').count();
	let decimal_mark = match (commas, dots) {
		(0, 0) => None,
		(_, 0) => (!grouped(&text, ',')).then_some(','),
		(0, 1) => (!(whole_units && grouped(&text, '.'))).then_some('.'),
		(0, _) => (!grouped(&text, '.')).then_some('.'),
		_ => Some(if text.rfind(',') > text.rfind('.') {
			','
		} else {
			'.'
		}),
	};
	let (whole, fraction) = match decimal_mark {
		Some(mark) => {
			let (whole, fraction) = text.rsplit_once(mark)?;
			let other = if mark == ',' { '.' } else { ',' };
			if fraction.contains(other) || whole.contains(mark) {
				return None;
			}
			if whole.contains(other) && !grouped(whole, other) {
				return None;
			}
			(whole.replace(other, ""), fraction)
		}
		None => (text.replace([',', '.'], ""), ""),
	};
	if whole.len() > 18 || fraction.len() > 6 {
		return None;
	}
	let whole: i128 = whole.parse().ok()?;
	let fraction: i128 = format!("{fraction:0<6}").parse().ok()?;
	Some(whole * SCALE + fraction)
}

/// `1,234,567`: one to three digits, then groups of exactly three.
fn grouped(text: &str, separator: char) -> bool {
	let mut groups = text.split(separator);
	let first = groups.next().unwrap_or("");
	(1..=3).contains(&first.len()) && groups.all(|group| group.len() == 3)
}

fn detect_currency(before: &str, after: &str, hint: Option<&str>) -> Option<String> {
	let around = format!("{before} {after}");
	let lower = around.to_ascii_lowercase();
	if around.contains('円') || lower.contains("yen") {
		return Some("JPY".into());
	}
	let code = around
		.split(|c: char| !c.is_ascii_alphabetic())
		.find_map(|word| match word.to_ascii_uppercase().as_str() {
			"CFP" | "FCFP" => Some("XPF"),
			upper => CODES.iter().find(|code| **code == upper).copied(),
		});
	if let Some(code) = code {
		return Some(code.into());
	}
	let named = |hint: &&str| {
		around
			.split(|c: char| !c.is_ascii_alphabetic())
			.any(|word| word.eq_ignore_ascii_case(hint))
	};
	if let Some(hint) = hint.filter(named) {
		return Some(hint.into());
	}
	if around.contains('€') || lower.contains("euro") {
		return Some("EUR".into());
	}
	if around.contains('£') {
		return Some("GBP".into());
	}
	if around.contains('¥') {
		return Some(if hint == Some("CNY") { "CNY" } else { "JPY" }.into());
	}
	if around.contains('$') {
		return Some(
			hint.filter(|code| DOLLARS.contains(code))
				.unwrap_or("USD")
				.into(),
		);
	}
	None
}

#[cfg(test)]
mod tests {
	use super::*;

	fn amount(value: &str, hint: Option<&str>) -> Option<(i64, String)> {
		parse_amount(value, hint).map(|amount| (amount.minor, amount.currency))
	}

	fn jpy(value: &str) -> Option<i64> {
		let (minor, currency) = amount(value, None)?;
		assert_eq!(currency, "JPY", "{value}");
		Some(minor)
	}

	#[test]
	fn japanese_formats() {
		let cases = [
			("¥12,340", 12_340),
			("12,340円", 12_340),
			("12340円", 12_340),
			("￥１２，３４０", 12_340),
			("１２，３４０円", 12_340),
			("12,340 円", 12_340),
			("¥ 12,340", 12_340),
			("¥12,340-", 12_340),
			("¥12,340.-", 12_340),
			("金12,340円也", 12_340),
			("12,340円（税込）", 12_340),
			("合計 12,340円", 12_340),
			("12,340.00円", 12_340),
			("JPY 12,340", 12_340),
			("12,340 yen", 12_340),
			("1,234,567円", 1_234_567),
			("0円", 0),
			("1万2千円", 12_000),
			("1万2,340円", 12_340),
			("１万２千円", 12_000),
			("12万円", 120_000),
			("1.5万円", 15_000),
			("3億2千万円", 320_000_000),
			("1兆円", 1_000_000_000_000),
			("千円", 1_000),
			("十万円", 100_000),
			("一万二千三百四十円", 12_340),
			("三百円", 300),
			("二〇〇円", 200),
			("▲1,200円", -1_200),
			("△1,200円", -1_200),
			("-1,200円", -1_200),
			("−¥1,200", -1_200),
			("¥-1,200", -1_200),
			("(1,200円)", -1_200),
		];
		for (value, expected) in cases {
			assert_eq!(jpy(value), Some(expected), "{value}");
		}
	}

	#[test]
	fn western_formats() {
		let cases = [
			("$1,234.56", None, 123_456, "USD"),
			("$12.5", None, 1_250, "USD"),
			("$10", Some("CAD"), 1_000, "CAD"),
			("US$ 10.00", None, 1_000, "USD"),
			("USD 12.50", None, 1_250, "USD"),
			("12.50 EUR", None, 1_250, "EUR"),
			("€1.234,56", None, 123_456, "EUR"),
			("1.234,56 €", None, 123_456, "EUR"),
			("1 234,56 €", None, 123_456, "EUR"),
			("1\u{202f}234,56\u{a0}€", None, 123_456, "EUR"),
			("12,34 €", None, 1_234, "EUR"),
			("45 euros", None, 4_500, "EUR"),
			("£99", None, 9_900, "GBP"),
			("¥100", Some("CNY"), 10_000, "CNY"),
			("12,340", Some("JPY"), 12_340, "JPY"),
			("12,340", Some("円"), 12_340, "JPY"),
			("(12.50)", Some("usd"), -1_250, "USD"),
			("1.234.567 KRW", None, 1_234_567, "KRW"),
			("206.000", Some("XPF"), 206_000, "XPF"),
			("206.000 XPF", Some("XPF"), 206_000, "XPF"),
			("19.500 xpf", Some("XPF"), 19_500, "XPF"),
			("12.340円", None, 12_340, "JPY"),
			("12.50", Some("USD"), 1_250, "USD"),
			("470 075 F CFP", None, 470_075, "XPF"),
			("12 340 CFP", None, 12_340, "XPF"),
			("470 075", Some("CFP"), 470_075, "XPF"),
		];
		for (value, hint, minor, currency) in cases {
			assert_eq!(
				amount(value, hint),
				Some((minor, currency.to_string())),
				"{value}"
			);
		}
	}

	#[test]
	fn not_an_amount() {
		let cases = [
			("", None),
			("12,340", None),
			("2024-03-14", Some("JPY")),
			("令和6年3月14日", Some("JPY")),
			("3月分", Some("JPY")),
			("10%", Some("JPY")),
			("3枚", Some("JPY")),
			("口座 1234567", Some("JPY")),
			("12,340円 (内消費税 1,121円)", None),
			("1,200円 × 3", None),
			("12.5円", None),
			("1,23,4円", None),
			("万円", None),
			("千葉銀行", None),
			("abc", Some("JPY")),
			("$1.234", None),
		];
		for (value, hint) in cases {
			assert_eq!(amount(value, hint), None, "{value}");
		}
	}

	#[test]
	fn currencies_named_in_a_text() {
		let named = |text: &str| currencies_in(text);
		assert_eq!(named("Net à payer 470 075 F CFP"), ["XPF"]);
		assert_eq!(
			named("Montant 206.000 XPF, soit 1 726,28 EUR"),
			["XPF", "EUR"]
		);
		assert_eq!(named("合計 12,340円"), ["JPY"]);
		assert_eq!(named("Total $12.00"), ["USD"]);
		assert_eq!(named("Total CAD 12.00 ($)"), ["CAD"]);
		assert_eq!(
			named("Consommation 350 KWH, puissance 6 KVA"),
			Vec::<String>::new()
		);
		assert_eq!(named("CFPB annexe"), Vec::<String>::new());
		assert!(currency_fits("CAD", &named("Total $12"), "Total $12"));
		assert!(!currency_fits(
			"JPY",
			&named("470 075 F CFP"),
			"470 075 F CFP"
		));
	}

	#[test]
	fn currencies() {
		assert_eq!(normalize_currency(" jpy "), Some("JPY".into()));
		assert_eq!(normalize_currency("円"), Some("JPY".into()));
		assert_eq!(normalize_currency("￥"), Some("JPY".into()));
		assert_eq!(normalize_currency("dollars"), None);
		assert_eq!(normalize_currency("CFP"), Some("XPF".into()));
		assert_eq!(normalize_currency("F CFP"), Some("XPF".into()));
		assert_eq!(normalize_currency("xpf"), Some("XPF".into()));
		assert_eq!(normalize_currency("€"), Some("EUR".into()));
		assert_eq!(normalize_currency("PEN"), Some("PEN".into()));
		for unit in ["KWH", "KVA", "kWh", "ABC", "XXX", "EURO2", ""] {
			assert_eq!(normalize_currency(unit), None, "{unit}");
		}
		assert_eq!(decimals("JPY"), 0);
		assert_eq!(decimals("EUR"), 2);
		assert_eq!(decimals("KWD"), 3);
	}
}
