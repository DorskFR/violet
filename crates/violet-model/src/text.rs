use std::borrow::Cow;
use std::collections::HashSet;

use serde_json::Value;

const THINK_OPEN: &str = "<think>";
const THINK_CLOSE: &str = "</think>";

/// Removes `<think>...</think>` reasoning blocks, including an unterminated one and one whose
/// opening tag was swallowed by the chat template.
pub fn strip_thinking(text: &str) -> String {
	let mut out = String::with_capacity(text.len());
	let mut rest = text;
	while let Some(start) = rest.find(THINK_OPEN) {
		out.push_str(&rest[..start]);
		match rest[start..].find(THINK_CLOSE) {
			Some(end) => rest = &rest[start + end + THINK_CLOSE.len()..],
			None => rest = "",
		}
	}
	out.push_str(rest);
	if let Some(end) = out.rfind(THINK_CLOSE) {
		out.drain(..end + THINK_CLOSE.len());
	}
	out.trim().to_string()
}

pub(crate) fn strip_fence(text: &str) -> &str {
	let trimmed = text.trim();
	let Some(inner) = trimmed.strip_prefix("```") else {
		return trimmed;
	};
	let inner = match inner.find('\n') {
		Some(line_end) => &inner[line_end + 1..],
		None => inner,
	};
	inner.trim_end().strip_suffix("```").unwrap_or(inner).trim()
}

pub(crate) fn clean_transcription(answer: &str) -> String {
	let text = strip_thinking(answer);
	let mut text = strip_fence(&text);
	if let Some((first, rest)) = text.split_once('\n') {
		let lead = first.trim().to_ascii_lowercase();
		let is_preamble = lead.ends_with(':')
			&& ["here is", "here's", "transcription", "the text", "sure"]
				.iter()
				.any(|start| lead.starts_with(start));
		if is_preamble {
			text = strip_fence(rest);
		}
	}
	if text.eq_ignore_ascii_case("[blank]") {
		return String::new();
	}
	text.to_string()
}

const LOOP_REPEATS: usize = 3;
const MAX_LOOP_LINES: usize = 12;
const MAX_TAIL_PERIOD: usize = 400;
const MIN_TAIL_LOOP_CHARS: usize = 24;
const MIN_COUNTED_LINE_CHARS: usize = 4;

/// Keeps one copy of whatever a transcription was looping on: a tail that repeats the same
/// characters at least three times up to the cut-off, and any line or block of lines
/// repeated at least three times in a row.
pub(crate) fn collapse_loops(text: &str) -> String {
	let text = cut_repeated_tail(text);
	let lines: Vec<&str> = text.lines().collect();
	let mut kept: Vec<&str> = Vec::with_capacity(lines.len());
	let mut at = 0;
	while at < lines.len() {
		let block = (1..=MAX_LOOP_LINES).find_map(|size| {
			let copies = consecutive_copies(&lines[at..], size);
			(copies >= LOOP_REPEATS).then_some((size, copies))
		});
		match block {
			Some((size, copies)) => {
				kept.extend(&lines[at..at + size]);
				at += size * copies;
			}
			None => {
				kept.push(lines[at]);
				at += 1;
			}
		}
	}
	kept.join("\n").trim().to_string()
}

fn consecutive_copies(lines: &[&str], size: usize) -> usize {
	let same = |a: &[&str], b: &[&str]| a.iter().zip(b).all(|(a, b)| a.trim() == b.trim());
	let mut copies = 1;
	while lines.len() >= size * (copies + 1)
		&& same(&lines[..size], &lines[size * copies..size * (copies + 1)])
	{
		copies += 1;
	}
	copies
}

fn cut_repeated_tail(text: &str) -> &str {
	let chars: Vec<char> = text.chars().collect();
	let total = chars.len();
	for period in 1..=MAX_TAIL_PERIOD.min(total / LOOP_REPEATS) {
		let repeated = (0..total - period)
			.take_while(|back| chars[total - 1 - back] == chars[total - 1 - back - period])
			.count();
		if repeated >= period * (LOOP_REPEATS - 1) && repeated + period >= MIN_TAIL_LOOP_CHARS {
			return truncate_head(text, total - repeated);
		}
	}
	text
}

fn counted_lines(text: &str) -> impl Iterator<Item = &str> {
	text.lines()
		.map(str::trim)
		.filter(|line| line.chars().count() >= MIN_COUNTED_LINE_CHARS)
}

