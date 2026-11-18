use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use uuid::Uuid;

/// A library root:
///
/// ```text
/// <root>/inbox/                 files dropped here are ingested
/// <root>/library/<Area>/<YYYY>/<YYYY-MM-DD> <Issuer> <Title>.pdf
/// <root>/library/.../<same stem>.json   sidecar
/// <root>/.originals/<sha256>.<ext>      inputs replaced by a split, merge or delete, for undo
/// <root>/.journal.jsonl                 one Entry per line, append-only
/// <root>/.index.sqlite                  index, jobs; rebuildable from sidecars except jobs
/// <root>/.pending.json                  the change in progress, rolled back if it never finished
/// <root>/.lock                          locked by the one process that may change the library
/// ```
#[derive(Debug, Clone)]
pub struct Library {
	root: PathBuf,
}

/// What a walk of `library/` found.
#[derive(Debug, Clone, Default)]
pub struct Scan {
	/// Document path relative to `library/` and its sidecar, sorted by path.
	pub documents: Vec<(PathBuf, crate::Sidecar)>,
	/// Sidecars that could not be used (unreadable, or without a document), with the reason.
	pub invalid: Vec<(PathBuf, String)>,
}

impl Library {
	/// Opens the root, creating the folders above if missing.
	pub fn open(root: impl Into<PathBuf>) -> crate::Result<Self> {
		let library = Self {
			root: std::path::absolute(root.into())?,
		};
		for dir in [library.inbox(), library.library_dir(), library.originals()] {
			fs::create_dir_all(dir)?;
		}
		Ok(library)
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
	/// Takes the exclusive lock that every process changing the library holds while it runs;
	/// it is released when the returned file is closed.
	pub fn lock(&self) -> crate::Result<fs::File> {
		let file = fs::OpenOptions::new()
			.create(true)
			.truncate(false)
			.write(true)
			.open(self.root.join(".lock"))?;
		match file.try_lock() {
			Ok(()) => Ok(file),
			Err(fs::TryLockError::WouldBlock) => {
				Err(crate::Error::Locked(self.root.display().to_string()))
			}
			Err(fs::TryLockError::Error(error)) => Err(error.into()),
		}
	}
	/// Sidecar path for a document path (same stem, `.json`).
	pub fn sidecar_path(document: &Path) -> PathBuf {
		document.with_extension("json")
	}
	/// Reads every sidecar under `library/`, with the document path relative to `library/`.
	/// Sidecars that cannot be used are left out; `scan` reports them.
	pub fn sidecars(&self) -> crate::Result<Vec<(PathBuf, crate::Sidecar)>> {
		Ok(self.scan()?.documents)
	}
	/// Like `sidecars`, also listing the sidecars that were skipped. Hidden files and folders
	/// are ignored.
	pub fn scan(&self) -> crate::Result<Scan> {
		let root = self.library_dir();
		let mut scan = Scan::default();
		let mut pending = vec![root.clone()];
		while let Some(dir) = pending.pop() {
			let mut files = Vec::new();
			for entry in fs::read_dir(&dir)? {
				let entry = entry?;
				if entry.file_name().to_string_lossy().starts_with('.') {
					continue;
				}
				if entry.file_type()?.is_dir() {
					pending.push(entry.path());
				} else {
					files.push(entry.path());
				}
			}
			files.sort();
			for sidecar_path in files.iter().filter(|path| is_json(path)) {
				let relative = |path: &Path| path.strip_prefix(&root).unwrap_or(path).to_path_buf();
				let document = files
					.iter()
					.find(|path| !is_json(path) && Self::sidecar_path(path) == *sidecar_path);
				let Some(document) = document else {
					scan.invalid
						.push((relative(sidecar_path), "no document".to_string()));
					continue;
				};
				let parsed = fs::read(sidecar_path)
					.map_err(crate::Error::from)
					.and_then(|bytes| Ok(serde_json::from_slice::<crate::Sidecar>(&bytes)?));
				match parsed {
					Ok(sidecar) => scan.documents.push((relative(document), sidecar)),
					Err(error) => scan
						.invalid
						.push((relative(sidecar_path), error.to_string())),
				}
			}
		}
		scan.documents.sort_by(|a, b| a.0.cmp(&b.0));
		scan.invalid.sort();
		Ok(scan)
	}
	/// Writes the document bytes and its sidecar at `relative` under `library/`, creating
	/// folders. Writes go to a temp file then rename, so a crash never leaves a half file.
	pub fn write_document(
		&self,
		relative: &Path,
		bytes: &[u8],
		sidecar: &crate::Sidecar,
	) -> crate::Result<()> {
		let path = self.resolve(relative)?;
		if let Some(parent) = path.parent() {
			fs::create_dir_all(parent)?;
		}
		write_atomic(&path, bytes)?;
		write_atomic(&Self::sidecar_path(&path), &sidecar_bytes(sidecar)?)
	}
	/// Rewrites only the sidecar of an existing document.
	pub fn write_sidecar(&self, relative: &Path, sidecar: &crate::Sidecar) -> crate::Result<()> {
		let path = self.resolve(relative)?;
		if !path.is_file() {
			return Err(crate::Error::NotFound(relative.display().to_string()));
		}
		write_atomic(&Self::sidecar_path(&path), &sidecar_bytes(sidecar)?)
	}
	/// Moves a document and its sidecar within `library/`, removing folders left empty.
	/// Refuses to overwrite: `to` and its sidecar must not exist.
	pub fn move_document(&self, from: &Path, to: &Path) -> crate::Result<()> {
		let source = self.resolve(from)?;
		let target = self.resolve(to)?;
		if !source.is_file() {
			return Err(crate::Error::NotFound(from.display().to_string()));
		}
		if source == target {
			return Ok(());
		}
		let source_sidecar = Self::sidecar_path(&source);
		let target_sidecar = Self::sidecar_path(&target);
		let moves_sidecar = source_sidecar.is_file() && source_sidecar != target_sidecar;
		let sidecar_taken = moves_sidecar && target_sidecar.exists();
		if target.exists() || sidecar_taken {
			return Err(crate::Error::Invalid(format!(
				"{} already exists",
				to.display()
			)));
		}
		if let Some(parent) = target.parent() {
			fs::create_dir_all(parent)?;
		}
		fs::rename(&source, &target)?;
		if moves_sidecar && let Err(error) = fs::rename(&source_sidecar, &target_sidecar) {
			let _ = fs::rename(&target, &source);
			return Err(error.into());
		}
		self.prune_empty(source.parent());
		Ok(())
	}
	/// Moves a file (inbox item or library document + sidecar) into `.originals/` named by its
	/// sha256, and returns that name. Idempotent when the same content is already there.
	/// A relative `path` is looked up under `library/`, then `inbox/`. The sidecar of a library
	/// document goes to `.originals/<sha256>.json`.
	pub fn stash_original(&self, path: &Path) -> crate::Result<String> {
		let source = self.locate(path)?;
		let sha256 = crate::hash::sha256_file(&source)?;
		let name = match source.extension() {
			Some(ext) => format!("{sha256}.{}", ext.to_string_lossy().to_lowercase()),
			None => sha256.clone(),
		};
		let originals = self.originals();
		fs::create_dir_all(&originals)?;
		let target = originals.join(&name);
		if source == target {
			return Ok(name);
		}
		let in_library = source.starts_with(self.library_dir());
		let sidecar = Self::sidecar_path(&source);
		if in_library && sidecar != source && sidecar.is_file() {
			move_file(&sidecar, &originals.join(format!("{sha256}.json")))?;
		}
		if target.is_file() {
			fs::remove_file(&source)?;
		} else {
			move_file(&source, &target)?;
		}
		if in_library {
			self.prune_empty(source.parent());
		}
		Ok(name)
	}

	/// Removes a document and its sidecar written by a change that did not finish, unless
	/// the sidecar there belongs to another document.
	pub fn discard(&self, relative: &Path, id: Uuid) -> crate::Result<()> {
		let path = self.resolve(relative)?;
		let sidecar = Self::sidecar_path(&path);
		match fs::read(&sidecar) {
			Ok(bytes) => {
				if serde_json::from_slice::<crate::Sidecar>(&bytes)?.id != id {
					return Ok(());
				}
				fs::remove_file(&sidecar)?;
			}
			Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
			Err(error) => return Err(error.into()),
		}
		match fs::remove_file(&path) {
			Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error.into()),
			_ => {}
		}
		sync_dir(path.parent());
		self.prune_empty(path.parent());
		Ok(())
	}

