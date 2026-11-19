# Multi-user support

Household scale: a few people share one library. Some documents are private to one person.

## Goals

- Every request runs as a known user, and every journal entry names that user.
- People sign in with a name plus a password or a passkey. Scripts and the phone use tokens.
- An area can be private to some users: its documents stay out of their search results, facets,
  activity and file endpoints, and out of the counts.
- The library stays the source of truth: users, credentials and area visibility live in files,
  never only in a database.
- Turning accounts on never locks out today's single user.

## Non-goals

- Per-document ACLs, groups, or object ownership.
- Several tenants in one server. Run a second instance on another library instead.
- Signup, invitations, email password reset. An admin sets or resets passwords; the CLI is the
  last resort.
- Client-certificate identity. The tegata mTLS gateway stays in front of violet and decides
  which devices may connect at all. It no longer says who the person is.

## Layers

1. **Device**: the gateway requires a client certificate on `violet.dorsk.dev`. Unchanged.
2. **Person**: violet's own login (this document).

## Login modes

`VIOLET_LOGIN` turns the feature on in steps, so that access never gets weaker than today while
it is rolled out:

| mode | request without a session or token |
|---|---|
| `off` (default) | runs as today (implicit admin without a users file, else 401). Login endpoints answer 404. |
| `optional` | runs as the implicit admin, as today. Login, passkeys and setup work. Used to verify login live before relying on it. |
| `required` | 401, once some account has a password or a passkey. Before that, the implicit admin, and setup is offered. |

The implicit admin records `actor: "user"` as today. Private areas do not hide anything from it,
so `optional` must not stay on once someone has private documents.

`VIOLET_PUBLIC_URL` (e.g. `https://violet.dorsk.dev`) is required when the mode is not `off`.
It gives the WebAuthn relying party: its host is the RP id, and its origin is the only origin
accepted for passkey ceremonies.

## Bootstrap

- While no account has a credential, `GET /api/auth` reports `setup: true` and the UI shows
  "Create the admin account" (name, password) to whoever reaches it. Anyone who reaches violet
  today is already the implicit admin, so this gives nobody new access.
- `POST /api/auth/setup` creates that admin with the password, as a journaled settings change,
  and signs them in. It is refused once any account has a credential.
- Existing documents and journal entries are untouched. Old entries keep `actor: "user"`; the
  Activity page shows them as before. The first admin sees every area that is not private, i.e.
  everything today.
- Recovery when nobody can sign in: `kubectl exec deploy/violet -- violet user password <name>`
  (reads the new password on stdin), or `VIOLET_LOGIN=optional` for a restart.

## Users file

`.users.json` at the library root, written only through the journal (`settings` action), parsed
strictly and reloaded when it changes, as before. The `identities` field is gone; a file that
still has it is refused.

```json
{ "users": [
  { "name": "alex", "role": "admin",
    "password": "$argon2id$v=19$m=19456,t=2,p=1$…",
    "passkeys": [{ "name": "phone", "created": "2026-11-19T10:00:00Z", "credential": { … } }],
    "tokens": [{ "name": "scanner", "sha256": "…", "scope": "write", "expires": null }] },
  { "name": "sam", "role": "member", "password": "$argon2id$…" }
] }
```

- `password`: an argon2id PHC string (OWASP parameters: m=19 MiB, t=2, p=1). A hash with other
  parameters still verifies; a new password always uses the current ones.
- `passkeys[].credential`: the WebAuthn credential as webauthn-rs serialises it (id, COSE public
  key, flags). The signature counter is not written back on login, so a login never changes the
  file. Synced passkeys report 0 anyway, and the cloned-key check is lost only for hardware keys.
- Undo restores the previous file, credentials included. The old hashes stay in `.originals/`
  like every replaced settings file. They are argon2id hashes, never passwords.

## Passwords

- 12 to 1024 bytes, no composition rules, not equal to the user name.
- At most two hashes run at a time (a semaphore), on the blocking pool.
- Changing one's own password needs the current one when there is one. An admin can set any
  user's password without it (reset).

## Passkeys

