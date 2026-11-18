use crate::text::{fold_accents, fold_name, fold_width};

const ARTICLES: [&str; 5] = ["la ", "le ", "les ", "l'", "the "];
const BRANCH_SEPARATORS: [&str; 3] = [" - ", " – ", " — "];
const LEGAL_FORMS: [&str; 6] = ["株式会社", "(株)", "（株）", "㈱", "有限会社", "合同会社"];
const LEGAL_SUFFIXES: [&str; 26] = [
	"sa",
	"sas",
	"sasu",
	"sarl",
	"eurl",
	"sci",
	"snc",
	"scp",
	"inc",
	"incorporated",
	"ltd",
	"limited",
	"llc",
	"plc",
	"gmbh",
	"ag",
	"bv",
	"nv",
	"co",
	"corp",
	"corporation",
	"company",
	"kk",
	"ab",
	"spa",
	"srl",
];
const ACRONYM_SKIPPED: [&str; 12] = [
	"de", "du", "des", "d", "la", "le", "les", "l", "et", "of", "the", "and",
];
const MIN_ACRONYM: usize = 2;

/// Comparison key for an issuer: case, width, accents, punctuation, a leading article, a
/// legal form (`SA`, `Inc.`, `株式会社`) and a branch after a spaced dash do not matter.
/// `Société Générale Calédonienne de Banque`, `SOCIETE GENERALE CALEDONIENNE DE BANQUE` and
/// `La Société Générale Calédonienne de Banque - Nouméa` share one key.
pub fn issuer_key(name: &str) -> String {
	let words = key_words(name);
	if words.is_empty() {
		return fold_name(name);
	}
	words.concat()
}

fn key_words(name: &str) -> Vec<String> {
	let mut text = fold_accents(&fold_width(name).replace('’', "'"));
	for form in LEGAL_FORMS {
		text = text.replace(form, " ");
	}
	let mut text = text.trim();
	if let Some(head) = BRANCH_SEPARATORS
		.iter()
		.find_map(|separator| text.split_once(separator).map(|(head, _)| head.trim()))
		.filter(|head| !head.is_empty())
	{
		text = head;
	}
	for article in ARTICLES {
		if let Some(rest) = text.strip_prefix(article) {
			text = rest.trim_start();
			break;
		}
	}
	let spaced: String = text
		.replace('.', "")
		.chars()
		.map(|c| if c.is_alphanumeric() { c } else { ' ' })
		.collect();
	let mut words: Vec<String> = spaced.split_whitespace().map(str::to_string).collect();
	while words.len() > 1
		&& words
			.last()
			.is_some_and(|last| LEGAL_SUFFIXES.contains(&last.as_str()))
	{
		words.pop();
	}
	words
}

/// The known issuer with the same [`issuer_key`], the first one listed when several are.
pub(crate) fn canonical_issuer(name: &str, known: &[String]) -> Option<String> {
	let key = issuer_key(name);
	if key.is_empty() {
		return None;
	}
	known
		.iter()
		.find(|candidate| issuer_key(candidate) == key)
		.cloned()
}

/// Known issuers without the other spellings of one already listed.
pub(crate) fn distinct_issuers(known: &[String]) -> Vec<String> {
	let mut keys = std::collections::HashSet::new();
	known
		.iter()
		.filter(|issuer| keys.insert(issuer_key(issuer)))
		.cloned()
		.collect()
}

/// Whether the initials of `name` (`SGCB` for `Société Générale Calédonienne de Banque`) are
/// printed in the text as a word of capitals.
pub(crate) fn acronym_printed(name: &str, text: &str) -> bool {
	let words = key_words(name);
	if words.iter().any(|word| !word.is_ascii()) {
		return false;
	}
	let acronym: String = words
		.iter()
		.filter(|word| !ACRONYM_SKIPPED.contains(&word.as_str()))
		.filter_map(|word| word.chars().next())
		.collect::<String>()
		.to_ascii_uppercase();
	acronym.len() >= MIN_ACRONYM
		&& fold_width(text)
			.split(|c: char| !c.is_alphanumeric())
			.any(|word| word == acronym)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn spellings_of_one_issuer_share_a_key() {
		let key = issuer_key("SOCIETE GENERALE CALEDONIENNE DE BANQUE");
		for spelling in [
			"Société Générale Calédonienne de Banque",
			"La SOCIETE GENERALE CALEDONIENNE DE BANQUE",
			"Société Générale Calédonienne de Banque – NOUMEA",
			"SOCIETE GENERALE CALEDONIENNE DE BANQUE S.A.",
			"Societe  Generale, Caledonienne de Banque",
		] {
			assert_eq!(issuer_key(spelling), key, "{spelling}");
		}
		assert_eq!(issuer_key("株式会社サンプル"), issuer_key("サンプル"));
		assert_eq!(issuer_key("(株)サンプル"), issuer_key("サンプル"));
		assert_eq!(issuer_key("Apple Japan, Inc."), issuer_key("APPLE JAPAN"));
		assert_eq!(issuer_key("L'Oréal"), issuer_key("Oréal"));
		assert_eq!(issuer_key("Coca-Cola"), issuer_key("coca cola"));
		assert_eq!(issuer_key("SA"), "sa");
	}

	#[test]
	fn different_issuers_keep_different_keys() {
		for (a, b) in [
			("SGCB", "SOCIETE GENERALE CALEDONIENNE DE BANQUE"),
			(
				"S.G. CALEDONIENNE DE BANQUE",
				"Société Générale Calédonienne de Banque",
			),
			("Banque de France", "Banque Populaire"),
			("EDF", "GDF"),
			("NTTドコモ", "NTT東日本"),
		] {
			assert_ne!(issuer_key(a), issuer_key(b), "{a} / {b}");
		}
	}

	#[test]
	fn the_most_used_known_spelling_wins() {
		let known = [
			"SOCIETE GENERALE CALEDONIENNE DE BANQUE".to_string(),
			"SGCB".to_string(),
			"Société Générale Calédonienne de Banque".to_string(),
		];
		assert_eq!(
			canonical_issuer("Société Générale Calédonienne de Banque", &known).as_deref(),
			Some("SOCIETE GENERALE CALEDONIENNE DE BANQUE")
		);
		assert_eq!(canonical_issuer("SGCB", &known).as_deref(), Some("SGCB"));
		assert_eq!(canonical_issuer("BNP Paribas", &known), None);
		assert_eq!(
			distinct_issuers(&known),
			["SOCIETE GENERALE CALEDONIENNE DE BANQUE", "SGCB"]
		);
	}

	#[test]
	fn acronyms_are_found_as_capital_words() {
		let name = "Société Générale Calédonienne de Banque";
		assert!(acronym_printed(name, "SGCB - Agence de Nouméa\nRelevé"));
		assert!(acronym_printed(name, "(SGCB)"));
		assert!(!acronym_printed(name, "sgcb"));
		assert!(!acronym_printed(name, "SGCBX"));
		assert!(!acronym_printed(name, "Société Générale"));
		assert!(!acronym_printed("株式会社サンプル", "サ"));
		assert!(!acronym_printed("Orange", "O"));
	}
}