	fn resolve(&self, relative: &Path) -> crate::Result<PathBuf> {
		let normal = relative
			.components()
			.all(|c| matches!(c, Component::Normal(_)));
		if !normal || relative.as_os_str().is_empty() {
			return Err(crate::Error::Invalid(format!(
				"{} is not a path inside the library",
				relative.display()
			)));
		}
		Ok(self.library_dir().join(relative))
	}

	fn locate(&self, path: &Path) -> crate::Result<PathBuf> {
		let candidates = if path.is_absolute() {
			vec![path.to_path_buf()]
		} else {
			vec![self.library_dir().join(path), self.inbox().join(path)]
		};
		candidates
			.into_iter()
			.find(|candidate| candidate.is_file())
			.ok_or_else(|| crate::Error::NotFound(path.display().to_string()))
	}

	fn prune_empty(&self, start: Option<&Path>) {
		let root = self.library_dir();
		let mut dir = start;
		while let Some(current) = dir {
			if current == root || !current.starts_with(&root) || fs::remove_dir(current).is_err() {
				break;
			}
			dir = current.parent();
		}
	}
}

fn is_json(path: &Path) -> bool {
	path.extension().is_some_and(|ext| ext == "json")
}

fn sidecar_bytes(sidecar: &crate::Sidecar) -> crate::Result<Vec<u8>> {
	let mut bytes = serde_json::to_vec_pretty(sidecar)?;
	bytes.push(b'\n');
	Ok(bytes)
}

