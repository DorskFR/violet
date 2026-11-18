use violet_core::DatePrecision;

use crate::text::{fold_width, kanji_digit};

const ERAS: [(&[&str], i32); 5] = [
	(&["令和", "令", "R"], 2018),
	(&["平成", "平", "H"], 1988),
	(&["昭和", "昭", "S"], 1925),
	(&["大正", "大", "T"], 1911),
	(&["明治", "明", "M"], 1867),
];

const MONTHS: [(&str, u32); 27] = [
	("january", 1),
	("february", 2),
	("march", 3),
	("april", 4),
	("may", 5),
	("june", 6),
	("july", 7),
	("august", 8),
	("september", 9),
	("october", 10),
	("november", 11),
	("december", 12),
	("janvier", 1),
	("février", 2),
	("fevrier", 2),
	("mars", 3),
	("avril", 4),
	("mai", 5),
	("juin", 6),
	("juillet", 7),
	("août", 8),
	("aout", 8),
	("septembre", 9),
	("octobre", 10),
	("novembre", 11),
	("décembre", 12),
	("decembre", 12),
];
const MIN_MONTH_PREFIX: usize = 3;
const ORDINALS: [&str; 6] = ["st", "nd", "rd", "th", "er", "e"];
/// Words printed around a date that do not change it.
const SKIPPED_WORDS: [&str; 25] = [
	"monday",
	"tuesday",
	"wednesday",
	"thursday",
	"friday",
	"saturday",
	"sunday",
	"mon",
	"tue",
	"tues",
	"wed",
	"thu",
	"thur",
	"thurs",
	"fri",
	"sat",
	"sun",
	"lundi",
	"mardi",
	"mercredi",
	"jeudi",
	"vendredi",
	"samedi",
	"dimanche",
	"le",
];
const RANGE_STARTS: [&str; 6] = [
	"période du ",
	"periode du ",
	"period from ",
	"du ",
	"from ",
	"de ",
];
const RANGE_SEPARATORS: [&str; 10] = [
	" au ", " to ", " à ", " a ", " - ", " – ", " — ", "~", "〜", "から",
];
const RANGE_ENDS: [&str; 2] = ["まで", "迄"];

const MIN_YEAR: i32 = 1868;
const MAX_YEAR: i32 = 2100;
/// Two-digit years below it are in the 2000s, the others in the 1900s.
const CENTURY_PIVOT: u32 = 50;

/// What a stated document date reads as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Stated {
	/// `YYYY-MM-DD`; without a printed day, a month (`mars 2014`, `令和6年3月分`) on its 1st.
	Date { date: String, day: bool },
	/// From the first date to the second, both `YYYY-MM-DD`.
	Range { start: String, end: String },
	/// Written out in French words, as notaries do: `L'AN DEUX MIL VINGT-TROIS LE ONZE AVRIL`.
	Words { date: String },
}

impl Stated {
	/// The document date: a range that is one calendar month is that month, another range
	/// its last day.
	pub(crate) fn document_date(&self) -> (String, DatePrecision) {
		match self {
			Self::Date { date, day: false } => (date.clone(), DatePrecision::Month),
			Self::Date { date, .. } | Self::Words { date } => (date.clone(), DatePrecision::Day),
			Self::Range { start, end } if is_whole_month(start, end) => {
				(start.clone(), DatePrecision::Month)
			}
			Self::Range { end, .. } => (end.clone(), DatePrecision::Day),
		}
	}
}

/// Turns a printed date into `YYYY-MM-DD`: ISO and slashed forms, `2024年3月14日`, compact
/// `20240314`, day-first `14/03/2024` and `14/03/24`, English and French month names
/// (`Mar 14, 2024`, `14 mars 2024`, `08 JAN 16`, `25NOV2023`), and Japanese era dates such as
/// `令和6年3月14日`, `平成元年1月8日` or `R6.3.14`. A time or a weekday next to it is ignored.
/// A date without a day gets day 01. `None` when it is not a real calendar date.
pub fn normalize_date(input: &str) -> Option<String> {
	read_date(input).map(|(date, _)| date)
}

