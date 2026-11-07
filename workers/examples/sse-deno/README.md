# sse-deno

Deno guest for the host-held SSE of EdgeR (EDG-55). Same behavior as
`../sse-go-wasm`, plus `GET /stream`, an ordinary `ReadableStream`.

- `GET /events`: `Authorization: Bearer <token>` against the vault secret
  `SSE_ACCESS_TOKEN`; 200 with `x-edger-sse-subscribe: room.1`, or 401.
- `POST /publish`: same authorization; the JSON body
  `{"events":[{"topic","data","event","id"}]}` becomes 1 to 8
  `x-edger-sse-publish` directives (test control channel).
- `GET /plain`: an ordinary answer, no SSE.
- `GET /cookies`: an ordinary answer with two `Set-Cookie` headers.
- Test-only controls: each `topics=` query parameter of `/events` becomes
  one `x-edger-sse-subscribe` header, and the `raw` list of the `/publish`
  body is appended verbatim as `x-edger-sse-publish` values.
- `GET /stream?n=5&gap=200`: `n` chunks spaced `gap` ms (a plain streamed
  response, the regression control of the SSE change).

The binary proof is `crates/edger-orchestrator/tests/sse_binary_e2e.rs`
(`#[ignore]`, WSL Linux, `EDGER_DENO_BIN` set; the same file also needs
`EDGER_GO_WASM_SSE_FIXTURE`, see `../sse-go-wasm`). Host-side behavior and
limits: `planning/edger/docs/sse.md`.
