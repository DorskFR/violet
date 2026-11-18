//! A plaintext IMAP server holding folders in memory, enough for the mail sync: it records
//! every command and asks for each literal before reading it.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use time::Date;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

pub struct Message {
	pub uid: u32,
	pub flags: BTreeSet<String>,
	pub received: Date,
	pub raw: Vec<u8>,
}

pub struct Folder {
	pub uidvalidity: u32,
	pub next_uid: u32,
	pub messages: Vec<Message>,
}

impl Folder {
	fn new(uidvalidity: u32) -> Self {
		Self {
			uidvalidity,
			next_uid: 1,
			messages: Vec::new(),
		}
	}
}

#[derive(Default)]
pub struct State {
	pub folders: BTreeMap<String, Folder>,
	/// Every command received, with the LOGIN password masked.
	pub commands: Vec<String>,
	/// How many literals the client sent.
	pub literals: usize,
	pub capabilities: Vec<String>,
	pub username: String,
	pub password: String,
}

#[derive(Clone)]
pub struct FakeImap {
	pub state: Arc<Mutex<State>>,
	pub port: u16,
}

const MONTHS: [&str; 12] = [
	"Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

fn imap_date(date: Date) -> String {
	format!(
		"{:02}-{}-{}",
		date.day(),
		MONTHS[usize::from(u8::from(date.month())) - 1],
		date.year()
	)
}

fn parse_date(text: &str) -> Date {
	let mut parts = text.split('-');
	let day: u8 = parts.next().unwrap().parse().unwrap();
	let month = parts.next().unwrap();
	let year: i32 = parts.next().unwrap().parse().unwrap();
	let month = MONTHS.iter().position(|name| *name == month).unwrap() as u8 + 1;
	Date::from_calendar_date(year, time::Month::try_from(month).unwrap(), day).unwrap()
}

fn header(raw: &[u8], name: &str) -> String {
	let text = String::from_utf8_lossy(raw);
	let head = text.split("\r\n\r\n").next().unwrap_or_default();
	let prefix = format!("{}:", name.to_lowercase());
	head.lines()
		.find(|line| line.to_lowercase().starts_with(&prefix))
		.map(|line| line[prefix.len()..].trim().to_lowercase())
		.unwrap_or_default()
}

/// A command split into its words; quoted strings and literals are one word each, a
/// parenthesised list one word without its parentheses.
fn words(bytes: &[u8]) -> Vec<String> {
	let mut words = Vec::new();
	let mut at = 0;
	while at < bytes.len() {
		match bytes[at] {
			b' ' | b'\r' | b'\n' => at += 1,
			b'"' => {
				let mut word = Vec::new();
				at += 1;
				while bytes[at] != b'"' {
					if bytes[at] == b'\\' {
						at += 1;
					}
					word.push(bytes[at]);
					at += 1;
				}
				at += 1;
				words.push(String::from_utf8(word).unwrap());
			}
			b'{' => {
				let close = at + bytes[at..].iter().position(|byte| *byte == b'}').unwrap();
				let length: usize = std::str::from_utf8(&bytes[at + 1..close])
					.unwrap()
					.trim_end_matches('+')
					.parse()
					.unwrap();
				let start = close + 3;
				words.push(String::from_utf8(bytes[start..start + length].to_vec()).unwrap());
				at = start + length;
			}
			b'(' => {
				let close = at + bytes[at..].iter().position(|byte| *byte == b')').unwrap();
				words.push(String::from_utf8(bytes[at + 1..close].to_vec()).unwrap());
				at = close + 1;
			}
			_ => {
				let end = bytes[at..]
					.iter()
					.position(|byte| matches!(byte, b' ' | b'\r' | b'\n'))
					.map_or(bytes.len(), |end| at + end);
				words.push(String::from_utf8(bytes[at..end].to_vec()).unwrap());
				at = end;
			}
		}
	}
	words
}

impl FakeImap {
	pub async fn start(username: &str, password: &str, capabilities: &[&str]) -> Self {
		let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
		let mut folders = BTreeMap::new();
		folders.insert("INBOX".to_string(), Folder::new(1000));
		let fake = Self {
			state: Arc::new(Mutex::new(State {
				folders,
				capabilities: capabilities.iter().map(|cap| cap.to_string()).collect(),
				username: username.into(),
				password: password.into(),
				..State::default()
			})),
			port: listener.local_addr().unwrap().port(),
		};
		let state = fake.state.clone();
		tokio::spawn(async move {
			loop {
				let (stream, _) = listener.accept().await.unwrap();
				tokio::spawn(serve(stream, state.clone()));
			}
		});
		fake
	}

	pub fn add_folder(&self, name: &str) {
		self.state
			.lock()
			.unwrap()
			.folders
			.insert(name.into(), Folder::new(2000));
	}

	/// Puts a mail in a folder and returns its UID.
	pub fn add(&self, folder: &str, received: Date, raw: &[u8]) -> u32 {
		let mut state = self.state.lock().unwrap();
		let folder = state.folders.get_mut(folder).unwrap();
		let uid = folder.next_uid;
		folder.next_uid += 1;
		folder.messages.push(Message {
			uid,
			flags: BTreeSet::new(),
			received,
			raw: raw.to_vec(),
		});
		uid
	}

	/// The folder as a server that lost its state would present it: a new UIDVALIDITY,
	/// new UIDs and no flags.
	pub fn renumber(&self, folder: &str) {
		let mut state = self.state.lock().unwrap();
		let folder = state.folders.get_mut(folder).unwrap();
		folder.uidvalidity += 1;
		folder.next_uid = 100;
		for message in &mut folder.messages {
			message.uid = folder.next_uid;
			message.flags.clear();
			folder.next_uid += 1;
		}
	}

	pub fn flags(&self, folder: &str) -> Vec<(u32, Vec<String>)> {
		let state = self.state.lock().unwrap();
		state.folders[folder]
			.messages
			.iter()
			.map(|message| (message.uid, message.flags.iter().cloned().collect()))
			.collect()
	}

	pub fn commands(&self) -> Vec<String> {
		self.state.lock().unwrap().commands.clone()
	}

	pub fn count(&self, prefix: &str) -> usize {
		self.commands()
			.iter()
			.filter(|command| command.starts_with(prefix))
			.count()
	}
}

/// Reads one command, asking for each synchronising literal.
async fn read_command(reader: &mut BufReader<TcpStream>, state: &Mutex<State>) -> Option<Vec<u8>> {
	let mut command = Vec::new();
	loop {
		let mut line = Vec::new();
		if reader.read_until(b'\n', &mut line).await.ok()? == 0 {
			return None;
		}
		command.extend_from_slice(&line);
		let trimmed = line.strip_suffix(b"\r\n")?;
		let Some(open) = trimmed.iter().rposition(|byte| *byte == b'{') else {
			return Some(command);
		};
		let Some(inner) = trimmed[open + 1..].strip_suffix(b"}") else {
			return Some(command);
		};
		let length: usize = std::str::from_utf8(inner).ok()?.parse().ok()?;
		state.lock().unwrap().literals += 1;
		reader.get_mut().write_all(b"+ send it\r\n").await.ok()?;
		let mut literal = vec![0; length];
		reader.read_exact(&mut literal).await.ok()?;
		command.extend_from_slice(&literal);
	}
}

fn search(folder: &Folder, args: &[String]) -> Vec<u32> {
	let mut keep: Vec<&Message> = folder.messages.iter().collect();
	let mut at = 0;
	while at < args.len() {
		let key = args[at].to_uppercase();
		let value = args.get(at + 1).cloned().unwrap_or_default();
		at += 1;
		match key.as_str() {
			"CHARSET" => at += 1,
			"UNDELETED" => keep.retain(|message| !message.flags.contains("\\Deleted")),
			"UNSEEN" => keep.retain(|message| !message.flags.contains("\\Seen")),
			"UNFLAGGED" => keep.retain(|message| !message.flags.contains("\\Flagged")),
			"SINCE" => {
				let since = parse_date(&value);
				keep.retain(|message| message.received >= since);
				at += 1;
			}
			"UNKEYWORD" => {
				keep.retain(|message| !message.flags.contains(&value));
				at += 1;
			}
			"FROM" | "TO" | "SUBJECT" => {
				let wanted = value.to_lowercase();
				keep.retain(|message| header(&message.raw, &key).contains(&wanted));
				at += 1;
			}
			other => panic!("the fake does not search {other}"),
		}
	}
	keep.iter().map(|message| message.uid).collect()
}

async fn serve(stream: TcpStream, state: Arc<Mutex<State>>) {
	let mut reader = BufReader::new(stream);
	if reader
		.get_mut()
		.write_all(b"* OK fake IMAP ready\r\n")
		.await
		.is_err()
	{
		return;
	}
	let mut selected: Option<String> = None;
	while let Some(command) = read_command(&mut reader, &state).await {
		let words = words(&command);
		let (tag, name) = (words[0].clone(), words[1].to_uppercase());
		let uid_command = if name == "UID" {
			Some(words[2].to_uppercase())
		} else {
			None
		};
		let recorded = if name == "LOGIN" {
			format!("LOGIN {} ***", words[2])
		} else {
			words[1..].join(" ")
		};
		let reply = {
			let mut state = state.lock().unwrap();
			state.commands.push(recorded);
			answer(
				&mut state,
				&mut selected,
				&tag,
				&name,
				uid_command.as_deref(),
				&words,
			)
		};
		if reader.get_mut().write_all(&reply).await.is_err() || name == "LOGOUT" {
			return;
		}
	}
}

fn answer(
	state: &mut State,
	selected: &mut Option<String>,
	tag: &str,
	name: &str,
	uid_command: Option<&str>,
	words: &[String],
) -> Vec<u8> {
	let ok = |text: &str| format!("{tag} OK {text}\r\n").into_bytes();
	let no = |text: &str| format!("{tag} NO {text}\r\n").into_bytes();
	match (name, uid_command) {
		("CAPABILITY", _) => {
			let mut out = format!(
				"* CAPABILITY IMAP4rev1 {}\r\n",
				state.capabilities.join(" ")
			)
			.into_bytes();
			out.extend(ok("CAPABILITY done"));
			out
		}
		("LOGIN", _) => {
			if words[2] == state.username && words[3] == state.password {
				ok("[CAPABILITY IMAP4rev1] logged in (welcome)")
			} else {
				no("[AUTHENTICATIONFAILED] wrong \"password")
			}
		}
		("SELECT", _) => match state.folders.get(&words[2]) {
			Some(folder) => {
				*selected = Some(words[2].clone());
				let mut out = format!(
					"* {} EXISTS\r\n* FLAGS (\\Seen \\Flagged \\Deleted)\r\n* OK [PERMANENTFLAGS (\\Seen \\Flagged \\Deleted \\*)] ok\r\n* OK [UIDVALIDITY {}] UIDs valid\r\n",
					folder.messages.len(),
					folder.uidvalidity
				)
				.into_bytes();
				out.extend(ok("[READ-WRITE] selected"));
				out
			}
			None => no("[NONEXISTENT] no such folder"),
		},
		("LOGOUT", _) => {
			let mut out = b"* BYE see you\r\n".to_vec();
			out.extend(ok("bye"));
			out
		}
		("EXPUNGE", _) => {
			if let Some(folder) = selected
				.as_ref()
				.and_then(|name| state.folders.get_mut(name))
			{
				folder
					.messages
					.retain(|message| !message.flags.contains("\\Deleted"));
			}
			ok("expunged")
		}
		("UID", Some(command)) => {
			let Some(current) = selected.clone() else {
				return no("nothing selected");
			};
			uid(state, &current, command, &words[3..], ok, no)
		}
		_ => format!("{tag} BAD unknown command\r\n").into_bytes(),
	}
}

fn uid(
	state: &mut State,
	current: &str,
	command: &str,
	args: &[String],
	ok: impl Fn(&str) -> Vec<u8>,
	no: impl Fn(&str) -> Vec<u8>,
) -> Vec<u8> {
	let uids: Vec<u32> = args
		.first()
		.map(|set| set.split(',').filter_map(|uid| uid.parse().ok()).collect())
		.unwrap_or_default();
	match command {
		"SEARCH" => {
			let found = search(&state.folders[current], args);
			let list: Vec<String> = found.iter().map(u32::to_string).collect();
			let mut out = format!("* SEARCH {}\r\n", list.join(" ")).into_bytes();
			out.extend(ok("search done"));
			out
		}
		"FETCH" => {
			let items = args[1].to_uppercase();
			let folder = &state.folders[current];
			let mut out = b"* 1 FETCH (FLAGS (\\Recent))\r\n".to_vec();
			for (position, message) in folder.messages.iter().enumerate() {
				if !uids.contains(&message.uid) {
					continue;
				}
				let mut parts = vec![format!("UID {}", message.uid)];
				if items.contains("RFC822.SIZE") {
					parts.push(format!("RFC822.SIZE {}", message.raw.len()));
				}
				if items.contains("INTERNALDATE") {
					parts.push(format!(
						"INTERNALDATE \"{} 09:30:00 +0900\"",
						imap_date(message.received)
					));
				}
				out.extend(format!("* {} FETCH ({}", position + 1, parts.join(" ")).into_bytes());
				if items.contains("BODY.PEEK[]") {
					out.extend(format!(" BODY[] {{{}}}\r\n", message.raw.len()).into_bytes());
					out.extend(&message.raw);
				}
				out.extend(b")\r\n");
			}
			out.extend(ok("fetch done"));
			out
		}
		"STORE" => {
			let folder = state.folders.get_mut(current).unwrap();
			for message in &mut folder.messages {
				if uids.contains(&message.uid) {
					message.flags.extend(args[2].split(' ').map(str::to_string));
				}
			}
			ok("stored")
		}
		"COPY" | "MOVE" => {
			let target = args[1].clone();
			if !state.folders.contains_key(&target) {
				return no("[TRYCREATE] no such folder");
			}
			let taken: Vec<Message> = {
				let folder = state.folders.get_mut(current).unwrap();
				let mut taken = Vec::new();
				for message in &folder.messages {
					if uids.contains(&message.uid) {
						taken.push(Message {
							uid: 0,
							flags: message.flags.clone(),
							received: message.received,
							raw: message.raw.clone(),
						});
					}
				}
				if command == "MOVE" {
					folder
						.messages
						.retain(|message| !uids.contains(&message.uid));
				}
				taken
			};
			let destination = state.folders.get_mut(&target).unwrap();
			for mut message in taken {
				message.uid = destination.next_uid;
				destination.next_uid += 1;
				destination.messages.push(message);
			}
			ok("done")
		}
		"EXPUNGE" => {
			let folder = state.folders.get_mut(current).unwrap();
			folder.messages.retain(|message| {
				!(uids.contains(&message.uid) && message.flags.contains("\\Deleted"))
			});
			ok("expunged")
		}
		_ => ok("ignored"),
	}
}