/// The date of a document as [`normalize_date`] reads it, a period (`du 01/03/2014 au
/// 31/03/2014`) or a date in French words, with how precise it is: a month (`mars 2014`,
/// `令和6年3月分`, a range over one calendar month) is its first day.
pub fn read_document_date(input: &str) -> Option<(String, DatePrecision)> {
	read_stated(input, true).map(|stated| stated.document_date())
}

/// The stated date; with `short_years` false, `24/03/14` and `24.03.14` are not read, since in
/// Japanese, Chinese or Korean they put the year first.
pub(crate) fn read_stated(input: &str, short_years: bool) -> Option<Stated> {
	if let Some((date, day)) = read_date_in(input, short_years) {
		return Some(Stated::Date { date, day });
	}
	if let Some((start, end)) = read_range(input, short_years) {
		return Some(Stated::Range { start, end });
	}
	words_date(input).map(|date| Stated::Words { date })
}

/// The date and whether its day is printed rather than assumed.
pub(crate) fn read_date(input: &str) -> Option<(String, bool)> {
	read_date_in(input, true)
}

/// [`read_date`]; with `short_years` false, a day, a month and a two-digit year are not read.
pub(crate) fn read_date_in(input: &str, short_years: bool) -> Option<(String, bool)> {
	let cleaned = without_clock(&fold_width(input));
	let mut text = cleaned.as_str();
	if let Some((year, month, day)) = named_month_date(text) {
		return Some((checked(year, month, day.unwrap_or(1))?, day.is_some()));
	}
	if let Some(day_mark) = text.find('日') {
		text = &text[..day_mark];
	}
	let (era_base, rest) = split_era(text);
	let numbers = numbers(rest)?;
	let short_year = short_years && era_base.is_none() && !rest.contains(['-', ' ', '年', '月']);
	let (year, month, day) = match (era_base, numbers.as_slice()) {
		(Some(base), &[(year, _), (month, _), (day, _)]) => (base + year as i32, month, day),
		(Some(base), &[(year, _), (month, _)]) => (base + year as i32, month, 1),
		(None, &[(compact, 8)]) => (
			(compact / 10_000) as i32,
			compact / 100 % 100,
			compact % 100,
		),
		(None, &[(year, 4), (month, _), (day, _)]) => (year as i32, month, day),
		(None, &[(year, 4), (month, _)]) => (year as i32, month, 1),
		(None, &[(month, 1 | 2), (year, 4)]) => (year as i32, month, 1),
		(None, &[(day, _), (month, _), (year, 4)]) if month <= 12 => (year as i32, month, day),
		(None, &[(month, _), (day, _), (year, 4)]) => (year as i32, month, day),
		(None, &[(day, 1 | 2), (month, 1 | 2), (year, 2)]) if short_year && month <= 12 => {
			(full_year(year), month, day)
		}
		(None, &[(month, 1 | 2), (day, 1 | 2), (year, 2)]) if short_year => {
			(full_year(year), month, day)
		}
		_ => return None,
	};
	if era_base.is_some() && numbers[0].0 == 0 {
		return None;
	}
	let with_day = numbers.len() != 2;
	Some((checked(year, month, day)?, with_day))
}

/// A day and a month printed without a year (`7 juil.`, `February 28`, `20/9 10h`), as
/// `(month, day)`.
pub(crate) fn day_and_month(input: &str) -> Option<(u32, u32)> {
	let text = without_clock(&fold_width(input));
	let (month, day) = match named_tokens(&text)? {
		(Some(month), numbers) => match numbers.as_slice() {
			&[(day, 1 | 2)] => (month, day),
			_ => return None,
		},
		(None, _) => match *numbers(&text)?.as_slice() {
			[(day, 1 | 2), (month, 1 | 2)] if text.contains(['/', '.']) && month <= 12 => {
				(month, day)
			}
			[(month, 1 | 2), (day, 1 | 2)] if text.contains(['/', '.']) => (month, day),
			_ => return None,
		},
	};
	((1..=12).contains(&month) && (1..=days_in_month(2000, month)).contains(&day))
		.then_some((month, day))
}

