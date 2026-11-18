//! The pages of a multi-page TIFF. `image` decodes only the first image file directory (IFD)
//! of a TIFF; every offset in a TIFF is absolute, so pointing the header at another IFD
//! makes that page the first one. `image` also rejects palette images: their IFD is
//! relabelled as 8-bit gray so the indices decode, and the colour map is applied after.

use std::collections::HashSet;

use image::{DynamicImage, Rgb, RgbImage};

use crate::{Error, Result};

const MAX_PAGES: usize = 1000;
const NEW_SUBFILE_TYPE: u16 = 254;
const BITS_PER_SAMPLE: u16 = 258;
const PHOTOMETRIC: u16 = 262;
const COLOR_MAP: u16 = 320;
const PALETTE: u64 = 3;
const BLACK_IS_ZERO: u64 = 1;
const REDUCED_RESOLUTION: u64 = 1;
const SHORT: u16 = 3;
const LONG: u16 = 4;
const LONG8: u16 = 16;

#[derive(Clone, Copy)]
struct Layout {
	little: bool,
	big: bool,
}

impl Layout {
	fn of(bytes: &[u8]) -> Option<Self> {
		let little = match bytes.get(..2)? {
			b"II" => true,
			b"MM" => false,
			_ => return None,
		};
		let layout = Self { little, big: false };
		match layout.read(bytes, 2, 2)? {
			42 => Some(layout),
			43 => Some(Self { little, big: true }),
			_ => None,
		}
	}

	fn read(self, bytes: &[u8], at: u64, width: usize) -> Option<u64> {
		let at = usize::try_from(at).ok()?;
		let field = bytes.get(at..at.checked_add(width)?)?;
		let fold = |value: u64, byte: &u8| value << 8 | u64::from(*byte);
		Some(if self.little {
			field.iter().rev().fold(0, fold)
		} else {
			field.iter().fold(0, fold)
		})
	}

	fn offset_width(self) -> usize {
		if self.big { 8 } else { 4 }
	}

	fn first_offset_at(self) -> u64 {
		if self.big { 8 } else { 4 }
	}

	fn write_first(self, bytes: &mut [u8], offset: u64) {
		self.write(
			bytes,
			self.first_offset_at() as usize,
			self.offset_width(),
			offset,
		);
	}

	fn write(self, bytes: &mut [u8], at: usize, width: usize, mut value: u64) {
		for index in 0..width {
			let byte = (value & 0xFF) as u8;
			value >>= 8;
			let position = if self.little {
				at + index
			} else {
				at + width - 1 - index
			};
			bytes[position] = byte;
		}
	}

	fn directory(self, bytes: &[u8], offset: u64) -> Option<Directory> {
		let (count_width, entry_width, value_at) = if self.big {
			(8, 20u64, 12)
		} else {
			(2, 12u64, 8)
		};
		let count = self.read(bytes, offset, count_width)?;
		let entries = offset.checked_add(count_width as u64)?;
		let mut directory = Directory::default();
		for index in 0..count {
			let entry = entries.checked_add(index.checked_mul(entry_width)?)?;
			let tag = self.read(bytes, entry, 2)? as u16;
			let field = entry + value_at;
			let kind = self.read(bytes, entry + 2, 2)? as u16;
			let value = || match kind {
				SHORT => self.read(bytes, field, 2),
				LONG => self.read(bytes, field, 4),
				LONG8 if self.big => self.read(bytes, field, 8),
				_ => None,
			};
			match tag {
				NEW_SUBFILE_TYPE => {
					directory.thumbnail =
						value().is_some_and(|value| value & REDUCED_RESOLUTION != 0);
				}
				BITS_PER_SAMPLE => directory.bits = value(),
				PHOTOMETRIC if kind == SHORT => {
					directory.photometric = Some((field, self.read(bytes, field, 2)?));
				}
				COLOR_MAP if kind == SHORT => {
					let count = self.read(bytes, entry + 4, self.offset_width())?;
					let at = if count * 2 <= self.offset_width() as u64 {
						field
					} else {
						self.read(bytes, field, self.offset_width())?
					};
					directory.color_map = Some((at, count));
				}
				_ => {}
			}
		}
		let next = entries.checked_add(count.checked_mul(entry_width)?)?;
		directory.next = self.read(bytes, next, self.offset_width())?;
		Some(directory)
	}

