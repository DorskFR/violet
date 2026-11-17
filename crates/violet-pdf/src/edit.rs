use std::collections::{BTreeSet, HashMap, HashSet};

use lopdf::content::Content;
use lopdf::{Dictionary, Document, Object, ObjectId, dictionary};

use crate::{Error, Result, require_pdf};

const MAX_CONTENT_BYTES: usize = 256 * 1024 * 1024;
const MAX_TREE_DEPTH: usize = 64;
const NAMED_BY: [(&[u8], &str); 2] = [(b"XObject", "Do"), (b"Font", "Tf")];
const INHERITED: [&[u8]; 4] = [b"Resources", b"MediaBox", b"CropBox", b"Rotate"];

fn pdf_err(err: lopdf::Error) -> Error {
	Error::Pdf(err.to_string())
}

pub(crate) struct Source {
	doc: Document,
	pages: Vec<ObjectId>,
}

impl Source {
	pub(crate) fn load(bytes: &[u8]) -> Result<Self> {
		require_pdf(bytes)?;
		let doc = Document::load_mem(bytes).map_err(pdf_err)?;
		if doc.is_encrypted() {
			return Err(Error::Pdf("encrypted".into()));
		}
		let pages: Vec<ObjectId> = doc.page_iter().collect();
		if pages.is_empty() {
			return Err(Error::Pdf("no pages".into()));
		}
		Ok(Self { doc, pages })
	}

	pub(crate) fn page_count(&self) -> u32 {
		self.pages.len() as u32
	}

	fn index(&self, page: u32) -> Result<usize> {
		match (page as usize).checked_sub(1) {
			Some(index) if index < self.pages.len() => Ok(index),
			_ => Err(Error::PageOutOfRange(page, self.page_count())),
		}
	}

	fn indices(&self, pages: &[u32]) -> Result<Vec<usize>> {
		pages.iter().map(|&page| self.index(page)).collect()
	}

	fn inherited(&self, page: &Dictionary, key: &[u8]) -> Option<Object> {
		let mut node = page;
		for _ in 0..MAX_TREE_DEPTH {
			if let Ok(value) = node.get(key) {
				return Some(value.clone());
			}
			let parent = node.get(b"Parent").and_then(Object::as_reference).ok()?;
			node = self.doc.get_dictionary(parent).ok()?;
		}
		None
	}

	fn rotation(&self, page: ObjectId) -> i64 {
		let degrees = self
			.doc
			.get_dictionary(page)
			.ok()
			.and_then(|dict| self.inherited(dict, b"Rotate"))
			.and_then(|value| match self.doc.dereference(&value).ok()?.1 {
				Object::Integer(degrees) => Some(*degrees as f64),
				Object::Real(degrees) => Some(*degrees as f64),
				_ => None,
			})
			.filter(|degrees| degrees.is_finite())
			.unwrap_or(0.0);
		(((degrees / 90.0).round() as i64) % 4 * 90).rem_euclid(360)
	}

	/// The page dictionary with its inherited attributes made explicit and its resources
	/// narrowed to what the content uses, still pointing at objects of this document.
	fn standalone_page(&self, id: ObjectId) -> Dictionary {
		let mut page = match self.doc.get_dictionary(id) {
			Ok(dict) => dict.clone(),
			Err(_) => dictionary! { "Type" => "Page" },
		};
		for key in INHERITED {
			if let Some(value) = self.inherited(&page, key) {
				page.set(key, value);
			}
		}
		if !page.has(b"MediaBox") {
			page.set("MediaBox", vec![0.into(), 0.into(), 612.into(), 792.into()]);
		}
		page.remove(b"Parent");
		page.remove(b"B");
		if let Some(resources) = self.used_resources(id, &page) {
			page.set("Resources", resources);
		}
		page
	}

