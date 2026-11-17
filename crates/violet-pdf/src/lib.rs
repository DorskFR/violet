//! PDF operations on in-memory bytes. Pages are 1-based everywhere. Rendering uses hayro,
//! editing uses lopdf; every edit returns a new, valid PDF and leaves the input untouched.

#[derive(Debug, thiserror::Error)]
pub enum Error {
	#[error("pdf: {0}")]
	Pdf(String),
	#[error("image: {0}")]
	Image(String),
	#[error("page {0} out of range (1..={1})")]
	PageOutOfRange(u32, u32),
	#[error("invalid: {0}")]
	Invalid(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// What an input file is, from its magic bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
	Pdf,
	Png,
	Jpeg,
	Other,
}

pub fn detect(bytes: &[u8]) -> Kind {
	todo!()
}

pub fn page_count(pdf: &[u8]) -> Result<u32> {
	todo!()
}

/// Renders a page to PNG so that its longest side is `max_side` pixels.
pub fn render_png(pdf: &[u8], page: u32, max_side: u32) -> Result<Vec<u8>> {
	todo!()
}

/// The text layer, one string per page. A page whose text is empty or mostly not printable
/// (an image-only scan, or a font without a usable ToUnicode map) yields `None`, meaning it
/// needs OCR.
pub fn text_layer(pdf: &[u8]) -> Result<Vec<Option<String>>> {
	todo!()
}

/// One PDF per group; each group lists 1-based pages in output order.
pub fn split(pdf: &[u8], groups: &[Vec<u32>]) -> Result<Vec<Vec<u8>>> {
	todo!()
}

/// Concatenates in order.
pub fn merge(pdfs: &[&[u8]]) -> Result<Vec<u8>> {
	todo!()
}

/// Adds `degrees` (a multiple of 90, may be negative) to each listed page's rotation.
pub fn rotate(pdf: &[u8], pages: &[u32], degrees: i32) -> Result<Vec<u8>> {
	todo!()
}

/// Removes the listed pages; removing every page is an error.
pub fn delete_pages(pdf: &[u8], pages: &[u32]) -> Result<Vec<u8>> {
	todo!()
}

/// Wraps a PNG or JPEG into a one-page PDF sized to the image at 300 dpi. JPEG is embedded
/// as-is (DCTDecode); PNG is decoded and stored Flate-compressed.
pub fn image_to_pdf(image: &[u8]) -> Result<Vec<u8>> {
	todo!()
}
