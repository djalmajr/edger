# Design: Runtime JS durável — workers Deno multi-processo isolados

**Data:** 2026-07-02
**Status:** proposta de arquitetura (aguardando aprovação para virar epic/stories)
**Origin:** decisão sobre a Story 07.04 (execução JS real) após medição de performance (`status/evidence/js-runtime-perf-2026-07-02.md`) e requisitos do operador.

## Objetivo

Executar código JS/TS de usuário de forma **durável, isolada e performática**, substituindo a ponte Deno CLI v1 (um `deno eval` por request) sem gambiarras, e satisfazendo quatro requisitos de produto:

1. **Alta compatibilidade** — frameworks JS (Express, Hono, etc.) rodam "como se fosse um Deno local": `node:` builtins, pacotes `npm:`, `Deno.serve`, `fetch`, TS, import maps, imports remotos/JSR.
2. **Isolamento forte** — código não-confiável de tenants diferentes não compartilha memória e não derruba o host.
3. **Controle de recursos** — teto enforçável de memória/CPU por worker; um worker não cresce indefinidamente.
4. **Performance** — sem o custo de spawn+re-import por request (~40 ms medidos → alvo poucos ms).

## Decisão: processos Deno persistentes, não V8 embutido

A medição (`js-runtime-perf-2026-07-02.md`) mostrou ~40 ms/request pela ponte (spawn ~10 ms + re-import ~30 ms por causa do cache-bust `?edger=<uuid>`). Três arquiteturas foram avaliadas:

| Eixo | deno_core embutido (isolates in-process) | **Processo Deno por worker (escolhido)** | deno_runtime embutido |
|---|---|---|---|
| Compat frameworks/npm (req. 1) | Reimplementar node compat + npm — nunca casa 100% | **Deno completo → Express/Hono/npm "just work"** | Bom, mas pinado e pesado |
| Isolamento (req. 2) | Lógico; crash nativo/OOM afeta o host | **Fronteira do SO: memória, crash contido** | Igual ao core |
| Controle de recursos (req. 3) | Difícil por-tenant; runaway afeta host | **rlimit/cgroup por processo, kill limpo** | Idem core |
| Performance (req. 4) | Melhor densidade/latência | **~poucos ms com processo quente** | Similar |
| Custo de manutenção | Vira mantenedor de um edge-runtime | **Baixo: usa o Deno como está** | Alto (pin de versão) |

**Conclusão:** para um runtime multi-tenant, a fronteira de processo é *mais segura* que isolates in-process e entrega compat de framework de graça. Embutir V8 seria passo atrás em isolamento e o mais caro de manter. O embedding permanece possível **como backend interno de cada processo** no futuro (atrás da mesma trait `IsolateTransport`), se um dia densidade/latência exigirem — sem abrir mão da fronteira de processo.

## O código já foi arquitetado para isto

Não é reescrita: o design multi-processo já está provisionado e só precisa ser realizado direito.

- `edger-core::{SerializedRequest, SerializedResponse}` — wire vocabulary do boundary.
- `edger-isolation::wire::{encode_frame, decode_frame}` — framing postcard length-prefixed.
- `edger-isolation::transport::{IsolateTransport, InProcessTransport, UdsTransport}` — `UdsTransport` é stub com o comentário: *"Multi-process rollout: UdsTransport will send length-prefixed postcard frames over Unix domain sockets; supervisor spawns child."*
- feature `multiproc` em `edger-isolation`.
- `edger-worker` supervisor + pool + `EphemeralGate` + lifecycle (Creating/Ready/Active/Idle/…).
- `edger-isolation::limits::{ResourceLimits, LimitGuard, CpuTimer}` — vocabulário de limites (memory_mb/cpu_time_ms/wall_timeout); enforcement de mem/CPU hoje é stub → esta arquitetura a torna real.

## Arquitetura alvo

```mermaid
flowchart LR
    subgraph Orchestrator (Rust)
      Pool[WorkerPool] --> Sup[Supervisor]
      Sup -->|spawn + sandbox + rlimit| W1
      Sup -->|UDS postcard frames| W1
    end
    subgraph "Worker host process (Deno, sandboxed)"
      W1[harness.ts] -->|import once| Mod[user module\nExpress/Hono/Deno.serve]
      W1 -->|serve requests over UDS| Mod
    end
```

- **Supervisor spawna** um processo `deno` persistente por worker (ou por tenant), com sandbox (permissões Deno mínimas + sandbox do SO) e limites de recurso do SO aplicados no spawn.
- Dentro do processo, um **harness** importa o módulo do usuário **uma vez**, captura o handler (`Deno.serve` / `export default { fetch }` / listener `node:http`), e serve requests recebidos do orquestrador via UDS.
- **Protocolo UDS + postcard** (`encode_frame`/`decode_frame`), length-prefixed — substitui o marcador de stdout. Suporta **streaming real** (frames de chunk) no lugar do bounded-first-chunk.
- **Módulo carregado uma vez**; hot-reload no deploy (invalidar/recriar o processo do worker quando a versão muda — encaixa no rescan/install da Fase 14).
- **Ciclo de vida**: pré-warm de processos, health, reciclar no crash (já temos `on_critical_error` + recuperação de pool), TTL/ephemeral.

