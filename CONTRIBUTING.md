# Contributing to EdgeR

Contributions are welcome under the repository's [O'Saasy License](LICENSE).
By submitting a contribution, you agree that it may be distributed under that
license and that you have the right to provide it.

EdgeR currently uses an inbound-equals-outbound contribution model: there is no
separate contributor license agreement. Do not contribute code that you cannot
license under the repository terms.

Before opening a pull request:

1. keep changes small and preserve worker isolation;
2. update planning and documentation when behavior changes;
3. run `cargo test --workspace`;
4. run `cargo clippy --workspace -- -D warnings`;
5. run `cargo fmt -- --check`;
6. run the relevant OTLP, cPanel, Helm or runtime-specific checks;
7. keep compatibility removals and breaking changes explicit.

Open an issue before beginning a broad architectural change. Pull requests
should explain the observable outcome, list the verification performed and
avoid combining unrelated cleanup with behavior changes.

Report vulnerabilities privately according to [SECURITY.md](SECURITY.md).
