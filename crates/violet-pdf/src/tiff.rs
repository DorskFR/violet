//! The pages of a multi-page TIFF. `image` decodes only the first image file directory (IFD)
//! of a TIFF; every offset in a TIFF is absolute, so pointing the header at another IFD
//! makes that page the first one.

use std::collections::HashSet;

use crate::{Error, Result};

const MAX_PAGES: usize = 1000;
const NEW_SUBFILE_TYPE: u16 = 254;
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
		let at = self.first_offset_at() as usize;
		let width = self.offset_width();
		let mut value = offset;
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

	/// The offset of the next IFD and whether this one is a reduced-resolution copy
	/// (a thumbnail) rather than a page.
	fn directory(self, bytes: &[u8], offset: u64) -> Option<(u64, bool)> {
		let (count_width, entry_width, value_at) = if self.big {
			(8, 20u64, 12)
		} else {
			(2, 12u64, 8)
		};
		let count = self.read(bytes, offset, count_width)?;
		let entries = offset.checked_add(count_width as u64)?;
		let mut thumbnail = false;
		for index in 0..count {
			let entry = entries.checked_add(index.checked_mul(entry_width)?)?;
			if self.read(bytes, entry, 2)? as u16 != NEW_SUBFILE_TYPE {
				continue;
			}
			let value = match self.read(bytes, entry + 2, 2)? as u16 {
				SHORT => self.read(bytes, entry + value_at, 2),
				LONG => self.read(bytes, entry + value_at, 4),
				LONG8 if self.big => self.read(bytes, entry + value_at, 8),
				_ => None,
			};
			thumbnail = value.is_some_and(|value| value & REDUCED_RESOLUTION != 0);
		}
		let next = entries.checked_add(count.checked_mul(entry_width)?)?;
		Some((self.read(bytes, next, self.offset_width())?, thumbnail))
	}
}

/// Decodes every page of a TIFF in order, skipping thumbnails. A chain of directories that
/// is cut short or loops ends there; a TIFF whose header cannot be read is decoded as a
/// single image.
pub(crate) fn pages<T>(bytes: &[u8], mut decode: impl FnMut(&[u8]) -> Result<T>) -> Result<Vec<T>> {
	let Some(layout) = Layout::of(bytes) else {
		return Ok(vec![decode(bytes)?]);
	};
	let Some(mut offset) = layout.read(bytes, layout.first_offset_at(), layout.offset_width())
	else {
		return Ok(vec![decode(bytes)?]);
	};
	let mut copy = bytes.to_vec();
	let mut seen = HashSet::new();
	let mut pages = Vec::new();
	while offset != 0 && seen.insert(offset) {
		let Some((next, thumbnail)) = layout.directory(bytes, offset) else {
			break;
		};
		if !thumbnail {
			if pages.len() == MAX_PAGES {
				return Err(Error::Image(format!(
					"a TIFF of more than {MAX_PAGES} pages"
				)));
			}
			layout.write_first(&mut copy, offset);
			pages.push(decode(&copy)?);
		}
		offset = next;
	}
	if pages.is_empty() {
		pages.push(decode(bytes)?);
	}
	Ok(pages)
}
