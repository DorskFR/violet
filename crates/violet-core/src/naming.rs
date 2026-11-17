use std::path::{Path, PathBuf};

/// Relative path under `library/` for a sidecar:
/// `<Area>/<YYYY>/<YYYY-MM-DD> <Issuer> <Title>.<ext>`; without a date the year folder is
/// `Undated` and the date prefix is dropped; without an issuer that part is dropped.
/// Components are sanitised: no `/ \ : * ? " < > |`, control chars or leading dots, whitespace
/// collapsed, at most 120 chars per component (cut on a char boundary), non-ASCII kept.
pub fn document_path(sidecar: &crate::Sidecar, ext: &str) -> PathBuf {
	todo!()
}

/// `path` if `exists(path)` is false, else `<stem> (2).<ext>`, `(3)`, ...
pub fn deconflict(path: &Path, exists: impl Fn(&Path) -> bool) -> PathBuf {
	todo!()
}

/// One sanitised path component.
pub fn sanitize(component: &str) -> String {
	todo!()
}
