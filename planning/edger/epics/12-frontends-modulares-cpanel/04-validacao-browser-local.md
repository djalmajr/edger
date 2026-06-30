# Story 12.04: Validacao Browser local

**Origin:** `planning/edger/epics/12-frontends-modulares-cpanel/00-overview.md`

## Context

O usuario ja usa o Browser integrado para validar fluxos locais. Para frontends modulares, a validacao visual e funcional deve ser parte do aceite, junto dos testes Rust.

**Depende de:** Story 12.01, Story 12.02, Story 12.03

## Files

| Path | Action | Reason |
|---|---|---|
| `planning/edger/status/evidence/` | edit | Registrar evidencia de Browser local |
| `edger-orchestrator/tests/value_parity.rs` | edit | Manter prova automatizada complementar |
| `docs/developers/06-operacao-e-testes.adoc` | edit | Documentar URL local e checks visuais |
| `workers/` | edit | Ajustar fixtures/apps de frontend quando necessario |

## Detail

### AS-IS

- Browser ja validou `/todos` local em checkpoints anteriores.
- Gates automatizados cobrem roteamento e seguranca, mas nao experiencia visual.

### TO-BE

- Runbook local cobre abrir app, navegar catalogo, inspecionar detalhe e validar erro operacional.
- Evidencia inclui URLs, respostas relevantes e capturas ou logs quando necessario.
- Teste automatizado continua sendo o gate principal para regressao.

### Scope

- **In:** Browser/Playwright local, evidencia, runbook, checks de layout basico.
- **Out:** teste cross-browser amplo, deploy remoto, auditoria visual completa.

### Critérios de aceite

- [ ] Fluxo Browser local abre sem erro em porta local documentada.
- [ ] Catalogo e detalhe nao exibem dados sensiveis.
- [ ] Estados de erro/acesso negado sao verificaveis.
- [ ] Evidencia fica em `planning/edger/status/evidence/`.

## Tasks

- [ ] Definir runbook de validacao local.
- [ ] Rodar Browser ou Playwright contra app local.
- [ ] Registrar evidencia objetiva.
- [ ] Atualizar docs e status da story.

## Verification

```bash
export ROOT_API_KEY
PORT=19080 RUNTIME_WORKER_DIRS=workers cargo run -p edger-orchestrator --bin edger
curl -sS http://127.0.0.1:19080/healthz
SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
```
