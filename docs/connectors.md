# Connectors

A connector brings documents into `inbox/` on a schedule: it signs in to a website as you and
takes its payslips, statements or invoices, or it takes the attachments of a mailbox. Each
connector runs on its own; one that fails does not hold up the others.

Connectors are added, edited and removed in Settings, Connectors, by anyone signed in who may
change documents (the `write` scope); [docs/api.md](api.md) has the endpoints behind it. A
connector belongs to its owner: the user who added it, or the user an admin chose when adding
or editing it. Members see and manage their own and cannot give them away; admins see and
manage all of them. Adding, editing or removing one takes effect at once, without a restart.

- Name: letters, digits, `-`, `_`, `.`; unique, and fixed once added. It names the connector
  in the API, in the records and in each document's sidecar.
- Kind: `moneyforward`, see below, or `imap`, a mailbox, see [docs/mail.md](mail.md). Fixed
  once added.
- Owner: only an admin sees this field, in a library with users. Giving a connector to another
  user also gives them the record of what it handled.
- Username and password: what to sign in to the site with. They are kept in
  `.connectors.sqlite`. The password is never shown again: not in the settings, an API answer,
  a log or an error. Leaving it empty in an edit keeps it. Changing the username or the
  password drops the saved sign-in, so the connector needs Connect again (a mailbox signs in at
  each run and never needs Connect).
- Schedule: see below, with the UTC offset its clock times are in, as `+09:00`, `-03:30` or
  `Z`. The form starts from the browser's. It is a fixed offset, not a time zone: it does not
  follow daylight saving.
- Area and tags: applied to every document the connector brings, over what the model chose.
- The options of the kind; each kind refuses the ones it does not know.

The documents a connector brings are filed for its owner, who may get them in their private
areas. Removing a connector deletes its username, password and saved sign-in. Its name and its
owner stay known, and so does the record of which remote documents it handled: when its owner
adds a connector under that name again, it does not fetch everything again. When anyone else
takes the name, an admin included, that record is forgotten first: they see none of the
earlier documents and their connector fetches whatever the site lists. The same holds for the
mails of a mailbox.

`VIOLET_CONNECTORS_CONFIG`, the JSON config file it named and the `username_env` and
`password_env` variables are gone: the server logs a warning when the variable is still set
and ignores it. Connectors that file described have to be added again in the settings, and
connected again.

## Schedules

| Schedule | Runs |
|---|---|
| `every <n>m`, `every <n>h`, `every <n>d` | that long after the last run that succeeded |
| `daily HH:MM` | every day at that time |
| `weekly <mon\|tue\|wed\|thu\|fri\|sat\|sun> HH:MM` | that day of each week |
| `monthly <1-28> HH:MM` | that day of each month |

The last run and the last one that succeeded are kept, so a restart does not run a connector
again, and a run that was due while the server was down happens once, right after the start.
A new connector waits until it is connected once (see below); a mailbox runs as soon as it is
added. A run that fails is tried again after 30 seconds,
then twice as long each time, up to every 6 hours, until one succeeds; the schedule counts from
there. `POST /api/connectors/:name/run`, or Run now in the settings, runs it at once.

## Signing in

This is for sites. A mailbox signs in with its username and password at each run, is never
asked for a code, and has no Connect.

A run signs in with the username and password and with what the connector saved from earlier sign-ins
(for example the cookie of a trusted device). Some sites then send a code by mail or SMS. A
scheduled run never waits for one: it stops, the connector shows `needs_login` and does not
run by itself again until you connect it. A new connector shows `needs_login` and does not run
by itself until its first connect, so no code is sent while nobody is there to enter it.

In Settings, Connectors: Connect starts the sign-in. When the site sends a code,
enter it within 15 minutes; a refused code can be typed again, and Send a new code starts over.
Once signed in, the connector runs at once and then on its schedule.

## What is kept

- `.connectors.sqlite` in the library root: the connectors themselves with their usernames
  and passwords, what each saved to sign in with, its last runs, and the name and owner of
  each removed connector. It is not library data
  and not in the journal. Losing it means adding the connectors again and entering a code for
  each site, nothing else. Treat it as a secret. A file from before connectors were kept in
  it is brought up to date when the server starts; the saved sign-ins it held go, since
  nothing describes their connectors any more.
- `.index.sqlite`: which remote documents were handled (`connector_items`) and where the
  files waiting in the inbox came from (`connector_files`). `violet reindex` does not rebuild
  them; without them every document the site still lists is fetched again, and the copies are
  flagged as duplicates.

A remote document is fetched once per connector and remote id. One that failed is tried again
on every run; an empty one, or one over 100 MB, is skipped. Fetch again in the settings (or
`POST /api/connectors/items/:id/refetch`) forgets a record and runs the connector. Each filed
document keeps where it came from in its sidecar:
`sources[].connector = {name, kind, remote_id, title?, date?}`.

## Money Forward (`moneyforward`)

