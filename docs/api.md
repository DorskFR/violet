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
| DELETE | `/api/documents/:id` | | `{entry}` (file kept in `.originals/`) |
| POST | `/api/merge` | `{ids: [id, id]}` | `{entry, path, size, sidecar}` |
| POST | `/api/upload` | multipart, field `file` (repeatable) | `{queued: n}` (written to `inbox/`) |
| GET | `/api/activity` | `limit` (default 50) | `[{entry, undone, undoable}]` |
| POST | `/api/activity/:id/undo` | | `{entry}` (the new `undo` entry) |
| GET | `/api/mail` | | `{enabled, accounts: [MailAccount], processed: [ProcessedMail]}` |
| POST | `/api/mail/:account/sync` | | 202 `{account, started: true}`; 404 when mail sync is off or there is no such account |
| POST | `/api/mail/processed/:id/reprocess` | | the forgotten `ProcessedMail`; 404 when there is no such record |

`Hit`, `Job`, `Entry` and `Sidecar` are the JSON forms of the `violet-core` types of the same
name. `Hit.snippet` is HTML with only `<mark>` tags; everything else is escaped.

`entry` is the id of the journal entry the change wrote, to pass to `/api/activity/:id/undo`.
It is `null` for a PATCH or a reanalyze that changes nothing. Errors use 400 (bad input, including an id that
is not a UUID), 404, 409 (undo refused) and 500.

PATCH leaves out what should stay: an absent field is kept, `"issuer": null` or `"date": null`
(or an empty string) clears it. `date` accepts any form `violet-model` can read and is stored
as `YYYY-MM-DD`. A fact without `amount_minor` gets one when its `value` reads as an amount.
`facets.currencies` counts documents by the currency of their facts. Search `min`/`max` are in
minor units; unknown query parameters are ignored everywhere.

Reanalyze asks the model again about the page texts the document already has (no OCR; a PDF
also sends small images of its first pages) and replaces `title`, `area`, `doc_type`, `issuer`,
`date`, `language`, `summary`, `tags`, `facts` and `confidence`. The file and its pages stay as
they are and the document is never split. The answer comes when the model is done, which can
take minutes while it is busy. It answers 400 when no model is configured or the document has
no text, and 409 when the document changed while the model worked. The journal entry is an
`update`, undone like a PATCH.

Split names the parts `<title> (1/2)`, `(2/2)`, ... and keeps the rest of the metadata; merge
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
already waiting (`name (2).pdf`). The body may be up to 200 MB.

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
