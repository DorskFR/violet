# HTTP API

JSON over `/api`. Documents are addressed by the `id` in their sidecar. Pages are 1-based.
Errors are `{"error": "<message>"}` with a 4xx/5xx status.

| Method | Path | Body / query | Answer |
|---|---|---|---|
| GET | `/api/status` | | `{version, inbox, jobs: {pending, running, done, failed}, failed: [Job], model: {vision: bool, text: bool}, journal: string\|null}`; `journal` says why changes are refused: a journal line that cannot be read, or a change that did not finish and could not be rolled back |
| GET | `/api/search` | `q, area, type, issuer, tag (repeatable), from, to, min, max, currency, limit, offset` | `{total, hits: [Hit]}` |
| GET | `/api/facets` | | `{areas, doc_types, issuers, tags, years, currencies}`, each `[{value, count}]` |
| GET | `/api/documents/:id` | | `{path, size, sidecar}` |
| GET | `/api/documents/:id/file` | | the file, `inline` disposition (see below) |
| GET | `/api/documents/:id/pages/:n` | `w` (max side, 1..=4096, default 1200), `v` (optional, first 8+ chars of `sidecar.sha256`) | PNG |
| PATCH | `/api/documents/:id` | `{title?, area?, doc_type?, issuer?, date?, tags?, facts?}` | `{entry, path, size, sidecar}` (renamed/moved to match) |
| POST | `/api/documents/:id/split` | `{groups: [[1,2],[3]]}` | `{entry, documents: [id]}` |
| POST | `/api/documents/:id/rotate` | `{pages: [1], degrees: 90}` | `{entry, path, size, sidecar}` |
| POST | `/api/documents/:id/delete-pages` | `{pages: [2]}` | `{entry, path, size, sidecar}` |
| POST | `/api/documents/:id/reanalyze` | | `{entry, path, size, sidecar}` (renamed/moved to match) |
| POST | `/api/documents/:id/reread` | | `{entry, path, size, sidecar}` (same id, renamed/moved to match) |
| POST | `/api/documents/:id/replace` | multipart, one field `file` | `{entry, path, size, sidecar}` (same id) |
| DELETE | `/api/documents/:id` | | `{entry}` (file kept in `.originals/`) |
| POST | `/api/merge` | `{ids: [id, id]}` | `{entry, path, size, sidecar}` |
| POST | `/api/upload` | multipart, field `file` (repeatable) | `{queued: n}` (written to `inbox/`) |
| GET | `/api/activity` | `limit` (default 50) | `[{entry, undone, undoable}]` |
| POST | `/api/activity/:id/undo` | | `{entry}` (the new `undo` entry) |
| GET | `/api/mail` | | `{enabled, accounts: [MailAccount], processed: [ProcessedMail]}` |
| POST | `/api/mail/:account/sync` | | 202 `{account, started: true}`; 404 when mail sync is off or there is no such account |
| POST | `/api/mail/processed/:id/reprocess` | | the forgotten `ProcessedMail`; 404 when there is no such record |

Search `q`: every word or quoted phrase must appear in the document (metadata or pages), in any
case and with or without accents. Words of three letters or fewer match whole words only, longer
ones also as prefixes, `word*` forces a prefix; CJK text matches anywhere. Short French and English
stop-words (`de`, `la`, `the`, …) are ignored next to other words. Matches in the title, issuer,
type, tags and summary rank above matches in the pages, a query that names the issuer or type ranks
it higher still, and equal scores list the newest first. Without `q`, newest first; undated
documents last. `Hit.snippet` comes from the page that matches the most terms.

`Hit`, `Job`, `Entry` and `Sidecar` are the JSON forms of the `violet-core` types of the same
name. `Hit.snippet` is HTML with only `<mark>` tags; everything else is escaped.

`entry` is the id of the journal entry the change wrote, to pass to `/api/activity/:id/undo`.
It is `null` for a PATCH, a reanalyze or a reread that changes nothing. Errors use 400 (bad input, including an id that
is not a UUID), 403 (cross-site, below), 404, 409 (undo refused), 415 (wrong `Content-Type`) and 500.

Requests that change something (POST, PATCH, DELETE) are refused with 403 when
`Sec-Fetch-Site` is `cross-site`, or when an `Origin` header is present and does not name the
request's `Host` (or `X-Forwarded-Host`). Clients without `Origin`, like curl and the CLI, pass.
Endpoints with a JSON body (PATCH, split, rotate, delete-pages, merge) need
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
keeps the metadata of the first id. Both, and delete, make new ids or retire old ones, so the
old id answers 404 until the change is undone.

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

Readable inputs: PDF, PNG, JPEG, GIF and TIFF (images become a PDF, one page per TIFF page), docx, xlsx, pptx, odt,
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

`GET /healthz` answers 200 while the process runs; `GET /readyz` once the index is open.
Every other non-`/api` path serves the UI (SPA fallback to `index.html`).
