# Security model

The assistant prepares drafts and remembers who you are waiting on. Sending stays a separate action in Tagos, outside this library.

## What a tool can do

`prepare_reply_draft` writes one draft and returns `sent: false`. The note is: "Saved as a draft in Tagos. Nothing was sent." `delete_draft` removes a draft. It does not remove received or sent mail. `forget_followups` deletes assistant memory for one account and only when `confirm` is true.

## Untrusted mail

Subjects and bodies are data. Speech uses a sanitized subject, capped at 60 characters, with control characters removed. A body cannot add a recipient. `search_messages` and `get_thread` wrap body text between untrusted markers.

## Guards

- A recipient the mailbox does not already know is rejected unless `confirm_new_recipient` is true.
- Automated local-parts (`noreply`, `notifications`, `mailer-daemon`, and the others in `guards.rs`) are not suggested as people to wait on.
- A verbatim quote longer than 500 characters is rejected.
- A draft body longer than 4000 characters is rejected.
- `due` in the past is rejected. A date-only due is 18:00 in the requested timezone.

## Memory

Stored fields are the counterpart address, an optional thread id, a short "about" string from the user, timestamps, status, and an optional draft id. Message bodies are not stored. `done`, `cancelled`, and `replied` rows older than 30 days are removed by the ticker. The demo keeps SHA-256 of OAuth codes and tokens, not the raw secrets.

## Demo login

Off by default, so the quickstart needs no account. `--auth-profile mcp` sends `WWW-Authenticate` on 401. `--auth-profile alexa` does not. `--public` without a profile is refused. `--demo-static-token` is refused unless the bind address is loopback. A bearer in the query string is 401. A present `Origin` must be the Inspector origin on port 6274 or the demo origin on port 8787.

The demo process does not call `oauth.tagos.one` or `mcp.tagos.one`.