fn temp_sibling(path: &Path) -> PathBuf {
	path.with_file_name(format!(".tmp-{}", Uuid::new_v4().simple()))
}

pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> crate::Result<()> {
	let temp = temp_sibling(path);
	let written = (|| {
		let mut file = fs::File::create(&temp)?;
		file.write_all(bytes)?;
		file.sync_all()?;
		fs::rename(&temp, path)
	})();
	if let Err(error) = written {
		let _ = fs::remove_file(&temp);
		return Err(error.into());
	}
	sync_dir(path.parent());
	Ok(())
}

fn move_file(from: &Path, to: &Path) -> crate::Result<()> {
	if fs::rename(from, to).is_ok() {
		return Ok(());
	}
	let temp = temp_sibling(to);
	let copied = fs::copy(from, &temp).and_then(|_| fs::rename(&temp, to));
	if let Err(error) = copied {
		let _ = fs::remove_file(&temp);
		return Err(error.into());
	}
	fs::remove_file(from)?;
	Ok(())
}

fn sync_dir(dir: Option<&Path>) {
	if let Some(dir) = dir
		&& let Ok(handle) = fs::File::open(dir)
	{
		let _ = handle.sync_all();
	}
}

#[cfg(test)]
mod tests {
	#[test]
	fn open_makes_the_root_absolute() {
		let dir = tempfile::tempdir().unwrap();
		let cwd = std::env::current_dir().unwrap();
		let relative = pathdiff(dir.path(), &cwd);
		let library = super::Library::open(&relative).unwrap();
		assert!(library.root().is_absolute());
		assert!(library.inbox().is_absolute());
		assert_eq!(library.root(), std::path::absolute(&relative).unwrap());
	}

	fn pathdiff(target: &std::path::Path, base: &std::path::Path) -> std::path::PathBuf {
		let mut up = std::path::PathBuf::new();
		for _ in base.components().skip(1) {
			up.push("..");
		}
		up.join(target.strip_prefix("/").unwrap())
	}

