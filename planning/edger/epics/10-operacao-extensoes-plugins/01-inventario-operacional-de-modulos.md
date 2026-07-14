# Story 10.01: Inventario operacional de modulos

**Origin:** `planning/edger/epics/10-operacao-extensoes-plugins/00-overview.md`

## Context

O Buntime mostra plugins como entidades operaveis. No edger, o registry ja tem capabilities, providers, hooks, status e diagnostics, mas o inventario ainda nao e tratado como contrato modular completo.

**Depende de:** Epic 06, Epic 08.13, Epic 08.17, Epic 08.26

## Files

| Path | Action | Reason |
|---|---|---|
| `crates/edger-orchestrator/src/admin.rs` | edit | Expor inventario de modulos sem misturar com logica de core |
| `crates/edger-orchestrator/src/extensions.rs` | edit | Consolidar shape operacional de modulo se necessario |
| `crates/edger-orchestrator/tests/admin_workers_plugins.rs` | edit | Provar resposta protegida e redaction de inventario |
| `planning/edger/docs/value-parity-matrix.md` | edit | Marcar evidencias novas da linha de extensoes/plugins |

## Detail

### AS-IS

- Extensoes podem ser listadas e habilitadas/desabilitadas.
- Diagnostics opcionais ja aparecem para gateway.
- Persistencia de status existe, mas nao ha inventario modular completo para agentes.

### TO-BE

- Inventario exposto por Admin API root-only inclui id, nome, versao, tipo de modulo, capacidades, dependencias, estado efetivo, origem de configuracao, diagnostics e campos seguros.
- O contrato evita segredos, paths sensiveis e detalhes de SDK provider.
- A representacao e estavel o suficiente para o futuro MCP consumir sem parsing de texto.

### Scope

- **In:** contrato JSON, redaction, testes de auth/status/capabilities.
- **Out:** UI, dynamic loader, marketplace.

### Critérios de aceite

- [ ] Endpoint ou payload existente retorna inventario modular completo para root.
- [ ] Cliente nao-root nao acessa estado operacional sensivel.
- [ ] Diagnostics nao vazam tokens, headers ou paths sensiveis.
- [ ] Teste cobre pelo menos uma extensao middleware e uma provider.

## Tasks

- [ ] Mapear campos atuais do registry para shape operacional estavel.
- [ ] Implementar redaction e serializacao do inventario.
- [ ] Adicionar testes de auth, shape e diagnostics.
- [ ] Atualizar matriz de valor com evidencia da story.

## Verification

```bash
cargo test -p edger-orchestrator --test admin_workers_plugins
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh
```

