//! Near-duplicates: two documents that are the same file, or look like copies of one another.
//! They are only flagged for review; nothing is merged or deleted on its own.

use std::collections::HashSet;

use crate::Sidecar;

/// Character similarity at or above which two texts with the same numbers are copies.
const SIMILARITY: f64 = 0.95;
/// Longer texts are compared only when equal.
const MAX_COMPARED_CHARS: usize = 30_000;

/// The fields of a sidecar a candidate must share, or none when it cannot be compared.
pub(crate) fn group_key(sidecar: &Sidecar) -> Option<(String, String, String)> {
	let issuer = sidecar.issuer.as_deref()?.trim().to_lowercase();
	let date = sidecar.date.as_deref()?.trim().to_string();
	if issuer.is_empty() || date.is_empty() {
		return None;
	}
	Some((issuer, date, sidecar.doc_type.trim().to_lowercase()))
}

/// Whether `a` and `b` are near-duplicates. Parts of one scan and pairs a user dismissed are
/// not. Others are when they are the same file whatever the metadata says, or have the same
/// issuer, date and type, page counts at most one apart, and the same text, or nearly the same
/// text with exactly the same numbers in it.
pub fn near_duplicates(a: &Sidecar, b: &Sidecar) -> bool {
	if a.id == b.id || a.not_duplicates.contains(&b.id) || b.not_duplicates.contains(&a.id) {
		return false;
	}
	let sources: HashSet<&str> = a.sources.iter().map(|s| s.sha256.as_str()).collect();
	if b.sources
		.iter()
		.any(|s| sources.contains(s.sha256.as_str()))
	{
		return false;
	}
	if a.sha256 == b.sha256 {
		return true;
	}
	match (group_key(a), group_key(b)) {
		(Some(ka), Some(kb)) if ka == kb => {}
		_ => return false,
	}
	if a.pages.len().abs_diff(b.pages.len()) > 1 {
		return false;
	}
	let (ta, tb) = (full_text(a), full_text(b));
	if ta.is_empty() || tb.is_empty() {
		return false;
	}
	if ta == tb {
		return true;
	}
	if numbers(&ta) != numbers(&tb) {
		return false;
	}
	similar(&ta, &tb)
}

/// Every page joined, with runs of whitespace as one space.
fn full_text(sidecar: &Sidecar) -> Vec<char> {
	let mut out = Vec::new();
	for page in &sidecar.pages {
		for word in page.text.split_whitespace() {
			if !out.is_empty() {
				out.push(' ');
			}
			out.extend(word.chars());
		}
	}
	out
}

fn digit(c: char) -> Option<char> {
	match c {
		'0'..='9' => Some(c),
		'０'..='９' => char::from_u32(u32::from(c) - u32::from('０') + u32::from('0')),
		_ => None,
	}
}

/// The numbers in a text, sorted: runs of digits joined by `.`, `,` or `/`.
fn numbers(text: &[char]) -> Vec<String> {
	let mut found = Vec::new();
	let mut i = 0;
	while i < text.len() {
		if digit(text[i]).is_none() {
			i += 1;
			continue;
		}
		let mut number = String::new();
		while i < text.len() {
			if let Some(d) = digit(text[i]) {
				number.push(d);
				i += 1;
			} else if matches!(text[i], '.' | ',' | '/')
				&& text.get(i + 1).and_then(|c| digit(*c)).is_some()
			{
				number.push(text[i]);
				i += 1;
			} else {
				break;
			}
		}
		found.push(number);
	}
	found.sort_unstable();
	found
}

/// Whether the Levenshtein distance is at most `1 - SIMILARITY` of the longer text, computed
/// only in the band of diagonals that bound allows.
fn similar(a: &[char], b: &[char]) -> bool {
	let longest = a.len().max(b.len());
	if longest > MAX_COMPARED_CHARS {
		return false;
	}
	let bound = ((1.0 - SIMILARITY) * longest as f64).floor() as usize;
	if a.len().abs_diff(b.len()) > bound {
		return false;
	}
	distance_within(a, b, bound).is_some()
}

