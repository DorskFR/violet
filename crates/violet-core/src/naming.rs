use std::path::{Path, PathBuf};

const MAX_CHARS: usize = 120;
/// Keeps a component, plus an extension and a ` (n)` suffix, under the 255-byte file name limit.
const MAX_BYTES: usize = 200;

/// Relative path under `library/` for a sidecar:
/// `<Area>/<YYYY>/<YYYY-MM-DD> <Issuer> <Title>.<ext>`; without a date the year folder is
/// `Undated` and the date prefix is dropped; without an issuer that part is dropped.
/// Components are sanitised: no `/ \ : * ? " < > |`, control chars or leading dots, whitespace
/// collapsed, at most 120 chars per component (cut on a char boundary), non-ASCII kept.
/// An empty area becomes `Unsorted`, an empty title `Untitled`, and a date that is not
/// `YYYY-MM-DD` counts as missing.
pub fn document_path(sidecar: &crate::Sidecar, ext: &str) -> PathBuf {
	let area = or_default(sanitize(&sidecar.area), "Unsorted");
	let title = or_default(sanitize(&sidecar.title), "Untitled");
	let date = sidecar
		.date
		.as_deref()
		.map(str::trim)
		.filter(|date| is_iso_date(date));
	let issuer = sidecar
		.issuer
		.as_deref()
		.map(sanitize)
		.filter(|issuer| !issuer.is_empty());

	let mut parts: Vec<&str> = Vec::with_capacity(3);
	parts.extend(date);
	parts.extend(issuer.as_deref());
	parts.push(&title);
	let mut name = sanitize(&parts.join(" "));

	let ext = sanitize(ext.trim_start_matches('.')).replace(' ', "");
	if !ext.is_empty() {
		name.push('.');
		name.push_str(&ext);
	}
	let year = date.map_or("Undated", |date| &date[..4]);
	PathBuf::from(area).join(year).join(name)
}

/// `path` if `exists(path)` is false, else `<stem> (2).<ext>`, `(3)`, ...
pub fn deconflict(path: &Path, exists: impl Fn(&Path) -> bool) -> PathBuf {
	if !exists(path) {
		return path.to_path_buf();
	}
	let name = path
		.file_name()
		.map(|name| name.to_string_lossy().into_owned())
		.unwrap_or_default();
	let (stem, ext) = match name.rfind('.') {
		Some(dot) if dot > 0 => (&name[..dot], &name[dot..]),
		_ => (name.as_str(), ""),
	};
	let mut n = 2u64;
	loop {
		let candidate = path.with_file_name(format!("{stem} ({n}){ext}"));
		if !exists(&candidate) {
			return candidate;
		}
		n += 1;
	}
}

/// One sanitised path component.
pub fn sanitize(component: &str) -> String {
	let replaced: String = component
		.chars()
		.map(|c| match c {
			'/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => ' ',
			c if c.is_control() => ' ',
			c => c,
		})
		.collect();
	let collapsed = replaced.split_whitespace().collect::<Vec<_>>().join(" ");
	let stripped = collapsed.trim_start_matches(['.', ' ']);

	let mut out = String::new();
	for (count, c) in stripped.chars().enumerate() {
		if count == MAX_CHARS || out.len() + c.len_utf8() > MAX_BYTES {
			break;
		}
		out.push(c);
	}
	out.truncate(out.trim_end().len());
	out
}

fn or_default(value: String, default: &str) -> String {
	if value.is_empty() {
		default.to_string()
	} else {
		value
	}
}

