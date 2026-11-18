use hayro::hayro_syntax::Pdf;
use image::codecs::jpeg::JpegEncoder;
use image::metadata::Orientation;
use image::{DynamicImage, ExtendedColorType, ImageFormat, Rgb, RgbImage, Rgba, RgbaImage};
use lopdf::{Dictionary, Document, Object, ObjectId, Stream, dictionary};
use violet_pdf::{
	Error, Kind, MAX_RENDER_SIDE, delete_pages, detect, image_to_pdf, merge, page_count,
	render_png, render_tiles, rotate, split, text_layer,
};

const WIDTH: i64 = 300;
const HEIGHT: i64 = 200;

struct Fixture {
	doc: Document,
	root: ObjectId,
	kids: Vec<Object>,
	resources: Dictionary,
	rotate: Option<i64>,
}

impl Fixture {
	fn new() -> Self {
		let mut doc = Document::with_version("1.5");
		let root = doc.new_object_id();
		let helvetica = doc.add_object(dictionary! {
			"Type" => "Font",
			"Subtype" => "Type1",
			"BaseFont" => "Helvetica",
			"Encoding" => "WinAnsiEncoding",
		});
		let resources = dictionary! { "Font" => dictionary! { "F1" => helvetica } };
		Self {
			doc,
			root,
			kids: Vec::new(),
			resources,
			rotate: None,
		}
	}

	fn resource(&mut self, category: &str, name: &str, object: impl Into<Object>) {
		let id = self.doc.add_object(object);
		if !self.resources.has(category.as_bytes()) {
			self.resources.set(category, Dictionary::new());
		}
		let entries = self
			.resources
			.get_mut(category.as_bytes())
			.unwrap()
			.as_dict_mut()
			.unwrap();
		entries.set(name, id);
	}

	fn page(&mut self, content: &str) -> ObjectId {
		self.page_with(content, Dictionary::new())
	}

	fn page_with(&mut self, content: &str, mut page: Dictionary) -> ObjectId {
		let content = self
			.doc
			.add_object(Stream::new(Dictionary::new(), content.as_bytes().to_vec()));
		page.set("Type", "Page");
		page.set("Parent", self.root);
		page.set("Contents", content);
		let id = self.doc.add_object(page);
		self.kids.push(id.into());
		id
	}

	fn finish(mut self) -> Vec<u8> {
		let mut pages = dictionary! {
			"Type" => "Pages",
			"Count" => self.kids.len() as i64,
			"Kids" => self.kids,
			"MediaBox" => vec![0.into(), 0.into(), WIDTH.into(), HEIGHT.into()],
			"Resources" => self.resources,
		};
		if let Some(rotate) = self.rotate {
			pages.set("Rotate", rotate);
		}
		self.doc.objects.insert(self.root, pages.into());
		let catalog = self
			.doc
			.add_object(dictionary! { "Type" => "Catalog", "Pages" => self.root });
		self.doc.trailer.set("Root", catalog);
		let mut out = Vec::new();
		self.doc.save_to(&mut out).unwrap();
		out
	}
}

fn label(text: &str) -> String {
	format!("BT /F1 24 Tf 20 150 Td ({text}) Tj ET")
}

fn text_pdf(labels: &[&str]) -> Vec<u8> {
	let mut fixture = Fixture::new();
	for text in labels {
		fixture.page(&label(text));
	}
	fixture.finish()
}

fn jpeg_bytes(image: &RgbImage) -> Vec<u8> {
	let mut out = Vec::new();
	JpegEncoder::new_with_quality(&mut out, 95)
		.encode(
			image.as_raw(),
			image.width(),
			image.height(),
			ExtendedColorType::Rgb8,
		)
		.unwrap();
	out
}

fn encoded(image: DynamicImage, format: ImageFormat) -> Vec<u8> {
	let mut out = std::io::Cursor::new(Vec::new());
	image.write_to(&mut out, format).unwrap();
	out.into_inner()
}

fn png_bytes(image: DynamicImage) -> Vec<u8> {
	encoded(image, ImageFormat::Png)
}

fn quadrants() -> RgbImage {
	RgbImage::from_fn(64, 32, |x, y| match (x < 32, y < 16) {
		(true, true) => Rgb([220, 30, 30]),
		(false, true) => Rgb([30, 200, 30]),
		(true, false) => Rgb([30, 30, 220]),
		(false, false) => Rgb([230, 230, 40]),
	})
}

fn with_exif_orientation(jpeg: &[u8], orientation: u8) -> Vec<u8> {
	let mut out = jpeg[..2].to_vec();
	out.extend_from_slice(&[0xFF, 0xE1, 0x00, 0x22]);
	out.extend_from_slice(b"Exif\0\0MM\0\x2A\0\0\0\x08\0\x01\x01\x12\0\x03\0\0\0\x01\0");
	out.extend_from_slice(&[orientation, 0, 0, 0, 0, 0, 0]);
	out.extend_from_slice(&jpeg[2..]);
	out
}

