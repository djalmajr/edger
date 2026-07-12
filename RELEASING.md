# Releasing EdgeR

## Checklist

1. Confirm the changelog describes user-visible and licensing changes.
2. Confirm Gitleaks and the dependency advisory audit are clean.
3. Run the Rust workspace test, clippy, formatting and optional OTLP gates.
4. Run the cPanel, Helm and planning/refinement gates.
5. Build the release binary and container image from a clean checkout.
6. Verify the image carries `org.opencontainers.image.licenses=O'Saasy-1.0`.
7. Exercise health, authenticated admin access and representative workers.
8. Generate or verify the third-party dependency license inventory distributed
   with binary and container artifacts.
9. Publish release notes that identify the applicable license and upgrade
   considerations.

The planned first O'Saasy release is `v0.2.0`. It must explicitly identify the
transition boundary from earlier MIT distributions. Creating the tag or
publishing artifacts requires explicit maintainer authorization after the
release rehearsal passes.
