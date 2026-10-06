# EdgeR Helm chart

## Chart source

The chart is published to the GitHub Container Registry at
`oci://ghcr.io/djalmajr/charts/edger` on every `vX.Y.Z` tag, one chart version
per tag, including pre-release tags such as `X.Y.Z-rc.N`. It is a public
package: pulling it needs no credentials.

The published chart records the `image.digest` (sha256) of the image built for
the same release, so installing it runs exactly that image, and no digest is
ever passed by hand. A chart packaged directly from this repository has an
empty digest and falls back to the `appVersion` tag.

The image and the chart are two ghcr packages that share the same tag:
`ghcr.io/djalmajr/edger` (image) and `ghcr.io/djalmajr/charts/edger` (chart).

## Rancher (UI)

### Add the chart repository

Apps → Repositories → Create, with target **OCI Repository**:

- Name: `edger`
- URL: `oci://ghcr.io/djalmajr/charts/edger`
- No authentication (public package)

Use the full chart path, not just the `charts` namespace: ghcr does not
expose the catalog listing Rancher would use to discover the charts inside a
namespace.

Declarative equivalent:

```yaml
apiVersion: catalog.cattle.io/v1
kind: ClusterRepo
metadata:
  name: edger
spec:
  url: oci://ghcr.io/djalmajr/charts/edger
  refreshInterval: 3600
```

Pre-release versions only appear with the "show pre-release versions" user
preference enabled in the Rancher preferences page.

### Install

Apps → Charts → `edger` → **Install**. Choose the namespace and the release
name (for example `edger`). The form is grouped into **Runtime**,
**Networking**, **Auth**, **Resources**, **Observability**, **Scaling** and
**Routing**.

While **Enable Ingress** is on, the Networking fields are:

- **Ingress Host** — hostname for the Ingress; wildcards such as
  `*.example.com` are allowed.
- **Ingress Class** — select with the IngressClasses available in the
  cluster; leave empty to use the cluster default.
- **Ingress Path** — path prefix routed to EdgeR (default `/`).
- **Ingress Path Type** — `Prefix` (default), `ImplementationSpecific` or
  `Exact`; use `ImplementationSpecific` for Kong string-prefix paths such as
  `/apps/`.
- **Ingress Annotations** — Ingress annotations as key/value pairs.
- **Enable TLS** — attach TLS configuration to the Ingress (default off);
  when on, **TLS Secret Name** asks for the existing Kubernetes TLS Secret.

The Auth fields are:

- **Root Key Secret** — existing Secret holding the root key. Leave empty to
  create one from **Root Key**.
- **Root Key Secret Field** — Secret data key holding the root key value
  (default `root-key`).
- **Root Key** — required while **Root Key Secret** is empty. The chart stores
  it in the `<release-name>-root-key` Secret.
- **Initial Root Password Secret** — optional existing Secret used to seed the
  `root` console user when that account does not yet exist, including a store
  that already has operator accounts created through the root token.
  The chart mounts the selected field read-only and passes only its path as
  `EDGER_ROOT_PASSWORD_FILE`. The root key remains available for token login.
- **Initial Root Password Secret Field** — field inside that existing Secret
  (default `password`). Leaving the Secret name empty disables password
  bootstrap.

For a chart-created root key, retrieve it with:

```bash
kubectl -n <namespace> get secret <release-name>-root-key \
  -o jsonpath='{.data.root-key}' | base64 --decode
echo
```

The initial password is never a chart value or ConfigMap entry. After a root
user exists, changing the mounted Secret does not replace that user's stored
password; use the authenticated password-change flow. Keep the API key database
on persistent storage so users and sessions survive a restart.
The cPanel **Users** page is root-only: root can create operator accounts with
explicit permissions and scopes, disable them, reset passwords, and revoke
their sessions. The root key remains a separate token credential.

### labdev example

Values taken from `values-labdev.yaml`:

| Field | Value |
| --- | --- |
| Enable Ingress | `true` |
| Ingress Class | `kong` |
| Ingress Host | `*.cloud4biz.com` |
| Ingress Path | `/apps/` |
| Ingress Path Type | `ImplementationSpecific` |
| Ingress Annotations | 3 key/value pairs (see below) |
| Enable TLS | `false` |
| Root Key Secret | `edger-root-key` |
| Root Key Secret Field | `root-key` |
| Persist Workers | `true` |
| Workers Volume Size | `5Gi` |

