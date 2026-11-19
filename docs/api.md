# HTTP API

JSON over `/api`. Documents are addressed by the `id` in their sidecar. Pages are 1-based.
Errors are `{"error": "<message>"}` with a 4xx/5xx status; a 429 carries `Retry-After` (seconds).

| Method | Path | Body / query | Answer |
|---|---|---|---|
| GET | `/api/status` | | `{version, inbox, jobs: {pending, running, done, failed}, failed: [Job], model: {vision: bool, text: bool}, journal: string\|null}`; `journal` says why changes are refused: a journal line that cannot be read, or a change that did not finish and could not be rolled back |
| GET | `/api/search` | `q, area, type, issuer, tag (repeatable), from, to, min, max, currency, limit, offset` | `{total, hits: [Hit], partial, understood}` |
| GET | `/api/facets` | | `{areas, doc_types, issuers, tags, years, currencies}`, each `[{value, count}]` |
| GET | `/api/documents/:id` | | `{path, size, sidecar, duplicates: [id]}` |
| GET | `/api/documents/:id/file` | | the file, `inline` disposition (see below) |
| GET | `/api/documents/:id/pages/:n` | `w` (max side, 1..=4096, default 1200), `v` (optional, first 8+ chars of `sidecar.sha256`) | PNG |
| PATCH | `/api/documents/:id` | `{title?, area?, doc_type?, issuer?, date?, tags?, facts?}` | `{entry, path, size, sidecar, duplicates}` (renamed/moved to match) |
| POST | `/api/documents/:id/split` | `{groups: [[1,2],[3]]}` | `{entry, documents: [id]}` |
| POST | `/api/documents/:id/rotate` | `{pages: [1], degrees: 90}` | `{entry, path, size, sidecar, duplicates}` |
| POST | `/api/documents/:id/delete-pages` | `{pages: [2]}` | `{entry, path, size, sidecar, duplicates}` |
| POST | `/api/documents/:id/reanalyze` | | `{entry, path, size, sidecar, duplicates}` (renamed/moved to match) |
| POST | `/api/documents/:id/reread` | | `{entry, path, size, sidecar, duplicates}` (same id, renamed/moved to match) |
| POST | `/api/documents/:id/replace` | multipart, one field `file` | `{entry, path, size, sidecar, duplicates}` (same id) |
| DELETE | `/api/documents/:id` | | `{entry}` (file kept in `.originals/`) |
| POST | `/api/documents/:id/not-duplicate` | `{of: id}` | `{entry, path, size, sidecar, duplicates}` |
| GET | `/api/duplicates` | | `{pairs: [[Copy, Copy]]}`, newest first |
| POST | `/api/merge` | `{ids: [id, id]}` | `{entry, path, size, sidecar, duplicates}` |
| POST | `/api/upload` | multipart, field `file` (repeatable) | `{queued: n}` (written to `inbox/`) |
| GET | `/api/activity` | `limit` (default 50) | `[{entry, undone, undoable}]` |
| POST | `/api/activity/:id/undo` | | `{entry}` (the new `undo` entry) |
| GET | `/api/mail` | | `{enabled, accounts: [MailAccount], processed: [ProcessedMail]}` |
| POST | `/api/mail/:account/sync` | | 202 `{account, started: true}`; 404 when mail sync is off or there is no such account |
| POST | `/api/mail/processed/:id/reprocess` | | the forgotten `ProcessedMail`; 404 when there is no such record |
| GET | `/api/auth` | | `{mode, setup, user: Me\|null}` (below); always public |
| POST | `/api/auth/setup` | `{name, password}` | `Me` and the session cookie |
| POST | `/api/auth/login` | `{name, password}` | `Me` and the session cookie |
| POST | `/api/auth/passkey/start` | `{name}` | `{ceremony, options: {publicKey}}` |
| POST | `/api/auth/passkey/finish` | `{ceremony, credential}` | `Me` and the session cookie |
| POST | `/api/auth/logout` | | 204, cookie cleared |
| PUT | `/api/account/password` | `{current?, password}` | `{entry}`, and a new session cookie |
| GET | `/api/account/passkeys` | | `[{name, created}]` |
| POST | `/api/account/passkeys/start` | `{name}` | `{ceremony, options: {publicKey}}` |
| POST | `/api/account/passkeys/finish` | `{ceremony, credential}` | `{entry, passkey: {name, created}}` |
| PATCH | `/api/account/passkeys/:name` | `{name}` | `{entry}` |
| DELETE | `/api/account/passkeys/:name` | | `{entry}`; 409 for the last credential |
| GET | `/api/account/sessions` | | `[{id, label, created, last_seen, expires, credential, current}]` |
| DELETE | `/api/account/sessions/:id` | | 204 |
| GET | `/api/me` | | `Me` (below) |
| GET | `/api/users` | | `{configured, users: [{name, role, password, passkeys, tokens: [{name, scope, expires}]}]}` |
| POST | `/api/users` | `{name, role, password?}` | `{entry}` |
| PATCH | `/api/users/:name` | `{role}` | `{entry}` |
| DELETE | `/api/users/:name` | | `{entry}` |
| PUT | `/api/users/:name/password` | `{password}` | `{entry}` |
| POST | `/api/users/:name/tokens` | `{name, scope, expires?}` | `{token, entry}`; `token` is shown this once |
| DELETE | `/api/users/:name/tokens/:token` | | `{entry}` |
| GET | `/api/areas` | | `{private: [{area, members}]}` |
| PUT | `/api/areas` | `{private: [{area, members}]}` | `{entry}` |