/// The edit distance between `a` and `b` when it is at most `bound`.
fn distance_within(a: &[char], b: &[char], bound: usize) -> Option<usize> {
	if a.len().abs_diff(b.len()) > bound {
		return None;
	}
	let over = bound + 1;
	let mut previous: Vec<usize> = (0..=b.len()).map(|j| j.min(over)).collect();
	let mut current = vec![over; b.len() + 1];
	for i in 1..=a.len() {
		let from = i.saturating_sub(bound).max(1);
		let to = (i + bound).min(b.len());
		current[0] = i.min(over);
		if from > 1 {
			current[from - 1] = over;
		}
		if to < b.len() {
			current[to + 1] = over;
		}
		let mut best = current[from - 1];
		for j in from..=to {
			let substitute = previous[j - 1] + usize::from(a[i - 1] != b[j - 1]);
			let value = substitute
				.min(previous[j] + 1)
				.min(current[j - 1] + 1)
				.min(over);
			current[j] = value;
			best = best.min(value);
		}
		if best > bound {
			return None;
		}
		std::mem::swap(&mut previous, &mut current);
	}
	let distance = previous[b.len()];
	(distance <= bound).then_some(distance)
}

#[cfg(test)]
mod tests {
	use uuid::Uuid;

	use super::*;
	use crate::testutil::sidecar;
	use crate::{PageText, Source, TextSource};

	fn doc(name: &str, pages: &[&str]) -> Sidecar {
		let mut s = sidecar(name);
		s.issuer = Some("SCI Malical".into());
		s.date = Some("2026-07-01".into());
		s.doc_type = "invoice".into();
		s.sha256 = crate::hash::sha256(name.as_bytes());
		s.sources = vec![Source {
			sha256: crate::hash::sha256(format!("source {name}").as_bytes()),
			original_name: format!("{name}.pdf"),
			pages: Vec::new(),
			mail: None,
			connector: None,
		}];
		s.pages = pages
			.iter()
			.enumerate()
			.map(|(i, text)| PageText {
				number: i as u32 + 1,
				text: text.to_string(),
				source: TextSource::Layer,
			})
			.collect();
		s
	}

	const INVOICE: &str = "SCI MALICAL FACTURE N° 07 2026\nLoyer du mois de juillet 2026 \
		pour le logement situé 12 rue des Lilas, 75011 Paris.\nMontant hors charges : 1.250,00 EUR\n\
		Charges : 150,00 EUR\nTotal à payer : 1.400,00 EUR\nÀ régler avant le 05/07/2026 par \
		virement sur le compte habituel. Merci de rappeler la référence de la facture.";

	fn chars(text: &str) -> Vec<char> {
		text.chars().collect()
	}

	#[test]
	fn copies_with_other_bytes_are_flagged() {
		let a = doc("facture", &[INVOICE]);
		let b = doc("facture (2)", &[&INVOICE.replace('\n', "  \n ")]);
		assert!(near_duplicates(&a, &b));
		assert!(near_duplicates(&b, &a));
	}

	#[test]
	fn a_few_ocr_errors_with_the_same_numbers_are_flagged() {
		let a = doc("facture", &[INVOICE]);
		let b = doc(
			"facture scan",
			&[&INVOICE
				.replace("Lilas", "Li1as")
				.replace("habituel", "habitue1")],
		);
		assert!(
			!near_duplicates(&a, &b),
			"OCR turning l into 1 changes the numbers"
		);
		let c = doc(
			"facture scan",
			&[&INVOICE
				.replace("Lilas", "Lilac")
				.replace("habituel", "habitual")],
		);
		assert!(near_duplicates(&a, &c));
	}

	#[test]
	fn a_template_with_another_period_or_amount_is_not_flagged() {
		let july = doc("juillet", &[INVOICE]);
		let august = doc(
			"aout",
			&[&INVOICE
				.replace("07 2026", "08 2026")
				.replace("05/07/2026", "05/08/2026")],
		);
		assert!(!near_duplicates(&july, &august));
		let raised = doc("hausse", &[&INVOICE.replace("1.400,00", "1.450,00")]);
		assert!(!near_duplicates(&july, &raised));
	}