fn image_xobject(filter: &str, width: i64, height: i64, bits: i64, data: Vec<u8>) -> Stream {
	let dict = dictionary! {
		"Type" => "XObject",
		"Subtype" => "Image",
		"Width" => width,
		"Height" => height,
		"ColorSpace" => if bits == 1 { "DeviceGray" } else { "DeviceRGB" },
		"BitsPerComponent" => bits,
		"Filter" => filter,
	};
	Stream::new(dict, data).with_compression(false)
}

const FULL_PAGE_IMAGE: &str = "q 300 0 0 200 0 0 cm /Im0 Do Q";

fn jpeg_fixture() -> Fixture {
	let mut fixture = Fixture::new();
	let image = RgbImage::from_pixel(60, 40, Rgb([200, 40, 40]));
	fixture.resource(
		"XObject",
		"Im0",
		image_xobject("DCTDecode", 60, 40, 8, jpeg_bytes(&image)),
	);
	fixture
}

fn jpeg_pdf() -> Vec<u8> {
	let mut fixture = jpeg_fixture();
	fixture.page(FULL_PAGE_IMAGE);
	fixture.finish()
}

fn bits(pattern: &str) -> Vec<u8> {
	let bits: Vec<u8> = pattern
		.bytes()
		.filter(|b| matches!(b, b'0' | b'1'))
		.map(|b| b - b'0')
		.collect();
	bits.chunks(8)
		.map(|chunk| {
			chunk
				.iter()
				.enumerate()
				.fold(0, |byte, (i, bit)| byte | bit << (7 - i))
		})
		.collect()
}

fn black_square_group4() -> Vec<u8> {
	bits(&format!(
		"001 00110101 000101 {} 000000000001 000000000001",
		"11".repeat(7)
	))
}

fn bilevel_pdf(filter: &str, data: Vec<u8>, parms: Option<Dictionary>) -> Vec<u8> {
	let mut fixture = Fixture::new();
	let mut image = image_xobject(filter, 8, 8, 1, data);
	if let Some(parms) = parms {
		image.dict.set("DecodeParms", parms);
	}
	fixture.resource("XObject", "Im0", image);
	fixture.page(FULL_PAGE_IMAGE);
	fixture.finish()
}

fn ccitt_pdf() -> Vec<u8> {
	let parms = dictionary! { "K" => -1, "Columns" => 8, "Rows" => 8 };
	bilevel_pdf("CCITTFaxDecode", black_square_group4(), Some(parms))
}