## Compatibilidade de frameworks (req. 1)

Cada worker é um Deno completo, então:
- `npm:express`, `npm:hono`, `node:http`/`node:stream`/`node:fs`, `Deno.serve`, TS, `deno.json`/import maps, imports remotos/JSR — todos funcionam sem reimplementação.
- O harness captura o ponto de entrada do framework:
  - `Deno.serve(handler)` → captura `handler`;
  - `export default { fetch }` / `export default fn` → usa direto;
  - `app.listen()` de Express (via `node:http.createServer`) → captura o listener (evolução do adapter que já existe), OU o framework bind num loopback interno ao processo e o harness faz proxy — capturar o handler é o caminho preferido.
- Meta: **matriz de compat expandida** com Express e Hono como fixtures `tested`.

## Controle de recursos (req. 3)

Porque cada worker é um processo, os limites viram **enforçáveis pelo SO** (hoje são stub):
- **Memória**: `RLIMIT_AS`/`RLIMIT_DATA` no spawn e/ou cgroup `memory.max`; `--v8-flags=--max-old-space-size` no Deno como segunda barreira. Estouro → processo morto pelo SO, supervisor recicla.
- **CPU**: cgroup `cpu.max` (quota) e/ou `RLIMIT_CPU` para teto absoluto; wall-clock timeout por request já existe.
- **FDs / processos**: `RLIMIT_NOFILE`, `RLIMIT_NPROC`.
- **Mapear** `WorkerConfig` (`memory_mb`, `timeout_ms`, `low_memory`, `max_requests`) → `ResourceLimits` → flags reais no spawn. `LimitGuard::check_memory`/`CpuTimer` deixam de ser stub.
- Observabilidade: RSS/CPU por worker no `/metrics` e na listagem do cPanel (encaixa na transparência da 14.05).

## Fases sugeridas (fatias verticais)

| Fase | Entrega | Validação |
|---|---|---|
| A — Transporte UDS mínimo | `UdsTransport` real sob `multiproc`: supervisor spawna 1 worker Deno persistente; 1 request round-trip por postcard/UDS | E2E: `GET` a um worker JS responde via UDS, sem `deno eval` |
| B — Módulo quente + paridade de kinds | Import uma vez; fetch/routes/SPA pelo processo persistente; matriz de exemplos atual verde | Compat suite atual passa via UDS; perf re-medida (alvo poucos ms) |
| C — Frameworks | Fixtures Express + Hono (npm) rodando; captura de listener node | `tested` na compat-matrix para Express/Hono |
| D — Limites de recurso reais | rlimit/cgroup no spawn; kill on breach; métricas RSS/CPU por worker | Teste: worker que aloca demais é morto e reciclado; teto respeitado |
| E — Streaming + hardening | Streaming real por frames; sandbox do SO (seccomp/landlock onde disponível); pré-warm/pool sizing | SSE/stream passthrough; negativos de sandbox |

## Riscos e mitigação

| Risco | Severidade | Mitigação |
|---|---|---|
| Densidade menor que isolates (memória por processo) | Média | Pré-warm limitado + reciclagem por idle/TTL; pool sizing configurável; embedding fica como backend futuro se densidade virar gargalo |
| cgroup v2 varia por SO (Linux vs macOS dev) | Média | rlimit como base portável; cgroup como reforço em Linux/produção; documentar tiers por plataforma |
| Protocolo UDS/streaming cresce em complexidade | Média | Reusar `wire::encode_frame`/postcard; contrato versionado; testes de round-trip e negativos |
| Captura de `app.listen()` de frameworks Node | Média | Evoluir o adapter `node:http` já existente; fixtures Express/Hono como gate |
| Cold start do pré-warm | Baixa | Processos quentes desde o boot; módulo importado uma vez |

## Non-goals (por ora)

- Embutir V8/`deno_core` in-process (fica como backend futuro, não descartado).
- cgroup/sandbox完整 multi-node/K8s (foco local; contratos prontos para produção).
- Densidade extrema estilo isolates (trade-off consciente por isolamento/compat).


> **Nota de protocolo (2026-07-02):** o boundary Rust↔Deno usa **frames JSON
> length-prefixed** (u32 LE + JSON UTF-8), não postcard — o outro lado é
> JavaScript e implementar postcard em JS não se paga. O `encode_frame`/
> `decode_frame` postcard fica reservado para um futuro worker-em-Rust
> (mesma trait `IsolateTransport`). O corpo trafega como array de bytes,
> mesma convenção da ponte v1.

## Reaproveitamento (não jogar fora)

- A ponte CLI v1 continua como fallback até a Fase B passar a matriz; só então vira legado.
- `IsolateTransport` permite `InProcessTransport` (mock/testes) e `UdsTransport` (produção) coexistirem.
- Quick win imediato possível antes da Fase A: remover o cache-bust `?edger=<uuid>` do import.

## Próximo passo

Transformar este design em epic (ex.: "Fase 15 — Runtime JS Durável Multi-processo") com as fases A–E como stories, ou re-escopar a Story 07.04 para apontar para ele. Recomendo epic próprio: é multi-fase, com ciclo de vida e risco próprios, como as outras capacidades grandes do EdgeR.
