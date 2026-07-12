# cPanel responsive layout — 2026-07-11

## Scope

- Responsive application shell and compact sidebar rail below 1200 px.
- Responsive Workers summary, filters, app headers, version rows and pagination.
- Responsive Overview pool metrics and runtime status values.
- Responsive Files header/actions and listing.
- Closed tooltips removed from layout flow so they cannot create hidden horizontal overflow.

## Browser evidence

Validated in the in-app Browser against the live runtime at `127.0.0.1:19080`.
The assertion compares the main content scroller's `scrollWidth` and `clientWidth`.

| Route | 1155 px | 768 px | 390 px |
| --- | --- | --- | --- |
| `/cpanel/` | 1091 / 1091 | 689 / 689 | 311 / 311 |
| `/cpanel/workers` | 1076 / 1076 | 689 / 689 | 311 / 311 |
| `/cpanel/workers/cpanel/0.2.0/files` | 1091 / 1091 | 704 / 704 | 326 / 326 |

All route/viewport combinations reported `overflow: false`. The unauthenticated login surface was also checked at 390 px and reported `bodyScrollWidth === bodyClientWidth`.

The initial failing Workers measurement at 1155 px was `978 / 916`, proving the fixed version grid caused internal horizontal scrolling before the responsive change.

## Gates

```text
bash planning/edger/scripts/cpanel-ui-gate.sh
cpanel-ui-gate ok

cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
exit 0
```

The planning refinement oracle still reports pre-existing documentation failures in
`epics/19-runtime-completude/00-overview.md` and
`epics/20-endurecimento-runtime/00-overview.md`: both are missing the required
`Epic acceptance criteria` and `Status` sections. These files were outside this
responsive-layout change.
