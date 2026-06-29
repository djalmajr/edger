# Status: Story 02.01 — setup edger-core crate

**Mode:** Checkpoint  
**Story:** `planning/edger/epics/02-edger-core/01-setup-core-crate.md`

## Completed
- `edger-core/Cargo.toml` workspace inherit; no sibling deps
- Module tree declared in `lib.rs` (manifest, error, extension stubs + re-exports)
- Unit test `modules_declared_and_reexported`

## Verification
- `cargo test -p edger-core` — 2 lib tests pass at this checkpoint

## Next step
Story 02.02 — core models