# Alexa+ profile of the demo login

Checked against the Alexa+ MCP Toolkit pages on 2026-10-04:

- [Account Linking for MCP Add-ons](https://developer.amazon.com/docs/alexaplus/add-ons/mcp-toolkit-account-linking.html)
- [Alexa+ MCP Authentication](https://developer.amazon.com/docs/alexaplus/add-ons/mcp-toolkit-authentication.html)
- [Alexa+ MCP QuickStart](https://developer.amazon.com/docs/alexaplus/add-ons/mcp-toolkit-quickstart.html)

The published account-linking page does not list Alexa's redirect URIs. It says the list is shown in the Alexa+ Developer Hub and by `alexa-ai configure-account-linking`. `demo/clients.json` keeps placeholders for that list. Inspector 2.9.0's CLI callback is `http://127.0.0.1:6276/oauth/callback`. The web UI origin is port 6274.

This demo implements the user-level authorization-code grant. The authentication page also describes a separate client-credentials tier. This server does not issue client-credentials tokens.

`--auth-profile mcp` answers 401 with `WWW-Authenticate`, matching MCP and the live TagosMail server. `--auth-profile alexa` answers 401 with the same JSON body and no `WWW-Authenticate` header, matching the toolkit page "What isn't supported".

| Requirement | Demo | Test |
| --- | --- | --- |
| PRM with `mail.read` and `mail.draft` | `/.well-known/oauth-protected-resource` and the path-aware `/mcp` document | `auth::tests::mcp_profile_unauthorized_includes_www_authenticate` |
| AS metadata includes `S256` | `/.well-known/oauth-authorization-server/demo-oauth` | `auth::tests::authorization_server_metadata_advertises_s256` |
| `authorization_response_iss_parameter_supported` | `true`; the consent redirect adds `iss` | `auth::tests::authorization_server_metadata_advertises_s256` |
| Static clients, no DCR | `demo/clients.json` | `auth::tests::authorize_without_resource_is_rejected` |
| `resource` required and equal to the base URL | missing or different resource is `invalid_request` | `auth::tests::authorize_without_resource_is_rejected` |
| PKCE S256 | wrong verifier is `invalid_grant` and burns the code | `auth::tests::wrong_verifier_is_rejected` |
| Authorization code is one-time, TTL 60s | second use and a clock advanced by 60s both fail | `auth::tests::reused_authorization_code_is_rejected`, `auth::tests::authorization_code_expires_after_sixty_seconds` |
| Refresh token on every token response, rotated | code exchange and refresh both return a new refresh token | `auth::tests::refresh_token_is_in_every_token_response` |
| Access token TTL 1 hour | `expires_in` is `ACCESS_TTL_SECS` (3600) | `auth::tests::refresh_token_is_in_every_token_response` |
| Only SHA-256 of codes and tokens is stored | the raw secret is not a map key | `auth::tests::stored_credentials_are_sha256_not_the_raw_secret` |
| Bearer only in the header | `?access_token=` is 401 even with a valid header | `auth::tests::bearer_in_query_is_unauthorized` |
| Audience is the base URL | a token minted for another audience is 401 | `auth::tests::token_for_another_audience_is_unauthorized` |
| 401 with `WWW-Authenticate` | `--auth-profile mcp` | `auth::tests::mcp_profile_unauthorized_includes_www_authenticate` |
| 401 without that header | `--auth-profile alexa` | `auth::tests::alexa_profile_unauthorized_omits_www_authenticate` |
| `mail.read` cannot prepare a draft | 403 `insufficient_scope` | `auth::tests::read_token_cannot_prepare_a_draft` |
| Origin allowlist | Inspector `localhost:6274` and the demo origin `127.0.0.1:8787`; anything else is 403 | `auth::tests::foreign_origin_is_forbidden` |
| `GET /mcp` is 405 | with a valid bearer | `auth::tests::authenticated_get_mcp_is_not_allowed` |
| `--public` needs demo login | refused otherwise | `auth::tests::public_bind_requires_demo_login` |
| `--demo-static-token` is loopback only | refused with `--public` | `auth::tests::static_token_refuses_a_public_bind` |
| No outbound HTTP client | demo sources and Cargo.toml do not name an HTTP client crate | `mcp::tests::demo_binary_does_not_depend_on_an_http_client` |
| Consent page | one button, "Link Tagos Mail (demo)" | `auth::tests::wrong_verifier_is_rejected` (the consent helper) |
| Inspector OAuth, both profiles | `scripts/oauth_inspector.sh` | manual script, exit 0 |