```yaml
konghq.com/preserve-host: "true"
konghq.com/protocols: https
konghq.com/strip-path: "true"
```

The `edger-root-key` Secret (key `root-key`) is pre-provisioned in the
namespace before the first install; the Helm section below shows the command.

### Upgrade

Apps → Installed Apps → `edger` → **Upgrade**, then choose the target version.
The release's current values come pre-filled; anything not exposed by the form
goes through **Edit YAML**. A release installed with Helm on the terminal also
appears in Installed Apps and can be upgraded from the UI — and vice versa.

## Topology: single replica by design

Workers live on the pod filesystem and the manifest index is in-memory, so a
second replica would install/serve different state per pod. Until worker
distribution exists, the chart **enforces** this: any render with
`replicaCount` greater than 1 or `hpa.enabled=true` **fails on purpose**, and
enabling worker persistence switches the Deployment to `strategy: Recreate`
(a RollingUpdate would multi-attach the RWO PVC or race two indices on the
same node). Do not try to scale by replicas; scale vertically or wait for
worker distribution.

## Routing: tenant allowlists and A/B splits

Two independent opt-ins, both **off by default**; each flag enables only its
own half of the per-app routing policy, and with both off the render has no
Tenancit dependency (no env, no volume, no Secret reference).

- **Enable Tenant Routing** (`tenantRouting.enabled`) — gate a worker's
  access by tenant allowlists. With it on, EdgeR resolves the request
  hostname against the Tenancit Consumer API and only serves apps whose
  routing policy lists the identified tenant slug.
- **Enable Weighted Routing** (`weightedRouting.enabled`) — split versionless
  requests across weighted app versions (A/B) from the app's routing policy.
  It can be enabled alone.

While tenant routing is on, the render **fails on purpose** until all of this
is provided:

- **Tenancit Identify URL** — the exact HTTPS URL of the Consumer API
  endpoint in this cluster, for example
  `https://tenancit.<host>.<cluster>/v1/identify`. The path must be exactly
  `/v1/identify`, with no query, fragment or userinfo; plain HTTP is only
  accepted for loopback hosts.
- **Tenancit Token Secret** and **Tenancit Token Secret Field** — an
  existing Kubernetes Secret holding the `tenant:identify` API client token.
  The chart never creates that Secret and there is no form field for the
  token: it is mounted read-only at the fixed path
  `/var/run/secrets/edger-tenancit/token` and referenced only through
  `EDGER_TENANCIT_TOKEN_FILE`, so the token never appears in the ConfigMap,
  in release values or in logs. Pre-provision it in the namespace, reading
  the value from a file:

```bash
kubectl -n <namespace> create secret generic tenancit-identify-token \
  --from-file=token=./tenancit-token.txt
```

The ConfigMap always carries `EDGER_TENANT_ROUTING_ENABLED` and
`EDGER_WEIGHTED_ROUTING_ENABLED` (`"true"`/`"false"`) and only carries
`EDGER_TENANCIT_IDENTIFY_URL` while tenant routing is on.
EdgeR reads the mounted token at startup; restart the pod after rotating the
Secret so the client uses the new credential.

Limits:

- The identified domain is **domain context, not user authentication**: it
  only decides which tenants may reach the app. Each worker keeps its own
  authentication/authorization for its data.
- The routing policy of an app is process-local memory backed by the workers
  PVC; with no policy for an app the current public routing applies. Do not
  assume a `traffic`/allowlist policy is already published.
- The chart remains single-replica (see the topology section above), so
  nothing in this section claims production readiness.

## Compression

EdgeR compresses **app responses** (brotli + gzip) through the full pipeline.
Three independent switches in the **Runtime** group, all mapped to the
`EDGER_COMPRESSION*` envs of the ConfigMap:

- **Enable Response Compression** (`compression.enabled`) — mounts the
  compression layer. When disabled, **no layer is mounted at all**: no
  `content-encoding`, no `406` for an unsatisfiable `Accept-Encoding`, and
  no `Vary` added by the layer.
- **Compression Minimum Body Size** (`compression.minBytes`, default `1024`)
  — the minimum body size to compress **when the size is known**. Unknown-size
  (streaming) bodies are always candidates: the encoder flushes per chunk.
