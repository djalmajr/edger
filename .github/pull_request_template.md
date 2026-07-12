## Summary

Describe the behavior or contract changed and why.

## Verification

- [ ] `cargo test --workspace`
- [ ] `cargo clippy --workspace -- -D warnings`
- [ ] `cargo fmt -- --check`
- [ ] Relevant cPanel, OTLP, Helm or runtime checks were run
- [ ] Documentation and planning were updated when behavior changed

## Security and compatibility

- [ ] No secrets, credentials or private data are included
- [ ] Worker isolation and control-plane authentication remain preserved
- [ ] Breaking or removed behavior is explicit in the compatibility matrix
