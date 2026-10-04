# Use your own mailbox

Depend on the `tagos-mail-assistant` crate. Do not depend on the demo binary.

Implement `MailBackend`:

- `owner_address`, `owner_name`
- `list_people`, `list_contexts`, `get_thread`, `search_messages`
- `create_draft`, `update_draft`, `delete_draft`, `list_drafts`

There is no send method. A backend that sends from one of these methods has left the contract. `list_people` and `list_contexts` should put unread first, then newer activity. `get_thread` should return the oldest messages first and honor `clamp_limit` (default 20, max 50). `search_messages` should treat whitespace-separated words as AND.

Implement `FollowupStore`, or use the in-memory store. The demo's SQLite store is a second implementation of the same trait. `store_contract_suite` is public behind the `test-support` feature; run it on your store.

Pass a `Clock`. Tests should use `FixedClock`. Production can use `SystemClock`.

Construct `Assistant::new(backend, store, clock)` and call `await_reply`, `inbox_briefing`, and `prepare_reply_draft`. The `account_key` argument isolates memory. The library does not check OAuth scopes; the host that exposes the tools does.

`last_mail_check_at` on `Prefs` is how the 60-second lazy reply check avoids reading the mailbox on every call.