Search `q`: every word or quoted phrase must appear in the document (metadata or pages), in any
case and with or without accents. Words of three letters or fewer match whole words only, longer
ones also as prefixes, `word*` forces a prefix; CJK text matches anywhere. Short French and English
stop-words (`de`, `la`, `the`, …) are ignored next to other words. Matches in the title, issuer,
type, tags and summary rank above matches in the pages, a query that names the issuer or type ranks
it higher still, and equal scores list the newest first. Without `q`, newest first; undated
documents last. `Hit.snippet` comes from the page that matches the most terms.

A few household words also match their synonyms in French, English and Japanese (`fiche de paie`,
`bulletin de paie`, `payslip`, `給与明細`; `voiture`, `auto`, `car`; `relevé`, `statement`; `facture`,
`invoice`, `請求書`; `impôts`, `tax`, `税`; `assurance`, `insurance`, `保険`; `électricité`,
`electricity`, `電気`; `loyer`, `rent`, `家賃`). When no document has every word, the documents
with any of them are returned with `partial: true`.

`q` is read for filters before the words are searched: a year (`2023`, `2023年`) is that year, a
month (`mars`, `March`, `Mar`, `3月`) with a year that month, alone that month in any year, several
years the range they span; an amount (`133,90 €`, `42 EUR`, `¥3,000`, `1 234,56 €`, or `133,90`
without a currency, read in hundredths) sets `min`, `max` and `currency`, several amounts in one
currency a range; `dernier`, `latest`, `last`, `最新` sorts newest first. Quoted words are never
read. A parameter given explicitly wins and the words that would have set it are dropped.
`understood` says what was applied: `{from?, to?, month?, min?, max?, currency?, latest?, terms:
[{text, kind}]}`, `kind` one of `date`, `amount`, `latest`, `text` as typed in `q`; quoting a
term's text in `q` searches it as words instead.

`Hit`, `Job`, `Entry` and `Sidecar` are the JSON forms of the `violet-core` types of the same
name. `Hit.snippet` is HTML with only `<mark>` tags; everything else is escaped.

`entry` is the id of the journal entry the change wrote, to pass to `/api/activity/:id/undo`.
It is `null` for a PATCH, a reanalyze or a reread that changes nothing. Errors use 400 (bad input, including an id that
is not a UUID), 401 (no user, below), 403 (cross-site, a read-only token, or not an admin), 404,
409 (undo refused), 415 (wrong `Content-Type`) and 500.

Requests that change something (POST, PATCH, DELETE) are refused with 403 when
`Sec-Fetch-Site` is `cross-site`, or when an `Origin` header is present and does not name the
request's `Host` (or `X-Forwarded-Host`). Clients without `Origin`, like curl and the CLI, pass.
Endpoints with a JSON body (PATCH, split, rotate, delete-pages, merge, not-duplicate) need
`Content-Type: application/json`, upload and replace need `multipart/form-data`; anything else
is 415.

PATCH leaves out what should stay: an absent field is kept, `"issuer": null` or `"date": null`
(or an empty string) clears it. `date` accepts any form `violet-model` can read and is stored
as `YYYY-MM-DD`. A month or a one-month period (`mars 2014`, `令和6年3月分`,
`du 01/03/2014 au 31/03/2014`) is stored as its first day with `"date_precision": "month"` in
the sidecar; the field is absent for a day. A fact without `amount_minor` gets one when its `value` reads as an amount.
`facets.currencies` counts documents by the currency of their facts. Search `min`/`max` are in
minor units; unknown query parameters are ignored everywhere.

