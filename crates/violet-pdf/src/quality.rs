//! Whether a page's text layer can be trusted or is better read again by OCR. Scanners that
//! OCR a Japanese page as if it were Latin leave a layer like `w+hwlnffi6r]xffioHR`: symbols
//! inside words, case flipping mid-word, digits between letters, words without vowels.

/// What a page of text is made of, in characters (whitespace excluded).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LayerScore {
	pub chars: usize,
	/// Replacement characters, controls and private-use code points.
	pub unusable: usize,
	/// Characters of the words that were judged (not numbers, not single letters).
	pub judged: usize,
	/// Characters of the judged words that do not look like words.
	pub junk: usize,
	pub latin: usize,
	pub vowels: usize,
	pub cjk: usize,
}

const MAX_UNUSABLE: f64 = 0.02;
const MIN_JUDGED: usize = 24;
const MAX_JUNK: f64 = 0.2;
/// A page in a CJK document without CJK text of its own: an English page of a Japanese
/// contract is fine, Latin garbage where Japanese should be is not.
const MAX_JUNK_FOREIGN: f64 = 0.08;
const MIN_LATIN_FOR_VOWELS: usize = 40;
const MIN_VOWELS: f64 = 0.28;
const MIN_VOWELS_FOREIGN: f64 = 0.33;
const CJK_PAGE: f64 = 0.3;
const MIN_CJK_PAGE_CHARS: usize = 20;
const LATIN_PAGE: f64 = 0.05;
const CJK_LANGUAGES: [&str; 5] = ["ja", "jp", "zh", "ko", "kr"];
const MIN_VOWELLESS: usize = 4;
const MAX_CASE_FLIPS: usize = 1;
const MAX_DIGIT_RUNS: usize = 2;

impl LayerScore {
	pub fn of(text: &str) -> Self {
		let mut score = Self::default();
		for c in text.chars().filter(|c| !c.is_whitespace()) {
			score.chars += 1;
			if is_unusable(c) {
				score.unusable += 1;
			} else if is_cjk(c) {
				score.cjk += 1;
			} else if c.is_alphabetic() {
				score.latin += 1;
				if is_vowel(c) {
					score.vowels += 1;
				}
			}
		}
		for token in text.split_whitespace() {
			let word = token.trim_matches(|c: char| !c.is_alphanumeric());
			match judge(word) {
				Judged::Skipped => {}
				Judged::Word => score.judged += word.chars().count(),
				Judged::Junk => {
					let length = word.chars().count();
					score.judged += length;
					score.junk += length;
				}
			}
		}
		score
	}

	fn share(part: usize, whole: usize) -> f64 {
		if whole == 0 {
			0.0
		} else {
			part as f64 / whole as f64
		}
	}

	pub fn junk_share(&self) -> f64 {
		Self::share(self.junk, self.judged)
	}

	pub fn vowel_share(&self) -> f64 {
		Self::share(self.vowels, self.latin)
	}

	pub fn cjk_share(&self) -> f64 {
		Self::share(self.cjk, self.cjk + self.latin)
	}

	fn is_cjk_page(&self) -> bool {
		self.cjk >= MIN_CJK_PAGE_CHARS && self.cjk_share() >= CJK_PAGE
	}

	/// Whether the text reads as text. `cjk_document` asks for more of a page with
	/// next to no CJK text of its own.
	pub fn trusted(&self, cjk_document: bool) -> bool {
		if Self::share(self.unusable, self.chars) > MAX_UNUSABLE {
			return false;
		}
		let foreign = cjk_document && self.cjk_share() < LATIN_PAGE;
		let (max_junk, min_vowels) = if foreign {
			(MAX_JUNK_FOREIGN, MIN_VOWELS_FOREIGN)
		} else {
			(MAX_JUNK, MIN_VOWELS)
		};
		if self.judged >= MIN_JUDGED && self.junk_share() > max_junk {
			return false;
		}
		self.latin < MIN_LATIN_FOR_VOWELS || self.vowel_share() >= min_vowels
	}
}

