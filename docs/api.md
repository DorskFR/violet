# HTTP API

JSON over `/api`. Documents are addressed by the `id` in their sidecar. Pages are 1-based.
Errors are `{"error": "<message>"}` with a 4xx/5xx status.

| Method | Path | Body / query | Answer |
|---|---|---|---|
| GET | `/api/status` | | `{version, inbox, jobs: {pending, running, done, failed}, failed: [Job], model: {vision: bool, text: bool}}` |
| GET | `/api/search` | `q, area, type, issuer, tag (repeatable), from, to, min, max, currency, limit, offset` | `{total, hits: [Hit]}` |
| GET | `/api/facets` | | `{areas, doc_types, issuers, tags, years}`, each `[{value, count}]` |
| GET | `/api/documents/:id` | | `{path, size, sidecar}` |
| GET | `/api/documents/:id/file` | | the file, `inline` disposition |
| GET | `/api/documents/:id/pages/:n` | `w` (max side, default 1200) | PNG |
| PATCH | `/api/documents/:id` | `{title?, area?, doc_type?, issuer?, date?, tags?, facts?}` | `{path, size, sidecar}` (renamed/moved to match) |
| POST | `/api/documents/:id/split` | `{groups: [[1,2],[3]]}` | `{documents: [id]}` |
| POST | `/api/documents/:id/rotate` | `{pages: [1], degrees: 90}` | `{path, size, sidecar}` |
| POST | `/api/documents/:id/delete-pages` | `{pages: [2]}` | `{path, size, sidecar}` |
| DELETE | `/api/documents/:id` | | `204` (file kept in `.originals/`) |
| POST | `/api/merge` | `{ids: [id, id]}` | `{path, size, sidecar}` |
| POST | `/api/upload` | multipart, field `file` (repeatable) | `{queued: n}` (written to `inbox/`) |
| GET | `/api/activity` | `limit` (default 50) | `[{entry, undone, undoable}]` |
| POST | `/api/activity/:id/undo` | | `{entry}` (the new `undo` entry) |

`Hit`, `Job`, `Entry` and `Sidecar` are the JSON forms of the `violet-core` types of the same
name. `Hit.snippet` is HTML with only `<mark>` tags; everything else is escaped.

`GET /healthz` answers 200 while the process runs; `GET /readyz` once the index is open.
Every other non-`/api` path serves the UI (SPA fallback to `index.html`).
