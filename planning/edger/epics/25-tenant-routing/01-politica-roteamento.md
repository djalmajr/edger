# Story 25.01: política por app, validação e persistência

**Origin:** `planning/edger/epics/25-tenant-routing/00-overview.md`.

## Context

O ponteiro `.edger-defaults` representa uma versão única. Permissões de tenant e pesos precisam ser alterados sem editar um artefato de release nem fazer `promote` a cada requisição. Objetivo: política opcional por **nome completo** do app, independente de cada versão, com atualização atômica e controle administrativo. O app sem política conserva o comportamento atual (TR-01).

## Traceability

Regras TR-01, TR-04, TR-07 e TR-08 do [epic](00-overview.md). Fonte: `manifest_index_stub.rs` (`default_versions`, `set_default_version`), `manifest_loader.rs` (persistência com rename/sync), `admin_api.rs` (auth, namespace e worker scope), `deploy.rs` (slot de mutação). Sem tela nesta história.

## Files

| Caminho | Ação | Motivo |
|---|---|---|
| `crates/edger-orchestrator/src/routing_policy.rs` | criar | Tipos de política, validação e snapshots em memória. |
| `crates/edger-orchestrator/src/manifest_loader.rs` | editar | Persistência e reload em diretório próprio, com escrita atômica. |
| `crates/edger-orchestrator/src/manifest_index_stub.rs` | editar | Estado por nome, validação de elegibilidade e troca atômica de política. |
| `crates/edger-orchestrator/src/admin_api.rs` | editar | GET/PUT/DELETE autenticados por nome em query; mutação root-only na primeira entrega. |
| `crates/edger-orchestrator/tests/routing_policy.rs` | criar | Contrato HTTP/persistência e testes negativos. |
| `crates/edger-orchestrator/src/state_export.rs` | editar | Incluir policy publicada e excluir arquivo temporário do ZIP de backup. |
| `crates/edger-orchestrator/tests/state_export.rs` | editar | Provar conteúdo e exclusão de temporário. |

## Detail

**AS-IS:** `defaultVersion` persistido em `{name,version}` e acionado por promote.

**TO-BE:** documento por app com `tenantAccess` (`public` ou `allowlist` não vazia de slugs) e `traffic` opcional com versões e pesos inteiros cuja soma é 100. O modo public padrão e tráfego ausente equivalem a nenhuma política. Slug segue o contrato Tenancit `^[a-z0-9]+(?:-[a-z0-9]+)*$`, até 63 caracteres; sem conversão implícita de caixa. Nomes duplicados, versão inexistente, staged, interna/desabilitada, pesos zero/negativos e soma inválida são rejeitados. CPanel/core/bundled não entram no split inicial. A API `GET|PUT|DELETE /api/admin/routing-policy?name=<nome-completo>` evita perda de `@scope/` pelo parâmetro de path. GET exige `workers:read` e escopo daquele worker; PUT/DELETE são **root-only** nesta primeira entrega, pois `workers:promote` não deve poder remover uma barreira de tenant. Cada PUT valida o estado completo, grava arquivo temporário com `sync_all`, renomeia e só então troca snapshot; erro preserva a política anterior. DELETE retira split/allowlist e volta ao default, sem tocar nas versões. Rescan/restart recarregam o estado, incluindo erro explícito e seguro para arquivo inválido.

**Dependências:** decidir se toda versão do host deve declarar o alias (TR-05); 25.02 e 25.03 consomem o snapshot. Política em arquivo local não equivale a consenso entre réplicas: operação multi-réplica exige store/controle externo antes de produção compartilhada.

### Acceptance criteria

- Política válida é lida após restart e policy ausente preserva comportamento anterior.
- Update inválido ou não autorizado não altera disco nem snapshot ativo.
- Rollback por DELETE remove política sem mudar o default da versão.

## Test-first plan

1. Teste falhando para PUT 80/20 + allowlist e GET round-trip após restart.
2. Teste falhando para cada variante inválida, garantindo que GET ainda devolve política anterior.
3. Teste de principal não-root, sem permissão ou fora de namespace/worker; nenhum arquivo criado.
4. Teste de DELETE/rollback, incluindo nome namespaced.

## Tasks

- [x] Escolher schema JSON fechado e registrar exemplos nos docs; definir significado explícito de ausência versus lista vazia.
- [x] Implementar validação pura de pesos, slugs, versões e limites de tamanho/quantidade.
- [x] Implementar armazenamento atômico com path seguro derivado do nome e root já configurado, sem secrets.
- [x] Integrar policy snapshot ao índice sem mutar `defaultVersion` por request.
- [x] Adicionar endpoints admin autorizados e respostas de erro estáveis.
- [x] Provar persistência/restart, concorrência de updates e rollback.
- [x] Provar que export de estado contém política publicada e nunca captura temporário.

## Verification

```bash
cargo test -p edger-orchestrator --test routing_policy
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
```

Registrar outputs em `planning/edger/status/evidence/`. O gate de planejamento exige relatório Mode 1 em `SCRATCH/refinement-report.txt` antes de `SCRATCH=<diretório> planning/edger/scripts/run-gates.sh`. A história só passa com teste HTTP e boot, não apenas unitário.

**Estado:** implementada e testada localmente; evidência em `../../status/evidence/tenant-routing-2026-09-26.md`. Store em arquivo local ainda não fornece consenso entre réplicas.