/// Which pages of a text layer to keep; the others need OCR. A page without a layer is
/// never trusted. `language` is the document's language when one is known (`ja`, `fr`, …);
/// otherwise a document with a page mostly in CJK script is taken for a CJK document.
pub fn trusted_layers(layers: &[Option<String>], language: Option<&str>) -> Vec<bool> {
	let scores: Vec<Option<LayerScore>> = layers
		.iter()
		.map(|layer| layer.as_deref().map(LayerScore::of))
		.collect();
	let cjk_document = match language {
		Some(language) => {
			let language = language.trim().to_ascii_lowercase();
			CJK_LANGUAGES
				.iter()
				.any(|code| language == *code || language.starts_with(&format!("{code}-")))
		}
		None => scores.iter().flatten().any(LayerScore::is_cjk_page),
	};
	scores
		.iter()
		.map(|score| score.is_some_and(|score| score.trusted(cjk_document)))
		.collect()
}

enum Judged {
	Skipped,
	Word,
	Junk,
}

/// Inside a word these join its parts (`l'avis`, `e-mail`, `S.A.`, `a@b.fr`, `x_1`).
const JOINERS: [char; 10] = ['\'', '’', '-', '.', '/', '&', '@', '_', '°', 'º'];
/// Between digits these are part of a number (`1,5`, `12:30`, `3+1`).
const NUMERIC_JOINERS: [char; 4] = [',', ':', '+', '%'];

fn judge(word: &str) -> Judged {
	let chars: Vec<char> = word.chars().collect();
	let letters = chars.iter().filter(|c| c.is_alphabetic()).count();
	if letters == 0 || (letters == 1 && chars.len() == 1) {
		return Judged::Skipped;
	}
	if chars.iter().any(|&c| is_cjk(c)) {
		return Judged::Word;
	}
	if is_machine_readable_zone(&chars) {
		return Judged::Skipped;
	}
	for (index, &c) in chars.iter().enumerate() {
		if c.is_alphanumeric() || JOINERS.contains(&c) {
			continue;
		}
		let between_digits = index > 0
			&& chars[index - 1].is_ascii_digit()
			&& chars.get(index + 1).is_some_and(char::is_ascii_digit);
		if !(between_digits && NUMERIC_JOINERS.contains(&c)) {
			return Judged::Junk;
		}
	}
	let mut flips = 0;
	let mut digit_runs = 0;
	let mut previous: Option<char> = None;
	for &c in &chars {
		if let Some(previous) = previous {
			if previous.is_lowercase() && c.is_uppercase() {
				flips += 1;
			}
			if c.is_ascii_digit() && !previous.is_ascii_digit() {
				digit_runs += 1;
			}
		} else if c.is_ascii_digit() {
			digit_runs += 1;
		}
		previous = Some(c);
	}
	let all_upper = chars.iter().all(|c| !c.is_lowercase());
	let vowelless = letters >= MIN_VOWELLESS && !all_upper && !chars.iter().any(|&c| is_vowel(c));
	if flips > MAX_CASE_FLIPS || vowelless || digit_runs > MAX_DIGIT_RUNS {
		Judged::Junk
	} else {
		Judged::Word
	}
}

/// A passport or ID card line such as `P<FRAMARTIN<<JEAN<<<<`.
fn is_machine_readable_zone(chars: &[char]) -> bool {
	chars.windows(2).any(|pair| pair == ['<', '<'])
		&& chars
			.iter()
			.all(|&c| c == '<' || c.is_ascii_uppercase() || c.is_ascii_digit())
}

fn is_vowel(c: char) -> bool {
	let base = c.to_lowercase().next().unwrap_or(c);
	"aeiouyàâäáãåæéèêëíìîïóòôöõøœúùûüýÿ".contains(base)
}

fn is_unusable(c: char) -> bool {
	c == char::REPLACEMENT_CHARACTER
		|| c.is_control()
		|| matches!(c as u32, 0xE000..=0xF8FF | 0xF_0000..=0x10_FFFF)
}

