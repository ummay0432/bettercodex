# Legacy Pi Codex request identity (“cloaking”)

## Status in the new project

This was a narrowly scoped compatibility layer for running the ChatGPT Codex
provider through Pi. It is preserved because the investigation and boundary
conditions were expensive to establish.

**Do not port this workaround into the Codex fork.** Official Codex already
emits its own first-party request identity through the transport that owns the
ChatGPT login. Reimplementing the Pi wrapper would add risk without adding
behavior.

## Accepted historical behavior

Every BetterCodex request to the authenticated ChatGPT Codex backend carried:

```http
Originator: codex_cli_rs
```

That matched Codex CLI 0.146.0. The Pi adaptation changed only `Originator`. It
did not replace or alter:

- the ChatGPT OAuth credential;
- the ChatGPT account header;
- model or reasoning effort;
- service tier;
- request payload;
- cache or session identity;
- retry policy; or
- WebSocket/SSE transport selection.

The selected model remained `openai-codex/gpt-5.6-sol` at `max` reasoning.
Transport retries resent that same request; they never substituted another
model. Fast mode was independent and added the official
`service_tier: "priority"` field only when explicitly enabled.

No token, account ID, or credential value is included in this document.

## Why it existed

Pi 0.83.0 and Codex CLI used the same user-authorized ChatGPT OAuth account but
identified their clients differently:

- Codex CLI sent `Originator: codex_cli_rs`.
- Pi assembled `Originator: pi` inside its provider after extension-supplied
  headers had been merged.

The historical investigation observed repeated semantic overload responses on
the Pi path while the Codex path did not show the corresponding failure.
Controlled external evidence changed only `Originator` and reproduced or
removed the response; changing only `User-Agent` did not. This supported a
request-routing/admission difference, but OpenAI did not document that backend
behavior as a stable priority guarantee.

“Cloaking” was the ecosystem term used for matching the official client
identity. The implementation was not a second authentication mechanism and did
not manufacture a subscription. It reused Pi's existing ChatGPT login and
changed the one client-identity header at the final transport boundary.

## Exact scope

The wrapper activated only when both model identifiers matched:

```text
provider = openai-codex
api      = openai-codex-responses
```

Transport rewriting was additionally limited to URL paths matching:

```text
/codex/responses
```

### HTTP/SSE

A per-request `fetch` wrapper:

1. inspected the final request URL;
2. delegated unrelated requests unchanged;
3. copied the existing headers for a Codex Responses request;
4. set only `originator` to `codex_cli_rs`; and
5. called the original fetch implementation with every other option intact.

### WebSocket

Pi assembled WebSocket headers after its public header hook, so the extension
could not override the final value through a supported header callback. The
workaround leased a reference-counted proxy around `globalThis.WebSocket` only
for an active Codex stream.

The proxy rewrote a handshake only when:

- the URL was the exact Codex Responses path; and
- the assembled headers still contained `originator: pi`.

Unrelated URLs, providers, existing non-Pi identities, and constructor argument
lists passed through unchanged. Concurrent Codex streams shared the lease.
Completion or failure released it, restored the exact prior constructor, and
did not overwrite a later third-party replacement.

Pi's Bun path cached a generated WebSocket constructor when no provider `env`
object existed. The wrapper supplied an owned copy of the same environment
values so each request resolved the active request-scoped proxy instead of a
stale cached constructor. Explicit SSE fallback never acquired a WebSocket
lease.

### Other authenticated Codex calls

The same header helper was applied to BetterCodex-owned requests for the
ChatGPT Codex model catalog, provider-native compaction, and first-party Codex
web search. Those paths continued to obtain credentials through Pi's model
registry.

Where an account header was required, the implementation decoded only the JWT
payload and read:

```text
https://api.openai.com/auth.chatgpt_account_id
```

It bounded token and claim lengths, required valid base64url/UTF-8/JSON, and
rejected empty or control-character-bearing account IDs. It did not treat local
decoding as authentication; the first-party server remained responsible for
validating the bearer token.

OAuth credentials were forwarded only to the exact expected first-party
ChatGPT origin. They were never logged, persisted in the repository, or sent
to an arbitrary URL.

## Source references

- Codex release: `rust-v0.146.0`
- Codex source revision:
  `e363b08c9175ac1cbe5893615dd2cb9ddf95043b`
- Codex default-client source:
  <https://github.com/openai/codex/blob/e363b08c9175ac1cbe5893615dd2cb9ddf95043b/codex-rs/login/src/auth/default_client.rs>
- Pi release: `v0.83.0`
- Pi source revision:
  `845d6ff1f6643aba440341cce877ce1c43ebbc39`
- Pi provider source:
  <https://github.com/earendil-works/pi/blob/845d6ff1f6643aba440341cce877ce1c43ebbc39/packages/ai/src/api/openai-codex-responses.ts>
- BetterCodex introduction commit: `61a342d`
- Request-scope hardening commit: `673b194`
- Final implementation blob:
  `a46048f968ed50cb0f2811e732ceb2f832900de5`

## Historical regression contract

The deleted tests checked:

- the exact pinned identity and source revision;
- case-insensitive replacement without mutation of auth, account, User-Agent,
  request body, model, effort, or turn state;
- URL- and prior-identity-scoped WebSocket rewriting;
- request-lifetime reference counting and exact constructor restoration;
- isolation from Pi's cached WebSocket constructor;
- SSE fallback without a WebSocket lease;
- real Pi WebSocket and SSE requests carrying `codex_cli_rs`;
- model-catalog and native-compaction requests carrying the same identity; and
- tracked-only installation from an empty working directory.

If a future upstream Codex regression appears, test Codex's native transport
directly. Do not restore this Pi-specific proxy as a generic workaround.
