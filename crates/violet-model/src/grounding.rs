//! Checks that tie what the model says about a document to the text printed on it: the
//! title is in the script of the page, a tag is a known topic or a word of the page.

use crate::text::{collapse_whitespace, fold_width, truncate_head};

/// Topics a tag may name without being printed on the document.
pub const TOPIC_TAGS: [&str; 81] = [
	"receipt",
	"invoice",
	"bill",
	"payment",
	"refund",
	"purchase",
	"warranty",
	"subscription",
	"delivery",
	"salary",
	"bonus",
	"income",
	"tax",
	"pension",
	"insurance",
	"health",
	"medical",
	"dental",
	"pharmacy",
	"prescription",
	"hospital",
	"vaccination",
	"checkup",
	"school",
	"university",
	"education",
	"childcare",
	"travel",
	"flight",
	"train",
	"hotel",
	"ticket",
	"booking",
	"event",
	"association",
	"hobby",
	"sport",
	"car",
	"vehicle",
	"parking",
	"bank",
	"banking",
	"loan",
	"mortgage",
	"credit",
	"card",
	"statement",
	"transfer",
	"investment",
	"utilities",
	"electricity",
	"gas",
	"water",
	"phone",
	"mobile",
	"internet",
	"rent",
	"rental",
	"lease",
	"housing",
	"property",
	"moving",
	"contract",
	"certificate",
	"application",
	"renewal",
	"residence",
	"visa",
	"passport",
	"identity",
	"immigration",
	"family",
	"marriage",
	"birth",
	"work",
	"employment",
	"legal",
	"notary",
	"government",
	"manual",
	"leaflet",
];

const CJK_WEIGHT: usize = 2;
const MIN_SCRIPT_LETTERS: usize = 8;
const PRESENT_SHARE_PERCENT: usize = 25;
const MIN_LINE_LETTERS: usize = 3;
const MAX_LINE_TITLE_CHARS: usize = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Script {
	/// Han, hiragana and katakana.
	Cjk,
	Latin,
	Other,
}

const SCRIPTS: [Script; 3] = [Script::Cjk, Script::Latin, Script::Other];

fn script_of(c: char) -> Option<Script> {
	match c {
		'\u{3040}'..='\u{30ff}'
		| '\u{31f0}'..='\u{31ff}'
		| '\u{3400}'..='\u{4dbf}'
		| '\u{4e00}'..='\u{9fff}'
		| '\u{f900}'..='\u{faff}'
		| '\u{ff66}'..='\u{ff9f}'
		| '々'
		| '〆' => Some(Script::Cjk),
		'a'..='z' | 'A'..='Z' | '\u{ff21}'..='\u{ff3a}' | '\u{ff41}'..='\u{ff5a}' => {
			Some(Script::Latin)
		}
		'\u{c0}'..='\u{24f}' | '\u{1e00}'..='\u{1eff}' if c.is_alphabetic() => Some(Script::Latin),
		_ if c.is_alphabetic() => Some(Script::Other),
		_ => None,
	}
}

/// Letters of each script in [`SCRIPTS`] order, a CJK character counting for more than one
/// since it carries more than a letter of an alphabet does.
fn script_weights(text: &str) -> [usize; 3] {
	let mut weights = [0; 3];
	for script in text.chars().filter_map(script_of) {
		match script {
			Script::Cjk => weights[0] += CJK_WEIGHT,
			Script::Latin => weights[1] += 1,
			Script::Other => weights[2] += 1,
		}
	}
	weights
}

fn dominant_script(text: &str) -> Option<Script> {
	let weights = script_weights(text);
	SCRIPTS
		.into_iter()
		.zip(weights)
		.filter(|(_, weight)| *weight > 0)
		.max_by_key(|(script, weight)| (*weight, *script == Script::Cjk))
		.map(|(script, _)| script)
}

/// Whether a title is written in a script the page text is written in. A title without
/// letters, any title for a page without enough letters to tell, and a title printed word
/// for word on the page all pass.
pub fn title_fits_text(title: &str, page_text: &str) -> bool {
	let Some(script) = dominant_script(title) else {
		return true;
	};
	let weights = script_weights(page_text);
	let total: usize = weights.iter().sum();
	if total < MIN_SCRIPT_LETTERS {
		return true;
	}
	let position = SCRIPTS
		.iter()
		.position(|candidate| *candidate == script)
		.unwrap_or(0);
	if weights[position] * 100 >= total * PRESENT_SHARE_PERCENT {
		return true;
	}
	let squeeze = |text: &str| -> String {
		fold_width(text)
			.chars()
			.filter(|c| !c.is_whitespace())
			.flat_map(char::to_lowercase)
			.collect()
	};
	squeeze(page_text).contains(&squeeze(title))
}

/// A title taken from the page itself: its first line that reads as words in the script of
/// the page.
pub(crate) fn title_from_text(page_text: &str) -> Option<String> {
	let script = dominant_script(page_text)?;
	page_text.lines().find_map(|line| {
		let line = collapse_whitespace(line);
		let line = line.trim_matches(|c: char| !c.is_alphanumeric());
		let letters = line
			.chars()
			.filter(|c| script_of(*c) == Some(script))
			.count();
		let noise = line
			.chars()
			.filter(|c| !c.is_whitespace() && script_of(*c) != Some(script))
			.count();
		let reads = letters >= MIN_LINE_LETTERS && letters > noise && !line.contains("://");
		reads.then(|| shorten(line))
	})
}

