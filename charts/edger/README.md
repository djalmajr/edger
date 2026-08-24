# EdgeR Helm chart

## Install with Rancher

In Rancher, open **Apps > Charts > edger**, select the desired version, and
install it into the target namespace.

1. Create or select a namespace suitable for the runtime.
2. Choose a release name, such as `edger`.
3. The selected chart version uses its associated EdgeR image by default. Use
   **Edit YAML** only when the cluster requires a private registry or an
   explicit image override.

4. Configure persistent storage for user worker packages to meet the cluster
   policy. cPanel and WebIDE are versioned with the EdgeR image and are restored
   from it whenever the pod is replaced.
5. Enable and configure Ingress, OpenTelemetry, or OIDC only when their
   required infrastructure is available.
6. Install the release and wait for the Deployment and enabled PVCs to become
   ready.

Provide a root key in the **Auth** section. The chart stores it in the
`<release-name>-root-key` Secret. It can be retrieved later with:

```bash
kubectl -n <namespace> get secret <release-name>-root-key \
  -o jsonpath='{.data.root-key}' | base64 --decode
echo
```

For direct Helm installations, set `rootKey.existingSecret` through a values
file when the cluster already manages this credential in a Secret.

## Topology: single replica by design

Workers live on the pod filesystem and the manifest index is in-memory, so a
second replica would install/serve different state per pod. Until worker
distribution exists, the chart **enforces** this: any render with
`replicaCount` greater than 1 or `hpa.enabled=true` **fails on purpose**, and
enabling worker persistence switches the Deployment to `strategy: Recreate`
(a RollingUpdate would multi-attach the RWO PVC or race two indices on the
same node). Do not try to scale by replicas; scale vertically or wait for
worker distribution.

## labdev overlay

The `values-labdev.yaml` overlay pins that topology (1 replica, PVC, HPA off,
Recreate) and expects the root key in the pre-provisioned `edger-root-key`
Secret. The CI deploy job runs exactly:

```bash
helm upgrade --install edger charts/edger \
  --namespace hyper \
  -f charts/edger/values-labdev.yaml \
  --set-string image.repository=repositorio.cithyper.click/centralit/edger \
  --set-string image.digest=sha256:<digest-do-build> \
  --history-max 5
```

The image is addressed by **digest** (immutable reference produced by the
build job), never by mutable tag.

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

### 0.2.0

- Initial EdgeR Helm chart release.
