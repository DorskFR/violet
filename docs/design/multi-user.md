# Multi-user support

Household scale: a few people share one library. Some documents are private to one person.

## Goals

- Every request runs as a known user, and every journal entry names that user.
- An area can be private to some users: its documents stay out of their search results, facets,
  activity and file endpoints, and out of the counts.
- No passwords to manage for browser use. Scripts and the phone use tokens.
- The library stays the source of truth: users and area visibility live in files, never only in
  the index.

## Non-goals

- Per-document ACLs, groups, or object ownership (paperless-ngx style). Each would have to live in
  sidecars and filter every index query.
- Several tenants in one server. Run a second instance on another library instead.
- Signup, invitations, password reset.

## Identity

Resolved per request, in this order:

1. **Client certificate.** The public host already requires mTLS. The gateway forwards the
   verified client certificate subject in a header (`VIOLET_IDENTITY_HEADER`, e.g. Envoy's
   `x-forwarded-client-cert` parsed for `Subject=` / `URI=`). violet trusts that header only when
   the peer address is in `VIOLET_TRUSTED_PROXIES`. The certificate's CN or email maps to a user.
2. **Bearer token.** `Authorization: Bearer vt_<random>`. Tokens are stored hashed (SHA-256),
   carry a name, an optional expiry and a scope (`read` | `write` | `admin`).
3. **None.** Without either, the request runs as `VIOLET_DEFAULT_USER` when set (a single-user
   setup keeps working with no config). If no default user is set, the request gets 401.

The CLI runs as `VIOLET_USER` or the OS user name. It is trusted, because it already has
filesystem access to the library.

## Users file

`library/.users.json`, which is not indexed. It is written through the journal like every
library change, so adding a user or revoking a token can be undone:

```json
{
  "users": [
    { "name": "alex", "role": "admin", "identities": ["CN=alex-phone", "alex@example.com"],
      "tokens": [{ "name": "scanner", "sha256": "…", "scope": "write", "expires": null }] },
    { "name": "sam", "role": "member", "identities": ["CN=sam-laptop"] }
  ]
}
```

Token secrets are shown once, at creation (`violet user token add alex scanner`). After that only
the hash is stored. A changed file is reloaded on the fly: users are added or revoked without a restart.

## Roles

| | member | admin |
|---|---|---|
| search, view, download visible documents | yes | yes |
| upload, edit, split, merge, rotate visible documents | yes | yes |
| undo own entries | yes | yes |
| undo another user's entry | no | yes |
| users, tokens, area visibility, mail accounts | no | yes |

## Private areas

A new file `library/.areas.json`, journaled, lists the private areas. Areas themselves keep
coming from the prompt and the filed documents:

```json
{ "private": [{ "area": "Health/Sam", "members": ["sam"] }] }
```

- An area that is not listed is visible to everyone.
- A document is visible when its area is visible. Its sidecar does not change.
- The index stores `area` per document. Every query (search, facets, activity, documents/{id},
  file, pages) adds `area IN (visible areas)`. The filter sits in one function in `index.rs` and
  takes a `Viewer`. No handler builds its own SQL.
- When a user cannot see a document, it answers 404, not 403. Its existence is not leaked.
- Moving a document into a private area is a normal journaled move.
- The model may file into a private area only if the uploader is a member of it. Mail-ingested
  documents carry the rule owner as uploader, so a user's mail can go straight into that user's
  private area.
- Merging documents from areas with different visibility is refused.

## Journal

`Entry.actor` already exists (`user` / `agent`). It becomes `user:<name>` or `agent` (an
automatic step on behalf of `user:<name>` records `agent` plus an `on_behalf_of` field). Old
entries read as `user:` with no name, which means the default user. The Activity page shows the
name.

## API changes

- `GET /api/me`: name, role, visible areas.
- Admin endpoints: `GET/POST/DELETE /api/users`, `/api/users/{name}/tokens`.
- Every existing endpoint filters by the viewer. Responses keep their shape.
- Mutating requests from a browser also pass the cross-site check (VIO-23).

## UI

- The current user's name shows in the header.
- A Settings page (admin only): users, tokens, private areas and their members.

## Rollout

1. Viewer plumbing with a default user only: no behaviour change, and every query goes through the
   filter. Tests show the filter is applied on every endpoint (a table test over the router).
2. Users file + tokens + `/api/me`.
3. Client-certificate identity. infra: check what header the gateway forwards for the client cert
   (Envoy `ClientTrafficPolicy` `clientCertificateRef` / XFCC forwarding) and set it on the violet
   route.
4. Private areas + UI.

## Open questions for the human

- Which client certificates map to which person, and does each person have their own cert today?
- Should members see each other's activity entries for shared areas? (Proposed: yes.)