/// Share of the lines, very short ones aside, that say what an earlier line already said.
pub(crate) fn repeated_share(text: &str) -> f32 {
	let total = counted_lines(text).count();
	if total == 0 {
		return 0.0;
	}
	let distinct = counted_lines(text).collect::<HashSet<_>>().len();
	1.0 - distinct as f32 / total as f32
}

/// Characters in the lines of the text, each different line counted once.
pub(crate) fn distinct_chars(text: &str) -> usize {
	counted_lines(text)
		.collect::<HashSet<_>>()
		.into_iter()
		.map(|line| line.chars().count())
		.sum()
}

/// The first JSON object in `text`, ignoring prose and code fences around it.
pub(crate) fn extract_json(text: &str) -> Option<Value> {
	for (start, _) in text.match_indices('{') {
		let mut stream = serde_json::Deserializer::from_str(&text[start..]).into_iter::<Value>();
		if let Some(Ok(value)) = stream.next()
			&& value.is_object()
		{
			return Some(value);
		}
	}
	None
}

/// Full-width ASCII forms to ASCII, so `１２，３４０` reads as `12,340`.
pub(crate) fn fold_width(text: &str) -> String {
	text.chars()
		.map(|c| match c {
			'\u{ff01}'..='\u{ff5e}' => char::from_u32(c as u32 - 0xfee0).unwrap_or(c),
			'\u{3000}' => ' ',
			'￥' => '¥',
			_ => c,
		})
		.collect()
}

/// Comparison key for names: width, case and whitespace do not matter.
pub(crate) fn fold_name(text: &str) -> String {
	fold_width(text)
		.chars()
		.filter(|c| !c.is_whitespace())
		.flat_map(char::to_lowercase)
		.collect()
}

const DIACRITICS: &[(&str, char)] = &[
	("àáâãäåāăą", 'a'),
	("çćĉċč", 'c'),
	("ď", 'd'),
	("èéêëēĕėęě", 'e'),
	("ĝğġģ", 'g'),
	("ĥ", 'h'),
	("ìíîïĩīĭį", 'i'),
	("ĵ", 'j'),
	("ķ", 'k'),
	("ĺļľ", 'l'),
	("ñńņň", 'n'),
	("òóôõöōŏő", 'o'),
	("ŕŗř", 'r'),
	("śŝşš", 's'),
	("ţť", 't'),
	("ùúûüũūŭůűų", 'u'),
	("ŵ", 'w'),
	("ýÿŷ", 'y'),
	("źżž", 'z'),
];

/// Lowercase, without accents on Latin letters.
pub(crate) fn fold_accents(text: &str) -> String {
	text.chars()
		.flat_map(char::to_lowercase)
		.map(|c| {
			DIACRITICS
				.iter()
				.find(|(accented, _)| accented.contains(c))
				.map_or(c, |(_, base)| *base)
		})
		.collect()
}

/// [`fold_name`] that also ignores accents: `Société` and `SOCIETE` are the same name.
pub(crate) fn fold_plain(text: &str) -> String {
	fold_accents(&fold_name(text))
}

pub(crate) fn collapse_whitespace(text: &str) -> String {
	text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn char_offset(text: &str, chars: usize) -> usize {
	text.char_indices()
		.nth(chars)
		.map_or(text.len(), |(offset, _)| offset)
}

pub(crate) fn truncate_head(text: &str, max_chars: usize) -> &str {
	&text[..char_offset(text, max_chars)]
}

/// Keeps the start and the end of a long text: headers sit at the top, totals at the bottom.
pub(crate) fn truncate_middle(text: &str, max_chars: usize) -> Cow<'_, str> {
	let total = text.chars().count();
	if total <= max_chars {
		return Cow::Borrowed(text);
	}
	let head = max_chars * 3 / 5;
	let tail = max_chars - head;
	Cow::Owned(format!(
		"{}\n[...]\n{}",
		text[..char_offset(text, head)].trim_end(),
		text[char_offset(text, total - tail)..].trim_start()
	))
}

