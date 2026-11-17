use std::path::Path;

/// Lowercase hex SHA-256 of a byte slice.
pub fn sha256(bytes: &[u8]) -> String {
	todo!()
}

/// Lowercase hex SHA-256 of a file, streamed.
pub fn sha256_file(path: &Path) -> crate::Result<String> {
	todo!()
}
