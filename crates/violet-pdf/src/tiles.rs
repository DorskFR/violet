use std::io::Cursor;

use image::{DynamicImage, GrayImage, ImageFormat};

use crate::{Error, Result};

const MAX_TILES: u32 = 6;
/// How far a cut may move from an even split, as a share of the tile height.
const SEARCH_SHARE: f32 = 0.2;
/// Rows on each side of a cut that count towards how empty it is, as a share of the height.
const GAP_SHARE: f32 = 0.004;

/// A page taller than it is wide is read in this many pieces, each about as tall as wide.
pub(crate) fn count(width: f32, height: f32) -> u32 {
	if width <= 0.0 || height <= 0.0 {
		return 1;
	}
	((height / width - 0.05).ceil() as u32).clamp(1, MAX_TILES)
}

/// The side that gives every one of `tiles` pieces of the page about `tile_pixels` pixels.
pub(crate) fn render_side(width: f32, height: f32, tiles: u32, tile_pixels: u32, max: u32) -> u32 {
	let scale =
		(f64::from(tile_pixels) * f64::from(tiles) / (f64::from(width) * f64::from(height))).sqrt();
	let side = (f64::from(width.max(height)) * scale).round();
	side.clamp(1.0, f64::from(max)) as u32
}

fn ink(image: &GrayImage) -> Vec<u64> {
	let background = {
		let mut histogram = [0u32; 256];
		for pixel in image.pixels() {
			histogram[usize::from(pixel[0])] += 1;
		}
		(0..=255u8)
			.max_by_key(|&level| histogram[usize::from(level)])
			.unwrap_or(255)
	};
	image
		.rows()
		.map(|row| {
			row.map(|pixel| u64::from(pixel[0].abs_diff(background)))
				.sum()
		})
		.collect()
}

/// The rows where the tiles start and end: `count + 1` values from 0 to the number of rows.
/// Each inner cut sits in the emptiest band of rows near an even split, so that it passes
/// between lines of text and not through one.
pub(crate) fn cuts(ink: &[u64], count: u32) -> Vec<u32> {
	let rows = ink.len();
	let mut sums = Vec::with_capacity(rows + 1);
	sums.push(0u64);
	for row in ink {
		sums.push(sums[sums.len() - 1] + row);
	}
	let gap = ((rows as f32 * GAP_SHARE).round() as usize).max(1);
	let around = |row: usize| sums[(row + gap).min(rows)] - sums[row.saturating_sub(gap)];
	let tile = rows as f32 / count as f32;
	let reach = (tile * SEARCH_SHARE) as usize;
	let mut cuts = vec![0];
	for index in 1..count {
		let even = (tile * index as f32).round() as usize;
		let lowest = (cuts[cuts.len() - 1] as usize + 1).max(even.saturating_sub(reach));
		let highest = (even + reach).min(rows.saturating_sub(1));
		let cut = (lowest..=highest)
			.min_by_key(|&row| (around(row), row.abs_diff(even)))
			.unwrap_or(even);
		if cut > cuts[cuts.len() - 1] as usize && cut < rows {
			cuts.push(cut as u32);
		}
	}
	cuts.push(rows as u32);
	cuts
}

/// Cuts the image into `count` pieces from top to bottom, as PNGs.
pub(crate) fn split(image: &DynamicImage, count: u32) -> Result<Vec<Vec<u8>>> {
	let cuts = cuts(&ink(&image.to_luma8()), count);
	cuts.windows(2)
		.map(|rows| {
			let mut png = Vec::new();
			image
				.crop_imm(0, rows[0], image.width(), rows[1] - rows[0])
				.write_to(&mut Cursor::new(&mut png), ImageFormat::Png)
				.map_err(|e| Error::Image(e.to_string()))?;
			Ok(png)
		})
		.collect()
}

#[cfg(test)]
mod tests {
	use image::{Luma, Rgb, RgbImage};

	use super::*;

