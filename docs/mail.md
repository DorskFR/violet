# Mail

violet can take documents from IMAP folders. A mailbox is a connector of the kind `imap`
([docs/connectors.md](connectors.md)): anyone who may change documents adds as many as they
like in Settings, Connectors, each with its own schedule, and members see and manage only
their own. The connector gives the mailbox its name, its owner, the username and the password
it signs in with (kept in `.connectors.sqlite`, never shown again), its schedule (the form
starts from `every 10m`), and the area and tags of the documents it brings. It signs in at
each run and never needs Connect or a code. A mailbox that fails (wrong password, server down)
is retried with a growing delay like any connector and does not hold up the others.

Its options say where the mailbox is and what to take from it:

```json
{
  "name": "home",
  "kind": "imap",
  "username": "me@example.com",
  "password": "…",
  "schedule": "every 10m",
  "area": "Housing",
  "tags": ["utilities"],
  "options": {
    "host": "imap.example.com",
    "port": 993,
    "security": "tls",
    "rules": [
      {
        "folder": "INBOX/Bills",
        "from": "billing@",
        "subject": "invoice",
        "filename_include": ["*.pdf"],
        "filename_exclude": ["*terms*"],
        "max_age_days": 30,
        "scope": "attachments",
        "include_inline": false,
        "action": "keyword"
      }
    ]
  }
}
```

| Option | Default | |
|---|---|---|
| `host` | required | A name or an address, without a scheme |
| `security` | `tls` | `tls` (implicit TLS), `starttls`, or `plain` (loopback hosts only, e.g. a local mail bridge). Certificates are checked against the system roots |
| `port` | 993 for `tls`, 143 otherwise | |
| `rules` | one rule with the defaults below | At least one, applied in order |

Rules:

| Field | Default | |
|---|---|---|
| `folder` | `INBOX` | Subfolders with the server's separator, e.g. `INBOX/Bills` or `INBOX.Bills` |
| `from`, `to`, `subject` | none | Substrings the server matches in those headers |
| `filename_include`, `filename_exclude` | none | Case-insensitive globs (`*`, `?`) on attachment names; exclude wins |
| `max_age_days` | 30 | Only mails the server received within that many days |
| `scope` | `attachments` | `attachments`, `eml` (the whole mail as one `.eml` file) or `both` |
| `include_inline` | false | Also take inline parts; images under 20 KB (logos, signatures) are always left out |
| `action` | `keyword` | How a taken mail is marked on the server, which also keeps the next search from matching it: `keyword` (the keyword `violet-ingested`), `keyword:<name>`, `seen`, `flag` or `move:<folder>`. Mails are never deleted |

An option or a rule field violet does not know, or a value that does not add up, is a 400
that says which. The documents are filed for the connector's owner, who may get them in their
private areas (see `docs/design/multi-user.md`); an admin may add a mailbox for another user by
choosing its owner.

Only attachments violet can read are taken: PDF, PNG, JPEG, office files, `.txt`, `.md` and
`.eml`; calendar invites, signatures and the like are left out. A mail with nothing to take
is recorded as skipped and left unmarked on the server.

What happens to a matching mail:

1. Its body is fetched without marking it read (`BODY.PEEK[]`), mails over 100 MB are
   skipped.
2. Each file is written into `inbox/` (through a hidden temporary file and a rename) and
   the mail is recorded in the index: the name of its mailbox as `account`, folder,
   UIDVALIDITY, UID, Message-ID, sender, subject and arrival time. A mail already recorded
   under its UID, or under its Message-ID in any folder of any mailbox of the same owner, is
   not taken again, so a server that renumbers a folder (new UIDVALIDITY) does not bring old
   mails back. A mail in the mailboxes of two users is taken for each of them.
3. The action runs on the server. Move uses `MOVE` when the server has it; otherwise the
   mail is copied, flagged `\Deleted` and, only with `UIDPLUS`, expunged by its UID alone.
   A plain `EXPUNGE` is never sent.
4. The pipeline files the documents as any other inbox file and records the mail in the
   sidecar's `sources[].mail` (`account`, `folder`, `uidvalidity`, `uid`, `message_id`,
   `from`, `subject`, `date`), with the connector's area and tags.

A run reports the new mails it looked at as `listed` and the files it put in the inbox as
`ingested`; the connector's `counts` are its mails by how they ended. The mails themselves are
listed by `GET /api/mail` and on the status page, not among the connector's `items`: an admin
sees those of every mailbox, anyone else those of their own.

Undoing the ingest of a mail's document does not fetch the mail again. The record of handled
mails is kept in `.index.sqlite` and is not rebuilt by `violet reindex` (like the job
queue); deleting the index makes the next run take recent mails again, which the pipeline
then sees as duplicates. `POST /api/mail/processed/:id/reprocess` forgets one record and runs
its mailbox, which takes the mail again if the folder search still matches it: that is the
case for skipped and failed mails (they are never marked) but not for a mail the action
already marked.

Removing a mailbox keeps its records for its owner, as for any connector: adding it again
under its name takes nothing twice, and when another user takes the name the records are
forgotten first.

## Moving from the config file

`VIOLET_MAIL_CONFIG`, the JSON file it named and the `password_env` variables are gone: the
server logs a warning when the variable is still set and ignores it. Add each account of that
file as an `imap` connector under the same name, with its host, port and security, its rules
(without `account` and `hints`), `every <interval_secs / 60>m` as its schedule, the rule's
`hints.area` and `hints.tags` as its area and tags, and `hints.owner` as its owner. Because
the name is the same, the mails that account already handled stay known and none is fetched
or listed again; the marks on the server hold them back as well. Such a name, which holds
records but never was a connector, can only be taken by an admin (409 for anyone else), who
chooses the owner. `POST /api/mail/:account/sync` is gone too: use
`POST /api/connectors/:name/run`.
