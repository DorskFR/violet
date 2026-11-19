# Connectors

A connector signs in to a website as you and brings its documents (payslips, statements,
invoices) into `inbox/` on a schedule. They are off unless `VIOLET_CONNECTORS_CONFIG` names a
JSON config file; a config that does not parse, names an unknown field, an unknown kind or an
option its kind does not know stops the server at startup. Each connector runs on its own;
one that fails does not hold up the others.

```json
{
  "utc_offset": "+09:00",
  "connectors": [
    {
      "name": "mf",
      "kind": "moneyforward",
      "username_env": "VIOLET_MF_USER",
      "password_env": "VIOLET_MF_PASSWORD",
      "schedule": "monthly 26 09:00",
      "hints": {"owner": "sam", "area": "Work", "tags": ["payroll"]},
      "options": {}
    }
  ]
}
```

- `utc_offset`: the offset the clock times of every schedule are in, as `+09:00`, `-03:30` or
  `Z`. Default UTC. It is a fixed offset, not a time zone: it does not follow daylight saving.
- `name`: letters, digits, `-`, `_`, `.`; unique. It names the connector in the API, in the
  records and in each document's sidecar, so renaming one makes it fetch everything again.
- `kind`: the site; see the kinds below.
- `username_env`, `password_env`: the environment variables holding what to sign in with. They
  are read at each sign-in; credentials are never in the config, an API answer or a log.
- `schedule`: see below.
- `hints`: `area` and `tags` are applied to every document the connector brings, over what the
  model chose; `owner` is the user they are filed for, who may get them in their private areas.
- `options`: settings of the kind; each kind refuses the ones it does not know.

## Schedules

| Schedule | Runs |
|---|---|
| `every <n>m`, `every <n>h`, `every <n>d` | that long after the last run that succeeded |
| `daily HH:MM` | every day at that time |
| `weekly <mon\|tue\|wed\|thu\|fri\|sat\|sun> HH:MM` | that day of each week |
| `monthly <1-28> HH:MM` | that day of each month |

The last run and the last one that succeeded are kept, so a restart does not run a connector
again, and a run that was due while the server was down happens once, right after the start.
A connector that never ran runs at the start. A run that fails is tried again after 30 seconds,
then twice as long each time, up to every 6 hours, until one succeeds; the schedule counts from
there. `POST /api/connectors/:name/run`, or Run now in the settings, runs it at once.

## Signing in

A run signs in with the credentials and with what the connector saved from earlier sign-ins
(for example the cookie of a trusted device). Some sites then send a code by mail or SMS. A
scheduled run never waits for one: it stops, the connector shows `needs_login` and does not
run by itself again until you connect it. A new connector with nothing saved shows
`needs_login` too, and its first run tells whether the site wants a code.

In Settings, Connectors, as an admin: Connect starts the sign-in. When the site sends a code,
enter it within 15 minutes; a refused code can be typed again, and Send a new code starts over.
Once signed in, the connector runs at once and then on its schedule.

## What is kept

- `.connectors.sqlite` in the library root: what each connector saved to sign in with, and
  its last runs. It is not library data and not in the journal. Losing it means entering a
  code for each site again and one run right away, nothing else. Treat it as a secret.
- `.index.sqlite`: which remote documents were handled (`connector_items`) and where the
  files waiting in the inbox came from (`connector_files`). `violet reindex` does not rebuild
  them; without them every document the site still lists is fetched again, and the copies are
  flagged as duplicates.

A remote document is fetched once per connector and remote id. One that failed is tried again
on every run; an empty one, or one over 100 MB, is skipped. Fetch again in the settings (or
`POST /api/connectors/items/:id/refetch`) forgets a record and runs the connector. Each filed
document keeps where it came from in its sidecar:
`sources[].connector = {name, kind, remote_id, title?, date?}`.

## Adding a kind

A kind is a module in `crates/violet/src/connectors/` that implements `Connector` and is
listed in `kinds::builtin()`:

```rust
pub fn builtin() -> Kinds {
	Kinds::new().with("moneyforward", moneyforward::connector)
}
```

Its function takes the `ConnectorConfig` and returns the connector, or an error for options it
does not know (parse them with `deny_unknown_fields`); it is called once at startup to check
the config and then for every sign-in. One value serves one sign-in and the run, or the code
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

Errors are shown to admins and logged: put no credential, cookie or token in them. The
framework does the rest: schedule, retries, which documents are new, the inbox, provenance,
hints, the API and the settings page.

`connectors::session` has what a kind needs to talk to a site over `reqwest`: `Session` sends
`Request`s with a browser-like cookie `Jar` and follows redirects itself (a POST answered 301,
302 or 303 goes on as a GET, 307 and 308 repeat it), so cookies set on the way are kept. Its
errors name the host and path only. A `Jar` is serialisable; `jar.only(&["device"])` picks
the cookies worth returning from `state()`, and `Session::with_jar` starts from saved ones.
The jar knows domains, paths, `Secure`, `Expires` and `Max-Age`; it has no public-suffix list
and ignores `SameSite`, which is enough for a session that only talks to the sites of one kind.