/// Whether the text names a year and nothing else that reads as a date: `Solde 2017`,
/// `Summer 2014`.
pub(crate) fn only_a_year(input: &str) -> bool {
	let years = input
		.split(|c: char| !c.is_ascii_digit())
		.filter(|run| !run.is_empty())
		.collect::<Vec<_>>();
	matches!(years.as_slice(), [year] if year.len() == 4)
}

fn full_year(short: u32) -> i32 {
	if short < CENTURY_PIVOT {
		2000 + short as i32
	} else {
		1900 + short as i32
	}
}

/// A start and an end date: `du 01/03/2014 au 31/03/2014`, `2024/4/1～2024/4/30`,
/// `from March 1, 2024 to March 31, 2024`. `None` when the end is before the start.
fn read_range(input: &str, short_years: bool) -> Option<(String, String)> {
	let folded = fold_width(input).to_lowercase();
	let mut text = folded.trim();
	for start in RANGE_STARTS {
		if let Some(rest) = text.strip_prefix(start) {
			text = rest;
			break;
		}
	}
	for end in RANGE_ENDS {
		text = text.strip_suffix(end).unwrap_or(text).trim_end();
	}
	let (from, to) = RANGE_SEPARATORS
		.iter()
		.find_map(|separator| text.split_once(separator))?;
	let (start, _) = read_date_in(from, short_years)?;
	let (end, _) = read_date_in(to, short_years)?;
	(start <= end).then_some((start, end))
}

fn is_whole_month(start: &str, end: &str) -> bool {
	let parts = |date: &str| -> Option<(i32, u32, u32)> {
		Some((
			date.get(..4)?.parse().ok()?,
			date.get(5..7)?.parse().ok()?,
			date.get(8..10)?.parse().ok()?,
		))
	};
	match (parts(start), parts(end)) {
		(Some((year, month, 1)), Some((end_year, end_month, end_day))) => {
			(year, month) == (end_year, end_month) && end_day == days_in_month(year, month)
		}
		_ => false,
	}
}

/// The text without times (`14:00`, `10h`, `5:31 PM`) and weekdays around a date.
fn without_clock(text: &str) -> String {
	let text = text.replace(['･', '・'], ".");
	let mut kept: Vec<&str> = Vec::new();
	for token in text.split_whitespace() {
		let bare = token.trim_matches(|c: char| matches!(c, ',' | '.' | '(' | ')'));
		let lower = bare.to_lowercase();
		let is_meridiem = matches!(lower.as_str(), "am" | "pm" | "a.m" | "p.m");
		if is_clock(&lower) || is_meridiem || SKIPPED_WORDS.contains(&lower.as_str()) {
			continue;
		}
		kept.push(token);
	}
	kept.join(" ").trim().to_string()
}

fn is_clock(token: &str) -> bool {
	let token = token
		.strip_suffix("am")
		.or_else(|| token.strip_suffix("pm"))
		.unwrap_or(token);
	let (hours, minutes) = match token.split_once([':', 'h']) {
		Some(split) => split,
		None => return false,
	};
	let digits = |part: &str, most: usize| {
		!part.is_empty() && part.len() <= most && part.chars().all(|c| c.is_ascii_digit())
	};
	let minutes = minutes.split(':').next().unwrap_or_default();
	digits(hours, 2) && ((minutes.is_empty() && token.ends_with('h')) || digits(minutes, 2))
}