Brings an employee's payslips (給与明細) and bonus slips (賞与明細) from Money Forward Cloud
Payroll and their withholding slips (源泉徴収票) from Cloud Year-End Adjustment, as the PDFs the
employee pages offer. It signs in with the employee's Money Forward ID, by email and password;
an ID that only signs in through Google, Apple or a passkey cannot be used.

- Username and password: the email and the password of the Money Forward ID.
- Documents (`options.documents`): which of `payslips`, `bonus_slips` and `withholding_slips`
  to bring. Default all three. Leave out the ones the employer does not use: a list page that
  fails fails the run.
- Since (`options.since`): a date as `YYYY-MM-DD`; documents dated before it are left out. A
  payslip or a bonus slip is dated by its payment day, a withholding slip by the day it was
  issued. A document whose date the list does not tell is always brought. Without it the first
  run brings the whole history.

Connecting it: Money Forward mails a 6-digit code to the account's address the first time a
device signs in. In Settings, Connectors, press Connect and enter that code. The connector then
keeps the cookie that makes it a device Money Forward knows (`sih`, good for about 13 months)
and nothing else; later sign-ins use the password and that cookie, without a code. When the
cookie runs out or is revoked, the next run stops with `needs_login`, a code is mailed, and
Connect sends another one to enter.

What it brings: every slip the list pages link to, once each. With several accounts under one
ID it takes the one Money Forward marks as active.

| Document | Remote id | File name |
|---|---|---|
| Payslip | `payslip:<id>` | `moneyforward-payslip-2026-10-23.pdf` |
| Bonus slip | `bonus:<id>` | `moneyforward-bonus-2026-06-30.pdf` |
| Withholding slip | `withholding:<id>` | `moneyforward-withholding-R07.pdf` (the 令和 year) |

A name falls back to the id of the document when its list entry tells no date. An answer that
is not a PDF is recorded as failed and tried again on the next run.

## Adding a kind

A kind runs one of two ways, and is listed in `kinds::builtin()` either way:

```rust
pub fn builtin() -> Kinds {
	Kinds::new()
		.with(moneyforward::kind(), moneyforward::connector)
		.with_runner(mail::kind::kind(), mail::kind::Imap)
}
```

- A site is a module in `crates/violet/src/connectors/` that implements `Connector`: it signs
  in, lists and fetches, and the framework keeps which documents were taken. The rest of this
  section is about that.
- A `Runner` does a whole run itself, for a source that listing and fetching does not fit. It
  checks the options (`check`), runs (`run`, with the app, the config, the names of the
  owner's other connectors of the kind and a `still` to ask whether the connector was edited
  or removed meanwhile) and answers for its own records, kept under the connector's name:
  `counts`, `kept` and `forget`. It signs in with the connector's username and password at
  each run and never asks for a code. Schedule, Run now, last run, error, backoff, owner and
  the API are the framework's as for a site. `imap` is one: its run is one sync of the
  mailbox, and its records are `mail_processed` and `mail_files`.

`kind()` returns the `KindView` the settings build their form from: the kind, its label, the
schedule the form starts from and its options, each of one of the types in
[docs/api.md](api.md) (text, number, boolean, one or several choices, date, list of text, or a
repeatable group of those). The other function takes the `ConnectorConfig`
and returns the connector, or an error for options it does not know (parse them with
`deny_unknown_fields`); it is called to check a connector when it is added or edited, and then
for every sign-in. One value serves one sign-in and the run, or the code
entry, that follows.

- `login(credentials, saved)` signs in; `saved` is what `state()` last returned. It answers
  `Login::Ready`, or `Login::NeedsCode { prompt }` after the site sent a code, with a prompt
  such as "Enter the code sent to a***@example.com".
- `submit_code(code)` finishes that sign-in. An error leaves it open for another try.
- `state()` is the JSON to keep for the next sign-in. It is stored after each sign-in and run
  and never shown; `None` keeps what is stored.
- `list()` returns every document the site holds now as `RemoteDoc { remote_id, file_name,
  title, date }`. `remote_id` must stay the same for a document for good; `file_name` needs
  its extension; `date` is `YYYY-MM-DD` when the site tells.
- `fetch(remote_id)` returns the bytes.

Errors are shown to the connector's owner and to admins, and logged: put no credential, cookie or token in them. The
framework does the rest: schedule, retries, which documents are new, the inbox, provenance,
area and tags, the API and the settings page.

`connectors::session` has what a kind needs to talk to a site over `reqwest`: `Session` sends
`Request`s with a browser-like cookie `Jar` and follows redirects itself (a POST answered 301,
302 or 303 goes on as a GET, 307 and 308 repeat it), so cookies set on the way are kept. Its
errors name the host and path only. A `Jar` is serialisable; `jar.only(&["device"])` picks
the cookies worth returning from `state()`, and `Session::with_jar` starts from saved ones.
The jar knows domains, paths, `Secure`, `Expires` and `Max-Age`; it has no public-suffix list
and ignores `SameSite`, which is enough for a session that only talks to the sites of one kind.