fn is_cjk(c: char) -> bool {
	matches!(c as u32, 0x3040..=0x30FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xAC00..=0xD7AF | 0xF900..=0xFAFF | 0xFF66..=0xFF9F | 0x2_0000..=0x3_FFFF)
}

#[cfg(test)]
mod tests {
	use super::*;

	const FRENCH_INVOICE: &str = "FACTURE N° 2024-0315\nEDF Entreprises — Service Client\n\
		Date d'émission : 15/03/2024\nRéférence client : 6012 3456 789\n\
		Votre consommation d'électricité du 01/02/2024 au 29/02/2024\n\
		Abonnement 12,34 € HT\nConsommation 412 kWh 98,76 € HT\nTVA 20 % 22,22 €\n\
		Total TTC à payer avant le 30/03/2024 : 133,32 €\n\
		Prélèvement sur le compte FR76 3000 4000 0312 3456 7890 143\n\
		Pour toute question, contactez-nous au 09 69 32 15 15 ou sur edf.fr/contact.";
	const FRENCH_TABLE: &str = "Brochure tarifaire au 1er janvier 2024\n\
		Carte Visa Classic 45,00 € / an\nCarte Visa Premier 134,00 € / an\n\
		Retrait DAB hors zone euro 3,50 € + 2,9 %\nVirement SEPA 0,00 €\n\
		Commission d'intervention 8,00 € / opération (max. 80 € / mois)\n\
		Frais de tenue de compte 2,00 € / mois\nOpposition chèque 15,00 €";
	const ENGLISH_LETTER: &str = "Dear Mr Smith,\n\
		Thank you for your order #A-10293 placed on 12 March 2024. Your parcel was shipped \
		by DHL Express (tracking 1Z999AA10123456784) and should arrive within 3-5 business days.\n\
		If you have any questions, email support@example.com or call +44 20 7946 0958.\n\
		Kind regards,\nCustomer Service, Example Ltd., 221B Baker Street, London NW1 6XE";
	const PASSPORT: &str = "RÉPUBLIQUE FRANÇAISE\nPASSEPORT\nType P Code FRA\n\
		Nom MARTIN\nPrénoms Jean Pierre\nNationalité Française\nDate de naissance 01 02 1980\n\
		P<FRAMARTIN<<JEAN<PIERRE<<<<<<<<<<<<<<<<<<<<\n12AB345678FRA8002014M3001017<<<<<<<<<<<<<<02";
	const JAPANESE: &str = "請求書\n株式会社サンプル 御中\n東京都渋谷区神南1-2-3\n\
		ご請求金額 ¥12,345（税込）\nお支払期限 2024年3月31日\n電気料金のお知らせ ご使用量 412kWh";
	const JAPANESE_WITH_ENGLISH: &str = "ご契約内容のお知らせ\nau Internet サービス\n\
		契約者 山田 太郎 様\nPlan: au Hikari Home 1G\nお問い合わせ 0120-123-456";
	const ENGLISH_TERMS: &str = "Terms of Service\n\
		These terms govern the use of the internet service provided under the contract. \
		The subscriber agrees to pay the monthly fee and to use the service in accordance \
		with applicable laws. The provider may suspend the service in case of late payment.";
	const GARBAGE_SYMBOLS: &str = "w+hwlnffi6r]xffioHR\nH oa-oo ffifr H Y6+tr+F zOzt.tt.Ot#ltl\n\
		12 E 28 Ht C/: il'tfrF:frffitt;EFtt Ltf\nIijrljilti:tirtq / ):r\n\
		Llrl{R ,iEffi' IE*6EI +tA 1TE 14-5 t\\^l-7)vLSrb\nFft T'J+# M E[ EE €# ?T\"t' _ CZ (C ? U\\ T\n\
		ili4AR7t,qffiffiffiH1f+ft I TE I 4# 5 6\nilr4gRtEfrZf;ffiElEa 1 TE 1 4 - 5";
	const GARBAGE_QUIET: &str = "O nool 1 8-ooz-oo2 a\niEtfiffi NIPPON EHEEIIF\n\
		Tlll lrtfrt ffifr Hffix tvlt Ltf jrljilt\nfrfrtr Wfifr ffiElEa rtfr lltt\n\
		EH tfrrt fiFFr llrl Ht pos iv tv Ltf ffi";

