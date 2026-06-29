# edger-ext-gateway (template)

Skeleton **Middleware** extension for edger. Copy this directory to create a new `edger-ext-<nome>` crate.

## Choose ONE

Each `edger-ext-*` crate implements **one** mode:

- `Middleware` (this template), or
- `AuthProvider` (see `edger-ext-auth`), or
- `WorkerHandler`

Do not mix modes in the same crate without mutually exclusive Cargo features.

## Create a new extension (< 30 min)

1. **Copy** `edger-ext-gateway/` → `edger-ext-<nome>/`
2. **Rename** in `Cargo.toml` (`name = "edger-ext-<nome>"`)
3. **Implement** the trait(s) for your mode in `src/lib.rs`
4. **Add** the member to workspace root `Cargo.toml`
5. **Register** in `edger-orchestrator/src/bin/edger.rs`:
   ```rust
   collect_extensions(vec![
       edger_ext_gateway::GatewayExtension::middleware(),
       // edger_ext_<nome>::YourExtension::middleware(),
   ])?;
   ```
6. **Write tests** under `tests/` and colocated `#[cfg(test)]`
7. **Run gates:**
   ```bash
   cargo test -p edger-ext-<nome>
   cargo test --workspace
   cargo clippy --workspace -- -D warnings
   ```

## Priority order

Lower `priority()` runs earlier in `on_request`. Auth (`edger-ext-auth`) uses `-100`; gateway uses `0`.

## What this template does

- `on_request` returns `None` (continue pipeline)
- With header `X-Gateway-Test`, increments an internal counter (for tests) and emits a trace log
- No reverse proxy, rate limiting, or TLS — add those in your own crate if needed