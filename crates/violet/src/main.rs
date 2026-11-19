use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, bail};
use clap::{Args, Parser, Subcommand};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;
use violet::auth::{AuthConfig, LoginMode, Role, Scope};
use violet::login;
use violet::ops::{self, Patch};
use violet::pipeline::{self, Outcome};
use violet::{App, Models, http, users, watcher};
use violet_core::{Query, naming};
use violet_model::ModelConfig;

#[derive(Parser)]
#[command(version, about)]
struct Cli {
	/// Library root, holding inbox/, library/ and the index.
	#[arg(
		long,
		global = true,
		env = "VIOLET_LIBRARY",
		default_value = "./library"
	)]
	library: PathBuf,
	#[command(flatten)]
	model: ModelArgs,
	#[command(flatten)]
	sign_in: SignInArgs,
	#[command(subcommand)]
	command: Command,
}

/// How requests sign in; see docs/design/multi-user.md.
#[derive(Args)]
struct SignInArgs {
	/// off, optional or required.
	#[arg(
		long,
		global = true,
		env = "VIOLET_LOGIN",
		default_value = "off",
		value_parser = |value: &str| value.parse::<LoginMode>()
	)]
	login: LoginMode,
	/// The address people reach violet at, e.g. https://violet.dorsk.dev; needed unless
	/// VIOLET_LOGIN is off.
	#[arg(long, global = true, env = "VIOLET_PUBLIC_URL")]
	public_url: Option<String>,
	/// Addresses or CIDR blocks of the proxies whose X-Forwarded-For names the client.
	#[arg(
		long,
		global = true,
		env = "VIOLET_TRUSTED_PROXIES",
		value_delimiter = ','
	)]
	trusted_proxies: Vec<String>,
	/// User the command line runs as; the OS user when not set.
	#[arg(long = "as", global = true, env = "VIOLET_USER")]
	local_user: Option<String>,
}

fn sign_in(args: &SignInArgs) -> anyhow::Result<AuthConfig> {
	let filled = |value: &Option<String>| {
		value
			.as_deref()
			.map(str::trim)
			.filter(|value| !value.is_empty())
			.map(str::to_string)
	};
	let trusted_proxies = args
		.trusted_proxies
		.iter()
		.map(|network| network.trim())
		.filter(|network| !network.is_empty())
		.map(|network| network.parse().map_err(anyhow::Error::msg))
		.collect::<anyhow::Result<Vec<_>>>()
		.context("VIOLET_TRUSTED_PROXIES")?;
	let public_url = filled(&args.public_url)
		.map(|url| {
			webauthn_rs::prelude::Url::parse(&url)
				.with_context(|| format!("VIOLET_PUBLIC_URL {url} is not a URL"))
		})
		.transpose()?;
	for gone in ["VIOLET_IDENTITY_HEADER", "VIOLET_DEFAULT_USER"] {
		if std::env::var_os(gone).is_some() {
			tracing::warn!(variable = gone, "no longer used: people sign in instead");
		}
	}
	let config = AuthConfig {
		mode: args.login,
		public_url,
		trusted_proxies,
		local_user: filled(&args.local_user).or_else(|| filled(&std::env::var("USER").ok())),
	};
	config.validate()?;
	Ok(config)
}

#[derive(Args)]
struct ModelArgs {
	/// OpenAI-compatible base URL of the vision model, ending before /chat/completions.
	#[arg(long, global = true, env = "VIOLET_VISION_URL")]
	vision_url: Option<String>,
	#[arg(long, global = true, env = "VIOLET_VISION_MODEL")]
	vision_model: Option<String>,
	#[arg(long, global = true, env = "VIOLET_VISION_KEY", hide_env_values = true)]
	vision_key: Option<String>,
	/// Separate endpoint for the analysis; the vision model does it when not set.
	#[arg(long, global = true, env = "VIOLET_TEXT_URL")]
	text_url: Option<String>,
	#[arg(long, global = true, env = "VIOLET_TEXT_MODEL")]
	text_model: Option<String>,
	#[arg(long, global = true, env = "VIOLET_TEXT_KEY", hide_env_values = true)]
	text_key: Option<String>,
	/// Seconds one model request may take.
	#[arg(
		long,
		global = true,
		env = "VIOLET_MODEL_TIMEOUT",
		default_value_t = 600
	)]
	model_timeout: u64,
	/// Seconds reading and analysing one inbox file may take before its job fails.
	#[arg(
		long,
		global = true,
		env = "VIOLET_JOB_TIMEOUT",
		default_value_t = violet::app::DEFAULT_JOB_DEADLINE.as_secs()
	)]
	job_timeout: u64,
}

