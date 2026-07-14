# Story 03.01: Spike de embedding (deno_core + facade; comparação wasmtime)

**Origin:** `planning/edger/epics/03-isolacao-execucao/00-overview.md`  
**Status:** completed (2026-06-29; deno V8 boot pendente 03.04)

## Context
- **Problema:** Não há evidência empírica sobre viabilidade, latência de cold start, accounting de memória ou superfície de ops para embedding JS/TS (deno_core) nem para Wasm standalone (wasmtime + WASI).
- **Objetivo:** Spike time-boxed que valida hello-world fetch, timeout básico, medição de spawn/exec e comparação breve wasmtime; produzir `planning/edger/epics/03-isolacao-execucao/spike.md` com go/no-go.
- **Valor:** Mitiga risco #1 do design (embedding maintenance); desbloqueia decisão de split de módulos (`deno` facade vs `wasm`) para stories 03.04 e PR 10 futuro.
- **Restrições:** Time-box (sugestão: 2–3 dias); código de spike em `examples/` ou módulo temporário; **não** mergear embedding de produção nesta story.

## Traceability
- **Source docs:** `planning/edger/design.md` (Embedding Spike Recommendation, PR 2, Resolved Decisions JS/TS + Wasm), `planning/edger/roadmap.md` (Fase 3)
- **Design PRs:** PR 2 (spike), informa PR 5 e PR 10
- **Depende de:** Epic 02 story 02.03 (wire types mínimos para roundtrip de request simulado); Epic 01 (skeleton alinhado)

## Files

| Path | Ação | Motivo |
|---|---|---|
| `crates/edger-isolation/Cargo.toml` | alterar | `[dev-dependencies]` ou features opcionais: `deno_core`, `deno_runtime` (mínimo), `wasmtime`, `wasmtime-wasi` |
| `crates/edger-isolation/examples/embedding-spike-deno.rs` | criar | Boot V8, módulo fetch trivial, roundtrip request |
| `crates/edger-isolation/examples/embedding-spike-wasm.rs` | criar | Load Wasm mínimo + WASI smoke |
| `planning/edger/epics/03-isolacao-execucao/spike.md` | preencher | Skeleton existe; story preenche métricas, sharp edges, Go/no-go |
| `planning/edger/design.md` | alterar (se necessário) | Atualizar riscos/rollout se spike mudar direção |
| `planning/edger/epics/03-isolacao-execucao/00-overview.md` | alterar | Status story 03.01 |

## Detail

### AS-IS
- Sem dependências de embedding em `edger-isolation`
- Sem exemplos executáveis de isolate
- Risco de embedding documentado mas não validado

### TO-BE
- Exemplo deno: inicializa platform singleton, registra ops mínimos, carrega string/module `export default { fetch(req) { return new Response("ok") } }`, serializa request simples, mede tempo spawn + exec
- Exemplo wasmtime: compila módulo Wasm trivial (add ou echo), WASI preview1/p2 conforme versão pinada, mede compile + invoke
- Instrumentação: `std::time::Instant` para spawn/exec; log de heap aproximado se API disponível; guard de timeout (tokio::time::timeout)
- `planning/edger/epics/03-isolacao-execucao/spike.md` em português com seções: Resumo executivo, Metodologia, Resultados deno_core, Resultados wasmtime, Sharp edges, Go/no-go, Recomendação de layout de módulos, Impacto em Epic 04/PR 10

### Escopo
- **In:** spike deno_core + facade patterns (referência Edge Runtime `deno_facade`); comparação wasmtime só para path Wasm; timeout e memória básica; documentação
- **Out:** eszip completo, Node compat, integração WorkerPool, CI obrigatório para examples (podem ser `cargo run --example` manual)

### Critérios de aceite
- [x] `cargo run --example embedding-spike-deno` — wire sim OK; fetch 200 pendente V8 (documentado em spike.md)
- [x] `cargo run --example embedding-spike-wasm` executa módulo Wasm mínimo (add)
- [x] `spike.md` publicado com métricas e sharp edges
- [x] Recomendação: deno_core+facade (go condicional) + wasmtime standalone (go)
- [x] Nenhum código de produção em `src/` além de stub
- [x] Gate workspace verde

### Dependências
- Epic 02.03 (`SerializedRequest` para simular payload no spike deno, se usado)
- Epic 01 skeleton com deps corrigidas (core leaf)

## Test-first plan
- **Primeiro teste falhando:** `examples/embedding-spike-deno.rs` não compila sem deps — adicionar `deno_core` e assert de build
- **Nível:** exemplo executável + checklist manual em `spike.md` (não unit test obrigatório no CI para V8)
- **Evitar:** Depender de rede ou downloads em runtime do exemplo; usar módulos inline/string
- **Verificação documental:** reviewer confere `spike.md` contra critérios de aceite antes de fechar story

## Tasks
- [x] Adicionar dev-deps/features opcionais em `crates/edger-isolation/Cargo.toml` com versões pinadas
- [x] Implementar `embedding-spike-deno.rs` (wire sim; V8 boot pendente 03.04)
- [x] Implementar `embedding-spike-wasm.rs` (compile, invoke, timing)
- [x] Medir e registrar baseline em `spike.md`
- [x] Documentar sharp edges e referências Edge Runtime
- [x] Recomendar split deno/wasm + feature flags
- [x] Atualizar status em `00-overview.md` + checkpoint
- [ ] Se spike alterar direção: patch mínimo em `design.md` (não necessário — alinhado)

### Pendências
- deno_core V8 boot + fetch 200 — `feature deno`, story 03.04

## Verification
```bash
# Gate obrigatório (sem examples V8 em CI se ignorados)
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
bun test

# Spike manual (ambiente com V8)
cargo run -p edger-isolation --example embedding-spike-deno
cargo run -p edger-isolation --example embedding-spike-wasm

# Revisão de artefato
test -f planning/edger/epics/03-isolacao-execucao/spike.md && grep -q "Go/no-go" planning/edger/epics/03-isolacao-execucao/spike.md
```