fn checked(year: i32, month: u32, day: u32) -> Option<String> {
	let valid = (MIN_YEAR..=MAX_YEAR).contains(&year)
		&& (1..=12).contains(&month)
		&& (1..=days_in_month(year, month)).contains(&day);
	valid.then(|| format!("{year:04}-{month:02}-{day:02}"))
}

/// The month a name or an abbreviation of at least three letters stands for.
fn named_month(word: &str) -> Option<u32> {
	let word = word.to_lowercase();
	if word.chars().count() < MIN_MONTH_PREFIX {
		return None;
	}
	let mut months = MONTHS
		.iter()
		.filter(|(name, _)| name.starts_with(&word))
		.map(|(_, month)| *month);
	let month = months.next()?;
	months.all(|other| other == month).then_some(month)
}

/// A date written with a month name: `March 14, 2024`, `14 Mar 2024`, `1er mars 2024`,
/// `March 2024`, `08 JAN 16`, `25NOV2023`.
fn named_month_date(text: &str) -> Option<(i32, u32, Option<u32>)> {
	let (month, numbers) = named_tokens(text)?;
	let month = month?;
	let (year, day) = match numbers.as_slice() {
		&[(year, 4)] => (year as i32, None),
		&[(day, 1 | 2), (year, 4)] | &[(year, 4), (day, 1 | 2)] => (year as i32, Some(day)),
		&[(day, 1 | 2), (year, 2)] => (full_year(year), Some(day)),
		_ => return None,
	};
	Some((year, month, day))
}

/// Numbers with their digit counts.
type Numbers = Vec<(u32, usize)>;

/// The month named in the text and its numbers with their digit counts; `None` when a word is
/// neither a month nor an ordinal suffix, or when two months are named.
fn named_tokens(text: &str) -> Option<(Option<u32>, Numbers)> {
	let mut month = None;
	let mut numbers = Vec::new();
	for token in text.split(|c: char| c.is_whitespace() || matches!(c, ',' | '.' | '-' | '/')) {
		let mut rest = token;
		let mut after_number = false;
		while !rest.is_empty() {
			let digits = rest
				.find(|c: char| !c.is_ascii_digit())
				.unwrap_or(rest.len());
			if digits > 0 {
				if digits > 4 {
					return None;
				}
				numbers.push((rest[..digits].parse().ok()?, digits));
				rest = &rest[digits..];
				after_number = true;
				continue;
			}
			let letters = rest
				.find(|c: char| c.is_ascii_digit())
				.unwrap_or(rest.len());
			let word = &rest[..letters];
			rest = &rest[letters..];
			if after_number && ORDINALS.contains(&word.to_lowercase().as_str()) {
				after_number = false;
				continue;
			}
			if month.replace(named_month(word)?).is_some() {
				return None;
			}
			after_number = false;
		}
	}
	if month.is_none() && numbers.is_empty() {
		return None;
	}
	Some((month, numbers))
}

/// A date written in French words, as in a notarial deed: `L'AN DEUX MIL VINGT-TROIS LE ONZE
/// AVRIL`, `le premier mars deux mille vingt-quatre`.
fn words_date(input: &str) -> Option<String> {
	let lower = input
		.to_lowercase()
		.replace(['-', '\'', '’', ',', '.'], " ");
	let mut month = None;
	let mut groups: Vec<Vec<&str>> = Vec::new();
	let mut group: Vec<&str> = Vec::new();
	for word in lower.split_whitespace() {
		let joins = word == "et" && !group.is_empty();
		if french_number(word).is_some() || joins || word == "premier" {
			group.push(word);
			continue;
		}
		if !group.is_empty() {
			groups.push(std::mem::take(&mut group));
		}
		if let Some(named) = MONTHS
			.iter()
			.find(|(name, _)| *name == word)
			.map(|(_, number)| *number)
			&& month.replace(named).is_some()
		{
			return None;
		}
	}
	if !group.is_empty() {
		groups.push(group);
	}
	let values: Vec<u32> = groups
		.iter()
		.map(|words| french_value(words))
		.collect::<Option<_>>()?;
	let (day, year) = match values.as_slice() {
		&[day, year] | &[year, day] if (1..=31).contains(&day) && year >= 1000 => (day, year),
		_ => return None,
	};
	checked(year as i32, month?, day)
}

