# Evidence: cPanel Browser validation 2026-06-30

## Runtime

```bash
ROOT_API_KEY=test-root PORT=19086 RUNTIME_WORKER_DIRS=workers \
  cargo run -p edger-orchestrator --bin edger
```

URL:

```text
http://127.0.0.1:19086/cpanel
```

## Browser checks

- Opened the cPanel worker through the in-app Browser.
- Entered root key `test-root` in the UI.
- Confirmed session text: `root · admin`.
- Confirmed overview values loaded from Admin APIs:
  - workers: `17`
  - modules/extensions: `4`
  - gateway requests: `0`
- Opened Workers, Modules, Gateway and Keys views.
- Created discard key `browser-check`; UI showed the raw key once.
- Revoked `browser-check`; the Keys table returned to the empty state.
- No console errors were observed during the checked flow.
- After the table escaping fix, reloaded `http://127.0.0.1:19086/cpanel`,
  authenticated again, and confirmed `root · admin`, workers `17`, modules `4`,
  requests `0` and hidden alert state.

Screenshot:

```text
planning/edger/status/evidence/cpanel-browser-2026-06-30.png
```

## Scope notes

- The cPanel is served from `workers/cpanel` as a Static SPA worker, not from `edger-core`.
- The UI consumes existing root-protected Admin APIs and does not access runtime files directly.
- The checked mutation was local-only and cleaned up during validation.
- No docker-compose dependency was required for this frontend validation.
- No remote deploy was performed.