- **Compression Level** (`compression.level`, default `default`) —
  `default`, `fastest`, `best` or a precise integer. `best` is brotli quality
  11, the most expensive setting for dynamic/streaming bodies; prefer
  `default` (quality 4) unless a specific ratio target justifies it.

Behavior, always on with compression enabled:

- Only responses marked as app are compressed; the control plane
  (`/health`, `/ready`, `/metrics`, admin API, MCP) is never compressed.
- Responses that already carry a `content-encoding`, `Cache-Control:
  no-transform`, an already-compressed media type, or an `attachment`
  disposition pass through intact.
- An `Accept-Encoding` that accepts **neither `br`, `gzip` nor `identity`**
  answers `406 Not Acceptable` **only for app responses** (same body and
  headers the layer would pass through, plus `Vary: Accept-Encoding`);
  the control plane returns its normal response, uncompressed, and never
  answers 406.
- `/metrics` exposes the byte counters
  `edger_http_compression_bytes_in_total{encoding="br"|"gzip"}` (response
  bytes before compression) and
  `edger_http_compression_bytes_out_total{encoding="br"|"gzip"}` (compressed
  bytes delivered), recorded when the compressed body ends — streaming and
  abandoned bodies count whatever passed.

Invalid `EDGER_COMPRESSION*` values never fail the boot: the binary logs a
warning and falls back to the default of that variable (`on` / `1024` /
`default`).

