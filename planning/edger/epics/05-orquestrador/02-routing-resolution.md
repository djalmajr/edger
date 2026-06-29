# Story 05.02: Resolução de rotas (namespaced paths, semver, reserved, plugin base)

**Origin:** `planning/edger/epics/05-orquestrador/00-overview.md`

## Context
- **Problema:** Requisições HTTP não são mapeadas para `WorkerRef` nem respeitam convenções Buntime de endereçamento.
- **Objetivo:** Portar lógica de resolução de path do Buntime para Rust puro no orchestrator.
- **Valor:** Workers acessíveis via `/name`, `/@scope/name`, versões semver e fallback homepage; plugins com base precedence.
- **Restrições:** Usar `WorkerManifest` / helpers do core; sem dispatch real nesta story (apenas `ResolvedRoute`).

## Traceability
- **Source docs:** `planning/edger/design.md` (Multi-Tenancy/Routing, Data Model), Buntime `planning/edger/design.md (mapping table)`, `planning/edger/design.md (WorkerPool; ai-memory zommehq/buntime)`
- **Design PR:** PR 6 (routing dentro do orchestrator básico)
- **Depende de:** Story 05.01, Epic 02 (`WorkerRef`, namespace helpers, semver)

## Files

| Path | Ação | Motivo |
|---|---|---|
| `edger-orchestrator/src/router.rs` | criar | parse path, semver, reserved |
| `edger-orchestrator/src/manifest_index_stub.rs` | criar | índice mínimo em memória para testes de routing (sem multi-dir) |
| `edger-orchestrator/src/lib.rs` | alterar | re-exports |
| `edger-orchestrator/tests/routing_resolution.rs` | criar | casos Buntime |
| `edger-core/src/manifest.rs` | alterar (se necessário) | helpers namespace/semver |

## Detail

### AS-IS
Sem parser de URL; manifests não carregados no orchestrator.

### TO-BE
- `ResolvedRoute` enum ou struct:
  - `Worker { worker: WorkerRef, rewritten_path: String, kind_hint: ExecutionKind }`
  - `Reserved { kind: ReservedPath }` — `/api`, `/health`, `/.well-known`
  - `HomepageFallback { worker: WorkerRef }`
  - `PluginBase { plugin: PluginRef, remainder: String }` — precedência base do plugin
- Regras:
  - Parse `@scope/name@version` ou `latest`
  - Semver resolution contra manifests disponíveis
  - Paths reservados não passam por worker dispatch
  - Plugin base: prefixo do plugin tem precedência sobre worker genérico
  - Colisão detectada via helpers do core (erro explícito)
- `resolve_route(path, base_href, manifest_index) -> Result<ResolvedRoute>`

### Escopo
- **In:** parsing, semver, reserved, plugin precedence, testes unitários extensivos
- **Out:** `load_manifests_from_dirs` completo (owned por story 07.01), rewrite de body/headers, shell injection, auth gate (05.04)

### Critérios de aceite
- [x] Tabela de casos Buntime coberta por testes (mín. 15 cenários) — 17 testes
- [x] `/@acme/app@1.0.0/foo` resolve worker correto com path reescrito `/foo`
- [x] `/health` → `Reserved`, não worker
- [x] Plugin com `base: /gateway` ganha precedência sobre worker homônimo
- [x] Colisão de nome retorna erro tipado (não panic)

## Pendências
- `PluginRef` reutiliza `WorkerManifest` (sem `PluginManifest` dedicado — Epic 07).
- `base_href` ignorado nesta story; pipeline 05.03 aplica rewrite completo.
- Paths desconhecidos retornam `NOT_FOUND` (homepage só para `/`).
- `load_manifests_from_dirs` real fica em 07.01; stub `ManifestIndex` cobre testes.

### Dependências
- Story 05.01 (servidor pode registrar router stub)
- Epic 02: parsers e `WorkerRef`

## Test-first plan
1. **Red:** `resolve("/hello")` com manifest `hello` → `Worker` — falha sem impl
2. **Red:** `resolve("/@acme/api@2.0.0")` com semver → versão exata
3. **Red:** `resolve("/health")` → `Reserved`
4. **Red:** plugin base `/p` vs worker `/p` → plugin wins
5. **Green:** implementar `router.rs` incrementalmente por grupo de casos
6. **Refactor:** extrair `PathParser` testável sem I/O

**Nível:** unitário (`tests/routing_resolution.rs` + `mod tests` em router)

## Tasks
- [x] Definir `ResolvedRoute`, `ReservedPath`, `ManifestIndex`
- [x] Implementar `manifest_index_stub.rs` (índice mínimo em memória para testes de routing)
- [x] Implementar parse de namespace + semver (`semver` crate)
- [x] Implementar reserved paths e homepage fallback
- [x] Implementar plugin base precedence
- [x] Integrar collision detection (`COLLISION` em `ManifestIndex::insert`)
- [x] Suite de testes com fixtures YAML em `tests/fixtures/manifests/`
- [x] Documentar tabela de mapeamento Buntime ↔ Rust no módulo (`router.rs` header)

## Verification
```bash
cargo test -p edger-orchestrator routing
cargo test -p edger-orchestrator
cargo clippy -p edger-orchestrator -- -D warnings
bun test
```