pub(crate) fn kanji_digit(c: char) -> Option<u32> {
	"〇一二三四五六七八九"
		.chars()
		.position(|k| k == c)
		.map(|digit| digit as u32)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn thinking_is_removed() {
		assert_eq!(strip_thinking("<think>\nhmm\n</think>\n\nanswer"), "answer");
		assert_eq!(strip_thinking("a<think>x</think>b<think>y</think>c"), "abc");
		assert_eq!(
			strip_thinking("reasoning without opening tag</think>answer"),
			"answer"
		);
		assert_eq!(strip_thinking("answer<think>cut off"), "answer");
		assert_eq!(strip_thinking("  plain  "), "plain");
	}

	#[test]
	fn fences_are_removed() {
		assert_eq!(
			strip_fence("```\n請求書\n合計 1,200円\n```"),
			"請求書\n合計 1,200円"
		);
		assert_eq!(strip_fence("```text\nabc\n```\n"), "abc");
		assert_eq!(strip_fence("no fence"), "no fence");
		assert_eq!(strip_fence("```\nunterminated"), "unterminated");
	}

	#[test]
	fn transcription_wrappers_are_removed() {
		assert_eq!(
			clean_transcription("<think>ok</think>Here is the transcription:\n```\n領収書\n```"),
			"領収書"
		);
		assert_eq!(clean_transcription("[BLANK]"), "");
		assert_eq!(clean_transcription("```\n[blank]\n```"), "");
		assert_eq!(clean_transcription("合計:\n1,200円"), "合計:\n1,200円");
	}

	#[test]
	fn repeated_lines_and_blocks_are_collapsed() {
		let looped = format!("領収書\n{}合計", "かぜ薬 | 600円\n".repeat(5));
		assert_eq!(collapse_loops(&looped), "領収書\nかぜ薬 | 600円\n合計");

		let block = "氏名 | \n住所 | \n電話 | \n";
		let looped = format!("申込書\n{}以上", block.repeat(3));
		assert_eq!(
			collapse_loops(&looped),
			"申込書\n氏名 | \n住所 | \n電話 | \n以上"
		);

		let twice = "a | 1\na | 1\nb | 2\nb | 2\nc";
		assert_eq!(collapse_loops(twice), twice);
		let alternating = "x\ny\nx\ny\nz";
		assert_eq!(collapse_loops(alternating), alternating);
		assert_eq!(collapse_loops(""), "");
	}

	#[test]
	fn repeated_tail_cut_mid_way_is_collapsed() {
		let looped = format!(
			"明細\n{}2024/01/05 | 振込 | 1,0",
			"2024/01/05 | 振込 | 1,000\n".repeat(40)
		);
		assert_eq!(collapse_loops(&looped), "明細\n2024/01/05 | 振込 | 1,000");

		let one_line = format!("注意事項 {}", "ご確認ください。".repeat(50));
		assert_eq!(collapse_loops(&one_line), "注意事項 ご確認ください。");

		let dotted = "目次 ........ 3";
		assert_eq!(collapse_loops(dotted), dotted);
		let short_tail = "合計 1,000\n1,000\n1,000";
		assert_eq!(collapse_loops(short_tail), "合計 1,000\n1,000\n1,000");
	}

	#[test]
	fn repetition_is_measured_on_lines() {
		assert_eq!(repeated_share(""), 0.0);
		assert_eq!(repeated_share("領収書です\n合計 1,200円"), 0.0);
		assert_eq!(
			repeated_share("同じ行です\n別の行です\n同じ行です\n同じ行です"),
			0.5
		);
		assert_eq!(
			distinct_chars("同じ行です\n別の行です\n同じ行です\n短い"),
			10
		);
	}

	#[test]
	fn first_json_object_is_found() {
		let fenced = "Sure {not json}:\n```json\n{\"a\": {\"b\": \"}\"}}\n```\ntrailing {\"c\":1}";
		assert_eq!(
			extract_json(fenced),
			Some(serde_json::json!({"a": {"b": "}"}}))
		);
		assert_eq!(extract_json("[1, 2]"), None);
		assert_eq!(extract_json("nothing"), None);
	}

	#[test]
	fn truncation_keeps_both_ends() {
		assert_eq!(truncate_middle("short", 10), "short");
		let long: String = ('a'..='z').collect();
		assert_eq!(truncate_middle(&long, 10), "abcdef\n[...]\nwxyz");
		assert_eq!(truncate_head("日本語のテキスト", 3), "日本語");
	}

	#[test]
	fn width_is_folded() {
		assert_eq!(fold_width("￥１２，３４０　円"), "¥12,340 円");
		assert_eq!(fold_name("ＮＴＴ ドコモ"), fold_name("nttドコモ"));
	}
}