fn french_number(word: &str) -> Option<u32> {
	Some(match word {
		"zéro" | "zero" => 0,
		"un" | "une" => 1,
		"deux" => 2,
		"trois" => 3,
		"quatre" => 4,
		"cinq" => 5,
		"six" => 6,
		"sept" => 7,
		"huit" => 8,
		"neuf" => 9,
		"dix" => 10,
		"onze" => 11,
		"douze" => 12,
		"treize" => 13,
		"quatorze" => 14,
		"quinze" => 15,
		"seize" => 16,
		"vingt" | "vingts" => 20,
		"trente" => 30,
		"quarante" => 40,
		"cinquante" => 50,
		"soixante" => 60,
		"cent" | "cents" => 100,
		"mil" | "mille" => 1000,
		_ => return None,
	})
}

fn french_value(words: &[&str]) -> Option<u32> {
	if words == ["premier"] {
		return Some(1);
	}
	let mut total = 0;
	let mut current = 0;
	let mut previous = 0;
	for word in words {
		if *word == "et" {
			continue;
		}
		let value = french_number(word)?;
		match value {
			1000 => {
				total += current.max(1) * 1000;
				current = 0;
			}
			100 => current = (current % 100).max(1) * 100 + current / 100 * 100,
			20 if previous == 4 => current += 76,
			_ => current += value,
		}
		previous = value;
	}
	Some(total + current)
}

fn split_era(text: &str) -> (Option<i32>, &str) {
	for (names, base) in ERAS {
		for name in names {
			let Some(rest) = text.strip_prefix(name) else {
				continue;
			};
			let rest = rest.trim_start_matches(['.', ' ']);
			if rest.starts_with(|c: char| c.is_ascii_digit() || c == '元' || is_kanji_numeral(c)) {
				return (Some(base), rest);
			}
		}
	}
	(None, text)
}

fn is_kanji_numeral(c: char) -> bool {
	c == '十' || kanji_digit(c).is_some()
}

/// The numbers of a date with their digit counts, or `None` when anything but numbers and
/// date separators is present.
fn numbers(text: &str) -> Option<Vec<(u32, usize)>> {
	let mut out = Vec::new();
	let mut token = String::new();
	for c in text.chars().chain([' ']) {
		if c.is_ascii_digit() || c == '元' || is_kanji_numeral(c) {
			token.push(c);
			continue;
		}
		if !matches!(c, '年' | '月' | '分' | '/' | '-' | '.' | ' ') {
			return None;
		}
		if !token.is_empty() {
			out.push(number(&token)?);
			token.clear();
		}
	}
	Some(out)
}

fn number(token: &str) -> Option<(u32, usize)> {
	if token == "元" {
		return Some((1, 1));
	}
	if token.chars().all(|c| c.is_ascii_digit()) {
		return (token.len() <= 8)
			.then(|| token.parse().ok())
			.flatten()
			.map(|n| (n, token.len()));
	}
	let value = match token.split_once('十') {
		Some((tens, units)) => {
			let tens = if tens.is_empty() {
				1
			} else {
				single_kanji(tens)?
			};
			let units = if units.is_empty() {
				0
			} else {
				single_kanji(units)?
			};
			tens * 10 + units
		}
		None => token
			.chars()
			.try_fold(0u32, |acc, c| Some(acc * 10 + kanji_digit(c)?))?,
	};
	Some((value, token.chars().count()))
}

fn single_kanji(text: &str) -> Option<u32> {
	let mut chars = text.chars();
	let digit = kanji_digit(chars.next()?)?;
	chars.next().is_none().then_some(digit)
}

