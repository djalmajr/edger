# Story 08.22: Worker semver range routing

**Status:** completed

**Origin:** `planning/edger/epics/08-valor-buntime/00-overview.md`

## Context

A matriz de paridade ainda marcava `Worker addressing, namespace e semver` como
`partial` porque o router só resolvia `latest` e versões exatas. Buntime entrega
valor operacional quando o operador pode publicar múltiplas versões e rotear uma
faixa compatível sem fixar sempre uma versão exata.

## Traceability

- `planning/edger/docs/value-parity-matrix.md`
- `planning/edger/docs/compat-matrix.md`
- `edger-orchestrator/src/manifest_index_stub.rs`
- `edger-orchestrator/tests/routing_resolution.rs`

## Files

| Path | Action | Reason |
|---|---|---|
| `edger-orchestrator/src/manifest_index_stub.rs` | alter | Resolver `semver::VersionReq` preservando `latest` e versão exata |
| `edger-orchestrator/tests/routing_resolution.rs` | alter | Provar ranges em rotas namespaced e unscoped |
| `planning/edger/docs/value-parity-matrix.md` | edit | Marcar linha de worker addressing como testada |
| `planning/edger/docs/compat-matrix.md` | edit | Registrar compatibilidade técnica de semver range |
| `planning/edger/status/evidence/story-08-22-runtime.txt` | create | Registrar comandos de verificação |

## Detail

### AS-IS

- `/name` e `/@scope/name` resolviam a maior versão semver.
- `/name@1.0.0` e `/@scope/name@1.0.0` resolviam versão exata.
- Sintaxes como `^1.0.0` e `~1.2.0` retornavam `NOT_FOUND`.

### TO-BE

- `latest` continua escolhendo a maior versão disponível.
- Versão exata continua sendo exata e não vira range implícito.
- Requests com sintaxe de range (`^`, `~`, comparadores, wildcard, vírgula ou
  `||`) usam `semver::VersionReq`.
- O maior worker habilitado que satisfaz o range é escolhido.
- Range sem match retorna `NOT_FOUND`.

### Scope

- **In:** resolução local de ranges semver no `ManifestIndex` e testes de rota.
- **Out:** upload/install de versões, mutação por versão na Admin API,
  persistência de seleção e UI.

### Test-first plan

- Primeiro comportamento: `/@acme/app@^1.0.0/foo` deve escolher `1.0.0` quando
  `2.0.0` também está disponível.
- Segundo comportamento: `/svc@~1.2.0/ping` deve escolher a maior versão que
  satisfaça o range sem cruzar para `1.4.0` ou `2.0.0`.
- Regressão protegida: request exato `1.0.0` não deve aceitar `1.0.1`.
- Testes de baixo valor evitados: snapshots de tabela de rotas e mocks do
  parser sem passar pelo router real.

## Acceptance criteria

- [x] Router resolve range semver para worker namespaced.
- [x] Router resolve range semver para worker unscoped.
- [x] Versão exata mantém semântica exata.
- [x] Range sem match retorna `NOT_FOUND`.
- [x] Matriz de valor e compat matrix refletem a evidência.

## Dependencies

- 08.08.

## Tasks

- [x] Adicionar testes de range semver em `routing_resolution.rs`.
- [x] Adicionar invariantes unitárias no `ManifestIndex`.
- [x] Implementar `semver::VersionReq` mantendo `latest` e exato.
- [x] Atualizar matrizes e status.

## Verification

```bash
cargo test -p edger-orchestrator --lib manifest_index_stub::tests
cargo test -p edger-orchestrator --test routing_resolution semver_range
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh
```