fn shorten(line: &str) -> String {
	if line.chars().count() <= MAX_LINE_TITLE_CHARS {
		return line.to_string();
	}
	let head = truncate_head(line, MAX_LINE_TITLE_CHARS);
	let cut = match head.rfind(' ') {
		Some(space) if space > head.len() / 2 => &head[..space],
		_ => head,
	};
	cut.trim_end_matches(|c: char| !c.is_alphanumeric())
		.to_string()
}

fn unaccented(c: char) -> &'static str {
	match c {
		'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => "a",
		'ç' | 'ć' | 'č' => "c",
		'ď' | 'đ' => "d",
		'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ė' | 'ę' | 'ě' => "e",
		'ì' | 'í' | 'î' | 'ï' | 'ī' | 'į' | 'ı' => "i",
		'ł' => "l",
		'ñ' | 'ń' | 'ň' => "n",
		'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ő' => "o",
		'ř' => "r",
		'ś' | 'š' | 'ş' => "s",
		'ť' => "t",
		'ù' | 'ú' | 'û' | 'ü' | 'ū' | 'ů' | 'ű' => "u",
		'ý' | 'ÿ' => "y",
		'ź' | 'ż' | 'ž' => "z",
		'æ' => "ae",
		'œ' => "oe",
		'ß' => "ss",
		_ => "",
	}
}

/// Comparison key for words: width, case and accents do not matter, and everything that is
/// not a letter or a digit is one space.
fn word_key(text: &str) -> String {
	let mut key = String::with_capacity(text.len());
	for c in fold_width(text).chars().flat_map(char::to_lowercase) {
		match unaccented(c) {
			"" if c.is_alphanumeric() => key.push(c),
			"" => key.push(' '),
			plain => key.push_str(plain),
		}
	}
	collapse_whitespace(&key)
}

fn occurs_as_words(key: &str, text_key: &str) -> bool {
	let edge = |c: Option<char>| c.is_some_and(|c| c.is_ascii_alphanumeric());
	text_key.match_indices(key).any(|(start, _)| {
		let before = text_key[..start].chars().next_back();
		let after = text_key[start + key.len()..].chars().next();
		!(edge(key.chars().next()) && edge(before))
			&& !(edge(key.chars().next_back()) && edge(after))
	})
}

/// The page text in the form [`TagGrounds::holds`] compares tags with.
pub struct TagGrounds {
	text_key: String,
}

impl TagGrounds {
	pub fn new(page_text: &str) -> Self {
		Self {
			text_key: word_key(page_text),
		}
	}

	/// Whether a tag is made of [`TOPIC_TAGS`] words or is printed in the page text.
	pub fn holds(&self, tag: &str) -> bool {
		let key = word_key(tag);
		if key.is_empty() {
			return false;
		}
		key.split(' ').all(|word| TOPIC_TAGS.contains(&word))
			|| occurs_as_words(&key, &self.text_key)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	const LEAFLET: &str = "Venez planter avec nous !\nJournée de reboisement au parc du Mont Vénus\nSamedi 2 mars de 8h à 12h\nwww.ville-exemple.nc\nVille de Port-Céleste";

	#[test]
	fn a_title_must_be_in_a_script_of_the_page() {
		assert!(!title_fits_text("イベント告知", LEAFLET));
		assert!(title_fits_text("Journée de reboisement", LEAFLET));
		assert!(title_fits_text("Reforestation day", LEAFLET));

		let payslip = "給与明細書 令和6年3月分\n株式会社サンプル\n支給額 312,450円 ID: A-1029";
		assert!(title_fits_text("給与明細", payslip));
		assert!(!title_fits_text("Payslip", payslip));
		assert!(title_fits_text("A-1029", payslip));
		assert!(!title_fits_text("Платёжная ведомость", payslip));

		let mixed = "Apple Store Ginza ご利用明細 iPhone 15 Pro 256GB Natural Titanium お買い上げありがとうございます";
		assert!(title_fits_text("ご利用明細", mixed));
		assert!(title_fits_text("iPhone 15 Pro", mixed));
	}

	#[test]
	fn a_title_is_not_judged_without_letters_to_compare() {
		assert!(title_fits_text("2024-03", LEAFLET));
		assert!(title_fits_text("領収書", ""));
		assert!(title_fits_text("領収書", "12,340 / 2024"));
	}

	#[test]
	fn the_fallback_title_is_the_first_line_of_words() {
		assert_eq!(
			title_from_text(
				"--- 12/34 ---\nhttps://example.org/a\n# Journée de reboisement !\nSamedi"
			)
			.as_deref(),
			Some("Journée de reboisement")
		);
		assert_eq!(
			title_from_text("TEL 03-1234-5678\n領収書\n合計 1,200円 ありがとうございました")
				.as_deref(),
			Some("領収書")
		);
		let long =
			"Avis aux habitants du quartier concernant les travaux de voirie prévus au printemps";
		assert_eq!(
			title_from_text(long).as_deref(),
			Some("Avis aux habitants du quartier concernant les travaux de")
		);
		assert_eq!(title_from_text("12 34\n---"), None);
	}

	#[test]
	fn tags_are_topics_or_printed_words() {
		let grounds = TagGrounds::new(LEAFLET);
		for tag in [
			"event",
			"Travel",
			"health insurance",
			"reboisement",
			"mont venus",
			"Port Celeste",
			"journee",
		] {
			assert!(grounds.holds(tag), "{tag}");
		}
		for tag in ["france", "paris", "new caledonia", "bois", "", "-"] {
			assert!(!grounds.holds(tag), "{tag}");
		}

		let japanese = TagGrounds::new("東京電力エナジーパートナー 電気料金 ＳＵＩＣＡ");
		assert!(japanese.holds("東京電力"));
		assert!(japanese.holds("suica"));
		assert!(!japanese.holds("関西電力"));
		assert!(!japanese.holds("sui"));
	}
}
