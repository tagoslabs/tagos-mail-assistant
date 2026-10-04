# Tagos Mail public tool contract

Read-only snapshot of the tools a host can call on the live Tagos Mail MCP
endpoint. This file is the contract the open assistant library and its demo
server must match. It does not describe how the service stores mail.

Endpoint: `POST https://mcp.tagos.one/mcp`  
Transport: Streamable HTTP. `GET /mcp` returns 405 (no SSE stream).  
Protocol versions accepted: `2026-07-28`, `2025-11-25`, `2025-06-18`, `2025-03-26`.

Checked against the service source on 2026-10-04. A call with no token
returns `401` and does not list tools, so the schemas below are the ones
the server advertises after a normal Tagos sign-in.

## Limits

- `limit` defaults to 20. `0` is treated as the default. Values above 50
  become 50.
- Message bodies are plain text, cut at 4000 characters, and wrapped as
  untrusted. There is no HTML body.

## Result shape

A successful tool returns JSON-RPC `result` with:

- `resultType`: `"complete"`
- `isError`: `false`
- `content`: one text block whose `text` is a JSON object

An error uses the same envelope with `isError: true`. The text is
`{"code","message"}`. Stable codes include `unauthenticated`,
`forbidden_scope`, `not_found`, `rate_limited`, `send_rejected`.
No stack traces.

Live tools declare `inputSchema` only. They do not declare `outputSchema`
and do not return `structuredContent`.

## Scopes

| Tools | Scope |
| --- | --- |
| `whoami` | any signed-in token |
| `search_messages`, `list_people`, `list_contexts`, `get_thread`, `get_message`, `list_folders` | `mail.read` |
| `create_draft`, `update_draft`, `get_draft`, `list_drafts`, `delete_draft` | `mail.draft` |
| `send_message`, `send_draft` | `mail.send` |

Hosts that only have `mail.read` and `mail.draft` do not see the send tools.
The assistant library does not call `send_message` or `send_draft`.

## Tools

### `whoami`

Input: `{}`  
Output text: account id and primary mailbox address.

### `search_messages`

Input:

```json
{
  "type": "object",
  "properties": {
    "query": { "type": "string" },
    "thread_id": { "type": "string" },
    "limit": { "type": "integer" }
  },
  "required": ["query"]
}
```

`query` is whitespace-separated words. Every word must match. Without
`thread_id`, a word may match subject, sender, recipient, or body, and
trash is excluded. With `thread_id`, a word matches the body only, and
trash is included. This is not a label or operator language (`from:`,
`is:unread`, and similar are not special).

Results are ranked by match score, not by date. An empty query is
`not_found`. A `thread_id` that is not a UUID is ignored (search runs
without that filter).

Output text:

```json
{
  "messages": [
    {
      "id": "uuid",
      "thread_id": "uuid",
      "subject": "string",
      "from": "string",
      "snippet": "untrusted wrapped text",
      "folder": "string",
      "received_at": "RFC3339 or empty",
      "untrusted": true
    }
  ],
  "untrusted": true
}
```

Snippets are cut to 500 characters before the untrusted wrap.

### `list_people`

Input: optional `q` (string), optional `limit` (integer).  
`q` is a case-insensitive substring of the address or the display name.

Order is not “most recently active first”. People with any unread mail
come first. Inside each group, later activity comes first. Ties break by
address. `limit` keeps the first rows of that order, after the `q` filter.

Output text: `{ "people": [ { "address", "name", "unread_count", "last_activity" } ] }`.  
`name` may be null. `last_activity` is an RFC3339 timestamp string.

### `list_contexts`

Input: required `entity_addr` (string), optional `limit`.

Order matches people: threads with unread mail first, then later
`last_message_at` first.

Output text:

```json
{
  "contexts": [
    {
      "thread_id": "uuid",
      "subject": "string",
      "last_message_at": "RFC3339",
      "unread_count": 0,
      "is_starred": false,
      "is_archived": false
    }
  ]
}
```

### `get_thread`

Input: required `thread_id` (UUID string), optional `limit`.  
There is no cursor argument.

Messages are oldest first (`received_at` ascending). `limit` keeps that
prefix. It does not keep the newest messages. A thread longer than the
cap (50) will not include its newest message in one call.

Output text: `{ "messages": [ ... ], "untrusted": true }`.  
Each message:

```json
{
  "id": "uuid",
  "thread_id": "uuid",
  "from": "Name <addr> or addr",
  "to": "Name <addr> or addr",
  "subject": "string",
  "body_text": "untrusted wrapped plain text",
  "received_at": "RFC3339",
  "is_read": false,
  "folder": "string",
  "untrusted": true
}
```

A missing or non-UUID `thread_id` is `not_found`.

### `get_message`

Input: required `message_id` (UUID).  
Output text: one message object, same fields as a `get_thread` message.
Unknown id is `not_found`.

### `list_folders`

Input: `{}`  
Output text: `{ "folders": [ { "name", "unread_count" } ] }`.

### `create_draft`

Input: required `to`, `subject`, `body_text`. Optional `thread_id`.  
Saves a draft. Does not send it.

### `update_draft`

Input: required `draft_id`. Optional `to`, `subject`, `body_text`.

### `get_draft`

Input: required `draft_id`.

### `list_drafts`

Input: optional `limit`.  
Output text: `{ "drafts": [ ... ] }`.

### `delete_draft`

Input: required `draft_id`.  
Discards that draft. Does not delete received or sent mail.

### `send_message` and `send_draft`

Present only when the token has `mail.send`.  
`send_message` requires `to`, `subject`, `body_text`, `idempotency_key`,
and `confirmed: true`. Optional `thread_id`.  
`send_draft` requires `draft_id`, `idempotency_key`, and `confirmed: true`.  
Both share a cap of 3 sends per account per UTC day.  
The assistant does not call either tool.

## Demo parity

The demo server should expose the read and draft names above with these
same public schemas, plus the assistant tools defined in a later contract
section. It must not expose `send_message` or `send_draft`.
