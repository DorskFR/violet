use std::borrow::Cow;

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
