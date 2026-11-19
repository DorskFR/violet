//! Money Forward Cloud as an employee: payslips and bonus slips from クラウド給与, withholding
//! slips from クラウド年末調整. It signs in with the Money Forward ID like a browser does; the
//! cookie that makes this a trusted device is all that is kept between runs.

mod html;
#[cfg(test)]
mod tests;

use anyhow::{anyhow, bail};
use reqwest::Url;
use serde::Deserialize;

use super::session::{Jar, Response, Session};
use super::{
	BoxFuture, Choice, Connector, ConnectorConfig, Credentials, KindView, Login, OptionInput,
	OptionSpec, RemoteDoc,
};
use html::Slip;

const USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36";
const CODE_PROMPT: &str = "Enter the 6-digit code Money Forward mailed to the account's address";
const DEVICE_COOKIES: &[&str] = &["sih", "cf_clearance"];
const MAX_HOPS: usize = 3;

const PAYSLIPS: &str = "/my/reports/payslips";
const PAYSLIP_PAGE: &str = "/my/reports/periods/{id}/payslip";
const PAYSLIP_PDF: &str = "/my/reports/periods/{id}/payslips_v2.pdf";
const BONUS_SLIPS: &str = "/my/reports/bonus_slips";
const BONUS_SLIP_PDF: &str = "/my/reports/bonus_periods/{id}/bonus_slips_v2.pdf";
const WITHHOLDING_SLIPS: &str = "/my/reports/withholding_slips";
const WITHHOLDING_SLIP_PDF: &str = "/my/reports/terms/{id}/withholding_slip.pdf";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Documents {
	Payslips,
	BonusSlips,
	WithholdingSlips,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct Options {
	#[serde(default = "every_document")]
	documents: Vec<Documents>,
	#[serde(default)]
	since: Option<String>,
}

fn every_document() -> Vec<Documents> {
	vec![
		Documents::Payslips,
		Documents::BonusSlips,
		Documents::WithholdingSlips,
	]
}

impl Options {
	fn parse(options: &serde_json::Value) -> anyhow::Result<Self> {
		let mut options: Self =
			serde_json::from_value(options.clone()).map_err(|error| anyhow!("options: {error}"))?;
		if options
			.since
			.as_deref()
			.is_some_and(|since| !html::is_iso_date(since))
		{
			bail!("options: since is not a date written YYYY-MM-DD");
		}
		let mut once = Vec::new();
		for documents in options.documents {
			if !once.contains(&documents) {
				once.push(documents);
			}
		}
		options.documents = once;
		Ok(options)
	}
}

/// Where the three sites are, without a trailing slash.
#[derive(Debug, Clone)]
struct Hosts {
	id: String,
	payroll: String,
	tax_adjustment: String,
}

impl Default for Hosts {
	fn default() -> Self {
		Self {
			id: "https://id.moneyforward.com".into(),
			payroll: "https://payroll.moneyforward.com".into(),
			tax_adjustment: "https://tax-adjustment.moneyforward.com".into(),
		}
	}
}

pub fn kind() -> KindView {
	let choice = |value: &str, label: &str| Choice {
		value: value.into(),
		label: label.into(),
	};
	let documents = [
		choice("payslips", "Payslips"),
		choice("bonus_slips", "Bonus slips"),
		choice("withholding_slips", "Withholding slips"),
	];
	KindView {
		kind: "moneyforward".into(),
		label: "Money Forward Cloud".into(),
		options: vec![
			OptionSpec {
				name: "documents".into(),
				label: "Documents".into(),
				help: None,
				input: OptionInput::Choices {
					default: documents
						.iter()
						.map(|choice| choice.value.clone())
						.collect(),
					choices: documents.into(),
				},
			},
			OptionSpec {
				name: "since".into(),
				label: "Since".into(),
				help: Some(
					"Older documents are left on the site; empty fetches everything.".into(),
				),
				input: OptionInput::Date,
			},
		],
	}
}

pub fn connector(config: &ConnectorConfig) -> anyhow::Result<Box<dyn Connector>> {
	Ok(Box::new(MoneyForward::new(
		Options::parse(&config.options)?,
		Hosts::default(),
	)?))
}

struct MoneyForward {
	options: Options,
	hosts: Hosts,
	id: Url,
	session: Session,
	credentials: Option<Credentials>,
	code_token: Option<String>,
}

fn blocked(response: &Response) -> anyhow::Result<()> {
	if matches!(response.status, 403 | 429) || response.status >= 500 {
		bail!(
			"{}{} answered {}",
			response.url.host_str().unwrap_or_default(),
			response.url.path(),
			response.status
		);
	}
	Ok(())
}

fn titled(word: &str, label: &str) -> String {
	if label.is_empty() {
		word.into()
	} else if label.contains(word) {
		label.into()
	} else {
		format!("{label} {word}")
	}
}

fn payslip(slip: &Slip) -> RemoteDoc {
	dated("payslip", "給与明細", slip)
}

fn bonus_slip(slip: &Slip) -> RemoteDoc {
	dated("bonus", "賞与明細", slip)
}

fn dated(kind: &str, word: &str, slip: &Slip) -> RemoteDoc {
	let date = html::date(&slip.label);
	let stem = date
		.clone()
		.or_else(|| html::month(&slip.label))
		.unwrap_or_else(|| slip.id.clone());
	RemoteDoc {
		remote_id: format!("{kind}:{}", slip.id),
		file_name: format!("moneyforward-{kind}-{stem}.pdf"),
		title: Some(titled(word, &slip.label)),
		date,
	}
}

fn withholding_slip(slip: &Slip) -> RemoteDoc {
	let stem =
		html::reiwa_year(&slip.label).map_or_else(|| slip.id.clone(), |year| format!("R{year:02}"));
	RemoteDoc {
		remote_id: format!("withholding:{}", slip.id),
		file_name: format!("moneyforward-withholding-{stem}.pdf"),
		title: Some(titled("源泉徴収票", &slip.label)),
		date: html::date(&slip.label),
	}
}

impl MoneyForward {
	fn new(options: Options, hosts: Hosts) -> anyhow::Result<Self> {
		Ok(Self {
			id: Url::parse(&hosts.id).map_err(|_| anyhow!("not a URL"))?,
			options,
			hosts,
			session: Session::new(USER_AGENT)?,
			credentials: None,
			code_token: None,
		})
	}

	fn at_id(&self, url: &Url) -> bool {
		url.origin() == self.id.origin()
	}

	/// The cookies of the ID site that make this a device it knows.
	fn device(&self, jar: &Jar) -> Jar {
		let host = self.id.host_str().unwrap_or_default();
		let mut kept = Jar::new();
		for cookie in jar.only(DEVICE_COOKIES).cookies() {
			let ours = host == cookie.domain
				|| host
					.strip_suffix(cookie.domain.as_str())
					.is_some_and(|rest| rest.ends_with('.'));
			if ours {
				kept.insert(cookie.clone());
			}
		}
		kept
	}

	async fn sign_in(&mut self, credentials: &Credentials) -> anyhow::Result<Login> {
		let id = &self.hosts.id;
		let page = self
			.session
			.get(&format!("{id}/sign_in"))
			.await?
			.success()?;
		if !page.url.path().starts_with("/sign_in") {
			return Ok(Login::Ready);
		}
		let token = html::csrf(&page.text())
			.ok_or_else(|| anyhow!("the Money Forward sign-in page has no form token"))?;
		let answer = self
			.session
			.post_form(
				&format!("{id}/sign_in/email"),
				&[
					("authenticity_token", token.as_str()),
					("mfid_user[email]", credentials.username.as_str()),
					("mfid_user[password]", credentials.password.as_str()),
				],
			)
			.await?;
		blocked(&answer)?;
		let path = answer.url.path();
		if path.starts_with("/email_otp") {
			let token = html::csrf(&answer.text())
				.ok_or_else(|| anyhow!("the Money Forward code page has no form token"))?;
			self.code_token = Some(token);
			return Ok(Login::NeedsCode {
				prompt: CODE_PROMPT.into(),
			});
		}
		if path.starts_with("/sign_in") {
			bail!("Money Forward refused the email or the password");
		}
		answer.success()?;
		Ok(Login::Ready)
	}

	async fn send_code(&mut self, code: &str) -> anyhow::Result<()> {
		let token = self
			.code_token
			.clone()
			.ok_or_else(|| anyhow!("no Money Forward sign-in waits for a code"))?;
		let answer = self
			.session
			.post_form(
				&format!("{}/email_otp", self.hosts.id),
				&[
					("authenticity_token", token.as_str()),
					("_method", "post"),
					("email_otp", code.trim()),
				],
			)
			.await?;
		blocked(&answer)?;
		let path = answer.url.path();
		if path.starts_with("/email_otp") {
			if let Some(token) = html::csrf(&answer.text()) {
				self.code_token = Some(token);
			}
			bail!("Money Forward refused the code");
		}
		self.code_token = None;
		if path.starts_with("/sign_in") {
			bail!("Money Forward ended the sign-in; start it again");
		}
		answer.success()?;
		Ok(())
	}

	/// Picks the account on the selector page and follows the way back to the site.
	async fn choose_account(&mut self, selector: &Response) -> anyhow::Result<Response> {
		let page = selector.text();
		let token = html::csrf(&page)
			.ok_or_else(|| anyhow!("the Money Forward account page has no form token"))?;
		let accounts = html::accounts(&page)
			.ok_or_else(|| anyhow!("the Money Forward account page lists no accounts"))?;
		let account = accounts
			.iter()
			.find(|account| account.active && account.available)
			.or_else(|| accounts.first())
			.ok_or_else(|| anyhow!("the Money Forward account page lists no accounts"))?;
		let action = &account.form_action;
		if !action.starts_with('/') || action.starts_with("//") {
			bail!("the Money Forward account page names no way on");
		}
		let answer = self
			.session
			.post_form(
				&format!("{}{action}", self.hosts.id),
				&[("authenticity_token", token.as_str())],
			)
			.await?;
		blocked(&answer)?;
		Ok(answer)
	}

	/// Gets a page of one of the sites, going through the ID site when the site asks who
	/// this is: the account selector, and a new sign-in when the ID session is gone.
	async fn open(&mut self, url: &str) -> anyhow::Result<Response> {
		let mut response = self.session.get(url).await?;
		let mut signed_in_again = false;
		for _ in 0..MAX_HOPS {
			if !self.at_id(&response.url) {
				return Ok(response);
			}
			blocked(&response)?;
			let path = response.url.path();
			if path.starts_with("/account_selector") {
				let landed = self.choose_account(&response).await?;
				if landed.url.as_str() == url {
					return Ok(landed);
				}
			} else if path.starts_with("/sign_in") && !signed_in_again {
				signed_in_again = true;
				let credentials = self
					.credentials
					.clone()
					.ok_or_else(|| anyhow!("Money Forward asks to sign in"))?;
				if self.sign_in(&credentials).await? != Login::Ready {
					bail!("Money Forward asks for a code again; connect from the settings");
				}
			} else {
				bail!(
					"Money Forward stopped at {}{path}",
					response.url.host_str().unwrap_or_default()
				);
			}
			response = self.session.get(url).await?;
		}
		if self.at_id(&response.url) {
			bail!("Money Forward keeps asking who this is");
		}
		Ok(response)
	}

	async fn page(&mut self, url: &str) -> anyhow::Result<String> {
		Ok(self.open(url).await?.success()?.text())
	}

	async fn payslips(&mut self) -> anyhow::Result<Vec<RemoteDoc>> {
		let payroll = self.hosts.payroll.clone();
		let mut slips = html::payslips(&self.page(&format!("{payroll}{PAYSLIPS}")).await?);
		if let Some(first) = slips.first() {
			let sidebar = format!("{payroll}{}", PAYSLIP_PAGE.replace("{id}", &first.id));
			if let Ok(page) = self.page(&sidebar).await {
				html::merge(&mut slips, html::payslips(&page));
			}
		}
		Ok(slips.iter().map(payslip).collect())
	}

	async fn bonus_slips(&mut self) -> anyhow::Result<Vec<RemoteDoc>> {
		let url = format!("{}{BONUS_SLIPS}", self.hosts.payroll);
		let slips = html::bonus_slips(&self.page(&url).await?);
		Ok(slips.iter().map(bonus_slip).collect())
	}

	async fn withholding_slips(&mut self) -> anyhow::Result<Vec<RemoteDoc>> {
		let url = format!("{}{WITHHOLDING_SLIPS}", self.hosts.tax_adjustment);
		let slips = html::withholding_slips(&self.page(&url).await?);
		Ok(slips.iter().map(withholding_slip).collect())
	}

	fn pdf_url(&self, remote_id: &str) -> Option<String> {
		let (kind, id) = remote_id.split_once(':')?;
		let (host, path) = match kind {
			"payslip" => (&self.hosts.payroll, PAYSLIP_PDF),
			"bonus" => (&self.hosts.payroll, BONUS_SLIP_PDF),
			"withholding" => (&self.hosts.tax_adjustment, WITHHOLDING_SLIP_PDF),
			_ => return None,
		};
		html::is_id(id).then(|| format!("{host}{}", path.replace("{id}", id)))
	}
}

impl Connector for MoneyForward {
	fn login<'a>(
		&'a mut self,
		credentials: &'a Credentials,
		saved: Option<serde_json::Value>,
	) -> BoxFuture<'a, anyhow::Result<Login>> {
		Box::pin(async move {
			if let Some(jar) = saved.and_then(|saved| serde_json::from_value::<Jar>(saved).ok()) {
				let device = self.device(&jar);
				self.session.jar.extend(device);
			}
			self.credentials = Some(credentials.clone());
			self.sign_in(credentials).await
		})
	}

	fn submit_code<'a>(&'a mut self, code: &'a str) -> BoxFuture<'a, anyhow::Result<()>> {
		Box::pin(self.send_code(code))
	}

	fn state(&self) -> Option<serde_json::Value> {
		let device = self.device(&self.session.jar);
		if device.cookies().is_empty() {
			return None;
		}
		serde_json::to_value(device).ok()
	}

	fn list(&mut self) -> BoxFuture<'_, anyhow::Result<Vec<RemoteDoc>>> {
		Box::pin(async move {
			let mut docs = Vec::new();
			for documents in self.options.documents.clone() {
				docs.extend(match documents {
					Documents::Payslips => self.payslips().await?,
					Documents::BonusSlips => self.bonus_slips().await?,
					Documents::WithholdingSlips => self.withholding_slips().await?,
				});
			}
			if let Some(since) = self.options.since.as_deref() {
				docs.retain(|doc| doc.date.as_deref().is_none_or(|date| date >= since));
			}
			Ok(docs)
		})
	}

	fn fetch<'a>(&'a mut self, remote_id: &'a str) -> BoxFuture<'a, anyhow::Result<Vec<u8>>> {
		Box::pin(async move {
			let url = self
				.pdf_url(remote_id)
				.ok_or_else(|| anyhow!("not a Money Forward document"))?;
			let response = self.open(&url).await?;
			if response.status != 200 {
				bail!("Money Forward answered {} for the PDF", response.status);
			}
			if !response.body.starts_with(b"%PDF") {
				bail!("Money Forward answered something that is not a PDF");
			}
			Ok(response.body)
		})
	}
}
