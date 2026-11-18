use std::io::Cursor;

use image::codecs::jpeg::JpegDecoder;
use image::{ExtendedColorType, ImageDecoder, ImageFormat, ImageReader};
use lopdf::{Dictionary, Document, Object, ObjectId, Stream, dictionary};

use crate::edit::save;
use crate::tiff;
use crate::{Error, Kind, Result, detect};

const DPI: f32 = 300.0;

pub(crate) struct Embedded {
	stream: Stream,
	width: u32,
	height: u32,
	exif_orientation: u8,
}

fn image_err(err: image::ImageError) -> Error {
	Error::Image(err.to_string())
}

fn xobject(width: u32, height: u32, color_space: &str) -> Dictionary {
	dictionary! {
		"Type" => "XObject",
		"Subtype" => "Image",
		"Width" => i64::from(width),
		"Height" => i64::from(height),
		"ColorSpace" => color_space,
		"BitsPerComponent" => 8,
	}
}

fn jpeg(bytes: &[u8]) -> Result<Embedded> {
	let mut decoder = JpegDecoder::new(Cursor::new(bytes)).map_err(image_err)?;
	let (width, height) = decoder.dimensions();
	let mut dict = match decoder.original_color_type() {
		ExtendedColorType::L8 => xobject(width, height, "DeviceGray"),
		ExtendedColorType::Rgb8 => xobject(width, height, "DeviceRGB"),
		ExtendedColorType::Cmyk8 => {
			let mut dict = xobject(width, height, "DeviceCMYK");
			let inverted: Vec<Object> = [1, 0, 1, 0, 1, 0, 1, 0]
				.into_iter()
				.map(Object::Integer)
				.collect();
			dict.set("Decode", inverted);
			dict
		}
		other => {
			return Err(Error::Image(format!(
				"unsupported JPEG colour type {other:?}"
			)));
		}
	};
	dict.set("Filter", "DCTDecode");
	Ok(Embedded {
		stream: Stream::new(dict, bytes.to_vec()).with_compression(false),
		width,
		height,
		exif_orientation: decoder
			.orientation()
			.map_or(1, |orientation| orientation.to_exif()),
	})
}

fn over_white(sample: u8, alpha: u8) -> u8 {
	let (sample, alpha) = (u32::from(sample), u32::from(alpha));
	((sample * alpha + 255 * (255 - alpha) + 127) / 255) as u8
}

fn decoded(bytes: &[u8], format: ImageFormat) -> Result<Embedded> {
	let image = ImageReader::with_format(Cursor::new(bytes), format)
		.decode()
		.map_err(image_err)?;
	let (width, height) = (image.width(), image.height());
	let color = image.color();
	let (color_space, samples) = match (color.has_color(), color.has_alpha()) {
		(true, false) => ("DeviceRGB", image.into_rgb8().into_raw()),
		(false, false) => ("DeviceGray", image.into_luma8().into_raw()),
		(true, true) => {
			let flat = image
				.into_rgba8()
				.pixels()
				.flat_map(|p| [0, 1, 2].map(|i| over_white(p[i], p[3])))
				.collect();
			("DeviceRGB", flat)
		}
		(false, true) => {
			let flat = image
				.into_luma_alpha8()
				.pixels()
				.map(|p| over_white(p[0], p[1]))
				.collect();
			("DeviceGray", flat)
		}
	};
	let mut stream = Stream::new(xobject(width, height, color_space), samples);
	stream.compress().map_err(|e| Error::Pdf(e.to_string()))?;
	Ok(Embedded {
		stream,
		width,
		height,
		exif_orientation: 1,
	})
}

pub(crate) fn image_to_pdf(bytes: &[u8]) -> Result<Vec<u8>> {
	let images = match detect(bytes) {
		Kind::Jpeg => vec![jpeg(bytes)?],
		Kind::Png => vec![decoded(bytes, ImageFormat::Png)?],
		Kind::Gif => vec![decoded(bytes, ImageFormat::Gif)?],
		Kind::Tiff => tiff::pages(bytes, |frame| decoded(frame, ImageFormat::Tiff))?,
		_ => return Err(Error::Image("not a PNG, JPEG, GIF or TIFF".into())),
	};
	let mut doc = Document::with_version("1.5");
	let pages = doc.new_object_id();
	let mut kids = Vec::with_capacity(images.len());
	for image in images {
		kids.push(page(&mut doc, pages, image)?.into());
	}
	let count = kids.len() as i64;
	doc.objects.insert(
		pages,
		dictionary! { "Type" => "Pages", "Kids" => kids, "Count" => count }.into(),
	);
	let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
	doc.trailer.set("Root", catalog);
	save(&mut doc)
}

fn page(doc: &mut Document, parent: ObjectId, embedded: Embedded) -> Result<ObjectId> {
	let Embedded {
		stream,
		width,
		height,
		exif_orientation,
	} = embedded;
	if width == 0 || height == 0 {
		return Err(Error::Image("empty image".into()));
	}
	let points = |pixels: u32| pixels as f32 * 72.0 / DPI;
	let (w, h) = match exif_orientation {
		5..=8 => (points(height), points(width)),
		_ => (points(width), points(height)),
	};
	let matrix = match exif_orientation {
		2 => [-w, 0.0, 0.0, h, w, 0.0],
		3 => [-w, 0.0, 0.0, -h, w, h],
		4 => [w, 0.0, 0.0, -h, 0.0, h],
		5 => [0.0, -h, -w, 0.0, w, h],
		6 => [0.0, -h, w, 0.0, 0.0, h],
		7 => [0.0, h, w, 0.0, 0.0, 0.0],
		8 => [0.0, h, -w, 0.0, w, 0.0],
		_ => [w, 0.0, 0.0, h, 0.0, 0.0],
	};
	let [a, b, c, d, e, f] = matrix;
	let content = format!("q {a:.4} {b:.4} {c:.4} {d:.4} {e:.4} {f:.4} cm /Im0 Do Q");
	let image = doc.add_object(stream);
	let content = doc.add_object(Stream::new(Dictionary::new(), content.into_bytes()));
	Ok(doc.add_object(dictionary! {
		"Type" => "Page",
		"Parent" => parent,
		"MediaBox" => vec![0.into(), 0.into(), w.into(), h.into()],
		"Resources" => dictionary! { "XObject" => dictionary! { "Im0" => image } },
		"Contents" => content,
	}))
}