#[derive(Subcommand)]
enum Command {
	/// Watch the inbox, work the queue and serve the API and UI.
	Serve {
		#[arg(long, env = "VIOLET_LISTEN", default_value = "0.0.0.0:8080")]
		listen: SocketAddr,
	},
	/// Copy files into the inbox and process them now.
	Ingest {
		#[arg(required = true)]
		files: Vec<PathBuf>,
	},
	/// Rebuild the index from the sidecars.
	Reindex,
	Search {
		query: String,
		#[arg(long)]
		json: bool,
	},
	/// Split a PDF into new documents, e.g. --groups 1-2,3.
	Split {
		id: Uuid,
		#[arg(long, required = true, value_delimiter = ',')]
		groups: Vec<String>,
	},
	Rotate {
		id: Uuid,
		#[arg(long, required = true, value_delimiter = ',')]
		pages: Vec<u32>,
		#[arg(long, allow_hyphen_values = true)]
		degrees: i32,
	},
	/// Change metadata; the file is renamed and moved to match.
	Move {
		id: Uuid,
		#[arg(long)]
		title: Option<String>,
		#[arg(long)]
		area: Option<String>,
		#[arg(long)]
		doc_type: Option<String>,
		/// Empty clears it.
		#[arg(long)]
		issuer: Option<String>,
		/// Empty clears it.
		#[arg(long)]
		date: Option<String>,
		/// Replaces the tags; repeat for several.
		#[arg(long = "tag")]
		tags: Option<Vec<String>>,
	},
	/// Read the files of documents again (text layer or OCR, then the analysis) and refile
	/// them under the same ids.
	Reread {
		#[arg(required = true)]
		ids: Vec<Uuid>,
	},
	/// Analyse documents again from the text they already have and refile them to match.
	Reanalyze {
		#[arg(required_unless_present = "all", conflicts_with = "all")]
		ids: Vec<Uuid>,
		/// Every document of the library, oldest first.
		#[arg(long)]
		all: bool,
		/// With --all, only documents created at or after this RFC 3339 time.
		#[arg(long, requires = "all", value_parser = rfc3339)]
		since: Option<OffsetDateTime>,
	},
	/// Undo a journal entry by its id.
	Undo {
		entry: Uuid,
	},
	Status,
	/// Users and their tokens, in .users.json; every change is journaled.
	User {
		#[command(subcommand)]
		command: UserCommand,
	},
}

#[derive(Subcommand)]
enum UserCommand {
	List,
	Add {
		name: String,
		/// member or admin.
		#[arg(long, default_value = "member", value_parser = |value: &str| value.parse::<Role>())]
		role: Role,
		/// Read the user's password from standard input.
		#[arg(long)]
		password_stdin: bool,
	},
	/// Set a user's password, read from standard input.
	Password {
		name: String,
	},
	Remove {
		name: String,
	},
	Token {
		#[command(subcommand)]
		command: TokenCommand,
	},
}

#[derive(Subcommand)]
enum TokenCommand {
	/// Prints the new token once; only its hash is kept.
	Add {
		user: String,
		name: String,
		/// read, write or admin.
		#[arg(long, default_value = "write", value_parser = |value: &str| value.parse::<Scope>())]
		scope: Scope,
		/// RFC 3339 time or YYYY-MM-DD after which the token no longer works.
		#[arg(long, value_parser = expiry)]
		expires: Option<OffsetDateTime>,
	},
	Revoke {
		user: String,
		name: String,
	},
}

fn expiry(value: &str) -> Result<OffsetDateTime, String> {
	if let Ok(at) = OffsetDateTime::parse(value, &Rfc3339) {
		return Ok(at);
	}
	let format = time::macros::format_description!("[year]-[month]-[day]");
	time::Date::parse(value, &format)
		.map(|date| date.midnight().assume_utc())
		.map_err(|_| format!("{value} is not an RFC 3339 time or a YYYY-MM-DD date"))
}

