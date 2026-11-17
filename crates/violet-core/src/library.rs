use std::path::{Path, PathBuf};

/// A library root:
///
/// ```text
/// <root>/inbox/                 files dropped here are ingested
/// <root>/library/<Area>/<YYYY>/<YYYY-MM-DD> <Issuer> <Title>.pdf
/// <root>/library/.../<same stem>.json   sidecar
/// <root>/.originals/<sha256>.<ext>      inputs replaced by a split, merge or delete, for undo
/// <root>/.journal.jsonl                 one Entry per line, append-only
/// <root>/.index.sqlite                  index, jobs; rebuildable from sidecars except jobs
/// ```
#[derive(Debug, Clone)]
pub struct Library {
	root: PathBuf,
}

impl Library {
	/// Opens the root, creating the folders above if missing.
	pub fn open(root: impl Into<PathBuf>) -> crate::Result<Self> {
		todo!()
	}
	pub fn root(&self) -> &Path {
		&self.root
	}
	pub fn inbox(&self) -> PathBuf {
		self.root.join("inbox")
	}
	pub fn library_dir(&self) -> PathBuf {
		self.root.join("library")
	}
	pub fn originals(&self) -> PathBuf {
		self.root.join(".originals")
	}
	pub fn journal_path(&self) -> PathBuf {
		self.root.join(".journal.jsonl")
	}
	pub fn index_path(&self) -> PathBuf {
		self.root.join(".index.sqlite")
	}
	/// Sidecar path for a document path (same stem, `.json`).
	pub fn sidecar_path(document: &Path) -> PathBuf {
		document.with_extension("json")
	}
	/// Reads every sidecar under `library/`, with the document path relative to `library/`.
	pub fn sidecars(&self) -> crate::Result<Vec<(PathBuf, crate::Sidecar)>> {
		todo!()
	}
	/// Writes the document bytes and its sidecar at `relative` under `library/`, creating
	/// folders. Writes go to a temp file then rename, so a crash never leaves a half file.
	pub fn write_document(
		&self,
		relative: &Path,
		bytes: &[u8],
		sidecar: &crate::Sidecar,
	) -> crate::Result<()> {
		todo!()
	}
	/// Rewrites only the sidecar of an existing document.
	pub fn write_sidecar(&self, relative: &Path, sidecar: &crate::Sidecar) -> crate::Result<()> {
		todo!()
	}
	/// Moves a document and its sidecar within `library/`, removing folders left empty.
	pub fn move_document(&self, from: &Path, to: &Path) -> crate::Result<()> {
		todo!()
	}
	/// Moves a file (inbox item or library document + sidecar) into `.originals/` named by its
	/// sha256, and returns that name. Idempotent when the same content is already there.
	pub fn stash_original(&self, path: &Path) -> crate::Result<String> {
		todo!()
	}
}
