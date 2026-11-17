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

const MIN_YEAR: i32 = 1868;
const MAX_YEAR: i32 = 2100;

/// Turns a printed date into `YYYY-MM-DD`: ISO and slashed forms, `2024年3月14日`, compact
/// `20240314`, day-first `14/03/2024`, English and French month names (`Mar 14, 2024`,
/// `14 mars 2024`), and Japanese era dates such as `令和6年3月14日`, `平成元年1月8日` or
/// `R6.3.14`. A date without a day gets day 01. `None` when it is not a real calendar date.
pub fn normalize_date(input: &str) -> Option<String> {
	read_date(input).map(|(date, _)| date)
}

/// The date and whether its day is printed rather than assumed.
pub(crate) fn read_date(input: &str) -> Option<(String, bool)> {
	let folded = fold_width(input);
	let mut text = folded.trim();
	if let Some((year, month, day)) = named_month_date(text) {
		return Some((checked(year, month, day.unwrap_or(1))?, day.is_some()));
	}
	if let Some(day_mark) = text.find('日') {
		text = &text[..day_mark];
	}
	let (era_base, rest) = split_era(text);
	let numbers = numbers(rest)?;
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
		(None, &[(day, _), (month, _), (year, 4)]) if month <= 12 => (year as i32, month, day),
		(None, &[(month, _), (day, _), (year, 4)]) => (year as i32, month, day),
		_ => return None,
	};
	if era_base.is_some() && numbers[0].0 == 0 {
		return None;
	}
	let with_day = numbers.len() != 2;
	Some((checked(year, month, day)?, with_day))
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
/// `March 2024`.
fn named_month_date(text: &str) -> Option<(i32, u32, Option<u32>)> {
	let mut month = None;
	let mut year = None;
	let mut day = None;
	for token in text.split(|c: char| c.is_whitespace() || matches!(c, ',' | '.' | '-' | '/')) {
		if token.is_empty() {
			continue;
		}
		let digits = token.chars().take_while(char::is_ascii_digit).count();
		let suffix = &token[digits..];
		if digits == 0 {
			if month.replace(named_month(token)?).is_some() {
				return None;
			}
		} else if digits == 4 && suffix.is_empty() {
			if year.replace(token.parse().ok()?).is_some() {
				return None;
			}
		} else if digits <= 2 && (suffix.is_empty() || ORDINALS.contains(&suffix)) {
			if day.replace(token[..digits].parse().ok()?).is_some() {
				return None;
			}
		} else {
			return None;
		}
	}
	Some((year?, month?, day))
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