- Name first, then passkey: `POST /api/auth/passkey/start {name}` returns the WebAuthn request
  options for that user's passkeys; `POST /api/auth/passkey/finish` checks the assertion and signs
  in. An unknown name and a name without passkeys answer the same error.
- Registration needs a session: `POST /api/account/passkeys/start {name}` then `…/finish`.
  User verification is required, attestation is `none`.
- Ceremony state lives in memory for 5 minutes, keyed by a random id the client sends back. One
  replica, so nothing has to be shared.
- The browser side uses `PublicKeyCredential.parseCreationOptionsFromJSON` /
  `parseRequestOptionsFromJSON` and `toJSON`, with a small base64url fallback.
- A passkey can be renamed or removed. The last credential of an account (no password, one
  passkey) cannot be removed.

## Sessions

- A random 256-bit token from the OS CSPRNG (`getrandom`) in a cookie
  `__Host-violet_session=…; Path=/; HttpOnly; Secure; SameSite=Lax`. Only its SHA-256 is
  stored, and the token never appears in logs.
- Store: `.sessions.sqlite` at the library root, its own connection, so a request never waits on
  the index lock. Losing the file signs everyone out and nothing else; it is not library data and
  is not journaled. Columns: token hash, user, credential, created, last seen, expires, label
  (browser and OS from the User-Agent).
- 30 days idle, 180 days at most. `last_seen` is written at most once an hour.
- A session names the credential it was opened with: `password:<hash of the PHC string>` or
  `passkey:<credential id>`. It is valid only while that credential is still in the users file
  unchanged. So a password change, a reset, a removed passkey, a removed user, or an undo that
  brings back an old password ends the sessions that relied on it, with no extra bookkeeping.
  The request that changes one's own password gets a new session in its response.
- Logout deletes the session and clears the cookie. `GET /api/account/sessions` lists one's
  sessions and `DELETE /api/account/sessions/{id}` ends one; `id` is a short prefix of the hash,
  never the token.
- A new token on every login (no fixation).

## CSRF

- Each session has its own CSRF token (256 random bits, stored with the session). `GET /api/auth`,
  `GET /api/me` and every login answer return it as `csrf`.
- A request authenticated by the cookie that changes something must send it in `X-CSRF-Token`,
  compared in constant time, and must also carry an `Origin` that matches the host (or
  `Sec-Fetch-Site: same-origin`). A missing `Origin` is refused for cookie requests; the existing
  check still lets curl and the CLI through when they use a token.
- Login, setup and passkey login have no session yet: they need the matching `Origin`.
- `SameSite=Lax` keeps the cookie out of cross-site subrequests and POSTs. Top-level links into
  violet from elsewhere still arrive signed in.
- Bearer tokens are not ambient, so they only get the existing check.

## Rate limiting

In memory, for password logins and setup, per account name, per client address and overall:

- 5 failures for a name in 15 minutes: that name is refused for 15 minutes (429 with
  `Retry-After`), whether the next password is right or not.
