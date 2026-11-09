# INF-34 rate-limit integration

`test_cloudflare_rate_limit.py` is an explicit consumer test for this EdgeR
control-plane manifest. It starts the real, already-cached Traefik image in a
short-lived Docker container and loads a dynamic file-provider configuration
derived from the supplied manifest. It is not part of the normal unit-test
discovery run.

## Prerequisites and command

- Docker Desktop is running and the exact image is already cached:
  `traefik@sha256:321153f6f732f912cf5c6f0597fe38743fc7895d482d7d8804a479ae0403e151`.
- Python 3 is available; YAML parsing uses PyYAML when present or the Ruby
  standard-library YAML module. The harness does not install packages or pull
  images.
- The EdgeR manifest and checkout are available locally.

Run it explicitly from this EdgeR checkout:

```bash
python3 deploy/production/tests/test_cloudflare_rate_limit.py \
  --manifest deploy/production/control-plane.yaml
```

CI uses the built-in socket-peer fixture. For a local comparison with a
policy manifest from another checkout, pass it explicitly; only its
`sourceRange` is replaced by the observed test peer:

```bash
python3 deploy/production/tests/test_cloudflare_rate_limit.py \
  --manifest deploy/production/control-plane.yaml \
  --allowlist /path/to/cloudflare-middleware.yaml \
  --allowlist-sha256 <sha256-of-that-file>
```

An external allowlist must be exactly the `cloudflare-only` Middleware, with
namespace absent or `edger`, `ipAllowList.sourceRange`, and no `ipStrategy`.
The caller-supplied SHA-256 must match the file before Docker is contacted;
the harness does not treat a differently named policy as EdgeR policy.

The harness refuses a missing or mismatched image. Its only Docker config
mount is a private temporary directory, mounted read-only. It creates one
labelled temporary bridge network, identifies its gateway as the trusted test
peer, and publishes the container only on an ephemeral `127.0.0.1` port. It
uses `docker exec` with the pinned image's existing `wget` for the container-
local direct peer. No Docker socket, home directory, cluster, DNS, or public
endpoint is mounted or contacted. Every
container is named `infra-inf34-ratelimit-<uuid>` and labelled
`infra.inf34.run=<uuid>`; cleanup checks that label before removing it.

## Configuration and trust boundary

The script parses the supplied EdgeR YAML, preserving the legacy
`edger-admin-ratelimit` object and the route's middleware order. It uses an
explicit local socket-peer fixture by default, or an optional allowlist
manifest, and changes only its `sourceRange` for the isolated run. The
observed Docker bridge gateway, source-range count, and hash are printed, and
the temporary dynamic config uses the same `ipAllowList` shape without
`ipStrategy`. The fixture proves the peer boundary; an optional policy file is
the separate local combination with the real Cloudflare ranges.

The test route is a local `ping@internal` service with `manualRouting`; the
original EdgeR match, service, port, Certificate, and limits are not used as a
shortcut or silently replaced. `cloudflare-only` authenticates the socket peer
first. Only after that check does `edger-admin-ratelimit-cloudflare` use
`CF-Connecting-IP` as a quota key. A direct peer with forged
`CF-Connecting-IP` and `X-Forwarded-For` must therefore receive 403.

## Cases and evidence limits

With the proposed chain, the harness consumes the unchanged `20/1s/100` burst
for visitor A and checks that A receives 429 while visitor B, on the same
allowlisted peer with a different header value, remains 200. It repeats that
consumer check with IPv4 and IPv6 header values. It also records the real
Traefik status for an absent header, an invalid value, repeated header fields,
and a comma-separated value. Those are extractor observations, not claims that
Traefik validates an IP or that any of them is a trusted Cloudflare identity.
The Docker network IPAM gateway and trusted status identify the test peer; any
access-log peer values and the exact image version/digest are printed too.

The negative mutations run against generated temporary provider files and do
not edit the source manifest:

```bash
python3 deploy/production/tests/test_cloudflare_rate_limit.py \
  --manifest deploy/production/control-plane.yaml \
  --mutation remove-source-criterion
python3 deploy/production/tests/test_cloudflare_rate_limit.py \
  --manifest deploy/production/control-plane.yaml \
  --mutation remove-cloudflare-only
```

Both mutation commands intentionally exit non-zero because they run the same
positive contract: A must receive 429 while B remains 200 and a direct forged
header must receive 403. Removing `sourceCriterion` reports B=429; removing
`cloudflare-only` reports direct peer 200. These are contract failures, not
successful or inverted mutation tests.

This experiment is local and does not prove Cloudflare Worker or Transform
Rule behavior, Pseudo IPv4 mode, origin TLS, DNS, or a real EdgeR request. The
Cloudflare/Worker/Transform/PseudoIPv4 checks remain trust-boundary gates
before any production cutover.

