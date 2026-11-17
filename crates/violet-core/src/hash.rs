use std::fmt::Write as _;
use std::io::Read;
use std::path::Path;

use sha2::{Digest, Sha256};

/// Lowercase hex SHA-256 of a byte slice.
pub fn sha256(bytes: &[u8]) -> String {
	let mut hasher = Sha256::new();
	hasher.update(bytes);
	hex(hasher.finalize().as_slice())
}

/// Lowercase hex SHA-256 of a file, streamed.
pub fn sha256_file(path: &Path) -> crate::Result<String> {
	let mut file = std::fs::File::open(path)?;
	let mut hasher = Sha256::new();
	let mut buffer = vec![0u8; 64 * 1024];
	loop {
		let read = file.read(&mut buffer)?;
		if read == 0 {
			break;
		}
		hasher.update(&buffer[..read]);
	}
	Ok(hex(hasher.finalize().as_slice()))
}

fn hex(bytes: &[u8]) -> String {
	let mut out = String::with_capacity(bytes.len() * 2);
	for byte in bytes {
		let _ = write!(out, "{byte:02x}");
	}
	out
}

#[cfg(test)]
mod tests {
	use super::*;

	const EMPTY: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
	const ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

	#[test]
	fn hashes_known_vectors() {
		assert_eq!(sha256(b""), EMPTY);
		assert_eq!(sha256(b"abc"), ABC);
	}

	#[test]
	fn file_hash_matches_slice_hash() {
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("big.bin");
		let bytes: Vec<u8> = (0..200_000u32).map(|n| (n % 251) as u8).collect();
		std::fs::write(&path, &bytes).unwrap();
		assert_eq!(sha256_file(&path).unwrap(), sha256(&bytes));
	}

	#[test]
	fn missing_file_is_an_io_error() {
		let dir = tempfile::tempdir().unwrap();
		assert!(matches!(
			sha256_file(&dir.path().join("nope")),
			Err(crate::Error::Io(_))
		));
	}
}