Reanalyze asks the model again about the page texts the document already has (no OCR; a PDF
also sends small images of its first pages) and replaces `title`, `area`, `doc_type`, `issuer`,
`date`, `date_precision`, `language`, `summary`, `tags`, `facts` and `confidence`. The file and its pages stay as
they are and the document is never split. The answer comes when the model is done, which can
take minutes while it is busy. It answers 400 when no model is configured or the document has
no text, and 409 when the document changed while the model worked. The journal entry is an
`update`, undone like a PATCH.

Reread reads the file of the document again as an inbox file is read (text layer or OCR of
every page, then the analysis as one document, never split, no page dropped) and replaces its
pages and metadata under the same id. An image or a mail an earlier version kept as it was
becomes the PDF or the mail it is read as now. The entry is an `update` when the bytes stay the
same and a `replace` otherwise. 400 when no model is configured or the format cannot be read
(`unsupported format .doc`), 409 when the document changed meanwhile.
`violet reread <id>...` does the same from the command line.

Replace puts one uploaded file (up to 200 MB) in the place of the file of the document, for
example a legacy `.doc` converted to `.docx` or PDF: the old file and its sidecar go to
`.originals/`, the new file is read and analysed as reread does, keeps the id and is added to
`sources`. The entry is a `replace`; undoing it puts the old file and sidecar back. 400 for no
file, several files, an unsupported format or no model, 404 for an unknown id.

Split groups must hold every page of the document exactly once (400 otherwise); pages are
removed with delete-pages. Split names the parts `<title> (1/2)`, `(2/2)`, ... and keeps the rest of the metadata; merge
keeps the metadata of the first id and joins the `sources` of consecutive parts of one file. Both, and delete, make new ids or retire old ones, so the
old id answers 404 until the change is undone.

Near-duplicates are flagged for review, never merged or deleted by the server. Documents that
share a `sources[].sha256` are parts of one scan and never copies. Otherwise two documents with
the same `sha256` (the same file, for example the same part split from two scans of one bundle)
are near-duplicates whatever their metadata, and two different files are when they have the
same issuer (ignoring case), date and `doc_type` (documents
without an issuer or a date are never flagged), their page counts differ by at most one, and
their text (every page, whitespace collapsed) is the same, or at least 95 % the same by edit
distance with exactly the same numbers in it (amounts, dates, references), so a monthly
template with another period or amount is not flagged. Texts over 30 000 characters are
compared only for equality. The pairs live in the index, are recomputed for a document on
every change to it and for all by `violet reindex`. `duplicates` lists the near-duplicates of
the document that the caller sees; `/api/duplicates` lists a pair only when the caller sees
both documents. Not-duplicate adds each id to the other's `sidecar.not_duplicates` (absent
when empty), so the pair is never flagged again, even after a reindex; the entry is a
`not_duplicate` that undo reverses, and `entry` is `null` when both already said so. 400 when
`of` is the document itself, 404 when either is unknown to the caller. Keeping one of a pair is
the delete of the other.

A `Copy` in `/api/duplicates` is a `Hit` with what tells it apart from the other side:
`created_at` (when it was filed, from the sidecar), `size` (bytes of the file, `null` when it
cannot be read), `sha256` (equal on both sides when they are the same file) and `sources`, one
`{original_name, mail?: {account, subject, date}}` per `sidecar.sources` entry, `mail` only
when the file came from a mail (`subject` and `date` may be `null`).

Page images: `sidecar.sha256` changes with every page edit. A request with `v` matching it is
answered `Cache-Control: immutable`; without `v` it must be revalidated (`ETag`). Only PDFs
have pages; other files answer 404. The file itself is served `inline` only as PDF, image or
plain text; anything else (HTML, office files, ...) is an `attachment` so it cannot run in the
UI's origin.

Undo answers 409 when the entry was already undone, is itself an undo, a later entry that is
not undone changed one of the same documents (undo that one first; `undoable` in the activity
feed says so), or a file is no longer the one its sidecar describes. Undoing an ingest takes
its documents out of the library into `.originals/`, next to the dropped file, which is not
put back in the inbox; drop it again to file it again. Undoing a duplicate changes nothing.

Upload keeps the last path component of each file name, sanitised, and never replaces a file
already waiting (`name (2).pdf`), even when several arrive at once; `violet ingest` and mail
sync name their files the same way. The body may be up to 200 MB. A job files the bytes it
read: a file written over in the inbox while it is being read stays there and is filed by a
job of its own, and a second copy of a waiting file is recorded as a duplicate once the first
is filed.

A page is left out of a filed document as blank only when it has no text at all; a page with
any text, even one word or a stamp, is kept.