	use super::*;
	use crate::testutil::sidecar;

	fn library() -> (tempfile::TempDir, Library) {
		let dir = tempfile::tempdir().unwrap();
		let library = Library::open(dir.path().join("root")).unwrap();
		(dir, library)
	}

	fn names(dir: &Path) -> Vec<String> {
		let mut names: Vec<String> = fs::read_dir(dir)
			.unwrap()
			.map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
			.collect();
		names.sort();
		names
	}

	#[test]
	fn open_creates_the_layout_and_is_repeatable() {
		let (dir, library) = library();
		let root = dir.path().join("root");
		assert_eq!(library.root(), root);
		assert!(library.inbox().is_dir());
		assert!(library.library_dir().is_dir());
		assert!(library.originals().is_dir());
		assert_eq!(library.inbox(), root.join("inbox"));
		assert_eq!(library.library_dir(), root.join("library"));
		assert_eq!(library.originals(), root.join(".originals"));
		assert_eq!(library.journal_path(), root.join(".journal.jsonl"));
		assert_eq!(library.index_path(), root.join(".index.sqlite"));
		Library::open(&root).unwrap();
	}

	#[test]
	fn sidecar_path_swaps_only_the_last_extension() {
		assert_eq!(
			Library::sidecar_path(Path::new("A/2024/Acme Inc. Bill.pdf")),
			PathBuf::from("A/2024/Acme Inc. Bill.json")
		);
	}

	#[test]
	fn write_document_then_read_back() {
		let (_dir, library) = library();
		let mut s = sidecar("Bill");
		s.extra
			.insert("future".into(), serde_json::json!({"nested": [1, 2]}));
		let relative = Path::new("Housing/2024/2024-01-02 Acme Inc. Bill.pdf");
		library.write_document(relative, b"%PDF-1", &s).unwrap();

		let folder = library.library_dir().join("Housing/2024");
		assert_eq!(
			names(&folder),
			[
				"2024-01-02 Acme Inc. Bill.json",
				"2024-01-02 Acme Inc. Bill.pdf"
			]
		);
		assert_eq!(
			fs::read(library.library_dir().join(relative)).unwrap(),
			b"%PDF-1"
		);
		assert_eq!(
			library.sidecars().unwrap(),
			vec![(relative.to_path_buf(), s)]
		);
	}

	#[test]
	fn write_document_replaces_existing_content() {
		let (_dir, library) = library();
		let relative = Path::new("A/Undated/doc.pdf");
		library
			.write_document(relative, b"one", &sidecar("one"))
			.unwrap();
		let second = sidecar("two");
		library.write_document(relative, b"two", &second).unwrap();
		assert_eq!(
			fs::read(library.library_dir().join(relative)).unwrap(),
			b"two"
		);
		assert_eq!(
			library.sidecars().unwrap(),
			vec![(relative.to_path_buf(), second)]
		);
		assert_eq!(
			names(&library.library_dir().join("A/Undated")),
			["doc.json", "doc.pdf"]
		);
	}

	#[test]
	fn paths_outside_the_library_are_rejected() {
		let (_dir, library) = library();
		let s = sidecar("x");
		for bad in ["../escape.pdf", "/abs/doc.pdf", "A/../../doc.pdf", ""] {
			let bad = Path::new(bad);
			assert!(matches!(
				library.write_document(bad, b"x", &s),
				Err(crate::Error::Invalid(_))
			));
			assert!(matches!(
				library.write_sidecar(bad, &s),
				Err(crate::Error::Invalid(_))
			));
			assert!(matches!(
				library.move_document(bad, Path::new("A/ok.pdf")),
				Err(crate::Error::Invalid(_))
			));
		}
		assert!(!library.root().join("escape.pdf").exists());
	}