fn user_command(app: &App, command: UserCommand) -> anyhow::Result<()> {
	let by = app.auth.local()?;
	let check = users::NO_CHECK;
	let entry = match command {
		UserCommand::List => {
			let Some(listed) = users::list(app)? else {
				println!("no users file: every request runs as the implicit admin");
				return Ok(());
			};
			for user in listed {
				let tokens: Vec<String> = user
					.tokens
					.iter()
					.map(|token| format!("{}:{:?}", token.name, token.scope).to_lowercase())
					.collect();
				println!(
					"{}\t{:?}\t{}\t{}\t{}",
					user.name,
					user.role,
					if user.password { "password" } else { "-" },
					user.passkeys.join(","),
					tokens.join(",")
				);
			}
			return Ok(());
		}
		UserCommand::Add {
			name,
			role,
			password_stdin,
		} => {
			let password = password_stdin.then(|| new_password(&name)).transpose()?;
			users::add(app, &by, check, &name, role, password)?
		}
		UserCommand::Password { name } => {
			let password = new_password(&name)?;
			users::set_password(app, &by, check, &name, password, None)?
		}
		UserCommand::Remove { name } => users::remove(app, &by, check, &name)?,
		UserCommand::Token {
			command: TokenCommand::Add {
				user,
				name,
				scope,
				expires,
			},
		} => {
			let (secret, entry) = users::add_token(app, &by, check, &user, &name, scope, expires)?;
			println!("{secret}");
			eprintln!("shown once; only its hash is kept");
			entry
		}
		UserCommand::Token {
			command: TokenCommand::Revoke { user, name },
		} => users::revoke_token(app, &by, check, &user, &name)?,
	};
	eprintln!("{}\t{}", entry.id, entry.summary);
	Ok(())
}

/// A password from standard input, hashed; one final line break is not part of it.
fn new_password(name: &str) -> anyhow::Result<String> {
	let mut password = String::new();
	std::io::Read::read_to_string(&mut std::io::stdin(), &mut password)
		.context("cannot read the password from standard input")?;
	let password = password
		.strip_suffix('\n')
		.map(|rest| rest.strip_suffix('\r').unwrap_or(rest))
		.unwrap_or(&password);
	login::check_password(name, password)?;
	Ok(login::hash_now(password)?)
}

fn models(args: &ModelArgs) -> anyhow::Result<Models> {
	let timeout = Duration::from_secs(args.model_timeout);
	let config =
		|name: &str, url: &Option<String>, model: &Option<String>, key: &Option<String>| {
			let filled =
				|value: &Option<String>| value.clone().filter(|value| !value.trim().is_empty());
			match (filled(url), filled(model)) {
				(Some(base_url), Some(model)) => Ok(Some(ModelConfig {
					base_url,
					model,
					api_key: filled(key),
					timeout,
				})),
				(None, None) => Ok(None),
				_ => bail!("VIOLET_{name}_URL and VIOLET_{name}_MODEL must be set together"),
			}
		};
	Ok(Models {
		vision: config(
			"VISION",
			&args.vision_url,
			&args.vision_model,
			&args.vision_key,
		)?,
		text: config("TEXT", &args.text_url, &args.text_model, &args.text_key)?,
		job_deadline: Some(Duration::from_secs(args.job_timeout)),
	})
}

fn logging() {
	let filter = tracing_subscriber::EnvFilter::try_from_default_env()
		.unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
	let builder = tracing_subscriber::fmt()
		.with_env_filter(filter)
		.with_writer(std::io::stderr);
	if std::env::var("VIOLET_LOG_FORMAT").is_ok_and(|format| format == "json") {
		builder.json().init();
	} else {
		builder.init();
	}
}

/// `1-2` or `3` to the pages it lists.
fn page_group(group: &str) -> anyhow::Result<Vec<u32>> {
	let mut pages = Vec::new();
	for part in group.split('+') {
		let (first, last) = match part.split_once('-') {
			Some((first, last)) => (first.trim().parse()?, last.trim().parse()?),
			None => {
				let page: u32 = part.trim().parse()?;
				(page, page)
			}
		};
		if first == 0 || last < first {
			bail!("{part} is not a page range");
		}
		pages.extend(first..=last);
	}
	Ok(pages)
}

