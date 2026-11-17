//! PDF operations on in-memory bytes. Pages are 1-based everywhere. Rendering uses hayro,
//! editing uses lopdf; every edit returns a new, valid PDF and leaves the input untouched.

mod edit;
mod text;
mod wrap;

use hayro::hayro_syntax::Pdf;
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::{PixmapSettings, RenderCache, RenderSettings};

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

/// Largest `max_side` accepted by [`render_png`].
pub const MAX_RENDER_SIDE: u32 = 8192;

const PDF_HEADER_WINDOW: usize = 1024;

pub fn detect(bytes: &[u8]) -> Kind {
	if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
		Kind::Png
	} else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
		Kind::Jpeg
	} else if bytes[..bytes.len().min(PDF_HEADER_WINDOW)]
		.windows(5)
		.any(|w| w == b"%PDF-")
	{
		Kind::Pdf
	} else {
		Kind::Other
	}
}

fn require_pdf(bytes: &[u8]) -> Result<()> {
	match detect(bytes) {
		Kind::Pdf => Ok(()),
		_ => Err(Error::Pdf("not a PDF".into())),
	}
}

fn open(pdf: &[u8]) -> Result<Pdf> {
	require_pdf(pdf)?;
	let doc = Pdf::new(pdf.to_vec()).map_err(|e| Error::Pdf(format!("{e:?}")))?;
	if doc.pages().is_empty() {
		return Err(Error::Pdf("no pages".into()));
	}
	Ok(doc)
}

pub fn page_count(pdf: &[u8]) -> Result<u32> {
	match edit::Source::load(pdf) {
		Ok(source) => Ok(source.page_count()),
		Err(err) => open(pdf)
			.map(|doc| doc.pages().len() as u32)
			.map_err(|_| err),
	}
}

/// Renders a page to PNG so that its longest side is `max_side` pixels.
pub fn render_png(pdf: &[u8], page: u32, max_side: u32) -> Result<Vec<u8>> {
	if max_side == 0 || max_side > MAX_RENDER_SIDE {
		return Err(Error::Invalid(format!(
			"max_side {max_side} not in 1..={MAX_RENDER_SIDE}"
		)));
	}
	let doc = open(pdf)?;
	let pages = doc.pages();
	let target = (page as usize)
		.checked_sub(1)
		.and_then(|index| pages.get(index))
		.ok_or(Error::PageOutOfRange(page, pages.len() as u32))?;
	let (width, height) = target.render_dimensions();
	let scale = max_side as f32 / width.max(height);
	let pixels = |side: f32| (side * scale).round().clamp(1.0, max_side as f32);
	let settings = PixmapSettings {
		x_scale: (pixels(width) + 0.25) / width,
		y_scale: (pixels(height) + 0.25) / height,
		bg_color: WHITE,
	};
	hayro::render(
		target,
		&RenderCache::new(),
		&Default::default(),
		&RenderSettings::default(),
		&settings,
	)
	.into_png()
	.map_err(|e| Error::Image(e.to_string()))
}

/// The text layer, one string per page. A page whose text is empty or mostly not printable
/// (an image-only scan, or a font without a usable ToUnicode map) yields `None`, meaning it
/// needs OCR.
pub fn text_layer(pdf: &[u8]) -> Result<Vec<Option<String>>> {
	Ok(text::extract(&open(pdf)?))
}

/// One PDF per group; each group lists 1-based pages in output order.
pub fn split(pdf: &[u8], groups: &[Vec<u32>]) -> Result<Vec<Vec<u8>>> {
	edit::split(pdf, groups)
}

/// Concatenates in order.
pub fn merge(pdfs: &[&[u8]]) -> Result<Vec<u8>> {
	edit::merge(pdfs)
}

/// Adds `degrees` (a multiple of 90, may be negative) to each listed page's rotation.
pub fn rotate(pdf: &[u8], pages: &[u32], degrees: i32) -> Result<Vec<u8>> {
	edit::rotate(pdf, pages, degrees)
}

/// Removes the listed pages; removing every page is an error.
pub fn delete_pages(pdf: &[u8], pages: &[u32]) -> Result<Vec<u8>> {
	edit::delete_pages(pdf, pages)
}

/// Wraps a PNG or JPEG into a one-page PDF sized to the image at 300 dpi. JPEG is embedded
/// as-is (DCTDecode) and shown according to its EXIF orientation; PNG is decoded, flattened
/// onto white if it has transparency, and stored Flate-compressed.
pub fn image_to_pdf(image: &[u8]) -> Result<Vec<u8>> {
	wrap::image_to_pdf(image)
}