	#[test]
	fn write_sidecar_needs_the_document() {
		let (_dir, library) = library();
		let relative = Path::new("A/Undated/doc.pdf");
		let mut s = sidecar("doc");
		assert!(matches!(
			library.write_sidecar(relative, &s),
			Err(crate::Error::NotFound(_))
		));
		library.write_document(relative, b"bytes", &s).unwrap();
		s.title = "renamed".into();
		library.write_sidecar(relative, &s).unwrap();
		assert_eq!(library.sidecars().unwrap()[0].1.title, "renamed");
		assert_eq!(
			fs::read(library.library_dir().join(relative)).unwrap(),
			b"bytes"
		);
	}

	#[test]
	fn scan_reports_unusable_sidecars_and_skips_hidden_files() {
		let (_dir, library) = library();
		let good = Path::new("A/2024/good.pdf");
		library
			.write_document(good, b"x", &sidecar("good"))
			.unwrap();
		let folder = library.library_dir().join("A/2024");
		fs::write(folder.join("broken.pdf"), b"x").unwrap();
		fs::write(folder.join("broken.json"), b"{ not json").unwrap();
		fs::write(folder.join("orphan.json"), b"{}").unwrap();
		fs::write(folder.join("loose.pdf"), b"x").unwrap();
		fs::write(folder.join(".tmp-abc"), b"x").unwrap();
		fs::create_dir(library.library_dir().join(".hidden")).unwrap();
		fs::write(library.library_dir().join(".hidden/x.json"), b"{}").unwrap();

		let scan = library.scan().unwrap();
		assert_eq!(scan.documents.len(), 1);
		assert_eq!(scan.documents[0].0, good);
		let invalid: Vec<&Path> = scan
			.invalid
			.iter()
			.map(|(path, _)| path.as_path())
			.collect();
		assert_eq!(
			invalid,
			[
				Path::new("A/2024/broken.json"),
				Path::new("A/2024/orphan.json")
			]
		);
		assert_eq!(library.sidecars().unwrap().len(), 1);
	}

	#[test]
	fn sidecars_are_sorted_by_path() {
		let (_dir, library) = library();
		for path in [
			"B/2023/b.pdf",
			"A/2024/z.pdf",
			"A/2024/a.pdf",
			"A/2020/m.pdf",
		] {
			library
				.write_document(Path::new(path), b"x", &sidecar(path))
				.unwrap();
		}
		let paths: Vec<PathBuf> = library
			.sidecars()
			.unwrap()
			.into_iter()
			.map(|(p, _)| p)
			.collect();
		let expected = [
			"A/2020/m.pdf",
			"A/2024/a.pdf",
			"A/2024/z.pdf",
			"B/2023/b.pdf",
		];
		assert_eq!(paths, expected.map(PathBuf::from));
	}

	#[test]
	fn move_document_takes_the_sidecar_and_prunes_folders() {
		let (_dir, library) = library();
		let from = Path::new("Old/2020/doc.pdf");
		let to = Path::new("New/2024/renamed.pdf");
		let s = sidecar("doc");
		library.write_document(from, b"bytes", &s).unwrap();
		library.move_document(from, to).unwrap();

		assert!(!library.library_dir().join("Old").exists());
		assert!(library.library_dir().is_dir());
		assert_eq!(
			names(&library.library_dir().join("New/2024")),
			["renamed.json", "renamed.pdf"]
		);
		assert_eq!(library.sidecars().unwrap(), vec![(to.to_path_buf(), s)]);
		library.move_document(to, to).unwrap();
	}

	#[test]
	fn move_document_keeps_folders_that_are_still_used() {
		let (_dir, library) = library();
		library
			.write_document(Path::new("A/2020/one.pdf"), b"1", &sidecar("one"))
			.unwrap();
		library
			.write_document(Path::new("A/2020/two.pdf"), b"2", &sidecar("two"))
			.unwrap();
		library
			.move_document(Path::new("A/2020/one.pdf"), Path::new("A/2021/one.pdf"))
			.unwrap();
		assert_eq!(names(&library.library_dir().join("A")), ["2020", "2021"]);
	}

