# Epic 20 — Endurecimento do Runtime (P0→P3)

**Origin:** análise comparativa edger vs Supabase edge-runtime + run2biz edge-runtime

## Contexto

**Problema:** a comparação com dois edge-runtimes maduros revelou furos de
segurança concretos, limites de recurso que são stubs, e lacunas de ops/DX. Este
epic endurece o runtime sem re-adicionar a opinião pesada (gateway/app-shell) que
o Epic 17 removeu de propósito.

**Princípio norteador:** pegar as versões "grossas" por-processo dos controles do
Supabase (que embute deno_core). NÃO embutir deno_core nem re-adicionar
main-runtime JS, namespace EdgeRuntime rico, event-worker, ONNX, ou tenancy de
banco — tudo isso é anti-recomendação explícita.

## Story backlog

| # | Story | Prioridade | Área | Tam. | Status |
|---|---|---|---|---|---|
| 01 | Sandbox de rede + cache: egress allowlist por-worker + DENO_DIR read-only/isolado | P0 | segurança | M | ✅ merged (PR #22) |
| 02 | OIDC: claims→namespaces, is_root só com role admin explícito | P0 | segurança | M | ✅ merged (PR #20) |
| 03 | Ciclo de vida do pool: circuit-breaker crash-loop + modo oneshot + pre-warm eager | P1 | isolamento | L | ✅ merged (PR #25) |
| 04 | Limites do processo: CPU-time soft/hard + RSS enforcement + recycle-por-causa | P1 | isolamento | L | ✅ merged (PR #26) |
| 05 | Cron real: parser 5-campos (crate) substituindo parser artesanal | P1 | dx | S | ✅ merged (PR #23) |
| 06 | SPA/fullstack: injeção de env (window.__env__) + rewrite de base href | P1 | ops | M | ✅ merged (PR #24) |
| 07 | Wasm higiene: cache de Module + fuel/epoch + StoreLimits + streaming (>64KB) | P1 | isolamento | M | ✅ merged (PR #21) |
| 08 | Admissão: rate-limit/cota por-worker (idle-timeout já coberto pelo wall-timeout) | P2 | segurança/ops | M | ✅ merged (PR #27) |
| 09 | Observabilidade: evento por-execução ✅ (PR #28); OTLP exporter → Epic 21.08 | P2 | observabilidade | M | ✅ concluída via 21.08 |
| 10 | Cron multi-réplica: leader-election (k8s Lease) | P2 | ops | M | ⏭️ deferido (follow-up) |
| 11 | Sinais de lifecycle ao JS: beforeunload/drain + waitUntil mínimo opt-in | P3 | dx | M | ✅ concluída via 21.11 |

**Status do epic:** 10/11 entregues e validados — todos P0 e P1, rate-limit,
evento por execução, OTLP e sinais de lifecycle/drain. A única cauda deferida é
leader election de cron multi-réplica; ver `follow-ups/e20-deferred-tail.md`.

## Roadmap / ordem de execução

Arquivos-quentes compartilhados: `edger-core/src/config.rs` + `manifest.rs`,
`edger-isolation/src/multiproc.rs` + `limits.rs` + `multiproc_harness.mjs`,
`edger-worker/src/pool.rs`. Por isso o cluster multiproc/pool/limits/harness roda
majoritariamente SEQUENCIAL; stories de arquivos disjuntos rodam em paralelo.

- **Onda 1 (paralela, disjunta):** 02 (oidc.rs), 05 (cron.rs), 06 (fullstack/static_spa), 07 (wasm/*). A cauda 09-OTLP foi consolidada na Story 21.08.
- **Onda 2 (P0 sandbox):** 01 (multiproc + config) — sozinha por tocar config/multiproc.
- **Onda 3 (cluster pool/limits, sequencial):** 03 → 04 → 08.
- **Onda 4:** 10 (cron leader-election, depende de 05) → 11 (harness signals, depois de 08).

Caminho crítico de segurança: 01, 02 primeiro (P0).

## Epic acceptance criteria

- [x] Egress do worker é allowlist por-worker; `--allow-net` aberto só é opt-in
- [x] DENO_DIR não é gravável cross-tenant (read-only pós-warm ou por-worker)
- [x] Nenhum JWT válido vira root sem role admin explícito; namespaces escopados
- [x] CPU-time soft/hard aplicado (recycle/kill); RSS enforced pelo runtime
- [x] Modo oneshot disponível; crash-loop tem backoff/circuit-breaker
- [ ] Parser de cron aceita shapes canônicos; leader election evita duplicação multi-réplica
- [x] SPA serve env em runtime sem rebuild
- [x] Wasm não recompila por request; tem limite de CPU/memória; body >64KB
- [x] rate-limit por-worker; idle-timeout no harness
- [x] OTLP exporta traces quando configurado (Story 21.08); evento por-execução com causa/custo entregue
- [x] Sinais `beforeunload`/drain e `waitUntil` bounded são observáveis (Story 21.11)
- [x] `fmt`/`clippy -D warnings`/`test --workspace` verdes; validação viva por feature

## Status

partially completed — 10 de 11 stories estão concluídas. O runtime está
endurecido para operação em instância única e exportação OTLP opt-in. A Story
20.10, leader election para cron em múltiplas réplicas, permanece deferida e não
é requisito para o produto local; deve voltar ao backlog somente com cenário
multi-réplica real e contrato de coordenação aprovado.

## Riscos

- `config.rs`/`manifest.rs` normalização já regrediu 2× (Epic 18) — rodar suíte inteira após cada iteração.
- Cluster pool/limits/harness tem alta sobreposição — sequenciar e rebasear.
- P0 são achados de agentes com file:line — confirmar cada citação antes de codar.
- Story 11 (waitUntil) tensiona o minimalista — manter opt-in e com teto de tempo.
