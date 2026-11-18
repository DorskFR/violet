# Mail sync

violet can take documents from IMAP folders. It is off unless `VIOLET_MAIL_CONFIG` names a
JSON config file; a config that does not parse or names an unknown field stops the server at
startup. Each account is synced on its own every `interval_secs` and when asked through
`POST /api/mail/:account/sync`; an account that fails (wrong password, server down, missing
password variable) is retried with a growing delay and does not hold up the others.

```json
{
  "accounts": [
    {
      "name": "home",
      "host": "imap.example.com",
      "port": 993,
      "security": "tls",
      "username": "me@example.com",
      "password_env": "VIOLET_IMAP_HOME",
      "interval_secs": 600
    }
  ],
  "rules": [
    {
      "account": "home",
      "folder": "INBOX/Bills",
      "from": "billing@",
      "subject": "invoice",
      "filename_include": ["*.pdf"],
      "filename_exclude": ["*terms*"],
      "max_age_days": 30,
      "scope": "attachments",
      "include_inline": false,
      "action": "keyword",
      "hints": {"area": "Housing", "tags": ["utilities"]}
    }
  ]
}
```

Accounts:

| Field | Default | |
|---|---|---|
| `name` | required | Letters, digits, `-`, `_`, `.`; used in the API path |
| `host`, `username` | required | |
| `password_env` | required | Name of the environment variable that holds the password; the password itself is never in the file |
| `security` | `tls` | `tls` (implicit TLS), `starttls`, or `plain` (loopback hosts only, e.g. a local mail bridge). Certificates are checked against the system roots |
| `port` | 993 for `tls`, 143 otherwise | |
| `interval_secs` | 600 | At least 60 |

Rules, applied in order for their account:

| Field | Default | |
|---|---|---|
| `account` | required | |
| `folder` | `INBOX` | Subfolders with the server's separator, e.g. `INBOX/Bills` or `INBOX.Bills` |
| `from`, `to`, `subject` | none | Substrings the server matches in those headers |
| `filename_include`, `filename_exclude` | none | Case-insensitive globs (`*`, `?`) on attachment names; exclude wins |
| `max_age_days` | 30 | Only mails the server received within that many days |
| `scope` | `attachments` | `attachments`, `eml` (the whole mail as one `.eml` file) or `both` |
| `include_inline` | false | Also take inline parts; images under 20 KB (logos, signatures) are always left out |
| `action` | `keyword` | How a taken mail is marked on the server, which also keeps the next search from matching it: `keyword` (the keyword `violet-ingested`), `keyword:<name>`, `seen`, `flag` or `move:<folder>`. Mails are never deleted |
| `hints.area` | none | Files the documents under this area instead of the one the model picks |
| `hints.tags` | none | Tags added to the documents |

Only attachments violet can read are taken: PDF, PNG, JPEG, office files, `.txt`, `.md` and
`.eml`; calendar invites, signatures and the like are left out. A mail with nothing to take
is recorded as skipped and left unmarked on the server.

What happens to a matching mail:

1. Its body is fetched without marking it read (`BODY.PEEK[]`), mails over 100 MB are
   skipped.
2. Each file is written into `inbox/` (through a hidden temporary file and a rename) and
   the mail is recorded in the index: account, folder, UIDVALIDITY, UID, Message-ID, sender,
   subject and arrival time. A mail already recorded under its UID, or under its Message-ID in
   any folder, is not taken again, so a server that renumbers a folder (new UIDVALIDITY) does
   not bring old mails back.
3. The action runs on the server. Move uses `MOVE` when the server has it; otherwise the
   mail is copied, flagged `\Deleted` and, only with `UIDPLUS`, expunged by its UID alone.
   A plain `EXPUNGE` is never sent.
4. The pipeline files the documents as any other inbox file and records the mail in the
   sidecar's `sources[].mail` (`account`, `folder`, `uidvalidity`, `uid`, `message_id`,
   `from`, `subject`, `date`), plus the rule's hints.

Undoing the ingest of a mail's document does not fetch the mail again. The record of handled
mails is kept in `.index.sqlite` and is not rebuilt by `violet reindex` (like the job
queue); deleting the index makes the next sync take recent mails again, which the pipeline
then sees as duplicates. `POST /api/mail/processed/:id/reprocess` forgets one record: the next
sync takes the mail again if the folder search still matches it, which is the case for
skipped and failed mails (they are never marked) but not for a mail the action already marked.