	#[test]
	fn parts_of_one_scan_are_not_flagged() {
		let a = doc("part 1", &[INVOICE]);
		let mut b = doc("part 2", &[INVOICE, ""]);
		b.sources = a.sources.clone();
		assert!(!near_duplicates(&a, &b));
	}

	#[test]
	fn issuer_date_and_type_must_be_known_and_equal() {
		let a = doc("a", &[INVOICE]);
		let mut b = doc("b", &[INVOICE]);
		b.issuer = Some("  sci malical ".into());
		b.doc_type = "Invoice".into();
		assert!(near_duplicates(&a, &b));
		let changes: [fn(&mut Sidecar); 5] = [
			|s: &mut Sidecar| s.issuer = None,
			|s: &mut Sidecar| s.date = None,
			|s: &mut Sidecar| s.issuer = Some("Other".into()),
			|s: &mut Sidecar| s.date = Some("2026-07-02".into()),
			|s: &mut Sidecar| s.doc_type = "receipt".into(),
		];
		for change in changes {
			let mut c = b.clone();
			change(&mut c);
			assert!(!near_duplicates(&a, &c), "{c:?}");
		}
		let (mut x, mut y) = (a.clone(), b.clone());
		x.issuer = None;
		y.issuer = None;
		assert!(!near_duplicates(&x, &y));
	}

	#[test]
	fn page_counts_may_differ_by_one() {
		let a = doc("a", &[INVOICE]);
		assert!(near_duplicates(&a, &doc("b", &[INVOICE, ""])));
		assert!(!near_duplicates(&a, &doc("c", &[INVOICE, "", ""])));
	}

	#[test]
	fn the_same_file_is_flagged_whatever_its_metadata() {
		let a = doc("a", &[INVOICE]);
		let mut same = doc("b", &[""]);
		same.sha256.clone_from(&a.sha256);
		same.issuer = Some("SOCIETE GENERALE".into());
		same.date = None;
		same.doc_type = "statement".into();
		assert!(near_duplicates(&a, &same));
		assert!(near_duplicates(&same, &a));
		assert!(!near_duplicates(&a, &a));
		let mut part = same.clone();
		part.sources = a.sources.clone();
		assert!(!near_duplicates(&a, &part), "parts of one scan");
		same.not_duplicates = vec![a.id];
		assert!(!near_duplicates(&a, &same));
		assert!(!near_duplicates(&same, &a));
	}

	#[test]
	fn empty_text_and_dismissed_pairs_are_not_flagged() {
		let a = doc("a", &[INVOICE]);
		assert!(!near_duplicates(&doc("c", &[""]), &doc("d", &[" "])));
		let mut b = doc("b", &[INVOICE]);
		b.not_duplicates = vec![Uuid::new_v4(), a.id];
		assert!(!near_duplicates(&a, &b));
		assert!(!near_duplicates(&b, &a));
	}

	#[test]
	fn numbers_are_runs_of_digits_and_separators() {
		assert_eq!(
			numbers(&chars("1.250,00 EUR le 05/07/2026, réf. ２０２６ et 3.")),
			["05/07/2026", "1.250,00", "2026", "3"]
		);
	}

	#[test]
	fn distance_is_bounded() {
		assert_eq!(
			distance_within(&chars("kitten"), &chars("sitting"), 3),
			Some(3)
		);
		assert_eq!(
			distance_within(&chars("kitten"), &chars("sitting"), 2),
			None
		);
		assert_eq!(distance_within(&chars(""), &chars("ab"), 2), Some(2));
		assert_eq!(distance_within(&chars("abc"), &chars("abc"), 0), Some(0));
		let long: Vec<char> = "x".repeat(MAX_COMPARED_CHARS + 1).chars().collect();
		assert!(!similar(&long, &long));
	}
}
