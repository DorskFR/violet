use mail_parser::{MessageParser, MessagePart, PartKind};
use violet_core::naming;

use super::config::Rule;
use crate::extract;

/// Inline images smaller than this are logos and signatures.
const MIN_INLINE_IMAGE: usize = 20 * 1024;
const MAX_NAME_CHARS: usize = 120;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picked {
	pub name: String,
	pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, Default)]
pub struct Parsed {
	pub message_id: Option<String>,
	/// `Name <address>`, or the address alone.
	pub from: Option<String>,
	pub subject: Option<String>,
	/// RFC 3339.
	pub date: Option<String>,
	pub files: Vec<Picked>,
	/// Names of the parts left out, with why.
	pub left_out: Vec<String>,
}

fn readable(ext: &str) -> bool {
	matches!(ext, "pdf" | "png" | "jpg" | "jpeg")
		|| extract::is_office(ext)
		|| extract::is_plain(ext)
}

fn extension_for(part: &MessagePart<'_>) -> &'static str {
	let Some(kind) = part.content_type() else {
		return "";
	};
	let subtype = kind.subtype().unwrap_or_default().to_ascii_lowercase();
	match (kind.ctype().to_ascii_lowercase().as_str(), subtype.as_str()) {
		("application", "pdf") => "pdf",
		("image", "png") => "png",
		("image", "jpeg" | "jpg") => "jpg",
		("message", "rfc822") => "eml",
		("text", "plain") => "txt",
		_ => "",
	}
}

fn bounded(name: &str) -> String {
	let ext = extract::extension(name);
	if name.chars().count() <= MAX_NAME_CHARS {
		return name.to_string();
	}
	let keep = MAX_NAME_CHARS.saturating_sub(ext.len() + 1);
	let stem: String = name.chars().take(keep).collect();
	let stem = stem.trim_end();
	if ext.is_empty() {
		stem.to_string()
	} else {
		format!("{stem}.{ext}")
	}
}

fn file_name(part: &MessagePart<'_>) -> String {
	let given = part
		.attachment_name()
		.unwrap_or_default()
		.rsplit(['/', '\\'])
		.next()
		.unwrap_or_default();
	let mut name = naming::sanitize(given);
	if name.is_empty() {
		name = "attachment".into();
	}
	if extract::extension(&name).is_empty() {
		let ext = extension_for(part);
		if !ext.is_empty() {
			name = format!("{name}.{ext}");
		}
	}
	bounded(&name)
}

fn is_image(part: &MessagePart<'_>) -> bool {
	part.content_type()
		.is_some_and(|kind| kind.ctype().eq_ignore_ascii_case("image"))
}

/// The files `rule` takes from a raw RFC 5322 message, and what the message says about
/// itself.
pub fn pick(raw: &[u8], rule: &Rule) -> Parsed {
	let Some(message) = MessageParser::new().parse(raw) else {
		return Parsed {
			left_out: vec!["the message could not be parsed".into()],
			..Parsed::default()
		};
	};
	let from = message.from().and_then(|from| from.first()).map(|sender| {
		match (sender.name(), sender.address()) {
			(Some(name), Some(address)) => format!("{name} <{address}>"),
			(None, Some(address)) => address.to_string(),
			(Some(name), None) => name.to_string(),
			(None, None) => String::new(),
		}
	});
	let subject = message.subject().map(str::to_string);
	let mut parsed = Parsed {
		message_id: message.message_id().map(str::to_string),
		from: from.filter(|from| !from.is_empty()),
		date: message.date().map(|date| date.to_rfc3339()),
		subject: subject.clone(),
		files: Vec::new(),
		left_out: Vec::new(),
	};
	if rule.scope.eml() {
		let stem = naming::sanitize(subject.as_deref().unwrap_or_default());
		let stem = if stem.is_empty() { "mail".into() } else { stem };
		parsed.files.push(Picked {
			name: bounded(&format!("{stem}.eml")),
			bytes: raw.to_vec(),
		});
	}
	if !rule.scope.attachments() {
		return parsed;
	}
	for part in message.parts() {
		let candidate = match part.kind() {
			PartKind::Binary | PartKind::InlineBinary => true,
			PartKind::Text | PartKind::Html => {
				part.is_attachment() && part.attachment_name().is_some()
			}
			PartKind::Multipart | PartKind::Message(_) => false,
		};
		if !candidate {
			continue;
		}
		let name = file_name(&part);
		let inline = part.is_inline() || !part.is_attachment();
		let reason = if inline && !rule.include_inline {
			Some("inline")
		} else if inline && is_image(&part) && part.decoded_len() < MIN_INLINE_IMAGE {
			Some("small inline image")
		} else if !readable(&extract::extension(&name)) {
			Some("not a kind of file violet reads")
		} else if !rule.wants(&name) {
			Some("filtered out by name")
		} else {
			None
		};
		match reason {
			Some(reason) => parsed.left_out.push(format!("{name}: {reason}")),
			None => parsed.files.push(Picked {
				name,
				bytes: part.decoded().into_owned(),
			}),
		}
	}
	parsed
}