	/// `None` whenever it cannot be shown that the unused entries are safe to drop.
	fn used_resources(&self, id: ObjectId, page: &Dictionary) -> Option<Dictionary> {
		let doc = &self.doc;
		let mut resources = page
			.get_deref(b"Resources", doc)
			.and_then(Object::as_dict)
			.ok()?
			.clone();
		let data = doc
			.get_page_content_with_limit(id, MAX_CONTENT_BYTES)
			.ok()?;
		let content = Content::decode_strict(&data).ok()?;
		for (category, operator) in NAMED_BY {
			let names: HashSet<&[u8]> = content
				.operations
				.iter()
				.filter(|operation| operation.operator == operator)
				.filter_map(|operation| operation.operands.first()?.as_name().ok())
				.collect();
			let Ok(entries) = resources.get_deref(category, doc).and_then(Object::as_dict) else {
				continue;
			};
			let mut kept = Dictionary::new();
			for (name, value) in entries
				.iter()
				.filter(|(name, _)| names.contains(name.as_slice()))
			{
				let target = doc.dereference(value).ok()?.1;
				let dict = match target {
					Object::Stream(stream) => &stream.dict,
					Object::Dictionary(dict) => dict,
					_ => return None,
				};
				let borrows_page_resources = dict
					.get(b"Subtype")
					.and_then(Object::as_name)
					.is_ok_and(|subtype| subtype == b"Form" || subtype == b"Type3")
					&& !dict.has(b"Resources");
				if borrows_page_resources {
					return None;
				}
				kept.set(name.clone(), value.clone());
			}
			resources.set(category, kept);
		}
		Some(resources)
	}
}

struct Builder {
	doc: Document,
	root: ObjectId,
	pages: Vec<ObjectId>,
}

impl Builder {
	fn new(version: &str) -> Self {
		let mut doc = Document::with_version(version);
		let root = doc.new_object_id();
		Self {
			doc,
			root,
			pages: Vec::new(),
		}
	}

	fn append(&mut self, source: &Source, indices: &[usize]) {
		let mut ids: HashMap<ObjectId, ObjectId> = HashMap::new();
		let mut pending: Vec<(ObjectId, ObjectId)> = Vec::new();
		let targets: Vec<(ObjectId, ObjectId)> = indices
			.iter()
			.map(|&index| {
				let old = source.pages[index];
				let new = self.doc.new_object_id();
				ids.entry(old).or_insert(new);
				(old, new)
			})
			.collect();

		for (old, new) in targets {
			let mut page = source.standalone_page(old);
			if ids[&old] != new {
				page.remove(b"Annots");
			}
			let mut page = Object::Dictionary(page);
			self.adopt(source, &mut page, &mut ids, &mut pending);
			if let Object::Dictionary(dict) = &mut page {
				dict.set("Type", "Page");
				dict.set("Parent", self.root);
			}
			self.doc.objects.insert(new, page);
			self.pages.push(new);
		}

		while let Some((old, new)) = pending.pop() {
			let Some(mut object) = source.doc.objects.get(&old).cloned() else {
				continue;
			};
			self.adopt(source, &mut object, &mut ids, &mut pending);
			self.doc.objects.insert(new, object);
		}
	}

	/// Points every reference inside `object` at this document, queueing the targets for
	/// copying. References to pages that are not being copied become null, which is what
	/// keeps links and form fields from dragging the rest of the source along.
	fn adopt(
		&mut self,
		source: &Source,
		object: &mut Object,
		ids: &mut HashMap<ObjectId, ObjectId>,
		pending: &mut Vec<(ObjectId, ObjectId)>,
	) {
		let mut stack = vec![object];
		while let Some(object) = stack.pop() {
			match object {
				Object::Reference(old) => {
					let old = *old;
					let new = ids.get(&old).copied().or_else(|| {
						let target = source.doc.objects.get(&old)?;
						if is_page_tree_node(target) {
							return None;
						}
						let new = self.doc.new_object_id();
						ids.insert(old, new);
						pending.push((old, new));
						Some(new)
					});
					*object = new.map_or(Object::Null, Object::Reference);
				}
				Object::Array(items) => stack.extend(items.iter_mut()),
				Object::Dictionary(dict) => stack.extend(dict.iter_mut().map(|(_, value)| value)),
				Object::Stream(stream) => {
					stack.extend(stream.dict.iter_mut().map(|(_, value)| value));
				}
				_ => {}
			}
		}
	}