- 20 failures from one client address in 15 minutes: that address is refused for 15 minutes. The
  address is the last `X-Forwarded-For` hop when the peer is in `VIOLET_TRUSTED_PROXIES` (the
  gateway's pod network), else the peer itself.
- 100 failures overall in 15 minutes: every password login waits 15 minutes.
- The answer is the same for an unknown name and a wrong password ("wrong name or password"),
  and an unknown name runs a dummy hash so both take as long.
- A restart clears the counters. The gateway's client certificate already limits who can try.

## Secrets

- Passwords, session tokens, CSRF tokens, API tokens and ceremony ids are never logged, and
  request bodies of auth endpoints are not traced.
- Comparisons of secrets are constant time; stored ones are compared by hash.

## Identity

Resolved per request, in this order:

1. **Session cookie** (when the mode is not `off`).
2. **Bearer token** `Authorization: Bearer vt_<random>`, as before (SHA-256 stored, name, expiry,
   scope `read` | `write` | `admin`).
3. **None**: see the login modes.

`VIOLET_IDENTITY_HEADER` and `VIOLET_DEFAULT_USER` are removed; `VIOLET_TRUSTED_PROXIES` now only
says whose `X-Forwarded-For` to believe for rate limiting. The CLI
runs as `VIOLET_USER` or the OS user, sees everything and is an admin: it already has the files.

## Roles

| | member | admin |
|---|---|---|
| search, view, download visible documents | yes | yes |
| upload, edit, split, merge, rotate visible documents | yes | yes |
| undo own entries | yes | yes |
| undo another user's entry | no | yes |
| own password, passkeys, sessions | yes | yes |
| users, passwords of others, tokens, area visibility, mail accounts | no | yes |

A change of the users file is refused when the sender would not be an admin afterwards (the
last admin cannot demote or remove themselves).

## Private areas

`.areas.json` at the library root, journaled:

```json
{ "private": [{ "area": "Health/Sam", "members": ["sam"] }] }
```

- An area that is not listed is visible to everyone.
- The index stores `area` per document. Every query (search, facets, activity, documents/{id},
  file, pages) adds the viewer's filter, built in one function in `index.rs` from
  `violet_core::Viewer`. No handler builds its own SQL.
- A document the user cannot see answers 404, not 403.
- An admin who is not a member of a private area does not see it, but can add themselves through
  `/api/areas`. Area names match without case.
- The model files into a private area only if the uploader is a member. Mail-ingested documents
  carry the rule owner as uploader. Files dropped straight into the inbox have no uploader and
  are never filed into a private area.
- Merging documents from areas with different visibility is refused.

## Journal

`Entry.actor` is `user:<name>`, `user` (the implicit admin, and every entry from before
accounts), or `agent` with `on_behalf_of`. Logins, logouts and sessions are not journaled; they
do not change the library. Password and passkey changes are `settings` entries whose summary
names the user and never a secret.

## API

`docs/api.md` is the reference. New:

| endpoint | who | |
|---|---|---|
| `GET /api/auth` | anyone | `{mode, setup, user}`: what the login page needs |
| `POST /api/auth/setup` | implicit admin, no credential yet | `{name, password}` → admin, signed in |
| `POST /api/auth/login` | anyone | `{name, password}` → cookie |
| `POST /api/auth/passkey/start` | anyone | `{name}` → `{ceremony, options}` |
| `POST /api/auth/passkey/finish` | anyone | `{ceremony, credential}` → cookie |
| `POST /api/auth/logout` | session | ends it |
| `PUT /api/account/password` | session | `{current?, password}` |
| `GET /api/account/passkeys` | session | name, created |
| `POST /api/account/passkeys/start` | session | `{name}` → `{ceremony, options}` |
| `POST /api/account/passkeys/finish` | session | `{ceremony, credential}` |
| `PATCH/DELETE /api/account/passkeys/{name}` | session | rename, remove |
| `GET /api/account/sessions`, `DELETE …/{id}` | session | list, end |
| `PUT /api/users/{name}/password` | admin | `{password}` reset |

`POST /api/users` takes an optional `password`. `GET /api/me` and the user list report
`password: bool` and the passkey names instead of identities. `via` is `implicit`, `session`,
`token` or `local`.

## UI

- `/login`: name, password, "Sign in", "Use a passkey". Shown on any 401 from the API; returns
  to the page the user came from. In setup mode it is "Create the admin account".
- Header: the user's name, linking to `/account`, and "Sign out".
- `/account`: change password, passkeys (add, rename, remove), sessions (end one).
- Settings (admin): users with role, password set or not, passkey count; add a user with an
  initial password; reset a password; remove. The identities field is gone.

## Rollout

1. Release with `VIOLET_LOGIN` defaulting to `off`: no behaviour change.
2. Deploy with `VIOLET_LOGIN=optional` and `VIOLET_PUBLIC_URL`. Verify with a temporary test
   account (password login, passkey through a virtual authenticator on a local instance), then
   remove it.
3. The human creates the admin account, signs in with the password, registers a passkey and
   signs in with it, while unauthenticated access still works.
4. `VIOLET_LOGIN=required`. Then the default becomes `required` in a later release.

## Earlier design

Steps 1, 2 and 4 of the first rollout (viewer plumbing, users file with tokens, private areas)
are built and stay. The certificate step (gateway XFCC → user) is dropped: no users file was ever
written live, so nothing needs migrating.