Readable inputs: PDF, PNG, JPEG, GIF and TIFF (images become a PDF, one page per TIFF page; palette TIFFs included), docx, xlsx, pptx, odt,
ods, odp, eml (also a file without extension that starts with mail headers), txt, md, csv,
tsv, vcf, ics, rtf, json and html. Anything else fails its job with `unsupported format .<ext>`
and stays in `inbox/`, as does an image that does not decode and a file whose analysis puts no
page with text in a document even after a retry; failed jobs are listed in `status.failed` and
tried again when the file changes or the process restarts. A page without text is shown to the
model as a photo (`doc_type` `photo`, a description as summary). A PDF page whose text layer is
garbled (symbols inside words, words without vowels, Latin text where a CJK document should
have CJK) is read by OCR instead; reread judges it by the document's language. A text file without any text
is filed under its name, tagged `no-text`.

Mail sync ([docs/mail.md](mail.md)) is off unless `VIOLET_MAIL_CONFIG` is set; then
`enabled` is true. `MailAccount` is `{name, host, folders, interval_secs, running, last_run,
last_success, last_error, last_report, counts: {ingested, skipped, failed}}`; `last_report` is
`{mails, ingested, files, skipped, failed, errors}` of the last run, times are RFC 3339 or
`null`, and no credential is ever part of it. `processed` lists the 50 newest handled mails of
every account: `{id, account, folder, uidvalidity, uid, message_id, sender, subject, received,
status, files, detail, error, processed_at}`, `status` one of `ingested`, `skipped` (`detail`
says why) and `failed` (`error`); an ingested mail the server refused to mark also has an
`error`. Sync answers before the run ends; poll `GET /api/mail` for `running` and the result.
Reprocess forgets the record so the next sync takes the mail again if its folder search still
matches it. The mail records are not rebuilt from the sidecars by `violet reindex`: like the
job queue they live only in `.index.sqlite`.

## Users

Without `.users.json` at the library root, every request runs as one implicit admin (`/api/me`
is `{name: null, role: "admin", via: "implicit", users: false, ...}`), as a single-user library
always has. Once the file exists, a request runs as, in this order: the user of its session
cookie (unless `VIOLET_LOGIN` is `off`), else the owner of an `Authorization: Bearer vt_…`
token, else what `VIOLET_LOGIN` says about nobody signed in:

| `VIOLET_LOGIN` | nobody signed in |
|---|---|
| `off` (default) | 401. `/api/auth/*` (but `GET /api/auth`) and `/api/account/*` answer 404 |
| `optional` | the implicit admin |
| `required` | 401 once some account has a password or a passkey, before that the implicit admin |

A token that is unknown or past its `expires` answers 401. `via` says which one it was:
`implicit`, `session`, `token` or `local` (the CLI). A users or areas file that does not parse
answers 500 to every API request until it is fixed. `VIOLET_PUBLIC_URL` (e.g.
`https://violet.dorsk.dev`, an origin without a path) must be set unless the mode is `off`: its
host is the WebAuthn relying party id and its origin the only one passkeys are accepted from.

`Me` is `{csrf, name, role, scope, via, areas, users, password, passkeys}`: `csrf` is the CSRF
token of the session (null unless `via` is `session`), `users` whether there is a users file,
`password` whether the user has one, `passkeys` the names of theirs.

`GET /api/auth` answers `{mode: "off"|"optional"|"required", setup, user}` to anyone: `setup` is
true while the mode is not `off` and no account has a credential, `user` is `Me`, or null when
the request would get 401.

**Signing in.** `POST /api/auth/login {name, password}` answers `Me` with a
`Set-Cookie: __Host-violet_session=…; Path=/; HttpOnly; Secure; SameSite=Lax`, a new session
each time; a wrong password and an unknown name both answer 401 `wrong name or password`. Five
failures for a name in 15 minutes refuse that name for 15 minutes, twenty from one client
refuse that client, a hundred overall refuse every password login, with 429 and `Retry-After`,
whether the next password is right or not. The client is the last `X-Forwarded-For` hop when
the peer is in `VIOLET_TRUSTED_PROXIES` (addresses or CIDR blocks), else the peer. Passkeys are
name first: `POST /api/auth/passkey/start {name}` answers `{ceremony, options}`, where `options`
is the WebAuthn `{publicKey: PublicKeyCredentialRequestOptionsJSON}` for the browser, and
`POST /api/auth/passkey/finish {ceremony, credential}` takes the `PublicKeyCredential.toJSON()`
of the assertion. An unknown name, a name without passkeys and an assertion that does not verify
answer 401 `wrong name or passkey`. A ceremony is good once, for 5 minutes. `POST
/api/auth/setup {name, password}` creates the first admin, with that password, and signs them
in; only the implicit admin may (403 otherwise), and only while no account has a credential
(409). `POST /api/auth/logout` ends the session and clears the cookie (204).

