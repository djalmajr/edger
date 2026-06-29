# Matriz de compatibilidade Buntime ↔ edger

**Status:** PLANNING SKELETON — preencher na story `07-hardening-compat-matrix.md`  
**Origin:** `planning/edger/design.md` (Migration notes, mapping table)

Legenda: `pending` = ainda não testado | `tested` | `partial` | `gap`

| Comportamento Buntime | edger | Status | Teste / notas |
|---|---|---|---|
| Worker addressing `/name`, `/@scope/name@ver` | orchestrator router | pending | `compat/routing.rs` |
| Manifest fields (mapping table design) | edger-core manifest | pending | `compat/manifest_fields.rs` |
| `fetch(req) -> Response` contract | isolation deno backend | pending | E2E 07.04 |
| `routes` export | isolation | pending | E2E 07.04 |
| SPA + `<base href>` inject | StaticSpa kind | pending | `compat/shell_spa.rs` |
| ApiKeyPrincipal + namespaces | auth gate | pending | `compat/auth_namespace.rs` |
| Root key bypass | auth | pending | |
| publicRoutes bypass | auth + hooks | pending | |
| Sliding TTL / ephemeral ttl=0 | worker pool | pending | `compat/worker_lifecycle.rs` |
| maxRequests cap | supervisor | pending | |
| onRequest hooks order + short-circuit | extension registry | pending | |
| Reserved paths `/api`, `/health` | router | pending | |
| Env filtering sensitive patterns | worker env | pending | |
| Cron internal requests | native scheduler 07.03 | pending | |
| Shell / micro-frontend | shell routing 07.02 | pending | |