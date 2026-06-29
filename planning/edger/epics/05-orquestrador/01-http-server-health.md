# Story 05.01: Servidor HTTP (axum/hyper) + endpoints /health e /ready

**Origin:** `planning/edger/epics/05-orquestrador/00-overview.md`

## Context
- **Problema:** Não existe processo listener; o runtime não expõe endpoints operacionais.
- **Objetivo:** Subir servidor HTTP mínimo com health/readiness alinhados ao Buntime.
- **Valor:** Primeiro marco visível — processo escuta porta e responde probes de orquestração/K8s.
- **Restrições:** axum ou hyper + tower; sem lógica de roteamento de workers nesta story; graceful shutdown stub.

## Traceability
- **Source docs:** `planning/edger/design.md` (Observability — Health/Readiness, PR 6)
- **Design PR:** PR 6 — `feat(orchestrator): basic HTTP server, routing, request pipeline + main composition`
- **Depends on:** Epic 02 (tipos mínimos), Epic 04 (WorkerPool mock para readiness), Story 05.02+ (roteamento completo)

## Files

| Path | Ação | Motivo |
|---|---|---|
| `edger-orchestrator/Cargo.toml` | alterar | axum/hyper, tower, http |
| `edger-orchestrator/src/lib.rs` | alterar | exportar módulo server |
| `edger-orchestrator/src/server.rs` | criar | bind, router mínimo, health/ready |
| `edger-orchestrator/src/bin/edger.rs` | criar | composição inicial + `PORT` |
| `edger-orchestrator/tests/health_integration.rs` | criar | test client contra servidor |
| `planning/edger/epics/05-orquestrador/00-overview.md` | alterar | status story |

## Detail

### AS-IS
Stub `orchestrator_stub()`; sem bin; sem listener.

### TO-BE
- `ServerState` com handles para pool (opcional nesta story) e flag de readiness
- Rotas fixas:
  - `GET /health` → 200 `{"status":"ok"}` (liveness)
  - `GET /ready` → 200 se manifests carregados + pool inicializado; 503 caso contrário
- `PORT` via env (default `3000` ou alinhado Buntime)
- `tracing` span por request; header `X-Request-Id` propagado ou gerado
- Shutdown via `ctrl_c` (stub que chama `pool.shutdown()` quando existir)

### Escopo
- **In:** servidor, health/ready, bin mínimo, testes de integração
- **Out:** resolução de workers (05.02), pipeline completo (05.03), métricas `/metrics`

### Critérios de aceite
- [ ] `cargo run -p edger-orchestrator --bin edger` escuta e responde `/health`
- [ ] `/ready` retorna 503 antes de init completo e 200 após
- [ ] Teste de integração sem rede externa (axum `TestClient` ou hyper client)
- [ ] `cargo clippy -p edger-orchestrator -- -D warnings` limpo

### Dependências
- Epic 02: `CoreError` ou tipos de resposta mínimos
- Epic 04: `WorkerPool` instanciável (pode ser vazio) para readiness

## Test-first plan
1. **Red:** teste `GET /health` retorna 200 — falha sem servidor
2. **Red:** teste `GET /ready` retorna 503 com estado não inicializado
3. **Green:** implementar `server.rs` + router axum
4. **Green:** marcar ready após init stub
5. **Refactor:** extrair `ServerConfig` (addr, timeouts)

**Nível:** integração (`tests/health_integration.rs`)

## Tasks
- [ ] Adicionar deps axum/hyper/tower/http ao `Cargo.toml`
- [ ] Criar `server.rs` com `Router` e handlers health/ready
- [ ] Criar `ServerState` + construtor `new_unready()` / `mark_ready()`
- [ ] Criar `src/bin/edger.rs` com `#[tokio::main]` e bind
- [ ] Propagar ou gerar `X-Request-Id` (middleware tower)
- [ ] Escrever testes de integração (health + ready 503/200)
- [ ] Documentar env `PORT` em comentário do bin ou AGENTS
- [ ] Atualizar status na overview do epic

## Verification
```bash
cargo test -p edger-orchestrator
cargo clippy -p edger-orchestrator -- -D warnings
cargo fmt -- --check
bun test
```