fn days_in_month(year: i32, month: u32) -> u32 {
	match month {
		4 | 6 | 9 | 11 => 30,
		2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
		2 => 28,
		_ => 31,
	}
}

#[cfg(test)]
mod tests {
	use super::normalize_date;

	fn check(cases: &[(&str, Option<&str>)]) {
		for (input, expected) in cases {
			assert_eq!(normalize_date(input).as_deref(), *expected, "{input}");
		}
	}

	#[test]
	fn western_dates() {
		check(&[
			("2024-03-14", Some("2024-03-14")),
			(" 2024/3/4 ", Some("2024-03-04")),
			("2024.03.14", Some("2024-03-14")),
			("2024年3月14日", Some("2024-03-14")),
			("2024年3月14日（木）", Some("2024-03-14")),
			("２０２４年３月１４日", Some("2024-03-14")),
			("20240314", Some("2024-03-14")),
			("14/03/2024", Some("2024-03-14")),
			("03/14/2024", Some("2024-03-14")),
			("2024-03", Some("2024-03-01")),
			("2024年3月", Some("2024-03-01")),
			("二〇二四年三月十四日", Some("2024-03-14")),
		]);
	}

	#[test]
	fn named_months() {
		check(&[
			("March 14, 2024", Some("2024-03-14")),
			("Jan 20, 2023", Some("2023-01-20")),
			("14 March 2024", Some("2024-03-14")),
			("Sept. 3rd, 2024", Some("2024-09-03")),
			("31-Dec-2022", Some("2022-12-31")),
			("1er mars 2024", Some("2024-03-01")),
			("31 juillet 2022", Some("2022-07-31")),
			("5 févr. 2023", Some("2023-02-05")),
			("août 2021", Some("2021-08-01")),
			("JANUARY 2025", Some("2025-01-01")),
			("jui 2024", None),
		]);
	}

	#[test]
	fn era_dates() {
		check(&[
			("令和6年3月14日", Some("2024-03-14")),
			("令和６年３月１４日", Some("2024-03-14")),
			("令和元年5月1日", Some("2019-05-01")),
			("令和六年三月十四日", Some("2024-03-14")),
			("令和 6 年 3 月 14 日", Some("2024-03-14")),
			("平成31年4月30日", Some("2019-04-30")),
			("平成元年1月8日", Some("1989-01-08")),
			("平成二十三年十二月三十一日", Some("2011-12-31")),
			("昭和64年1月7日", Some("1989-01-07")),
			("昭和50年10月1日", Some("1975-10-01")),
			("大正15年12月24日", Some("1926-12-24")),
			("明治45年7月29日", Some("1912-07-29")),
			("R6.3.14", Some("2024-03-14")),
			("R6/3/14", Some("2024-03-14")),
			("H31.4.30", Some("2019-04-30")),
			("S63.12.1", Some("1988-12-01")),
			("令6.3.14", Some("2024-03-14")),
			("令和6年3月", Some("2024-03-01")),
			("令和6年3月分", Some("2024-03-01")),
			("令和0年3月1日", None),
		]);
	}

	#[test]
	fn two_digit_years_are_day_first() {
		check(&[
			("27/04/18", Some("2018-04-27")),
			("3/12/18", Some("2018-12-03")),
			("02/25/16", Some("2016-02-25")),
			("17.03.18", Some("2018-03-17")),
			("10.01.22", Some("2022-01-10")),
			("31/12/86", Some("1986-12-31")),
			("31/02/18", None),
			("24-03-14", None),
			("11 09 26", None),
			("10/17", None),
		]);
	}