	const WHITE: Rgb<u8> = Rgb([255, 255, 255]);
	const BLACK: Rgb<u8> = Rgb([0, 0, 0]);

	/// A white page with black text lines over the given row ranges.
	fn page(width: u32, height: u32, lines: &[(u32, u32)]) -> RgbImage {
		let mut image = RgbImage::from_pixel(width, height, WHITE);
		for &(from, to) in lines {
			for y in from..to {
				for x in width / 4..width / 2 {
					image.put_pixel(x, y, BLACK);
				}
			}
		}
		image
	}

	fn heights(tiles: &[Vec<u8>]) -> Vec<u32> {
		tiles
			.iter()
			.map(|png| image::load_from_memory(png).unwrap().height())
			.collect()
	}

	#[test]
	fn counts_tiles_from_the_shape() {
		assert_eq!(count(1500.0, 1061.0), 1);
		assert_eq!(count(1000.0, 1000.0), 1);
		assert_eq!(count(1000.0, 1040.0), 1);
		assert_eq!(count(1061.0, 1500.0), 2);
		assert_eq!(count(600.0, 1700.0), 3);
		assert_eq!(count(300.0, 9000.0), MAX_TILES);
		assert_eq!(count(0.0, 100.0), 1);
	}

	#[test]
	fn render_side_fills_the_pixel_budget() {
		let side = render_side(595.0, 842.0, 2, 2_000_000, 8192);
		let area = side as f32 * (side as f32 * 595.0 / 842.0);
		assert!((area - 4_000_000.0).abs() < 20_000.0, "{side}");
		assert_eq!(render_side(842.0, 595.0, 1, 2_000_000, 8192), 1682);
		assert_eq!(render_side(100.0, 10_000.0, 6, 2_500_000, 8192), 8192);
	}

	#[test]
	fn cuts_between_lines_not_through_them() {
		let image = page(200, 400, &[(20, 180), (186, 215), (240, 380)]);
		assert_eq!(
			cuts(&ink(&image::imageops::grayscale(&image)), 2),
			[0, 184, 400]
		);
	}

	#[test]
	fn cuts_evenly_when_nothing_is_emptier() {
		assert_eq!(cuts(&[0; 300], 3), [0, 100, 200, 300]);
		assert_eq!(cuts(&[7; 300], 2), [0, 150, 300]);
		assert_eq!(cuts(&[0; 300], 1), [0, 300]);
	}

	#[test]
	fn cuts_stay_ordered_on_tiny_images() {
		assert_eq!(cuts(&[0; 2], 6), [0, 1, 2]);
		assert_eq!(cuts(&[0; 1], 3), [0, 1]);
		assert_eq!(cuts(&[], 2), [0, 0]);
	}

	#[test]
	fn ink_is_measured_against_the_background() {
		let mut dark = GrayImage::from_pixel(10, 4, Luma([10]));
		dark.put_pixel(3, 2, Luma([250]));
		assert_eq!(ink(&dark), [0, 0, 240, 0]);
	}

	#[test]
	fn split_covers_the_image_once() {
		let image = DynamicImage::ImageRgb8(page(120, 500, &[(10, 150), (170, 330), (350, 490)]));
		let tiles = split(&image, 3).unwrap();
		assert_eq!(heights(&tiles), [167, 166, 167]);
		let mut joined = RgbImage::new(120, 500);
		let mut top = 0;
		for tile in &tiles {
			let tile = image::load_from_memory(tile).unwrap().into_rgb8();
			assert_eq!(tile.width(), 120);
			image::imageops::replace(&mut joined, &tile, 0, i64::from(top));
			top += tile.height();
		}
		assert_eq!(joined, image.into_rgb8());
	}

	#[test]
	fn split_is_deterministic() {
		let image = DynamicImage::ImageRgb8(page(90, 260, &[(5, 100), (140, 250)]));
		assert_eq!(split(&image, 2).unwrap(), split(&image, 2).unwrap());
	}
}
