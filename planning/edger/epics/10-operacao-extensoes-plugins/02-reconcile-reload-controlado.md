# Story 10.02: Reconcile e reload controlado

**Origin:** `planning/edger/epics/10-operacao-extensoes-plugins/00-overview.md`

## Context

Enable/disable runtime ja existe, mas reload/rescan ainda e lacuna. O edger precisa de um fluxo operacional claro para reconciliar estado desejado e estado efetivo sem prometer dynamic loading de crates Rust.

**Depende de:** Story 10.01, Epic 08.26

## Files

| Path | Action | Reason |
|---|---|---|
| `crates/edger-orchestrator/src/admin.rs` | edit | Adicionar comando protegido de dry-run/reconcile quando cabivel |
| `crates/edger-orchestrator/src/extensions.rs` | edit | Calcular diferencas entre estado desejado e efetivo |
| `crates/edger-orchestrator/tests/admin_workers_plugins.rs` | edit | Provar dry-run, aplicacao segura e casos que exigem restart |
| `docs/developers/06-operacao-e-testes.adoc` | edit | Documentar operacao local e limites |

## Detail

### AS-IS

- Status store persiste enable/disable de extensoes.
- Registry reconstruido respeita status persistido.
- Nao ha contrato para dry-run, reconcile nem classificacao de mudanca.

### TO-BE

- Admin API suporta dry-run de reconcile mostrando acoes, risco e necessidade de restart.
- Mudancas de status ja suportadas podem ser aplicadas localmente.
- Mudancas que exigem novo binario, nova crate ou dependencia ficam classificadas como restart required.

### Scope

- **In:** dry-run, reconcile de status/configuracao suportada, classificacao de restart.
- **Out:** hot loading de crate, download remoto de plugins, deploy remoto.

### Critérios de aceite

- [ ] Dry-run nao altera estado.
- [ ] Reconcile aplica somente mudancas suportadas em runtime.
- [ ] Mudancas nao suportadas retornam resposta explicita de restart required.
- [ ] Resposta contem request ID e diagnostics seguros.

## Tasks

- [ ] Definir modelo de diff entre estado desejado e efetivo.
- [ ] Implementar caminho dry-run sem side effect.
- [ ] Implementar aplicacao de mudancas runtime ja suportadas.
- [ ] Cobrir restart required em teste automatizado.

## Verification

```bash
cargo test -p edger-orchestrator --test admin_workers_plugins
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh
```

