# Story 12.03: Packaging de frontends como workers

**Origin:** `planning/edger/epics/12-frontends-modulares-cpanel/00-overview.md`

## Context

Para manter o projeto modular, frontends devem ser apps/workers versionados e isolados. Isso evita que cPanel, shell e futuras interfaces virem dependencia do core ou do binario principal.

**Depende de:** Story 12.01 para a fatia cPanel minima. Story 12.02 continua dona do shell/catalogo completo.

## Files

| Path | Action | Reason |
|---|---|---|
| `workers/` | edit | Definir layout de frontends modulares |
| `crates/edger-orchestrator/tests/manifest_loader.rs` | edit | Provar manifest/autodiscovery para frontend modular |
| `crates/edger-orchestrator/tests/value_parity.rs` | edit | Provar app frontend servido localmente |
| `docs/developers/06-operacao-e-testes.adoc` | edit | Documentar build/serve local |

## Detail

### AS-IS

- Static SPA e autodiscovery de `index.html` ja funcionam.
- Shell demo e todos provam apps locais.
- Nao ha convencao para frontends de produto como modulos.

### TO-BE

- Frontend modular tem layout previsivel, manifest, base path seguro e versionamento.
- Build artefact pode ser servido pelo runtime sem acoplar framework ao core.
- cPanel/shell/webide futuros seguem a mesma convencao.

### Scope

- **In:** layout local, manifest, versionamento, validacao de path/base.
- **Out:** hospedagem remota, bundler padrao obrigatorio, marketplace.

### Critérios de aceite

- [x] Frontend modular e descoberto como worker/app.
- [x] Base path nao permite hijack de `/api`, `/health` ou `/.well-known`.
- [x] Versionamento e namespace seguem regras de workers.
- [x] Docs explicam como rodar localmente.

## Tasks

- [x] Definir convencao de layout para frontends.
- [x] Provar autodiscovery/manifest em teste.
- [x] Adicionar exemplo local minimo se necessario.
- [x] Atualizar docs de operacao.

## Verification

```bash
cargo test -p edger-orchestrator --test manifest_loader
cargo test -p edger-orchestrator --test value_parity
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh
```

## Status

completed (2026-06-30) - `workers/core/cpanel/manifest.yaml` define `name`, `version`, `entrypoint`, `injectBase` e `visibility`, e o worker e servido como Static SPA sob `/cpanel`. `workers/shell-demo/manifest.yaml` exclui `cpanel`, e `crates/edger-orchestrator/tests/shell_gateway.rs` prova que a rota do cPanel bypassa o shell e recebe `<base href="/cpanel/" />`. Shell/catalogo completo continua na Story 12.02.