	fn trusted(text: &str, cjk_document: bool) -> bool {
		LayerScore::of(text).trusted(cjk_document)
	}

	#[test]
	fn real_text_is_trusted() {
		for text in [
			FRENCH_INVOICE,
			FRENCH_TABLE,
			ENGLISH_LETTER,
			PASSPORT,
			JAPANESE,
			JAPANESE_WITH_ENGLISH,
			ENGLISH_TERMS,
		] {
			let score = LayerScore::of(text);
			assert!(score.trusted(false), "{score:?}\n{text}");
		}
		for text in [JAPANESE, JAPANESE_WITH_ENGLISH, ENGLISH_TERMS] {
			assert!(trusted(text, true), "{text}");
		}
	}

	#[test]
	fn garbage_is_not_trusted() {
		for text in [GARBAGE_SYMBOLS, GARBAGE_QUIET] {
			let score = LayerScore::of(text);
			assert!(!score.trusted(false), "{score:?}\n{text}");
			assert!(!score.trusted(true), "{score:?}\n{text}");
		}
	}

	#[test]
	fn replacement_characters_are_not_trusted() {
		let text = format!(
			"{FRENCH_INVOICE} \u{FFFD}\u{FFFD}\u{FFFD}\u{FFFD}\u{FFFD}\u{FFFD}\u{FFFD}\u{FFFD}"
		);
		assert!(!trusted(&text, false));
		assert!(trusted(&format!("{FRENCH_INVOICE} \u{FFFD}"), false));
	}

	#[test]
	fn short_pages_are_trusted_unless_unusable() {
		assert!(trusted("Page 2 / 3", false));
		assert!(trusted("Annexe B", true));
		assert!(!trusted("\u{FFFD}\u{FFFD}", false));
	}

	#[test]
	fn latin_pages_of_a_cjk_document_must_read_well() {
		let borderline = "Service contract tvlt for the subscriber Hffix rtfr \
			the provider agrees lltt to deliver the service every month";
		assert!(trusted(borderline, false));
		assert!(!trusted(borderline, true));
	}

	#[test]
	fn the_document_language_or_a_cjk_page_makes_a_cjk_document() {
		let borderline = "Service contract tvlt for the subscriber Hffix rtfr \
			the provider agrees lltt to deliver the service every month";
		let layers = [
			Some(JAPANESE.to_string()),
			Some(borderline.to_string()),
			None,
			Some(GARBAGE_SYMBOLS.to_string()),
		];
		assert_eq!(trusted_layers(&layers, None), [true, false, false, false]);
		assert_eq!(
			trusted_layers(&layers, Some("fr")),
			[true, true, false, false]
		);
		let latin = [
			Some(borderline.to_string()),
			Some(FRENCH_INVOICE.to_string()),
		];
		assert_eq!(trusted_layers(&latin, None), [true, true]);
		assert_eq!(trusted_layers(&latin, Some("ja")), [false, true]);
		assert_eq!(trusted_layers(&latin, Some("JA-jp")), [false, true]);
	}

	#[test]
	fn words_are_judged_by_their_shape() {
		for word in [
			"l'avis",
			"e-mail",
			"S.A.",
			"support@example.com",
			"iPhone",
			"McDonald",
			"12h30",
			"A4",
			"N°6",
			"kWh",
			"412kWh",
			"TTC",
			"SNCF",
			"1er",
			"3ème",
			"2024-0315",
			"1,5",
			"12:30",
			"Größe",
		] {
			assert!(
				matches!(judge(word), Judged::Word | Judged::Skipped),
				"{word}"
			);
		}
		for word in [
			"w+hwlnffi6r]xffioHR",
			"EfrZf;ffiElEa",
			"Llrl",
			"ilr4gRtEfrZf",
			"zOzt.tt.Ot#ltl",
			"GrOuPe",
			"a,b",
		] {
			assert!(matches!(judge(word), Judged::Junk), "{word}");
		}
	}
}
