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
**Networking**, **Auth**, **Resources**, **Observability** and **Scaling**.

While **Enable Ingress** is on, the Networking fields are:

- **Ingress Host** — hostname for the Ingress; wildcards such as
  `*.example.com` are allowed.
- **Ingress Class** — optional `ingressClassName`.
- **Ingress Path** — path prefix routed to EdgeR (default `/`).
- **Ingress Path Type** — `Prefix` (default), `ImplementationSpecific` or
  `Exact`; use `ImplementationSpecific` for Kong string-prefix paths such as
  `/apps/`.
- **Ingress Annotations** — YAML map of Ingress annotations (default `{}`).
- **Enable TLS** — attach TLS configuration to the Ingress (default off);
  when on, **TLS Secret Name** asks for the existing Kubernetes TLS Secret.

The Auth fields are:

- **Root Key Secret** — existing Secret holding the root key. Leave empty to
  create one from **Root Key**.
- **Root Key Secret Field** — Secret data key holding the root key value
  (default `root-key`).
- **Root Key** — required while **Root Key Secret** is empty. The chart stores
  it in the `<release-name>-root-key` Secret, retrievable later with:

```bash
kubectl -n <namespace> get secret <release-name>-root-key \
  -o jsonpath='{.data.root-key}' | base64 --decode
echo
```

### labdev example

Values taken from `values-labdev.yaml`:

| Field | Value |
| --- | --- |
| Enable Ingress | `true` |
| Ingress Class | `kong` |
| Ingress Host | `*.cloud4biz.com` |
| Ingress Path | `/apps/` |
| Ingress Path Type | `ImplementationSpecific` |
| Ingress Annotations | see below |
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

## Access and validation

Without an Ingress, forward the service from a machine with cluster access:

```bash
kubectl -n <namespace> port-forward service/<release-name> 3000:3000
curl --fail http://127.0.0.1:3000/healthz
open http://127.0.0.1:3000/cpanel/
```

The Deployment exposes `/livez` and `/ready` probes. The configured root key is
mounted from its Secret and is required for root control-plane access.

## Release notes

### 0.3.1

- Published as an OCI chart at `oci://ghcr.io/djalmajr/charts/edger`, with
  the release image digest recorded in `image.digest`.

### 0.2.0

- Initial EdgeR Helm chart release.