async fn shutdown() {
	let interrupt = tokio::signal::ctrl_c();
	let mut terminate =
		tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();
	let terminated = async {
		match &mut terminate {
			Some(signal) => {
				signal.recv().await;
			}
			None => std::future::pending().await,
		}
	};
	tokio::select! {
		_ = interrupt => {}
		() = terminated => {}
	}
}

async fn serve(app: Arc<App>, listen: SocketAddr) -> anyhow::Result<()> {
	let connecting = violet::connectors::start(&app)?;
	let recovered = watcher::recover(&app)?;
	if recovered > 0 {
		tracing::info!(recovered, "jobs put back in the queue");
	}
	let watching = tokio::spawn(watcher::watch(app.clone(), watcher::Settings::default()));
	let working = tokio::spawn(watcher::work(app.clone()));
	let listener = tokio::net::TcpListener::bind(listen)
		.await
		.with_context(|| format!("cannot listen on {listen}"))?;
	tracing::info!(
		%listen,
		library = %app.library.root().display(),
		vision = app.vision.is_some(),
		text = app.text.is_some(),
		"serving"
	);
	axum::serve(
		listener,
		http::router(app).into_make_service_with_connect_info::<SocketAddr>(),
	)
	.with_graceful_shutdown(shutdown())
	.await?;
	watching.abort();
	working.abort();
	for task in connecting {
		task.abort();
	}
	Ok(())
}

fn copy_into_inbox(app: &App, file: &Path) -> anyhow::Result<PathBuf> {
	let given = file
		.file_name()
		.map(|name| name.to_string_lossy().into_owned())
		.unwrap_or_default();
	let name = naming::sanitize(&given);
	if name.is_empty() {
		bail!("{} has no usable file name", file.display());
	}
	let inbox = app.library.inbox();
	let temp = inbox.join(format!(".ingest-{}.part", Uuid::new_v4().simple()));
	std::fs::copy(file, &temp).with_context(|| format!("cannot read {}", file.display()))?;
	Ok(app.library.into_inbox(&temp, Path::new(&name))?)
}

async fn ingest(app: &Arc<App>, files: &[PathBuf]) -> anyhow::Result<()> {
	let mut failed = 0;
	for file in files {
		let relative = copy_into_inbox(app, file)?;
		match pipeline::ingest(app, &relative).await {
			Ok(Outcome::Filed { entry, documents }) => {
				for (id, path) in documents {
					println!("{}\tfiled\t{id}\t{}", file.display(), path.display());
				}
				println!("{}\tentry\t{entry}", file.display());
			}
			Ok(Outcome::Duplicate { of, .. }) => {
				println!("{}\tduplicate\t{of}", file.display());
			}
			Ok(Outcome::Missing) => println!("{}\tmissing", file.display()),
			Err(error) => {
				failed += 1;
				eprintln!("{}\tfailed\t{error}", file.display());
			}
		}
	}
	if failed > 0 {
		bail!("{failed} file(s) failed and were left in the inbox");
	}
	Ok(())
}

fn rfc3339(value: &str) -> Result<OffsetDateTime, time::error::Parse> {
	OffsetDateTime::parse(value, &Rfc3339)
}

async fn reanalyze(
	app: &Arc<App>,
	ids: Vec<Uuid>,
	all: bool,
	since: Option<OffsetDateTime>,
) -> anyhow::Result<()> {
	let ids = if all {
		let mut documents = app.library.sidecars()?;
		documents.retain(|(_, sidecar)| since.is_none_or(|since| sidecar.created_at >= since));
		documents.sort_by_key(|(_, sidecar)| sidecar.created_at);
		documents
			.into_iter()
			.map(|(_, sidecar)| sidecar.id)
			.collect()
	} else {
		ids
	};
	let mut failed = 0;
	for id in &ids {
		match pipeline::reanalyze(app, *id).await {
			Ok(changed) => {
				let outcome = match changed.entry {
					Some(_) => "reanalyzed",
					None => "unchanged",
				};
				println!("{id}\t{outcome}\t{}", changed.result.path.display());
			}
			Err(error) => {
				failed += 1;
				eprintln!("{id}\tfailed\t{error}");
			}
		}
	}
	if failed > 0 {
		bail!(
			"{failed} of {} document(s) could not be read again",
			ids.len()
		);
	}
	Ok(())
}

