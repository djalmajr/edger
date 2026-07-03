# Story 17.B: Data plane aberto (worker soberano)

**Origin:** `planning/edger/epics/17-edger-minimalista/00-overview.md`

## Context

- **Problema:** hoje o edger autentica o chamador **antes** do worker ver o request (`AuthGate::authorize` no pipeline, `visibility: public/protected`). Isso decide auth pelo worker.
- **Objetivo:** remover a auth do caminho do worker — o worker recebe o request **cru**, com `Authorization` e demais headers intactos, e faz (ou não) sua própria auth.
- **Valor:** worker soberano; edger vira multiplexador puro.
- **Restrições:** control plane (`/api/admin/*`) continua protegido (17.A). Egress e injeção de env/secrets continuam (worker precisa das credenciais dele).

## Traceability
- `edger-orchestrator/src/pipeline.rs` (`authorize`, `is_public_worker`, `skip_hooks`)

## Files
| Path | Action | Reason |
|---|---|---|
| `edger-orchestrator/src/pipeline.rs` | edit | Remover `authorize()`/principal do dispatch de worker; passar request cru |
| `edger-orchestrator/src/auth.rs` | edit | `AuthGate` deixa de gatear data plane (só control plane, via 17.A) |

## Detail
### Scope
- **In:** remover authorize/principal/publicRoutes do caminho do worker; garantir que `Authorization` chega ao worker.
- **Out:** middleware de auth *dentro* do worker (responsabilidade do worker; lib futura).

### Acceptance criteria
- [ ] Request a um worker chega sem gate de auth do edger, com `Authorization` intacto (teste E2E: worker ecoa o header recebido).
- [ ] `/api/admin/*` segue protegido (17.A).
- [ ] `visibility`/`publicRoutes` deixam de afetar o dispatch (removidos em 17.E).

### Dependencies
- Story 17.A

## Tasks
- [ ] Remover authorize/principal do `dispatch_worker`; header `Authorization` preservado até o worker.
- [ ] E2E: worker soberano que valida o próprio token; gate do control plane inalterado.

## Verification
```bash
cargo test -p edger-orchestrator
curl -H "Authorization: Bearer qualquer-coisa" http://127.0.0.1:3000/<worker>  # worker decide
```