	/// The colours of an 8-bit palette image. The map holds every red, then every green,
	/// then every blue, as 16-bit values.
	fn palette(self, bytes: &[u8], directory: &Directory) -> Result<Vec<Rgb<u8>>> {
		let bits = directory.bits.unwrap_or(1);
		if bits != 8 {
			return Err(Error::Image(format!(
				"a palette TIFF with {bits} bits per sample"
			)));
		}
		let invalid = || Error::Image("a palette TIFF without a valid colour map".into());
		let (at, count) = directory.color_map.ok_or_else(invalid)?;
		if count != 3 * 256 {
			return Err(invalid());
		}
		let channel = |plane: u64, index: u64| {
			self.read(bytes, at.checked_add(2 * (plane * 256 + index))?, 2)
				.map(|value| (value >> 8) as u8)
		};
		(0..256)
			.map(|index| {
				Some(Rgb([
					channel(0, index)?,
					channel(1, index)?,
					channel(2, index)?,
				]))
			})
			.collect::<Option<_>>()
			.ok_or_else(invalid)
	}
}

#[derive(Default)]
struct Directory {
	next: u64,
	/// A reduced-resolution copy (a thumbnail) rather than a page.
	thumbnail: bool,
	bits: Option<u64>,
	/// Where the photometric interpretation is stored, and its value.
	photometric: Option<(u64, u64)>,
	/// Where the colour map starts, and its number of values.
	color_map: Option<(u64, u64)>,
}

fn colored(indices: DynamicImage, palette: &[Rgb<u8>]) -> DynamicImage {
	let indices = indices.into_luma8();
	let (width, height) = indices.dimensions();
	let mut image = RgbImage::new(width, height);
	for (pixel, index) in image.pixels_mut().zip(indices.pixels()) {
		*pixel = palette[usize::from(index.0[0])];
	}
	image.into()
}

/// Decodes every page of a TIFF in order, skipping thumbnails. A chain of directories that
/// is cut short or loops ends there; a TIFF whose header cannot be read is decoded as a
/// single image.
pub(crate) fn pages<T>(
	bytes: &[u8],
	mut decode: impl FnMut(&[u8]) -> Result<DynamicImage>,
	mut finish: impl FnMut(DynamicImage) -> Result<T>,
) -> Result<Vec<T>> {
	let Some(layout) = Layout::of(bytes) else {
		return Ok(vec![finish(decode(bytes)?)?]);
	};
	let Some(mut offset) = layout.read(bytes, layout.first_offset_at(), layout.offset_width())
	else {
		return Ok(vec![finish(decode(bytes)?)?]);
	};
	let mut copy = bytes.to_vec();
	let mut seen = HashSet::new();
	let mut pages = Vec::new();
	while offset != 0 && seen.insert(offset) {
		let Some(directory) = layout.directory(bytes, offset) else {
			break;
		};
		if !directory.thumbnail {
			if pages.len() == MAX_PAGES {
				return Err(Error::Image(format!(
					"a TIFF of more than {MAX_PAGES} pages"
				)));
			}
			layout.write_first(&mut copy, offset);
			pages.push(match directory.photometric {
				Some((at, PALETTE)) => {
					let palette = layout.palette(bytes, &directory)?;
					layout.write(&mut copy, at as usize, 2, BLACK_IS_ZERO);
					let page = colored(decode(&copy)?, &palette);
					layout.write(&mut copy, at as usize, 2, PALETTE);
					finish(page)?
				}
				_ => finish(decode(&copy)?)?,
			});
		}
		offset = directory.next;
	}
	if pages.is_empty() {
		pages.push(finish(decode(bytes)?)?);
	}
	Ok(pages)
}
