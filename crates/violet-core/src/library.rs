use std::collections::{HashMap, HashSet};
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
/// <root>/.originals/<sha256>.sidecar.json   their sidecars; older ones `<sha256>.sidecar.<n>.json`
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
	/// What does not add up, with the reason: sidecars that cannot be used (unreadable, or
	/// without a document), documents without a sidecar or sharing one, and documents with the
	/// id of another (all of them are in `documents`; the index keeps the last).
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
	/// Sidecar path for a document path: its extension replaced by `.json`, or `.json` added to
	/// a name without one.
	pub fn sidecar_path(document: &Path) -> PathBuf {
		let name = document.file_name().unwrap_or_default();
		if crate::naming::extension(&name.to_string_lossy()).is_some() {
			return document.with_extension("json");
		}
		let mut name = name.to_os_string();
		name.push(".json");
		document.with_file_name(name)
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
			let relative = |path: &Path| path.strip_prefix(&root).unwrap_or(path).to_path_buf();
			let sidecars: HashSet<&PathBuf> = files.iter().filter(|path| is_json(path)).collect();
			let mut owners: HashMap<PathBuf, &PathBuf> = HashMap::new();
			for document in files.iter().filter(|path| !is_json(path)) {
				let sidecar = Self::sidecar_path(document);
				if !sidecars.contains(&sidecar) {
					scan.invalid
						.push((relative(document), "no sidecar".to_string()));
				} else if let Some(owner) = owners.get(&sidecar) {
					let reason = format!(
						"its sidecar {} belongs to {}",
						relative(&sidecar).display(),
						relative(owner).display()
					);
					scan.invalid.push((relative(document), reason));
				} else {
					owners.insert(sidecar, document);
				}
			}
			for sidecar_path in sidecars {
				let Some(document) = owners.get(sidecar_path) else {
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
		let mut first: HashMap<Uuid, &Path> = HashMap::new();
		for (path, sidecar) in &scan.documents {
			match first.get(&sidecar.id) {
				Some(other) => scan.invalid.push((
					path.clone(),
					format!("same id {} as {}", sidecar.id, other.display()),
				)),
				None => {
					first.insert(sidecar.id, path);
				}
			}
		}
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
	/// Name under `.originals/` for content with this hash that came from `path`.
	pub fn original_name(sha256: &str, path: &Path) -> String {
		let name = path.file_name().unwrap_or_default().to_string_lossy();
		match crate::naming::extension(&name) {
			Some(ext) => format!("{sha256}.{ext}"),
			None => sha256.to_string(),
		}
	}
	/// Moves a file (inbox item or library document + sidecar) into `.originals/` named by its
	/// sha256, and returns that name. Idempotent when the same content is already there.
	/// A relative `path` is looked up under `library/`, then `inbox/`. The sidecar of a library
	/// document is kept with [`Library::stash_sidecar`].
	pub fn stash_original(&self, path: &Path) -> crate::Result<String> {
		let source = self.locate(path)?;
		let sha256 = crate::hash::sha256_file(&source)?;
		let name = Self::original_name(&sha256, &source);
		let originals = self.originals();
		fs::create_dir_all(&originals)?;
		let target = originals.join(&name);
		if source == target {
			return Ok(name);
		}
		let in_library = source.starts_with(self.library_dir());
		let sidecar = Self::sidecar_path(&source);
		if in_library && sidecar != source && sidecar.is_file() {
			self.keep_sidecar(&sha256, &fs::read(&sidecar)?)?;
			fs::remove_file(&sidecar)?;
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
	/// Keeps the sidecar of the document with content `sha256` as
	/// `.originals/<sha256>.sidecar.json`. A different one already there is first moved to the
	/// next free `<sha256>.sidecar.<n>.json`: nothing in `.originals/` is overwritten.
	pub fn stash_sidecar(&self, sha256: &str, sidecar: &crate::Sidecar) -> crate::Result<()> {
		self.keep_sidecar(sha256, &sidecar_bytes(sidecar)?)
	}
	/// The sidecars kept for content `sha256`, newest first, ending with the `<sha256>.json` older
	/// libraries used. Files that do not read as a sidecar are left out.
	pub fn stashed_sidecars(&self, sha256: &str) -> crate::Result<Vec<crate::Sidecar>> {
		let originals = self.originals();
		let mut names = vec![format!("{sha256}.sidecar.json")];
		names.extend(
			(1..=self.sidecars_aside(sha256))
				.rev()
				.map(|n| format!("{sha256}.sidecar.{n}.json")),
		);
		names.push(format!("{sha256}.json"));
		let mut sidecars = Vec::new();
		for name in names {
			match fs::read(originals.join(&name)) {
				Ok(bytes) => sidecars.extend(serde_json::from_slice(&bytes).ok()),
				Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
				Err(error) => return Err(error.into()),
			}
		}
		Ok(sidecars)
	}
	fn keep_sidecar(&self, sha256: &str, bytes: &[u8]) -> crate::Result<()> {
		let originals = self.originals();
		fs::create_dir_all(&originals)?;
		let target = originals.join(format!("{sha256}.sidecar.json"));
		match fs::read(&target) {
			Ok(kept) if kept == bytes => return Ok(()),
			Ok(_) => {
				let n = self.sidecars_aside(sha256) + 1;
				let aside = originals.join(format!("{sha256}.sidecar.{n}.json"));
				if fs::hard_link(&target, &aside).is_err() {
					copy_new(&target, &aside)?;
				}
				fs::remove_file(&target)?;
			}
			Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
			Err(error) => return Err(error.into()),
		}
		write_atomic(&target, bytes)
	}
	/// How many `<sha256>.sidecar.<n>.json` there are, numbered from 1.
	fn sidecars_aside(&self, sha256: &str) -> u32 {
		let originals = self.originals();
		(1..)
			.take_while(|n| {
				originals
					.join(format!("{sha256}.sidecar.{n}.json"))
					.exists()
			})
			.last()
			.unwrap_or(0)
	}
	/// Writes `bytes` to `.originals/<name>` unless a file of that name is already there.
	pub fn stash_bytes(&self, bytes: &[u8], name: &str) -> crate::Result<()> {
		let originals = self.originals();
		fs::create_dir_all(&originals)?;
		let target = originals.join(name);
		if !target.is_file() {
			write_atomic(&target, bytes)?;
		}
		Ok(())
	}
	/// Moves `temp`, a finished file inside `inbox/`, to `relative` in the inbox, or to the
	/// first free `<stem> (n).<ext>` next to it. Never replaces a file, even one that appears
	/// at the same moment. Returns the path it took, relative to `inbox/`.
	pub fn into_inbox(&self, temp: &Path, relative: &Path) -> crate::Result<PathBuf> {
		let inbox = self.inbox();
		loop {
			let candidate =
				crate::naming::deconflict(relative, |candidate| inbox.join(candidate).exists());
			let target = inbox.join(&candidate);
			if let Some(parent) = target.parent() {
				fs::create_dir_all(parent)?;
			}
			match fs::hard_link(temp, &target) {
				Ok(()) => fs::remove_file(temp)?,
				Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
				Err(_) => match copy_new(temp, &target) {
					Ok(()) => fs::remove_file(temp)?,
					Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
					Err(error) => return Err(error.into()),
				},
			}
			sync_dir(target.parent());
			return Ok(candidate);
		}
	}
	/// Takes `path` (in `inbox/`) out of the inbox if it still holds the content `sha256`,
	/// which must already be in `.originals/`. A file replaced since is left in the inbox.
	/// Returns whether the file was taken out.
	pub fn release_inbox(&self, path: &Path, sha256: &str) -> crate::Result<bool> {
		let inbox = self.inbox();
		let source = if path.is_absolute() {
			path.to_path_buf()
		} else {
			inbox.join(path)
		};
		let Ok(relative) = source.strip_prefix(&inbox).map(Path::to_path_buf) else {
			return Err(crate::Error::Invalid(format!(
				"{} is not in the inbox",
				path.display()
			)));
		};
		let claimed = source.with_file_name(format!(".claim-{}", Uuid::new_v4().simple()));
		match fs::rename(&source, &claimed) {
			Ok(()) => {}
			Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
			Err(error) => return Err(error.into()),
		}
		if crate::hash::sha256_file(&claimed)? == sha256 {
			fs::remove_file(&claimed)?;
			sync_dir(claimed.parent());
			return Ok(true);
		}
		self.into_inbox(&claimed, &relative)?;
		Ok(false)
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

fn copy_new(from: &Path, to: &Path) -> std::io::Result<()> {
	let mut target = fs::OpenOptions::new()
		.write(true)
		.create_new(true)
		.open(to)?;
	let copied =
		std::io::copy(&mut fs::File::open(from)?, &mut target).and_then(|_| target.sync_all());
	if copied.is_err() {
		let _ = fs::remove_file(to);
	}
	copied
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
		assert_eq!(
			Library::sidecar_path(Path::new("A/2024/x.json.txt")),
			PathBuf::from("A/2024/x.json.json")
		);
	}

	#[test]
	fn sidecar_path_of_a_name_without_extension_keeps_the_whole_name() {
		assert_eq!(
			Library::sidecar_path(Path::new("A/2024/Acme Inc. Bill")),
			PathBuf::from("A/2024/Acme Inc. Bill.json")
		);
		assert_eq!(
			Library::sidecar_path(Path::new("A/README")),
			PathBuf::from("A/README.json")
		);
		let (_dir, library) = library();
		for (path, title) in [
			("A/Acme Inc. Bill", "bill"),
			("A/Acme Inc. Receipt", "receipt"),
		] {
			library
				.write_document(Path::new(path), b"x", &sidecar(title))
				.unwrap();
		}
		let scan = library.scan().unwrap();
		assert!(scan.invalid.is_empty(), "{:?}", scan.invalid);
		let paths: Vec<&Path> = scan.documents.iter().map(|(p, _)| p.as_path()).collect();
		assert_eq!(
			paths,
			[
				Path::new("A/Acme Inc. Bill"),
				Path::new("A/Acme Inc. Receipt")
			]
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
				Path::new("A/2024/loose.pdf"),
				Path::new("A/2024/orphan.json")
			]
		);
		assert_eq!(library.sidecars().unwrap().len(), 1);
	}

	#[test]
	fn scan_reports_shared_sidecars_and_duplicate_ids() {
		let (_dir, library) = library();
		let s = sidecar("bill");
		library
			.write_document(Path::new("A/2024/bill.pdf"), b"x", &s)
			.unwrap();
		library
			.write_document(Path::new("B/2024/copy.pdf"), b"x", &s)
			.unwrap();
		fs::write(library.library_dir().join("A/2024/bill.txt"), b"x").unwrap();

		let scan = library.scan().unwrap();
		let paths: Vec<&Path> = scan.documents.iter().map(|(p, _)| p.as_path()).collect();
		assert_eq!(
			paths,
			[Path::new("A/2024/bill.pdf"), Path::new("B/2024/copy.pdf")]
		);
		assert_eq!(
			scan.invalid,
			[
				(
					PathBuf::from("A/2024/bill.txt"),
					"its sidecar A/2024/bill.json belongs to A/2024/bill.pdf".to_string()
				),
				(
					PathBuf::from("B/2024/copy.pdf"),
					format!("same id {} as A/2024/bill.pdf", s.id)
				),
			]
		);
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
	fn into_inbox_never_replaces_a_waiting_file() {
		let (_dir, library) = library();
		let inbox = library.inbox();
		fs::write(inbox.join("scan.pdf"), b"first").unwrap();
		let temp = inbox.join(".upload.part");
		fs::write(&temp, b"second").unwrap();
		let taken = library.into_inbox(&temp, Path::new("scan.pdf")).unwrap();
		assert_eq!(taken, Path::new("scan (2).pdf"));
		assert!(!temp.exists());
		assert_eq!(fs::read(inbox.join("scan.pdf")).unwrap(), b"first");
		assert_eq!(fs::read(inbox.join("scan (2).pdf")).unwrap(), b"second");
	}

	#[test]
	fn concurrent_into_inbox_keeps_every_file() {
		let (_dir, library) = library();
		let inbox = library.inbox();
		let threads: Vec<_> = (0..8)
			.map(|n| {
				let library = library.clone();
				let temp = inbox.join(format!(".upload-{n}.part"));
				fs::write(&temp, format!("file {n}")).unwrap();
				std::thread::spawn(move || library.into_inbox(&temp, Path::new("scan.pdf")))
			})
			.collect();
		let mut taken: Vec<PathBuf> = threads
			.into_iter()
			.map(|thread| thread.join().unwrap().unwrap())
			.collect();
		taken.sort();
		taken.dedup();
		assert_eq!(taken.len(), 8);
		let mut contents: Vec<Vec<u8>> = taken
			.iter()
			.map(|name| fs::read(inbox.join(name)).unwrap())
			.collect();
		contents.sort();
		let mut expected: Vec<Vec<u8>> = (0..8).map(|n| format!("file {n}").into_bytes()).collect();
		expected.sort();
		assert_eq!(contents, expected);
	}

	#[test]
	fn release_inbox_takes_only_the_content_it_was_given() {
		let (_dir, library) = library();
		let inbox = library.inbox();
		let path = inbox.join("scan.pdf");
		fs::write(&path, b"old").unwrap();
		let old = crate::hash::sha256(b"old");
		library.stash_bytes(b"old", &format!("{old}.pdf")).unwrap();
		assert!(library.release_inbox(Path::new("scan.pdf"), &old).unwrap());
		assert!(!path.exists());
		assert_eq!(
			fs::read(library.originals().join(format!("{old}.pdf"))).unwrap(),
			b"old"
		);

		fs::write(&path, b"new").unwrap();
		assert!(!library.release_inbox(&path, &old).unwrap());
		assert_eq!(fs::read(&path).unwrap(), b"new");
		assert_eq!(names(&inbox), ["scan.pdf"]);
		assert!(!library.release_inbox(Path::new("gone.pdf"), &old).unwrap());
	}

	#[test]
	fn stash_bytes_keeps_what_is_there() {
		let (_dir, library) = library();
		library.stash_bytes(b"one", "x.pdf").unwrap();
		library.stash_bytes(b"two", "x.pdf").unwrap();
		assert_eq!(fs::read(library.originals().join("x.pdf")).unwrap(), b"one");
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
			[format!("{sha}.pdf"), format!("{sha}.sidecar.json")]
		);
		assert_eq!(library.stashed_sidecars(&sha).unwrap(), [s]);
		assert!(names(&library.library_dir()).is_empty());
		assert!(library.sidecars().unwrap().is_empty());
	}

	#[test]
	fn stashed_sidecars_and_json_inputs_never_overwrite_each_other() {
		let (_dir, library) = library();
		let json = b"{\"title\": \"not a sidecar\"}";
		let sha = crate::hash::sha256(json);
		fs::write(library.inbox().join("data.json"), json).unwrap();
		assert_eq!(
			library.stash_original(Path::new("data.json")).unwrap(),
			format!("{sha}.json")
		);
		let relative = Path::new("A/2024/data.json.txt");
		let first = sidecar("first");
		library.write_document(relative, json, &first).unwrap();
		assert_eq!(
			library.stash_original(relative).unwrap(),
			format!("{sha}.json.txt")
		);
		let mut second = sidecar("second");
		second.id = first.id;
		library.write_document(relative, json, &second).unwrap();
		library.stash_original(relative).unwrap();
		library.stash_sidecar(&sha, &second).unwrap();
		library.stash_sidecar(&sha, &sidecar("third")).unwrap();

		assert_eq!(
			names(&library.originals()),
			[
				format!("{sha}.json"),
				format!("{sha}.json.txt"),
				format!("{sha}.sidecar.1.json"),
				format!("{sha}.sidecar.2.json"),
				format!("{sha}.sidecar.json"),
			]
		);
		assert_eq!(
			fs::read(library.originals().join(format!("{sha}.json"))).unwrap(),
			json
		);
		let titles: Vec<String> = library
			.stashed_sidecars(&sha)
			.unwrap()
			.into_iter()
			.map(|sidecar| sidecar.title)
			.collect();
		assert_eq!(titles, ["third", "second", "first"]);
	}

	#[test]
	fn stashed_sidecars_still_reads_the_old_name() {
		let (_dir, library) = library();
		let s = sidecar("old");
		let bytes = serde_json::to_vec(&s).unwrap();
		library.stash_bytes(&bytes, "abc.json").unwrap();
		assert_eq!(library.stashed_sidecars("abc").unwrap(), [s]);
		assert!(library.stashed_sidecars("def").unwrap().is_empty());
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
