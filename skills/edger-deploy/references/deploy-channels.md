# Deploy channels

All four channels land in the same admin API handler, so behavior (limits,
collisions, versions) is identical: cPanel is a UI over REST, and the two MCP
servers are JSON-RPC wrappers over the same REST routes.

Credentials: `Authorization: Bearer $EDGER_API_KEY` (or the `x-api-key`
header when no Bearer is present; if both are present, Bearer wins).
`$EDGER_API_KEY` can be the root key or a scoped `egk_` API key; the server
applies the key's permissions. Plain `curl` passes the CSRF check — it is
only enforced when the request looks browser-originated (`Origin` or
`Sec-Fetch-*` present).

## cPanel ("Deploy an app")

The cPanel is served at `$EDGER_URL/cpanel/` (for example
`https://<host>/apps/cpanel/` behind the `/apps` prefix). Open the
**Deploy an app** dialog:

1. Choose the zip (cap: 64 MiB). The dialog previews `name@version` and the
   entrypoint client-side before deploying.
2. Deploy: the UI POSTs the raw zip to `POST /api/admin/workers/install` with
   `content-type: application/zip` and `x-edger-package-name` set to the zip
   file name.
3. The worker is active without a runtime restart.

The deploy and the Refresh action need the `workers:install` permission.
The workers view also offers **Set as default** (promote a version back to
default, `workers:promote` — applied immediately) and **Delete version**
(`workers:delete`), the latter behind a confirmation dialog; the Files tab
lists, uploads and deletes files under `files:read` / `files:write` /
`files:delete` (file deletions behind a confirmation dialog too), and every
action is hidden unless the key holds the matching permission.

## REST

```bash
curl -sS -X POST "$EDGER_URL/api/admin/workers/install" \
  -H "Authorization: Bearer $EDGER_API_KEY" \
  -H "content-type: application/zip" \
  --data-binary @my-spa-1.0.0.zip
```

- **Body:** the raw zip (not JSON), up to 64 MiB.
- **Query:** `force=true` and/or `staged=true` (both default false).
- **Accepted headers:** `x-edger-package-name` (name hint when the manifest
  has no `name`), `x-edger-expected-revision` (the CAS revision for
  `force`).
- **Responses:** `201` for a new version, `200` for a replacement. The JSON
  body carries `name`, `version`, `url` (`/my-spa`), `kind`, `origin`,
  `revision` (save it for a later `force`) and `staged`.

## MCP HTTP (`$EDGER_URL/api/mcp`)

`POST`-only JSON-RPC. The install tool accepts **`zipBase64` only** —
`zipPath` is a local-transport argument and is rejected over HTTP. Body
limit: 96 MiB.

```bash
curl -sS -X POST "$EDGER_URL/api/mcp" \
  -H "Authorization: Bearer $EDGER_API_KEY" \
  -H "content-type: application/json" \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"edger.install_worker","arguments":{"zipBase64":"<base64 of the zip>","packageName":"my-spa","staged":true}}}'
```

Other arguments: `force` (boolean, default false), `expectedRevision`
(string; required with `force`). Tool failures come back as
`result.isError: true`, with `_meta.status` when a failure has an HTTP
status behind it.

## MCP stdio (`edger-mcp`)

A local binary that speaks newline-delimited JSON-RPC on stdin/stdout. Build
it from the repo with `cargo run -q -p edger-mcp --bin edger-mcp`.

Environment:

- `EDGER_URL` and `EDGER_ROOT_KEY` — both required together for the
  control-plane tools; `https` is required outside loopback. Despite the
  name, `EDGER_ROOT_KEY` accepts a scoped `egk_` API key: the value is sent
  as the Bearer credential and the server applies its permissions.
- `EDGER_MCP_WORKSPACE_ROOT` — optional; defaults to the current directory.
  `zipPath` (and any `workspaceRoot` argument) must stay inside it.

Example client registration (Claude Code; the shell expands the variables):

```bash
claude mcp add edger \
  -e "EDGER_URL=$EDGER_URL" \
  -e "EDGER_ROOT_KEY=$EDGER_API_KEY" \
  -e "EDGER_MCP_WORKSPACE_ROOT=$PWD" \
  -- cargo run -q -p edger-mcp --bin edger-mcp
```