fn jbig2_pdf() -> Vec<u8> {
	let mmr = black_square_group4();
	let mut data = vec![0, 0, 0, 0, 48, 0, 1, 0, 0, 0, 19];
	data.extend_from_slice(&[0, 0, 0, 8, 0, 0, 0, 8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
	data.extend_from_slice(&[0, 0, 0, 1, 38, 0, 1, 0, 0, 0, 18 + mmr.len() as u8]);
	data.extend_from_slice(&[0, 0, 0, 8, 0, 0, 0, 8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
	data.extend_from_slice(&mmr);
	bilevel_pdf("JBIG2Decode", data, None)
}

fn cid_font(doc: &mut Document, to_unicode: Option<&[(u16, u32)]>) -> Dictionary {
	let descriptor = doc.add_object(dictionary! {
		"Type" => "FontDescriptor",
		"FontName" => "Mincho",
		"Flags" => 4,
		"FontBBox" => vec![0.into(), (-200).into(), 1000.into(), 900.into()],
		"ItalicAngle" => 0,
		"Ascent" => 880,
		"Descent" => -120,
		"CapHeight" => 700,
		"StemV" => 80,
	});
	let descendant = doc.add_object(dictionary! {
		"Type" => "Font",
		"Subtype" => "CIDFontType2",
		"BaseFont" => "Mincho",
		"CIDSystemInfo" => dictionary! {
			"Registry" => Object::string_literal("Adobe"),
			"Ordering" => Object::string_literal("Identity"),
			"Supplement" => 0,
		},
		"FontDescriptor" => descriptor,
		"DW" => 1000,
	});
	let mut font = dictionary! {
		"Type" => "Font",
		"Subtype" => "Type0",
		"BaseFont" => "Mincho",
		"Encoding" => "Identity-H",
		"DescendantFonts" => vec![descendant.into()],
	};
	if let Some(map) = to_unicode {
		let entries: String = map
			.iter()
			.map(|(cid, unicode)| format!("<{cid:04X}> <{unicode:04X}>\n"))
			.collect();
		let cmap = format!(
			"/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n\
			 /CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n\
			 /CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n\
			 1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n\
			 {} beginbfchar\n{entries}endbfchar\n\
			 endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n",
			map.len()
		);
		let cmap = doc.add_object(Stream::new(Dictionary::new(), cmap.into_bytes()));
		font.set("ToUnicode", cmap);
	}
	font
}

const JAPANESE: &str = "請求書 東京都渋谷区 ご請求金額は一万円です。";

fn cid_pdf(to_unicode: impl Fn(u16, char) -> Option<u32>) -> Vec<u8> {
	let mut fixture = Fixture::new();
	let cids: Vec<(u16, char)> = JAPANESE
		.chars()
		.enumerate()
		.map(|(i, c)| (i as u16 + 1, c))
		.collect();
	let map: Vec<(u16, u32)> = cids
		.iter()
		.filter_map(|&(cid, c)| Some((cid, to_unicode(cid, c)?)))
		.collect();
	let font = cid_font(&mut fixture.doc, (!map.is_empty()).then_some(&map));
	fixture.resource("Font", "F2", font);
	let hex: String = cids.iter().map(|(cid, _)| format!("{cid:04X}")).collect();
	fixture.page(&format!("BT /F2 10 Tf 10 100 Td <{hex}> Tj ET"));
	fixture.finish()
}

fn decode(png: &[u8]) -> RgbImage {
	assert_eq!(detect(png), Kind::Png);
	image::load_from_memory(png).unwrap().to_rgb8()
}

fn near(pixel: &Rgb<u8>, expected: [u8; 3]) -> bool {
	pixel
		.0
		.iter()
		.zip(expected)
		.all(|(&a, b)| a.abs_diff(b) <= 40)
}

fn reopen(pdf: &[u8]) -> u32 {
	let lopdf_pages = Document::load_mem(pdf).unwrap().get_pages().len();
	let hayro_pages = Pdf::new(pdf.to_vec()).unwrap().pages().len();
	assert_eq!(lopdf_pages, hayro_pages);
	assert_eq!(page_count(pdf).unwrap() as usize, lopdf_pages);
	for page in 1..=lopdf_pages as u32 {
		render_png(pdf, page, 32).unwrap();
	}
	lopdf_pages as u32
}

fn texts(pdf: &[u8]) -> Vec<String> {
	text_layer(pdf)
		.unwrap()
		.into_iter()
		.map(Option::unwrap_or_default)
		.collect()
}

fn page_dict(pdf: &[u8], page: u32) -> Dictionary {
	let doc = Document::load_mem(pdf).unwrap();
	doc.get_dictionary(doc.get_pages()[&page]).unwrap().clone()
}

fn rotation(pdf: &[u8], page: u32) -> i64 {
	page_dict(pdf, page)
		.get(b"Rotate")
		.and_then(Object::as_i64)
		.unwrap_or(0)
}

#[test]
fn detects_kinds_from_magic_bytes() {
	assert_eq!(detect(&text_pdf(&["a"])), Kind::Pdf);
	assert_eq!(detect(b"\n\njunk before header %PDF-1.4"), Kind::Pdf);
	assert_eq!(detect(&jpeg_bytes(&quadrants())), Kind::Jpeg);
	assert_eq!(detect(&png_bytes(quadrants().into())), Kind::Png);
	assert_eq!(
		detect(&encoded(quadrants().into(), ImageFormat::Gif)),
		Kind::Gif
	);
	assert_eq!(detect(b"II*\0tiff"), Kind::Other);
	assert_eq!(detect(b""), Kind::Other);
	assert_eq!(detect(b"PK\x03\x04"), Kind::Other);
	assert_eq!(detect(b"%PDF"), Kind::Other);
}

#[test]
fn counts_pages() {
	assert_eq!(page_count(&text_pdf(&["one"])).unwrap(), 1);
	assert_eq!(page_count(&text_pdf(&["one", "two", "three"])).unwrap(), 3);
	assert!(matches!(page_count(b"not a pdf"), Err(Error::Pdf(_))));
	assert!(matches!(page_count(&text_pdf(&[])), Err(Error::Pdf(_))));
}

#[test]
fn renders_text_page_at_requested_size() {
	let pdf = text_pdf(&["Hello"]);
	for max_side in [1, 77, 300, 1000] {
		let image = decode(&render_png(&pdf, 1, max_side).unwrap());
		assert_eq!(image.width(), max_side);
		assert_eq!(
			image.height(),
			(max_side as f32 * 2.0 / 3.0).round().max(1.0) as u32,
			"max_side {max_side}"
		);
	}
	let image = decode(&render_png(&pdf, 1, 300).unwrap());
	assert!(near(image.get_pixel(290, 190), [255, 255, 255]));
	let ink = image.pixels().filter(|p| near(p, [0, 0, 0])).count();
	assert!(ink > 100, "text was not drawn ({ink} dark pixels)");
}

#[test]
fn render_rejects_bad_arguments() {
	let pdf = text_pdf(&["a", "b"]);
	assert!(matches!(
		render_png(&pdf, 0, 100),
		Err(Error::PageOutOfRange(0, 2))
	));
	assert!(matches!(
		render_png(&pdf, 3, 100),
		Err(Error::PageOutOfRange(3, 2))
	));
	assert!(matches!(render_png(&pdf, 1, 0), Err(Error::Invalid(_))));
	assert!(matches!(
		render_png(&pdf, 1, MAX_RENDER_SIDE + 1),
		Err(Error::Invalid(_))
	));
	assert!(matches!(render_png(b"garbage", 1, 100), Err(Error::Pdf(_))));
}

fn tall_pdf() -> Vec<u8> {
	let mut fixture = Fixture::new();
	fixture.page_with(
		"BT /F1 24 Tf 20 560 Td (Top) Tj ET BT /F1 24 Tf 20 40 Td (Bottom) Tj ET",
		dictionary! { "MediaBox" => vec![0.into(), 0.into(), 300.into(), 600.into()] },
	);
	fixture.finish()
}

fn dark_pixels(image: &RgbImage) -> usize {
	image.pixels().filter(|p| near(p, [0, 0, 0])).count()
}

#[test]
fn a_wide_page_is_one_tile_of_the_requested_size() {
	let tiles = render_tiles(&text_pdf(&["Hello"]), 1, 60_000).unwrap();
	assert_eq!(tiles.len(), 1);
	let image = decode(&tiles[0]);
	assert_eq!((image.width(), image.height()), (300, 200));
}

#[test]
fn a_tall_page_is_cut_into_tiles_that_cover_it() {
	let pdf = tall_pdf();
	let tiles: Vec<RgbImage> = render_tiles(&pdf, 1, 45_000)
		.unwrap()
		.iter()
		.map(|tile| decode(tile))
		.collect();
	assert_eq!(tiles.len(), 2);
	let whole = decode(&render_png(&pdf, 1, 424).unwrap());
	assert_eq!((whole.width(), whole.height()), (212, 424));
	assert!(tiles.iter().all(|tile| tile.width() == 212));
	assert_eq!(tiles.iter().map(RgbImage::height).sum::<u32>(), 424);
	for tile in &tiles {
		assert!((170..=254).contains(&tile.height()), "{}", tile.height());
		assert!(dark_pixels(tile) > 50, "a tile without its text");
	}
	assert_eq!(
		tiles.iter().map(dark_pixels).sum::<usize>(),
		dark_pixels(&whole)
	);
}

#[test]
fn render_tiles_rejects_bad_arguments() {
	let pdf = text_pdf(&["a"]);
	assert!(matches!(render_tiles(&pdf, 1, 0), Err(Error::Invalid(_))));
	assert!(matches!(
		render_tiles(&pdf, 2, 1000),
		Err(Error::PageOutOfRange(2, 1))
	));
	assert!(matches!(
		render_tiles(b"garbage", 1, 1000),
		Err(Error::Pdf(_))
	));
}

#[test]
fn render_honours_rotation() {
	let mut fixture = Fixture::new();
	fixture.page_with("0 0 100 100 re f", dictionary! { "Rotate" => 90 });
	let image = decode(&render_png(&fixture.finish(), 1, 300).unwrap());
	assert_eq!((image.width(), image.height()), (200, 300));
	assert!(near(image.get_pixel(20, 20), [0, 0, 0]));
	assert!(near(image.get_pixel(20, 280), [255, 255, 255]));
	assert!(near(image.get_pixel(180, 20), [255, 255, 255]));
}

#[test]
fn renders_scanned_image_formats() {
	let jpeg = decode(&render_png(&jpeg_pdf(), 1, 150).unwrap());
	assert!(near(jpeg.get_pixel(75, 50), [200, 40, 40]));
	for (name, pdf) in [("ccitt", ccitt_pdf()), ("jbig2", jbig2_pdf())] {
		let image = decode(&render_png(&pdf, 1, 150).unwrap());
		for (x, y) in [(10, 10), (75, 50), (140, 90)] {
			assert!(
				near(image.get_pixel(x, y), [0, 0, 0]),
				"{name} not decoded at {x},{y}"
			);
		}
	}
}

#[test]
fn text_layer_reads_words_and_lines() {
	let mut fixture = Fixture::new();
	fixture.page(
		"BT /F1 12 Tf 20 150 Td (Invoice) Tj ( 2024) Tj ET \
		 BT /F1 12 Tf 20 120 Td (Total) Tj 80 0 Td (42) Tj 0 -20 Td [(Hel) -20 (lo) -400 (there)] TJ ET",
	);
	fixture.page("");
	fixture.page(&label("second"));
	assert_eq!(
		text_layer(&fixture.finish()).unwrap(),
		[
			Some("Invoice 2024\nTotal 42\nHello there".to_string()),
			None,
			Some("second".to_string())
		]
	);
}

#[test]
fn text_layer_keeps_japanese_with_to_unicode() {
	let pdf = cid_pdf(|_, c| Some(c as u32));
	assert_eq!(text_layer(&pdf).unwrap(), [Some(JAPANESE.to_string())]);
}

#[test]
fn text_layer_rejects_unmappable_fonts() {
	assert_eq!(text_layer(&cid_pdf(|_, _| None)).unwrap(), [None]);
	assert_eq!(
		text_layer(&cid_pdf(|cid, _| Some(0xE000 + cid as u32))).unwrap(),
		[None]
	);
	assert_eq!(
		text_layer(&cid_pdf(|cid, _| Some(cid as u32))).unwrap(),
		[None]
	);
}

#[test]
fn text_layer_sends_scans_to_ocr() {
	let sentence = "This scanned page carries a complete recognised text layer under the image.";
	let mut fixture = jpeg_fixture();
	fixture.page(FULL_PAGE_IMAGE);
	fixture.page(&format!("{FULL_PAGE_IMAGE} BT /F1 8 Tf 280 5 Td (1) Tj ET"));
	fixture.page(&format!(
		"{FULL_PAGE_IMAGE} BT 3 Tr /F1 6 Tf 5 100 Td ({sentence}) Tj ET"
	));
	assert_eq!(
		text_layer(&fixture.finish()).unwrap(),
		[None, None, Some(sentence.to_string())]
	);
}

#[test]
fn text_layer_survives_broken_page_content() {
	let mut fixture = Fixture::new();
	fixture.page("BT /F1 12 Tf 20 150 Td (unterminated");
	fixture.page("\u{0}\u{1}((( BT ]] >> /F1 Tf <zz> Tj [ ET");
	fixture.page(&label("intact"));
	assert_eq!(
		text_layer(&fixture.finish()).unwrap(),
		[None, None, Some("intact".to_string())]
	);
	assert!(matches!(text_layer(b"garbage"), Err(Error::Pdf(_))));
}

#[test]
fn split_keeps_order_and_inherited_attributes() {
	let mut fixture = Fixture::new();
	fixture.rotate = Some(90);
	for text in ["one", "two", "three", "four"] {
		fixture.page(&label(text));
	}
	let pdf = fixture.finish();
	let before = pdf.clone();
	let parts = split(&pdf, &[vec![3, 1], vec![2], vec![4, 4, 2]]).unwrap();
	assert_eq!(pdf, before);
	assert_eq!(parts.len(), 3);
	let expected: [&[&str]; 3] = [&["three", "one"], &["two"], &["four", "four", "two"]];
	for (part, expected) in parts.iter().zip(expected) {
		assert_eq!(reopen(part) as usize, expected.len());
		assert_eq!(texts(part), expected);
		for page in 1..=expected.len() as u32 {
			let dict = page_dict(part, page);
			assert_eq!(dict.get(b"Rotate").unwrap().as_i64().unwrap(), 90);
			assert!(dict.has(b"MediaBox") && dict.has(b"Resources"));
			let image = decode(&render_png(part, page, 300).unwrap());
			assert_eq!((image.width(), image.height()), (200, 300));
		}
	}
}

#[test]
fn split_output_is_self_contained_and_small() {
	const IMAGE_BYTES: usize = 48 * 48 * 3;
	let mut fixture = Fixture::new();
	let mut seed = 0x2545_F491_4F6C_DD1Du64;
	for index in 0..6 {
		let noise = (0..IMAGE_BYTES).map(|_| random(&mut seed) as u8).collect();
		let image = Stream::new(
			dictionary! {
				"Type" => "XObject", "Subtype" => "Image", "Width" => 48, "Height" => 48,
				"ColorSpace" => "DeviceRGB", "BitsPerComponent" => 8,
			},
			noise,
		);
		fixture.resource(
			"XObject",
			&format!("Im{index}"),
			image.with_compression(false),
		);
	}
	let pages: Vec<ObjectId> = (0..6)
		.map(|index| {
			fixture.page(&format!(
				"q 100 0 0 100 0 0 cm /Im{index} Do Q {}",
				label(&format!("page {index}"))
			))
		})
		.collect();
	let link = fixture.doc.add_object(dictionary! {
		"Type" => "Annot",
		"Subtype" => "Link",
		"Rect" => vec![0.into(), 0.into(), 10.into(), 10.into()],
		"Dest" => vec![pages[5].into(), "Fit".into()],
	});
	fixture
		.doc
		.get_dictionary_mut(pages[1])
		.unwrap()
		.set("Annots", vec![link.into()]);
	let pdf = fixture.finish();
	assert!(pdf.len() > 6 * IMAGE_BYTES);

	let parts = split(&pdf, &[vec![2], vec![1, 6]]).unwrap();
	assert!(
		parts[0].len() < IMAGE_BYTES + 4096,
		"single page is {} bytes",
		parts[0].len()
	);
	assert!(parts[1].len() < 2 * IMAGE_BYTES + 4096);
	assert_eq!(reopen(&parts[0]), 1);
	assert_eq!(reopen(&parts[1]), 2);
	assert_eq!(texts(&parts[0]), ["page 1"]);
	assert_eq!(texts(&parts[1]), ["page 0", "page 5"]);
	let rendered = decode(&render_png(&parts[0], 1, 300).unwrap());
	assert_eq!(rendered, decode(&render_png(&pdf, 2, 300).unwrap()));
}

#[test]
fn split_rejects_bad_groups() {
	let pdf = text_pdf(&["a", "b"]);
	assert!(split(&pdf, &[]).unwrap().is_empty());
	assert!(matches!(
		split(&pdf, &[vec![1], vec![]]),
		Err(Error::Invalid(_))
	));
	assert!(matches!(
		split(&pdf, &[vec![1, 3]]),
		Err(Error::PageOutOfRange(3, 2))
	));
	assert!(matches!(
		split(&pdf, &[vec![0]]),
		Err(Error::PageOutOfRange(0, 2))
	));
	assert!(matches!(split(b"garbage", &[vec![1]]), Err(Error::Pdf(_))));
}

#[test]
fn merge_concatenates_despite_object_id_collisions() {
	let first = text_pdf(&["a1", "a2"]);
	let second = text_pdf(&["b1"]);
	let image = jpeg_pdf();
	let merged = merge(&[&first, &second, &image, &first]).unwrap();
	assert_eq!(reopen(&merged), 6);
	assert_eq!(texts(&merged), ["a1", "a2", "b1", "", "a1", "a2"]);
	let rendered = decode(&render_png(&merged, 4, 150).unwrap());
	assert!(near(rendered.get_pixel(75, 50), [200, 40, 40]));
	assert_eq!(texts(&merge(&[&second]).unwrap()), ["b1"]);
	assert!(matches!(merge(&[]), Err(Error::Invalid(_))));
	assert!(matches!(merge(&[&first, b"garbage"]), Err(Error::Pdf(_))));
}

#[test]
fn rotate_adds_to_existing_rotation() {
	let mut fixture = Fixture::new();
	fixture.rotate = Some(90);
	fixture.page(&label("inherits"));
	fixture.page_with(&label("own"), dictionary! { "Rotate" => 270 });
	fixture.page_with(&label("odd"), dictionary! { "Rotate" => -450 });
	let pdf = fixture.finish();

	let turned = rotate(&pdf, &[1, 2, 2], 90).unwrap();
	assert_eq!(reopen(&turned), 3);
	assert_eq!(
		[1, 2, 3].map(|page| rotation(&turned, page)),
		[180, 0, -450]
	);
	assert_eq!(texts(&turned), ["inherits", "own", "odd"]);
	let image = decode(&render_png(&turned, 1, 300).unwrap());
	assert_eq!((image.width(), image.height()), (300, 200));

	let back = rotate(&pdf, &[1, 3], -180).unwrap();
	assert_eq!([1, 2, 3].map(|page| rotation(&back, page)), [270, 270, 90]);
	assert_eq!(rotation(&rotate(&pdf, &[2], 810).unwrap(), 2), 0);
	assert_eq!(rotation(&rotate(&pdf, &[1], 0).unwrap(), 1), 90);

	assert!(matches!(rotate(&pdf, &[1], 45), Err(Error::Invalid(_))));
	assert!(matches!(
		rotate(&pdf, &[4], 90),
		Err(Error::PageOutOfRange(4, 3))
	));
	assert!(matches!(rotate(b"garbage", &[1], 90), Err(Error::Pdf(_))));
}

#[test]
fn deletes_pages() {
	let pdf = text_pdf(&["one", "two", "three"]);
	let kept = delete_pages(&pdf, &[2, 2]).unwrap();
	assert_eq!(reopen(&kept), 2);
	assert_eq!(texts(&kept), ["one", "three"]);
	assert_eq!(
		texts(&delete_pages(&pdf, &[]).unwrap()),
		["one", "two", "three"]
	);
	assert_eq!(texts(&delete_pages(&pdf, &[3, 1]).unwrap()), ["two"]);
	assert!(matches!(
		delete_pages(&pdf, &[1, 2, 3]),
		Err(Error::Invalid(_))
	));
	assert!(matches!(
		delete_pages(&pdf, &[4]),
		Err(Error::PageOutOfRange(4, 3))
	));
	assert!(matches!(delete_pages(b"garbage", &[1]), Err(Error::Pdf(_))));
}

#[test]
fn wraps_jpeg_without_reencoding() {
	let jpeg = jpeg_bytes(&quadrants());
	let pdf = image_to_pdf(&jpeg).unwrap();
	assert_eq!(reopen(&pdf), 1);
	assert_eq!(text_layer(&pdf).unwrap(), [None]);
	let doc = Document::load_mem(&pdf).unwrap();
	let embedded: Vec<&Stream> = doc
		.objects
		.values()
		.filter_map(|o| o.as_stream().ok())
		.filter(|s| s.dict.has(b"Subtype"))
		.collect();
	assert_eq!(embedded.len(), 1);
	assert_eq!(
		embedded[0].dict.get(b"Filter").unwrap().as_name().unwrap(),
		b"DCTDecode"
	);
	assert_eq!(embedded[0].content, jpeg);
	let media_box: Vec<f32> = page_dict(&pdf, 1)
		.get(b"MediaBox")
		.unwrap()
		.as_array()
		.unwrap()
		.iter()
		.map(|v| v.as_float().unwrap())
		.collect();
	assert_eq!(
		media_box,
		[0.0, 0.0, 64.0 * 72.0 / 300.0, 32.0 * 72.0 / 300.0]
	);

	let gray = DynamicImage::from(quadrants()).into_luma8();
	let mut gray_jpeg = Vec::new();
	JpegEncoder::new(&mut gray_jpeg)
		.encode(gray.as_raw(), 64, 32, ExtendedColorType::L8)
		.unwrap();
	let rendered = decode(&render_png(&image_to_pdf(&gray_jpeg).unwrap(), 1, 64).unwrap());
	let expected = gray.get_pixel(48, 8).0[0];
	assert!(near(rendered.get_pixel(48, 8), [expected; 3]));
}

#[test]
fn wraps_gif_as_a_decoded_image() {
	let image = quadrants();
	let pdf = image_to_pdf(&encoded(image.clone().into(), ImageFormat::Gif)).unwrap();
	assert_eq!(reopen(&pdf), 1);
	let rendered = decode(&render_png(&pdf, 1, 64).unwrap());
	assert_eq!(rendered.dimensions(), image.dimensions());
	for (x, y) in [(8, 8), (48, 8), (8, 24), (48, 24)] {
		assert!(
			near(rendered.get_pixel(x, y), image.get_pixel(x, y).0),
			"at {x},{y}"
		);
	}
	assert!(matches!(
		image_to_pdf(b"GIF89a broken"),
		Err(Error::Image(_))
	));
}

#[test]
fn wraps_jpeg_following_exif_orientation() {
	let jpeg = jpeg_bytes(&quadrants());
	for exif in 1..=8 {
		let pdf = image_to_pdf(&with_exif_orientation(&jpeg, exif)).unwrap();
		let mut expected = DynamicImage::from(quadrants());
		expected.apply_orientation(Orientation::from_exif(exif).unwrap());
		let expected = expected.to_rgb8();
		let rendered = decode(&render_png(&pdf, 1, 64).unwrap());
		assert_eq!(
			rendered.dimensions(),
			expected.dimensions(),
			"orientation {exif}"
		);
		let (w, h) = expected.dimensions();
		for (x, y) in [
			(w / 4, h / 4),
			(3 * w / 4, h / 4),
			(w / 4, 3 * h / 4),
			(3 * w / 4, 3 * h / 4),
		] {
			let want = expected.get_pixel(x, y);
			assert!(
				near(rendered.get_pixel(x, y), want.0),
				"orientation {exif} at {x},{y}"
			);
		}
	}
}

#[test]
fn wraps_png_as_flate() {
	let pdf = image_to_pdf(&png_bytes(quadrants().into())).unwrap();
	assert_eq!(reopen(&pdf), 1);
	let doc = Document::load_mem(&pdf).unwrap();
	let image = doc
		.objects
		.values()
		.filter_map(|o| o.as_stream().ok())
		.find(|s| s.dict.has(b"Subtype"))
		.unwrap();
	assert_eq!(
		image.dict.get(b"Filter").unwrap().as_name().unwrap(),
		b"FlateDecode"
	);
	assert_eq!(
		image.decompressed_content().unwrap(),
		quadrants().into_raw()
	);
	assert_eq!(decode(&render_png(&pdf, 1, 64).unwrap()), quadrants());

	let translucent =
		RgbaImage::from_fn(20, 20, |x, _| Rgba([0, 0, 0, if x < 10 { 255 } else { 0 }]));
	let rendered = decode(
		&render_png(
			&image_to_pdf(&png_bytes(translucent.into())).unwrap(),
			1,
			20,
		)
		.unwrap(),
	);
	assert!(near(rendered.get_pixel(4, 10), [0, 0, 0]));
	assert!(near(rendered.get_pixel(15, 10), [255, 255, 255]));

	let gray = DynamicImage::from(quadrants()).into_luma8();
	let rendered = decode(
		&render_png(
			&image_to_pdf(&png_bytes(gray.clone().into())).unwrap(),
			1,
			64,
		)
		.unwrap(),
	);
	assert_eq!(rendered, DynamicImage::from(gray).to_rgb8());
}

#[test]
fn image_to_pdf_rejects_non_images() {
	let jpeg = jpeg_bytes(&quadrants());
	let png = png_bytes(quadrants().into());
	assert!(matches!(image_to_pdf(b""), Err(Error::Image(_))));
	assert!(matches!(
		image_to_pdf(&text_pdf(&["a"])),
		Err(Error::Image(_))
	));
	assert!(matches!(image_to_pdf(&jpeg[..10]), Err(Error::Image(_))));
	assert!(matches!(
		image_to_pdf(&png[..png.len() / 2]),
		Err(Error::Image(_))
	));
}

fn random(seed: &mut u64) -> u64 {
	*seed ^= *seed << 13;
	*seed ^= *seed >> 7;
	*seed ^= *seed << 17;
	*seed
}

fn exercise(bytes: &[u8]) -> bool {
	let _ = detect(bytes);
	let _ = image_to_pdf(bytes);
	[
		page_count(bytes).is_ok(),
		render_png(bytes, 1, 48).is_ok(),
		text_layer(bytes).is_ok(),
		split(bytes, &[vec![1]]).is_ok(),
		merge(&[bytes, bytes]).is_ok(),
		rotate(bytes, &[1], 90).is_ok(),
		delete_pages(bytes, &[2]).is_ok(),
	]
	.contains(&true)
}

fn mixed_pdf() -> Vec<u8> {
	merge(&[
		&text_pdf(&["alpha", "beta"]),
		&jpeg_pdf(),
		&cid_pdf(|_, c| Some(c as u32)),
		&ccitt_pdf(),
	])
	.unwrap()
}

#[test]
fn garbage_is_an_error() {
	let mut seed = 0x9E37_79B9_7F4A_7C15u64;
	for length in [0, 1, 7, 64, 1000, 20_000] {
		let noise: Vec<u8> = (0..length).map(|_| random(&mut seed) as u8).collect();
		assert!(!exercise(&noise), "random bytes accepted as a PDF");
		let headed = [b"%PDF-1.7\n".as_slice(), &noise].concat();
		assert!(
			!exercise(&headed),
			"random bytes behind a PDF header accepted"
		);
	}
}

#[test]
fn truncated_and_corrupted_input_does_not_panic() {
	let pdf = mixed_pdf();
	for step in 0..48 {
		exercise(&pdf[..pdf.len() * step / 48]);
	}
	let mut seed = 0xD1B5_4A32_D192_ED03u64;
	for _ in 0..150 {
		let mut damaged = pdf.clone();
		for _ in 0..=random(&mut seed) % 8 {
			let at = random(&mut seed) as usize % damaged.len();
			damaged[at] = random(&mut seed) as u8;
		}
		exercise(&damaged);
	}
}

#[test]
fn hostile_structures_do_not_overflow_the_stack() {
	const DEPTH: usize = 200_000;
	let nested_arrays = format!("{}{}", "[".repeat(DEPTH), "]".repeat(DEPTH));
	let nested_dicts = format!("{}0{}", "<</A ".repeat(DEPTH), ">>".repeat(DEPTH));
	for payload in [nested_arrays, nested_dicts] {
		let object = format!(
			"%PDF-1.4\n1 0 obj\n<</Type /Catalog /Pages 2 0 R /X {payload}>>\nendobj\n\
			 2 0 obj\n<</Type /Pages /Kids [3 0 R] /Count 1>>\nendobj\n\
			 3 0 obj\n<</Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] /Contents 4 0 R>>\nendobj\n\
			 4 0 obj\n<</Length {}>>\nstream\n{payload} n\nendstream\nendobj\n\
			 trailer\n<</Root 1 0 R>>\n%%EOF\n",
			payload.len() + 2
		);
		exercise(object.as_bytes());
	}

	let mut chain = String::from("%PDF-1.4\n1 0 obj\n<</Type /Catalog /Pages 2 0 R>>\nendobj\n");
	let levels = 5000;
	for id in 2..levels {
		chain += &format!(
			"{id} 0 obj\n<</Type /Pages /Parent {} 0 R /Kids [{} 0 R] /Count 1>>\nendobj\n",
			id - 1,
			id + 1
		);
	}
	chain += &format!(
		"{levels} 0 obj\n<</Type /Page /Parent {} 0 R /MediaBox [0 0 10 10]>>\nendobj\n",
		levels - 1
	);
	chain += "trailer\n<</Root 1 0 R>>\n%%EOF\n";
	exercise(chain.as_bytes());

	let cyclic = "%PDF-1.4\n1 0 obj\n<</Type /Catalog /Pages 2 0 R>>\nendobj\n\
		2 0 obj\n<</Type /Pages /Parent 2 0 R /Kids [2 0 R 3 0 R 2 0 R] /Count 3 /Resources 2 0 R>>\nendobj\n\
		3 0 obj\n<</Type /Page /Parent 3 0 R /Contents 3 0 R /Annots [3 0 R 2 0 R]>>\nendobj\n\
		trailer\n<</Root 1 0 R>>\n%%EOF\n";
	exercise(cyclic.as_bytes());
}

fn edits_cleanly(pdf: &[u8]) {
	let merged = merge(&[pdf, pdf]).unwrap();
	assert_eq!(
		texts(&merged),
		["one", "two", "three", "one", "two", "three"]
	);
	assert_eq!(reopen(&merged), 6);
	let parts = split(pdf, &[vec![3], vec![2, 1]]).unwrap();
	assert_eq!(reopen(&parts[1]), 2);
	assert_eq!(texts(&parts[0]), ["three"]);
	assert_eq!(texts(&parts[1]), ["two", "one"]);
	let turned = rotate(pdf, &[2], 270).unwrap();
	assert_eq!(reopen(&turned), 3);
	assert_eq!([1, 2, 3].map(|page| rotation(&turned, page)), [0, 270, 0]);
	assert_eq!(texts(&turned), ["one", "two", "three"]);
	assert_eq!(texts(&delete_pages(pdf, &[1]).unwrap()), ["two", "three"]);
}

#[test]
fn handles_object_and_xref_streams() {
	let mut doc = Document::load_mem(&text_pdf(&["one", "two", "three"])).unwrap();
	let mut modern = Vec::new();
	doc.save_modern(&mut modern).unwrap();
	assert!(modern.windows(7).any(|w| w == b"/ObjStm"));
	assert_eq!(page_count(&modern).unwrap(), 3);
	assert_eq!(texts(&modern), ["one", "two", "three"]);
	edits_cleanly(&modern);
}

#[test]
fn handles_encryption_with_an_empty_user_password() {
	use lopdf::{EncryptionState, EncryptionVersion, Permissions};

	let mut doc = Document::load_mem(&text_pdf(&["one", "two", "three"])).unwrap();
	let id = Object::string_literal("0123456789abcdef");
	doc.trailer.set("ID", vec![id.clone(), id]);
	let version = EncryptionVersion::V2 {
		document: &doc,
		owner_password: "owner",
		user_password: "",
		key_length: 128,
		permissions: Permissions::all(),
	};
	let state = EncryptionState::try_from(version).unwrap();
	doc.encrypt(&state).unwrap();
	let mut encrypted = Vec::new();
	doc.save_to(&mut encrypted).unwrap();
	assert!(encrypted.windows(8).any(|w| w == b"/Encrypt"));
	assert_eq!(page_count(&encrypted).unwrap(), 3);
	assert_eq!(texts(&encrypted), ["one", "two", "three"]);
	edits_cleanly(&encrypted);
}