fn is_iso_date(date: &str) -> bool {
	let bytes = date.as_bytes();
	bytes.len() == 10
		&& bytes.iter().enumerate().all(|(i, b)| {
			if i == 4 || i == 7 {
				*b == b'-'
			} else {
				b.is_ascii_digit()
			}
		})
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::testutil::sidecar;

	#[test]
	fn sanitize_removes_forbidden_characters() {
		assert_eq!(sanitize(r#"a/b\c:d*e?f"g<h>i|j"#), "a b c d e f g h i j");
		assert_eq!(sanitize("tab\there\nnew\u{7}bell"), "tab here new bell");
		assert_eq!(sanitize("  many   spaces  "), "many spaces");
	}

	#[test]
	fn sanitize_strips_leading_dots() {
		assert_eq!(sanitize("..hidden"), "hidden");
		assert_eq!(sanitize(". .. /etc"), "etc");
		assert_eq!(sanitize(".."), "");
		assert_eq!(sanitize("Acme Inc."), "Acme Inc.");
	}

	#[test]
	fn sanitize_keeps_non_ascii_and_cuts_on_char_boundaries() {
		assert_eq!(sanitize("健康保険 領収書"), "健康保険 領収書");
		let long_ascii = "x".repeat(300);
		assert_eq!(sanitize(&long_ascii).chars().count(), 120);
		let long_cjk = "保".repeat(300);
		let cut = sanitize(&long_cjk);
		assert!(cut.chars().count() <= 120);
		assert!(cut.len() <= 200);
		assert!(cut.chars().all(|c| c == '保'));
		let spaced = format!("{} tail", "y".repeat(119));
		assert_eq!(sanitize(&spaced), "y".repeat(119));
	}

	#[test]
	fn document_path_with_everything() {
		let mut s = sidecar("Invoice 42");
		s.area = "Housing".into();
		s.issuer = Some("Acme Inc.".into());
		s.date = Some("2024-03-05".into());
		assert_eq!(
			document_path(&s, "pdf"),
			PathBuf::from("Housing/2024/2024-03-05 Acme Inc. Invoice 42.pdf")
		);
		assert_eq!(document_path(&s, ".pdf"), document_path(&s, "pdf"));
	}

	#[test]
	fn document_path_without_date_or_issuer() {
		let mut s = sidecar("Lease");
		s.area = "Housing".into();
		s.issuer = None;
		s.date = None;
		assert_eq!(
			document_path(&s, "pdf"),
			PathBuf::from("Housing/Undated/Lease.pdf")
		);
		s.issuer = Some("  ".into());
		s.date = Some("March 2024".into());
		assert_eq!(
			document_path(&s, "pdf"),
			PathBuf::from("Housing/Undated/Lease.pdf")
		);
		s.issuer = Some("Landlord".into());
		assert_eq!(
			document_path(&s, ""),
			PathBuf::from("Housing/Undated/Landlord Lease")
		);
	}

	#[test]
	fn document_path_cannot_escape_the_library() {
		let mut s = sidecar("../../etc/passwd");
		s.area = "../..".into();
		s.issuer = Some("a/b".into());
		s.date = Some("2024-01-02".into());
		let path = document_path(&s, "p/d:f");
		assert_eq!(
			path,
			PathBuf::from("Unsorted/2024/2024-01-02 a b etc passwd.pdf")
		);
		assert!(
			path.components()
				.all(|c| matches!(c, std::path::Component::Normal(_)))
		);
		s.title = "///".into();
		assert!(document_path(&s, "pdf").ends_with("2024-01-02 a b Untitled.pdf"));
	}

	#[test]
	fn document_path_keeps_file_names_within_limits() {
		let mut s = sidecar(&"題".repeat(200));
		s.issuer = Some("発行者".repeat(60));
		s.date = Some("2024-01-02".into());
		let path = document_path(&s, "pdf");
		let name = path.file_name().unwrap().to_str().unwrap();
		assert!(name.starts_with("2024-01-02 発行者"));
		assert!(name.ends_with(".pdf"));
		assert!(name.len() <= 204);
	}

	#[test]
	fn deconflict_counts_up() {
		let free = Path::new("A/2024/doc.pdf");
		assert_eq!(deconflict(free, |_| false), free);
		assert_eq!(
			deconflict(free, |p| p == free),
			PathBuf::from("A/2024/doc (2).pdf")
		);
		let taken = [
			PathBuf::from("A/2024/doc.pdf"),
			PathBuf::from("A/2024/doc (2).pdf"),
		];
		assert_eq!(
			deconflict(free, |p| taken.iter().any(|t| t == p)),
			PathBuf::from("A/2024/doc (3).pdf")
		);
	}

	#[test]
	fn deconflict_handles_dots_and_missing_extensions() {
		let dotted = Path::new("A/Acme Inc. Invoice.pdf");
		assert_eq!(
			deconflict(dotted, |p| p == dotted),
			PathBuf::from("A/Acme Inc. Invoice (2).pdf")
		);
		let bare = Path::new("A/README");
		assert_eq!(
			deconflict(bare, |p| p == bare),
			PathBuf::from("A/README (2)")
		);
	}
}
