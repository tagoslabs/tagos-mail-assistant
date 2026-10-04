# Contributing

The license is MIT.

Run, from the repository root:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --workspace
```

A behavior change needs a named test. The test name is the claim. Fixtures use `example.com`, `example.org`, `example.net`, or `demo.tagos.example` only.

Do not add a send method to `MailBackend`. Do not add an HTTP client to the library crate. Do not commit `.env`, keys, or `data/`.
