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
5. Enable and configure Ingress, OpenTelemetry, HPA, or OIDC only when their
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
