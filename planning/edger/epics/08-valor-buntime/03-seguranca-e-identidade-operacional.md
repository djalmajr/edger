# Story 08.03: Segurança e identidade operacional

**Origin:** `planning/edger/epics/08-valor-buntime/00-overview.md`

## Context
- **Problema:** Buntime protege chamadas internas, paths reservados, CSRF, request IDs, limites e env sensível. edger precisa entregar valor equivalente antes de ampliar API e serviços.
- **Objetivo:** Fechar fronteiras de segurança operacional para APIs administrativas, dispatch interno e workers por namespace.
- **Valor:** Permite operar o runtime com confiança e evita que paridade de funcionalidade abra brechas.
- **Restrições:** `edger-core` permanece vocabulário puro; checks de I/O ficam no orchestrator/extensões; headers internos não devem virar credencial pública.

## Status
completed (2026-06-29) — segurança operacional v1 entregue: listagem de workers namespace-aware, CSRF same-origin para mutações administrativas browser-facing, bypass interno somente com principal root, request ID confirmado em admin/errors, limites HTTP retornando `413`/`431` antes do dispatch e env filtering unificado para WASI.

## Traceability
- **Source docs:** `planning/edger/epics/07-avancado/07-hardening-compat-matrix.md`, `planning/edger/docs/value-parity-matrix.md`
- **Buntime refs:** `<buntime-repo>/apps/site/src/content/docs/ops/security.md`
- **Prototype refs:** none.
- **Business rules:** mutação operacional depende de auth; namespace limita acesso mesmo com chave válida.

## Files

| Path | Action | Reason |
|---|---|---|
| `crates/edger-core/src/security.rs` | create | Tipos puros para escopo, request id e decisões de auth |
| `crates/edger-core/src/errors.rs` | edit | Erros públicos de segurança e limites |
| `crates/edger-orchestrator/src/auth.rs` | edit | Root, namespace e internal-call guard |
| `crates/edger-orchestrator/src/security.rs` | create | Guard HTTP de CSRF/internal-call para rotas admin |
| `crates/edger-orchestrator/src/wire.rs` | edit | Body/header limits no ingress com erros HTTP corretos |
| `crates/edger-orchestrator/src/pipeline.rs` | edit | Aplicar request id, limits e deny de paths reservados |
| `crates/edger-orchestrator/src/admin_api.rs` | edit | Aplicar listagem namespace-aware e CSRF em mutações |
| `crates/edger-orchestrator/src/manifest_index_stub.rs` | edit | Filtrar inventário admin por namespace do principal |
| `crates/edger-orchestrator/tests/security_operational.rs` | create | Cobertura de CSRF, namespace, internal header e limits |
| `crates/edger-worker/src/factory.rs` | edit | Filtrar env sensível antes de criar isolate |
| `crates/edger-isolation/src/wasm/wasi.rs` | edit | Reusar detector puro de env sensível |
| `docs/developers/06-operacao-e-testes.adoc` | edit | Documentar CSRF, namespace e limites operacionais |
| `planning/edger/docs/value-parity-matrix.md` | edit | Evidência para segurança operacional |

## Detail

### AS-IS
- Há auth gateway e regras de namespace em evolução, mas sem fechamento completo de CSRF, headers internos e env filtering.
- Limites de body/header estão previstos na Epic 07 hardening.
- Request ID e erro público ainda não são contrato consolidado para todos os fluxos.

### TO-BE
- Cada request recebe ou preserva request ID e ele atravessa orchestrator, pool e worker.
- Paths reservados administrativos não são roteados para workers.
- Mutations admin exigem proteção CSRF ou mecanismo equivalente quando usadas por browser.
- Chaves por namespace limitam workers, plugins e serviços acessíveis.
- Env sensível é filtrado por default e só bindings permitidos chegam ao isolate.

### Approach

| Decisão story-time | Escolha | Motivo |
|---|---|---|
| CSRF v1 | Aplicar em mutações admin quando houver `Origin` ou headers `Sec-Fetch-*` | Protege browser/cpanel sem quebrar clientes CLI com Bearer token |
| Bypass interno | Header `x-edger-internal: true` só bypassa CSRF depois de autenticação root | Evita transformar header interno em credencial pública |
| Namespace v1 | `GET /api/admin/workers` aceita key com `workers:read` e filtra por namespace | Entrega valor Buntime de inventário scoped sem liberar mutações reais |
| Mutação v1 | Continua root-only e protegida por auth/CSRF | Nesta story retornava `501`; 08.11/08.13 entregaram toggles runtime em memória, enquanto persistência segura ainda não existe |
| Limites HTTP | Mapear corpo excedido para `413` e headers excedidos para `431` | Erro operacional correto antes de iniciar worker |
| Env filtering | Centralizar detector puro em `edger-core::security` e reusar no WASI | Mantém core sem I/O e evita padrões divergentes |

### Risks
- CSRF estrito para todo `POST` quebraria CLI/curl; por isso o v1 diferencia browser-originated requests de API clients autenticados.
- Listagem namespace-aware de workers não equivale a autorização completa para plugins, files e uploads; essas superfícies ainda não existem no edger e ficam explicitamente fora desta fatia.
- Header limits são aplicados no wire do orchestrator; limites de headers emitidos por worker já existem na serialização de resposta.

### Scope
- **In:** request IDs, internal-call guard, CSRF para mutações browser-facing, limits, env filtering e namespace checks.
- **Out:** OAuth completo, SSO, secrets manager externo, auditoria formal.

### Acceptance criteria
- [x] Paths administrativos reservados nunca caem no dispatch de worker.
- [x] Mutação sem token/CSRF válido retorna 401/403.
- [x] Chave namespaced não lista nem altera recursos de outro namespace.
- [x] Headers internos são aceitos apenas de chamadas internas autenticadas.
- [x] Body/header limit retorna erro tipado e não inicia worker.
- [x] Env filtrado é coberto por teste.

### Dependencies
- Story 08.01 para matriz.
- Story 08.02 para superfície administrativa inicial.

## Tasks
- [x] Fase 1 — Contratos puros de segurança.
  - Done when: `crates/edger-core/src/security.rs` definir header interno, mutating methods, validação same-origin, permissões, namespace opcional e detector de env sensível; `lib.rs` reexportar sem I/O.
- [x] Fase 2 — Admin security guard.
  - Done when: `admin_api.rs` aplicar CSRF/internal guard nas mutações e `GET /api/admin/workers` filtrar por principal com `workers:read`.
- [x] Fase 3 — Limits HTTP de ingresso.
  - Done when: `wire.rs` transformar body excedido em `PAYLOAD_TOO_LARGE`/`413` e header excedido em `HEADER_TOO_LARGE`/`431` antes de iniciar worker.
- [x] Fase 4 — Env filtering unificado.
  - Done when: WASI usar `edger_core::is_sensitive_env_key` e testes cobrirem padrões Buntime como `DATABASE_URL`, `OPENAI_API_KEY`, `_TOKEN`, `_PASSWORD`.
- [x] Fase 5 — Testes operacionais.
  - Done when: `security_operational.rs` cobrir CSRF, internal header, namespace-filtered workers, request ID em erro admin e limits sem dispatch.
- [x] Fase 6 — Documentação e matriz.
  - Done when: docs operacionais e `value-parity-matrix.md` refletirem a segurança v1 e as lacunas restantes.

## Verification
```bash
cargo test -p edger-orchestrator --test security_operational
cargo test -p edger-worker -- env
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh
```
