use hayro::hayro_interpret::font::{Glyph, GlyphRun};
use hayro::hayro_interpret::hayro_cmap::{BfString, WritingMode};
use hayro::hayro_interpret::util::TransformExt;
use hayro::hayro_interpret::{
	BlendMode, ClipPath, Context, Device, DrawMode, DrawProps, Image, ImageDrawProps,
	InterpreterCache, InterpreterSettings, SoftMask, interpret_page,
};
use hayro::hayro_syntax::Pdf;
use hayro::kurbo::{BezPath, Point, Rect, Vec2};

const GLYPH_UNITS: f64 = 1000.0;
const UNKNOWN_ADVANCE: f64 = 500.0;
const LINE_BREAK_EM: f64 = 0.5;
const BACKTRACK_EM: f64 = 1.5;
const WORD_GAP_EM: f64 = 0.25;
const CJK_WORD_GAP_EM: f64 = 1.0;
const SCAN_IMAGE_COVERAGE: f64 = 0.5;
const SCAN_MIN_CHARS: usize = 64;

pub(crate) fn extract(pdf: &Pdf) -> Vec<Option<String>> {
	let cache = InterpreterCache::new();
	let settings = InterpreterSettings::default();
	pdf.pages()
		.iter()
		.map(|page| {
			let (width, height) = page.render_dimensions();
			let bbox = Rect::new(0.0, 0.0, width as f64, height as f64);
			let mut context = Context::new(
				page.initial_transform(true).to_kurbo(),
				bbox,
				&cache,
				page.xref(),
				settings.clone(),
			);
			let mut device = TextDevice::default();
			interpret_page(page, &mut context, &mut device);
			device.finish(bbox.area())
		})
		.collect()
}

struct Cursor {
	end: Point,
	direction: Vec2,
	size: f64,
}

#[derive(Default)]
struct TextDevice {
	text: String,
	cursor: Option<Cursor>,
	image_area: f64,
}

impl TextDevice {
	fn push(&mut self, piece: &str, origin: Point, advance: Vec2, size: f64) {
		let placed = size.is_finite() && size > 0.0 && origin.is_finite();
		if let (true, Some(last)) = (placed, &self.cursor) {
			let em = last.size.max(size);
			let delta = origin - last.end;
			let along = delta.dot(last.direction);
			let across = delta.cross(last.direction).abs();
			let gap = if is_cjk(self.text.chars().next_back()) && is_cjk(piece.chars().next()) {
				CJK_WORD_GAP_EM
			} else {
				WORD_GAP_EM
			};
			if across > LINE_BREAK_EM * em || along < -BACKTRACK_EM * em {
				self.separate('\n');
			} else if along > gap * em {
				self.separate(' ');
			}
		}
		self.text.push_str(piece);
		let length = advance.hypot();
		if placed && length.is_finite() && length > 0.0 {
			self.cursor = Some(Cursor {
				end: origin + advance,
				direction: advance / length,
				size,
			});
		}
	}

	fn separate(&mut self, separator: char) {
		while self.text.ends_with(' ') {
			self.text.pop();
		}
		if !self.text.is_empty() && !self.text.ends_with('\n') {
			self.text.push(separator);
		}
	}

	fn finish(self, page_area: f64) -> Option<String> {
		let (mut printable, mut unusable) = (0usize, 0usize);
		for c in self.text.chars().filter(|c| !c.is_whitespace()) {
			if is_unusable(c) {
				unusable += 1;
			} else {
				printable += 1;
			}
		}
		let scanned = self.image_area >= SCAN_IMAGE_COVERAGE * page_area;
		if printable == 0 || unusable * 2 > printable || (scanned && printable < SCAN_MIN_CHARS) {
			return None;
		}
		let text: String = self
			.text
			.chars()
			.filter(|&c| c == '\n' || !c.is_control())
			.collect();
		let lines: Vec<&str> = text
			.lines()
			.map(str::trim)
			.filter(|line| !line.is_empty())
			.collect();
		Some(lines.join("\n"))
	}
}

fn is_unusable(c: char) -> bool {
	c == char::REPLACEMENT_CHARACTER
		|| c.is_control()
		|| matches!(c as u32, 0xE000..=0xF8FF | 0xF_0000..=0x10_FFFF)
}

fn is_cjk(c: Option<char>) -> bool {
	c.is_some_and(|c| matches!(c as u32, 0x2E80..=0x9FFF | 0xAC00..=0xD7AF | 0xF900..=0xFAFF | 0xFF00..=0xFFEF | 0x2_0000..=0x3_FFFF))
}

impl<'a> Device<'a> for TextDevice {
	fn draw_path(&mut self, _: &BezPath, _: DrawProps<'a>, _: &DrawMode) {}

	fn push_clip_path(&mut self, _: &ClipPath) {}

	fn push_transparency_group(&mut self, _: f32, _: Option<SoftMask<'a>>, _: BlendMode) {}

	fn draw_glyph_run(&mut self, run: &GlyphRun<'_, 'a>, props: DrawProps<'a>, _: &DrawMode) {
		let vertical = matches!(run.writing_mode(), WritingMode::Vertical);
		for glyph in run.glyphs() {
			let transform = props.transform * glyph.transform();
			let origin = transform * Point::ZERO;
			let across = transform * Point::new(GLYPH_UNITS, 0.0) - origin;
			let down = transform * Point::new(0.0, -GLYPH_UNITS) - origin;
			let advance = if vertical {
				down
			} else {
				let width = match &**glyph {
					Glyph::Outline(outline) => outline.advance_width().map(f64::from),
					Glyph::Type3(_) => None,
				};
				across * (width.unwrap_or(UNKNOWN_ADVANCE) / GLYPH_UNITS)
			};
			let size = across.hypot().max(down.hypot());
			match glyph.as_unicode() {
				Some(BfString::Char(c)) => {
					self.push(c.encode_utf8(&mut [0; 4]), origin, advance, size)
				}
				Some(BfString::String(s)) => self.push(&s, origin, advance, size),
				None => self.push("\u{FFFD}", origin, advance, size),
			}
		}
	}

	fn draw_image(&mut self, image: Image<'a, '_>, props: ImageDrawProps<'a>) {
		let area =
			props.transform.determinant().abs() * image.width() as f64 * image.height() as f64;
		if area.is_finite() {
			self.image_area += area;
		}
	}

	fn pop_clip(&mut self) {}

	fn pop_transparency_group(&mut self) {}
}
