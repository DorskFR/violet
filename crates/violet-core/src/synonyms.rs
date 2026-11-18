//! Household words that mean the same thing in French, English and Japanese, so a search for
//! one finds documents that use another.

use crate::index::{fold, tokens};

const GROUPS: &[&[&str]] = &[
	&[
		"fiche de paie",
		"bulletin de paie",
		"bulletin de salaire",
		"payslip",
		"pay slip",
		"給与明細",
	],
	&["voiture", "auto", "automobile", "car", "自動車"],
	&["relevé", "statement"],
	&["facture", "invoice", "請求書"],
	&["impôt", "tax", "taxe", "税"],
	&["assurance", "insurance", "保険"],
	&["électricité", "electricity", "電気"],
	&["loyer", "rent", "家賃"],
];
/// The longest entry, in words.
const LONGEST: usize = 3;

/// The folded words of a text.
fn words(text: &str) -> Vec<Vec<char>> {
	let folded = fold(text);
	tokens(&folded)
		.into_iter()
		.map(|(start, end)| folded[start..end].to_vec())
		.collect()
}

/// `typed` is `entry`, or its plural with `s` or `x`.
fn same_word(typed: &[char], entry: &[char]) -> bool {
	typed == entry
		|| typed.len() == entry.len() + 1
			&& typed.starts_with(entry)
			&& matches!(typed.last(), Some('s' | 'x'))
}

/// The entries meaning the same as the first words of `words` (whitespace-separated, as typed),
/// with how many of those words they cover. The longest entry wins.
pub(crate) fn lookup(typed: &[&str]) -> Option<(usize, &'static [&'static str])> {
	let typed: Vec<Vec<Vec<char>>> = typed.iter().take(LONGEST).map(|word| words(word)).collect();
	let mut best: Option<(usize, &'static [&'static str])> = None;
	for group in GROUPS {
		for entry in *group {
			let entry = words(entry);
			let len = entry.len();
			if len > typed.len() || best.is_some_and(|(longest, _)| longest >= len) {
				continue;
			}
			let fits = typed[..len]
				.iter()
				.zip(&entry)
				.all(|(word, wanted)| word.len() == 1 && same_word(&word[0], wanted));
			if fits {
				best = Some((len, group));
			}
		}
	}
	best
}

#[cfg(test)]
mod tests {
	use super::*;

	fn found(query: &str) -> Option<(usize, Vec<&'static str>)> {
		let typed: Vec<&str> = query.split_whitespace().collect();
		lookup(&typed).map(|(len, group)| (len, group.to_vec()))
	}

	#[test]
	fn entries_match_in_any_case_with_or_without_accents_and_in_the_plural() {
		for query in ["facture", "Factures", "INVOICE", "invoices", "請求書"] {
			assert_eq!(
				found(query).unwrap().1,
				["facture", "invoice", "請求書"],
				"{query}"
			);
		}
		for query in ["impots", "Impôts", "tax", "税"] {
			assert_eq!(found(query).unwrap().1[0], "impôt", "{query}");
		}
		for query in ["electricite", "Électricité", "電気"] {
			assert_eq!(found(query).unwrap().1[0], "électricité", "{query}");
		}
		for query in ["releve", "Relevés", "statements"] {
			assert_eq!(found(query).unwrap().1, ["relevé", "statement"], "{query}");
		}
		for query in ["loyer", "rent", "家賃", "assurance", "insurance", "保険"] {
			assert!(found(query).is_some(), "{query}");
		}
	}

	#[test]
	fn phrases_cover_their_words_and_the_longest_wins() {
		let payslip = |query| found(query).map(|(len, group)| (len, group[0]));
		assert_eq!(payslip("fiche de paie mars"), Some((3, "fiche de paie")));
		assert_eq!(payslip("bulletin de paie"), Some((3, "fiche de paie")));
		assert_eq!(payslip("Bulletins de salaire"), Some((3, "fiche de paie")));
		assert_eq!(payslip("pay slip"), Some((2, "fiche de paie")));
		assert_eq!(payslip("payslips 2014"), Some((1, "fiche de paie")));
		assert_eq!(payslip("給与明細"), Some((1, "fiche de paie")));
		assert_eq!(payslip("voiture"), Some((1, "voiture")));
		assert_eq!(payslip("auto"), Some((1, "voiture")));
	}

	#[test]
	fn other_words_have_no_synonyms() {
		for query in [
			"fiche",
			"bulletin",
			"paie",
			"carrefour",
			"cars2",
			"de facture",
			"auto-entrepreneur",
			"",
		] {
			assert_eq!(found(query), None, "{query}");
		}
	}
}