	#[test]
	fn move_document_refuses_missing_sources_and_taken_targets() {
		let (_dir, library) = library();
		let a = Path::new("A/a.pdf");
		let b = Path::new("A/b.pdf");
		assert!(matches!(
			library.move_document(a, b),
			Err(crate::Error::NotFound(_))
		));
		library.write_document(a, b"a", &sidecar("a")).unwrap();
		library.write_document(b, b"b", &sidecar("b")).unwrap();
		assert!(matches!(
			library.move_document(a, b),
			Err(crate::Error::Invalid(_))
		));
		assert_eq!(fs::read(library.library_dir().join(a)).unwrap(), b"a");
		assert_eq!(fs::read(library.library_dir().join(b)).unwrap(), b"b");
	}

	#[test]
	fn stash_inbox_file_by_hash() {
		let (_dir, library) = library();
		let dropped = library.inbox().join("Scan 001.PDF");
		fs::write(&dropped, b"scan").unwrap();
		let name = library.stash_original(&dropped).unwrap();
		assert_eq!(name, format!("{}.pdf", crate::hash::sha256(b"scan")));
		assert!(!dropped.exists());
		assert_eq!(fs::read(library.originals().join(&name)).unwrap(), b"scan");

		fs::write(&dropped, b"scan").unwrap();
		assert_eq!(
			library.stash_original(Path::new("Scan 001.PDF")).unwrap(),
			name
		);
		assert!(!dropped.exists());
		assert_eq!(names(&library.originals()), std::slice::from_ref(&name));
		assert_eq!(
			library
				.stash_original(&library.originals().join(&name))
				.unwrap(),
			name
		);
		assert!(library.originals().join(&name).is_file());
	}

	#[test]
	fn stash_file_without_extension() {
		let (_dir, library) = library();
		fs::write(library.inbox().join("README"), b"plain").unwrap();
		let name = library.stash_original(Path::new("README")).unwrap();
		assert_eq!(name, crate::hash::sha256(b"plain"));
	}

	#[test]
	fn stash_library_document_with_its_sidecar() {
		let (_dir, library) = library();
		let relative = Path::new("A/2024/doc.pdf");
		let s = sidecar("doc");
		library.write_document(relative, b"content", &s).unwrap();
		let name = library.stash_original(relative).unwrap();
		let sha = crate::hash::sha256(b"content");
		assert_eq!(name, format!("{sha}.pdf"));
		assert_eq!(
			names(&library.originals()),
			[format!("{sha}.json"), format!("{sha}.pdf")]
		);
		let stashed: crate::Sidecar = serde_json::from_slice(
			&fs::read(library.originals().join(format!("{sha}.json"))).unwrap(),
		)
		.unwrap();
		assert_eq!(stashed, s);
		assert!(names(&library.library_dir()).is_empty());
		assert!(library.sidecars().unwrap().is_empty());
	}

	#[test]
	fn stash_missing_file_is_not_found() {
		let (_dir, library) = library();
		assert!(matches!(
			library.stash_original(Path::new("nope.pdf")),
			Err(crate::Error::NotFound(_))
		));
	}

	#[test]
	fn only_one_opener_holds_the_lock() {
		let (_dir, library) = library();
		let held = library.lock().unwrap();
		let again = Library::open(library.root()).unwrap();
		assert!(matches!(again.lock(), Err(crate::Error::Locked(_))));
		drop(held);
		assert!(again.lock().is_ok());
	}

	#[test]
	fn discard_removes_only_the_document_it_wrote() {
		let (_dir, library) = library();
		let ours = sidecar("ours");
		let path = Path::new("A/2024/ours.pdf");
		library.write_document(path, b"pdf", &ours).unwrap();
		library.discard(path, Uuid::new_v4()).unwrap();
		assert!(library.library_dir().join(path).is_file());
		library.discard(path, ours.id).unwrap();
		assert!(names(&library.library_dir()).is_empty());

		library.write_document(path, b"pdf", &ours).unwrap();
		fs::remove_file(Library::sidecar_path(&library.library_dir().join(path))).unwrap();
		library.discard(path, ours.id).unwrap();
		assert!(names(&library.library_dir()).is_empty());
		library.discard(path, ours.id).unwrap();
	}
}