	#[test]
	fn compact_and_short_named_months() {
		check(&[
			("08 JAN 16", Some("2016-01-08")),
			("25NOV2023", Some("2023-11-25")),
			("22Mar2026", Some("2026-03-22")),
			("22MAY17", Some("2017-05-22")),
			("21Jul15", Some("2015-07-21")),
			("le 14 mars 2024", Some("2024-03-14")),
			("lundi 3 juin 2024", Some("2024-06-03")),
			("2025･07.31", Some("2025-07-31")),
			("03/2014", Some("2014-03-01")),
			("10 aoét 2016", None),
		]);
	}

	#[test]
	fn times_and_weekdays_are_ignored() {
		check(&[
			("23 Oct. 2024 14:00", Some("2024-10-23")),
			("19 oct. 2022 12:00, mercredi", Some("2022-10-19")),
			("20/9/2024 10h", Some("2024-09-20")),
			("Mon, March 4, 2024 5:31 PM", Some("2024-03-04")),
			("2024-03-14T10:00:00", None),
		]);
	}

	#[test]
	fn document_dates_carry_their_precision() {
		use violet_core::DatePrecision::{Day, Month};
		let cases = [
			("2024-03-14", Some(("2024-03-14", Day))),
			("mars 2014", Some(("2014-03-01", Month))),
			("令和6年3月分", Some(("2024-03-01", Month))),
			("du 01/03/2014 au 31/03/2014", Some(("2014-03-01", Month))),
			(
				"Période du 1er février 2024 au 29 février 2024",
				Some(("2024-02-01", Month)),
			),
			("2024/4/1～2024/4/30", Some(("2024-04-01", Month))),
			("du 31 mai 2015 au 30 juin 2015", Some(("2015-06-30", Day))),
			(
				"from March 1, 2024 to March 15, 2024",
				Some(("2024-03-15", Day)),
			),
			("du 30/06/2015 au 31/05/2015", None),
			(
				"L'AN MIL NEUF CENT QUATRE VINGT SIX Le onze avril",
				Some(("1986-04-11", Day)),
			),
			(
				"le premier mars deux mille vingt-quatre",
				Some(("2024-03-01", Day)),
			),
			(
				"l'an deux mil vingt et un, le trente et un décembre",
				Some(("2021-12-31", Day)),
			),
			("L'AN DEUX MIL VINGT-TROIS", None),
			("Solde 2017", None),
		];
		for (input, expected) in cases {
			assert_eq!(
				super::read_document_date(input),
				expected.map(|(date, precision)| (date.to_string(), precision)),
				"{input}"
			);
		}
	}

	#[test]
	fn days_without_a_year_and_years_alone() {
		use super::{day_and_month, only_a_year};
		assert_eq!(day_and_month("7 juil."), Some((7, 7)));
		assert_eq!(day_and_month("February 28"), Some((2, 28)));
		assert_eq!(day_and_month("23. Oct. 14:00"), Some((10, 23)));
		assert_eq!(day_and_month("19. oct. 12:00, mercredi"), Some((10, 19)));
		assert_eq!(day_and_month("20/9 10h"), Some((9, 20)));
		assert_eq!(day_and_month("February 30"), None);
		assert_eq!(day_and_month("juillet"), None);
		assert!(only_a_year("Solde 2017"));
		assert!(only_a_year("Summer 2014"));
		assert!(!only_a_year("2024+10 tr 1"));
		assert!(!only_a_year("7 juil."));
	}

	#[test]
	fn invalid_dates() {
		check(&[
			("", None),
			("2024-02-30", None),
			("2023-02-29", None),
			("2024-02-29", Some("2024-02-29")),
			("1900-02-29", None),
			("2024-13-01", None),
			("2024-00-10", None),
			("24-03-14", None),
			("2024", None),
			("令和6年", None),
			("14 Marchember 2024", None),
			("Ma 14, 2024", None),
			("March 14, 2024 2025", None),
			("June 31, 2024", None),
			("2024-03-14T10:00:00", None),
			("0001-01-01", None),
			("3024-01-01", None),
		]);
	}
}