**Sessions** last 30 days idle and 180 days at most. A session is bound to the credential it
was opened with: changing or resetting the password, removing the passkey, removing the user,
or undoing a change that brought back an older password ends it. The request that changes
one's own password gets a new session cookie, with the same CSRF token.

**CSRF.** A request signed in by the cookie that changes something (any method but GET, HEAD,
OPTIONS) must send the session's token in `X-CSRF-Token` and an `Origin` that matches the host
(or `Sec-Fetch-Site: same-origin`); 403 otherwise. Login, setup and passkey sign-in need that
`Origin` too. Any request whose `Origin` names another host, or with
`Sec-Fetch-Site: cross-site`, cannot change anything; requests with a bearer token and no
`Origin`, like curl and the CLI, pass.

**One's account.** `/api/account/*` needs a session: 401 when nobody is signed in and the mode
requires it, 403 `sign in first` for tokens and the implicit admin. `PUT /api/account/password
{current?, password}` needs `current` when the user has a password (400 without it, 403 when
wrong, counted like a wrong login). Passwords are 12 to 1024 bytes and not the user name (400
otherwise). `POST /api/account/passkeys/start {name}` answers `{ceremony, options}` with the
`{publicKey: PublicKeyCredentialCreationOptionsJSON}` (user verification required, attestation
`none`, the user's passkeys excluded); `…/finish {ceremony, credential}` takes the
`toJSON()` of the new credential, 400 when it does not verify or the ceremony is gone or
another session's. A passkey name is 1 to 64 characters without spaces at either end, unique
per user regardless of case. Removing the last credential of an account (no password, one
passkey) is 409. `GET /api/account/sessions` lists one's live sessions: `id` (16 hex characters,
a prefix of the token's hash), `label` (browser and system from the User-Agent), `created`,
`last_seen`, `expires` (when it ends if it stays idle), `credential` (`password` or `passkey`)
and `current`; `DELETE /api/account/sessions/:id` ends one (204, and clears the cookie when it
is the current one), 404 for an id that is not one's own.

`role` is `member` or `admin`; `scope` is what the request may do, the token's scope capped by
the role (a session has the role's): `read` (GET only, others 403), `write` (documents, uploads,
undo of one's own entries), `admin` (also `/api/users`, `/api/areas`, `/api/mail`, and undoing
anyone's entry). Members undo their own entries and the agent's entries made for them; entries
with `actor: "user"` (the implicit admin, and every entry from before users) belong to nobody in
particular, so only admins undo them. Every change of the users file or of the private areas
is a `settings` journal entry (`{file, original, sha256}`, the previous version in
`.originals/<sha256>.users.json`), undone like any other; a later change of the same file must
be undone first. Passwords are stored as argon2id PHC strings (m=19 MiB, t=2, p=1; other
parameters still verify), passkeys as webauthn-rs credentials, and neither appears in any
answer. A change of the users file through the API is refused with 409 when its sender would no
longer be an admin afterwards (an admin resets their own password under their account, not with
`PUT /api/users/:name/password`). Tokens are stored as the SHA-256 of the whole secret;
`violet user token add <user> <name> [--scope] [--expires]` prints one, `violet user token
revoke <user> <name>` revokes it, `violet user add <name> [--role] [--password-stdin]`,
`violet user password <name>` (the password on standard input), `violet user list|remove`
manage users. The CLI runs as `VIOLET_USER`, else the OS user, with full access.

`areas` in `/api/me` are the areas of the documents the user sees. A private area
(`.areas.json`, `PUT /api/areas`, members must be users) is seen only by its members: its
documents are left out of search, facets and activity for everyone else, and every endpoint
about one of them answers 404. An entry is in a user's activity when every document it names is
in an area they see; settings entries only for admins. A PATCH into an area the user cannot see
is 400, and so is a merge of documents from areas that different people can see. The model
files an upload into a private area only when its uploader is a member (else `Unsorted`); the
uploader is the user who sent it, the mail rule's `hints.owner` for mail, else nobody (never a
private area). `Entry.actor` is `user` (the implicit admin), `user:<name>` or `agent`, with
`on_behalf_of: <name>` when the agent worked for a user.

`GET /healthz` answers 200 while the process runs; `GET /readyz` once the index is open.
Every other non-`/api` path serves the UI (SPA fallback to `index.html`).