	fn finish(mut self) -> Result<Vec<u8>> {
		let kids: Vec<Object> = self.pages.iter().map(|&id| id.into()).collect();
		let count = kids.len() as i64;
		self.doc.objects.insert(
			self.root,
			dictionary! { "Type" => "Pages", "Kids" => kids, "Count" => count }.into(),
		);
		let catalog = self
			.doc
			.add_object(dictionary! { "Type" => "Catalog", "Pages" => self.root });
		self.doc.trailer.set("Root", catalog);
		save(&mut self.doc)
	}
}

fn is_page_tree_node(object: &Object) -> bool {
	object
		.as_dict()
		.and_then(Dictionary::get_type)
		.is_ok_and(|kind| kind == b"Page" || kind == b"Pages")
}

pub(crate) fn save(doc: &mut Document) -> Result<Vec<u8>> {
	let mut out = Vec::new();
	doc.save_to(&mut out)
		.map_err(|e| Error::Pdf(e.to_string()))?;
	Ok(out)
}

fn version_key(version: &str) -> (u32, u32) {
	let mut parts = version.split('.').map(|part| part.parse().unwrap_or(0));
	(parts.next().unwrap_or(0), parts.next().unwrap_or(0))
}

pub(crate) fn split(pdf: &[u8], groups: &[Vec<u32>]) -> Result<Vec<Vec<u8>>> {
	let source = Source::load(pdf)?;
	let groups: Vec<Vec<usize>> = groups
		.iter()
		.map(|group| match group.is_empty() {
			true => Err(Error::Invalid("empty page group".into())),
			false => source.indices(group),
		})
		.collect::<Result<_>>()?;
	groups
		.iter()
		.map(|group| {
			let mut builder = Builder::new(&source.doc.version);
			builder.append(&source, group);
			builder.finish()
		})
		.collect()
}

pub(crate) fn merge(pdfs: &[&[u8]]) -> Result<Vec<u8>> {
	if pdfs.is_empty() {
		return Err(Error::Invalid("nothing to merge".into()));
	}
	let sources: Vec<Source> = pdfs
		.iter()
		.map(|pdf| Source::load(pdf))
		.collect::<Result<_>>()?;
	let version = sources
		.iter()
		.map(|source| source.doc.version.as_str())
		.max_by_key(|version| version_key(version))
		.unwrap_or("1.7");
	let mut builder = Builder::new(version);
	for source in &sources {
		let all: Vec<usize> = (0..source.pages.len()).collect();
		builder.append(source, &all);
	}
	builder.finish()
}

pub(crate) fn rotate(pdf: &[u8], pages: &[u32], degrees: i32) -> Result<Vec<u8>> {
	if degrees % 90 != 0 {
		return Err(Error::Invalid(format!(
			"rotation {degrees} is not a multiple of 90"
		)));
	}
	let mut source = Source::load(pdf)?;
	let targets: BTreeSet<ObjectId> = source
		.indices(pages)?
		.into_iter()
		.map(|index| source.pages[index])
		.collect();
	for id in targets {
		let rotation = (source.rotation(id) + i64::from(degrees)).rem_euclid(360);
		source
			.doc
			.get_dictionary_mut(id)
			.map_err(pdf_err)?
			.set("Rotate", rotation);
	}
	save(&mut source.doc)
}

pub(crate) fn delete_pages(pdf: &[u8], pages: &[u32]) -> Result<Vec<u8>> {
	let source = Source::load(pdf)?;
	let removed: HashSet<usize> = source.indices(pages)?.into_iter().collect();
	let kept: Vec<usize> = (0..source.pages.len())
		.filter(|index| !removed.contains(index))
		.collect();
	if kept.is_empty() {
		return Err(Error::Invalid("cannot delete every page".into()));
	}
	let mut builder = Builder::new(&source.doc.version);
	builder.append(&source, &kept);
	builder.finish()
}