`edger.install_worker` takes `zipPath` or `zipBase64` (exactly one), plus
`packageName`, `force`, `staged`, `expectedRevision` — the same semantics as
over HTTP. With `zipPath` the agent only passes the path to a local zip.

## API-key permissions

Catalog: `workers:read`, `workers:install`, `workers:delete`,
`workers:promote`, `workers:toggle`, `workers:invoke`, `files:read`,
`files:write`, `files:delete`, `observability:read`, `keys:manage`
(`*` is not storable; root passes everything).

| Operation | Permission |
|---|---|
| `GET /api/admin/workers` (list; read the `revision` field) | `workers:read` |
| `POST /api/admin/workers/install` | `workers:install` |
| `POST /api/admin/workers/{name}/promote` | `workers:promote` |
| `POST /api/admin/workers/{name}/enable`, `.../disable` | `workers:toggle` |
| `DELETE /api/admin/workers/{name}` | `workers:delete` |
| `GET /api/admin/workers/{name}/files`, `.../files/download` | `files:read` |
| `POST /api/admin/workers/{name}/files` (zip upload; any user version) | `files:write` |
| `POST /api/admin/workers/{name}/files/delete` (batch; per-item errors in the 200 body) | `files:delete` |

Keys also carry scopes: `namespaces` (default `["*"]`) and `workers` (exact
name or suffix glob such as `p-abc*`, default `["*"]`). A non-root key
outside its scope gets `403 FORBIDDEN` on install and `404 NOT_FOUND` on
list/delete/promote/enable/disable and the file routes. A deploy key is
created with
`POST /api/admin/keys` (needs `keys:manage`); the raw `egk_` key is returned
once.

## Versions, staged and promote

- **Public versions are immutable as a whole.** Reinstalling the same
  `name@version` answers `409 COLLISION`; `force` on a public version
  answers `409 DEPLOY_PUBLIC_VERSION_IMMUTABLE`. Update by installing a new
  version. File-level edits are the exception: `POST .../files` (upload,
  `files:write`) and `POST .../files/delete` (`files:delete`) mutate files
  of any user version, public included — the version stays the same
  `name@version`, its revision advances.
- **`force`** replaces an existing **internal** draft version only, and only
  with `x-edger-expected-revision` (REST) / `expectedRevision` (MCP)
  matching the installed revision — the compare-and-swap; a stale revision
  answers `409 DEPLOY_REVISION_STALE`.
- **Staged install.** `?staged=true` (REST) or `"staged": true` (MCP) on a
  `public` worker: the new version is installed and enabled, but `/<name>/`
  keeps serving the current default until you promote.
- **Promote.** `POST $EDGER_URL/api/admin/workers/<name>/promote?version=X`
  (MCP: `edger.promote_worker` with `name` and `version`). Only existing
  public versions are eligible.
- **What answers `/<name>/` (unversioned URL):** the promoted version, if it
  is still an enabled public non-staged version; otherwise the highest semver
  among enabled public non-staged versions (the literal `latest` wins only
  when no version parses as semver). Keep `version` a SemVer (or `latest`):
  the field is a free string, but when no version is promoted, resolving
  `/<name>/` parses every candidate version, and a non-SemVer candidate
  fails the route with `PARSE_ERROR` (HTTP 400) instead of picking another.
  Version-pinned URLs `/<name>@<version>/` serve that exact version, staged
  included.
- **Delete.** `DELETE /api/admin/workers/<name>?version=X` (one version) or
  `DELETE /api/admin/workers/<name>` (all versions of the name). MCP:
  `edger.delete_worker` with `version`, or with `allVersions: true` — exactly
  one of the two, never both. Deleting a bundled or overlay version is
  refused with `409 CORE_WORKER_IMMUTABLE`.

## Behind a prefix

When the runtime sits behind a prefix-stripping proxy, set `$EDGER_URL` to
the public base including the prefix — for example
`EDGER_URL=https://<host>/apps`. The proxy strips `/apps` and forwards
`X-Forwarded-Prefix: /apps`, so the control plane (`/api/admin/...`,
`/api/mcp`) and the published app (`/<name>/`) both work through that URL,
and the injected `<base href>` keeps the prefix (see
[spa-contract.md](spa-contract.md)).