async fn reread(app: &Arc<App>, ids: &[Uuid]) -> anyhow::Result<()> {
	let mut failed = 0;
	for id in ids {
		match pipeline::reread(app, *id).await {
			Ok(changed) => {
				let outcome = match changed.entry {
					Some(_) => "reread",
					None => "unchanged",
				};
				println!("{id}\t{outcome}\t{}", changed.result.path.display());
			}
			Err(error) => {
				failed += 1;
				eprintln!("{id}\tfailed\t{error}");
			}
		}
	}
	if failed > 0 {
		bail!(
			"{failed} of {} document(s) could not be read again",
			ids.len()
		);
	}
	Ok(())
}

fn reindex(app: &App) -> anyhow::Result<()> {
	let reindexed = app.reindex()?;
	println!("{} documents", reindexed.documents);
	let library = app.library.library_dir();
	for (path, reason) in &reindexed.problems {
		eprintln!("warning: {}: {reason}", library.join(path).display());
	}
	if !reindexed.problems.is_empty() {
		bail!(
			"{} problem(s) in the library; no file was changed",
			reindexed.problems.len()
		);
	}
	Ok(())
}

fn print_document(view: &ops::DocumentView) {
	println!("{}\t{}", view.sidecar.id, view.path.display());
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
	let cli = Cli::parse();
	logging();
	let reads_only = matches!(
		cli.command,
		Command::Search { .. }
			| Command::Status
			| Command::User {
				command: UserCommand::List
			}
	);
	let sign_in = sign_in(&cli.sign_in)?;
	let models = models(&cli.model)?;
	let app = if reads_only {
		App::open_read_only(&cli.library, models)
	} else {
		App::open(&cli.library, models)
	}
	.with_context(|| format!("cannot open the library at {}", cli.library.display()))?;
	app.auth.configure(sign_in)?;
	match cli.command {
		Command::Serve { listen } => serve(app, listen).await?,
		Command::Ingest { files } => ingest(&app, &files).await?,
		Command::Reindex => reindex(&app)?,
		Command::Search { query, json } => {
			let mut query = Query {
				text: Some(query),
				..Query::default()
			};
			violet_model::understand(&mut query);
			let result = app.index().search(&query)?;
			if json {
				println!("{}", serde_json::to_string_pretty(&result)?);
			} else {
				for hit in &result.hits {
					println!(
						"{}\t{}\t{}\t{}",
						hit.id,
						hit.date.as_deref().unwrap_or("-"),
						hit.title,
						hit.path.display()
					);
				}
				eprintln!("{} of {}", result.hits.len(), result.total);
			}
		}
		Command::Split { id, groups } => {
			let groups = groups
				.iter()
				.map(|group| page_group(group))
				.collect::<anyhow::Result<Vec<_>>>()?;
			for id in ops::split(&app, id, &groups)?.result.documents {
				print_document(&ops::document(&app, id)?);
			}
		}
		Command::Rotate { id, pages, degrees } => {
			print_document(&ops::rotate(&app, id, &pages, degrees)?.result);
		}
		Command::Move {
			id,
			title,
			area,
			doc_type,
			issuer,
			date,
			tags,
		} => {
			let patch = Patch {
				title,
				area,
				doc_type,
				issuer: issuer.map(Some),
				date: date.map(Some),
				tags,
				facts: None,
			};
			print_document(&ops::update(&app, id, patch)?.result);
		}
		Command::Reread { ids } => reread(&app, &ids).await?,
		Command::Reanalyze { ids, all, since } => reanalyze(&app, ids, all, since).await?,
		Command::Undo { entry } => println!("{}", ops::undo(&app, entry)?.summary),
		Command::User { command } => user_command(&app, command)?,
		Command::Status => {
			let model = app.model_status().await;
			let status = ops::status(&app, model)?;
			println!("violet {}", status.version);
			println!("inbox\t{}", status.inbox);
			println!(
				"jobs\t{} pending, {} running, {} done, {} failed",
				status.jobs.pending, status.jobs.running, status.jobs.done, status.jobs.failed
			);
			println!("model\tvision {}, text {}", model.vision, model.text);
			if let Some(problem) = &status.journal {
				println!("journal\t{problem}");
			}
			for job in &status.failed {
				println!(
					"failed\t{}\t{}\t{}",
					job.id,
					job.payload["path"].as_str().unwrap_or("?"),
					job.error.as_deref().unwrap_or_default()
				);
			}
		}
	}
	Ok(())
}