Breach note (size channel): a compressed body leaks information through its
size. Responses that reflect user input together with a secret (CSRF token,
session data) can leak the secret by size — mitigate with `Cache-Control:
no-transform` on those routes, per-request masked tokens, or
`EDGER_COMPRESSION=off`. See
[planning doc](../../planning/edger/docs/compression.md#nota-breach-canal-de-tamanho).

## Helm (terminal)

### Get the overlay

The labdev overlay lives in the repository at `charts/edger/values-labdev.yaml`
and is also shipped inside the published chart package:

```bash
helm pull oci://ghcr.io/djalmajr/charts/edger --version X.Y.Z --untar
# the overlay lands at edger/values-labdev.yaml
```

### Pre-provision the root key

Before the first install, create the `edger-root-key` Secret in the namespace.
Read the key from a file — never pass it on the command line:

```bash
kubectl -n hyper create secret generic edger-root-key \
  --from-file=root-key=./edger-root-key.txt
```

### Install or upgrade

```bash
helm upgrade --install edger oci://ghcr.io/djalmajr/charts/edger \
  --version X.Y.Z \
  -n hyper \
  -f edger/values-labdev.yaml \
  --history-max 5
```

If you are at the repository root, use the versioned overlay with
`-f charts/edger/values-labdev.yaml`. The overlay pins the single-replica
topology and expects the `edger-root-key` Secret. The published chart already
carries the image digest in `image.digest`, so no digest is passed by hand —
and never declare
`image.digest: ""` in a values file of your own: the empty value overrides the
chart's digest and the image falls back to the mutable tag.

### Verify

```bash
helm history edger -n hyper
kubectl -n hyper rollout status deploy/edger
kubectl -n hyper get deploy edger \
  -o jsonpath='{.spec.template.spec.containers[0].image}'
# ghcr.io/djalmajr/edger@sha256:...
curl https://<host>/apps/health
```

The `@sha256:` reference in the printed image proves the pod runs the
immutable image of the release. Kong strips the `/apps/` prefix (the
`konghq.com/strip-path` annotation), so `/apps/health` reaches EdgeR's
`/health` route.

### Roll back

```bash
helm rollback edger <revision> -n hyper
```

## Upgrade notes

With worker persistence enabled, the Deployment uses the `Recreate` strategy
because the workers PVC is `ReadWriteOnce`: an upgrade terminates the pod and
starts the new one, so EdgeR is down for a few seconds. The installed workers
live on the PVC and survive the upgrade.

From this version on, with `userWorkers.persistence.enabled` and
`runtime.persistCoreWorkerOverlay` (default `true`), the core worker overlay
(`/app/core-worker-overlays`) also survives restarts and upgrades: it is
mounted from the workers PVC under `.edger/core-overlays`. Content from the
previous `emptyDir` is not migrated, because it was already lost on every
restart. On image upgrades, the highest enabled cPanel semver is active
unless a version was explicitly promoted; a promoted version remains the
default.

## Access and validation

Without an Ingress, forward the service from a machine with cluster access:

```bash
kubectl -n <namespace> port-forward service/<release-name> 3000:3000
curl --fail http://127.0.0.1:3000/healthz
open http://127.0.0.1:3000/cpanel/
```

The Deployment exposes `/livez` and `/ready` probes. The configured root key is
mounted from its Secret and is required for root control-plane access.

## Backup and restore

The EdgeR state (installed workers, the core worker overlay and the API key
store) is backed up online through the admin API:

```bash
kubectl -n <namespace> port-forward service/<release-name> 3000:3000
curl --fail -H "authorization: Bearer <root-key>" \
  -o edger-state-$(date +%Y%m%d).zip \
  http://127.0.0.1:3000/api/admin/state/export
```

The route requires the root key and answers `application/zip`
(`Content-Disposition: attachment; filename="edger-state-<timestamp>.zip"`).
The export is consistent: it waits up to 30 s for in-flight deploys to
settle (otherwise `503 STATE_BUSY`), and a mutation started while the export
runs answers `409 STATE_EXPORT_IN_PROGRESS`. The zip contains `user-roots/0/`
(each user worker root), `core-overlay/` (the overlay root), `api-keys.db`
(a consistent copy made with `VACUUM INTO`) and `edger-state.json` (format,
EdgeR version, creation date and the source paths). Transient deploy files,
the raw database file and its sidecars, the top-level `.edger/` of the user
roots and symlinks are not included.

The zip contains the key hashes and every installed worker: keep it as a
secret. Sending it to a bucket is still manual; a scheduled upload lands in
0.3.3.

### Restore

Restore is offline: stop EdgeR, replace the workers PVC content with the zip
content, start EdgeR again.

1. Scale the deployment down:

   ```bash
   kubectl -n <namespace> scale deploy/<release-name> --replicas=0
   ```

2. Run a helper pod that mounts the workers PVC
   (`<release-name>-user-workers`, or `existingClaim` when set) as root:

   ```yaml
   apiVersion: v1
   kind: Pod
   metadata:
     name: edger-restore
     namespace: <namespace>
   spec:
     containers:
       - name: restore
         image: busybox:1.37
         command: ["sh", "-c", "sleep infinity"]
         volumeMounts:
           - name: workers
             mountPath: /data
     restartPolicy: Never
     volumes:
       - name: workers
         persistentVolumeClaim:
           claimName: <release-name>-user-workers
   ```

3. Empty the PVC, extract the zip into the chart paths (`user-roots/0/` at
   the PVC root, `core-overlay/` at `.edger/core-overlays/`, `api-keys.db`
   at `.edger/api-keys.db`) and fix ownership — the EdgeR pod runs as UID
   `10001`, the helper extracts as root:

   ```bash
   kubectl -n <namespace> cp edger-state-<date>.zip edger-restore:/tmp/
   kubectl -n <namespace> exec edger-restore -- sh -c '
     rm -rf /data/..?* /data/.[!.]* /data/*
     mkdir -p /data/.edger/core-overlays /tmp/restore
     unzip -q /tmp/edger-state-<date>.zip -d /tmp/restore
     cp -a /tmp/restore/user-roots/0/. /data/
     cp -a /tmp/restore/core-overlay/. /data/.edger/core-overlays/
     if [ -f /tmp/restore/api-keys.db ]; then
       cp /tmp/restore/api-keys.db /data/.edger/api-keys.db
     fi
     chown -R 10001:10001 /data
   '
   ```

   The `api-keys.db` destination is the `apiKeysDb` path from the zip's
   `edger-state.json`, translated onto the PVC. With the chart default that
   path is `/app/workers/.edger/api-keys.db`, i.e. `.edger/api-keys.db` at
   the PVC root (the command above). If `apiKeys.dbPath` points outside
   `/app/workers`, copy the `api-keys.db` entry to that path translated onto
   the corresponding volume, and mount that volume at the matching path in
   the helper pod as well.

4. Remove the helper pod and scale back up:

   ```bash
   kubectl -n <namespace> delete pod edger-restore
   kubectl -n <namespace> scale deploy/<release-name> --replicas=1
   ```

## Release notes

### 0.3.1

- Published as an OCI chart at `oci://ghcr.io/djalmajr/charts/edger`, with
  the release image digest recorded in `image.digest`.

### 0.2.0

- Initial EdgeR Helm chart release.
