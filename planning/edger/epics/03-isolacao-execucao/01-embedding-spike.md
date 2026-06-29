# Story 03.01: Spike de embedding (deno_core + facade; comparação wasmtime)

**Origin:** `planning/edger/epics/03-isolacao-execucao/00-overview.md`

## Context
- **Problema:** Não há evidência empírica sobre viabilidade, latência de cold start, accounting de memória ou superfície de ops para embedding JS/TS (deno_core) nem para Wasm standalone (wasmtime + WASI).
- **Objetivo:** Spike time-boxed que valida hello-world fetch, timeout básico, medição de spawn/exec e comparação breve wasmtime; produzir documento `spike.md` com go/no-go.
- **Valor:** Mitiga risco #1 do design (embedding maintenance); desbloqueia decisão de split de módulos (`deno` facade vs `wasm`) para stories 03.04 e PR 10 futuro.
- **Restrições:** Time-box (sugestão: 2–3 dias); código de spike em `examples/` ou módulo temporário; **não** mergear embedding de produção nesta story.

## Traceability
- **Source docs:** `planning/edger/design.md` (Embedding Spike Recommendation, PR 2, Resolved Decisions JS/TS + Wasm), `planning/edger/roadmap.md` (Fase 3)
- **Design PRs:** PR 2 (spike), informa PR 5 e PR 10
- **Depends on:** Epic 02 story 02.03 (wire types mínimos para roundtrip de request simulado); Epic 01 (skeleton alinhado)

## Files

| Path | Ação | Motivo |
|---|---|---|
| `crates/edger-isolation/Cargo.toml` | alterar | `[dev-dependencies]` ou features opcionais: `deno_core`, `deno_runtime` (mínimo), `wasmtime`, `wasmtime-wasi` |
| `crates/edger-isolation/examples/embedding-spike-deno.rs` | criar | Boot V8, módulo fetch trivial, roundtrip request |
| `crates/edger-isolation/examples/embedding-spike-wasm.rs` | criar | Load Wasm mínimo + WASI smoke |
| `planning/edger/epics/03-isolacao-execucao/spike.md` | criar | Resultados, métricas, sharp edges, recomendação de módulos |
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
- `spike.md` em português com seções: Resumo executivo, Metodologia, Resultados deno_core, Resultados wasmtime, Sharp edges, Go/no-go, Recomendação de layout de crates/módulos, Impacto em Epic 04/PR 10

### Escopo
- **In:** spike deno_core + facade patterns (referência Edge Runtime `deno_facade`); comparação wasmtime só para path Wasm; timeout e memória básica; documentação
- **Out:** eszip completo, Node compat, integração WorkerPool, CI obrigatório para examples (podem ser `cargo run --example` manual)

### Critérios de aceite
- [ ] `cargo run --example embedding-spike-deno` completa roundtrip fetch com resposta 200 (ambiente com toolchain V8 ok)
- [ ] `cargo run --example embedding-spike-wasm` executa módulo Wasm mínimo via WASI
- [ ] `spike.md` publicado com métricas (spawn_ms, exec_ms) e lista de sharp edges (V8 platform, op registration, async ops, versões pinadas)
- [ ] Recomendação explícita: manter deno_core+facade para JS/TS e wasmtime standalone para Wasm (ou justificar desvio)
- [ ] Nenhum código de produção em `src/` além de re-exports opcionais do spike
- [ ] Gate workspace continua verde (examples podem ser `#[ignore]` em CI se V8 indisponível — documentar)

### Dependências
- Epic 02.03 (`SerializedRequest` para simular payload no spike deno, se usado)
- Epic 01 skeleton com deps corrigidas (core leaf)

## Test-first plan
- **Primeiro teste falhando:** `examples/embedding-spike-deno.rs` não compila sem deps — adicionar `deno_core` e assert de build
- **Nível:** exemplo executável + checklist manual em `spike.md` (não unit test obrigatório no CI para V8)
- **Evitar:** Depender de rede ou downloads em runtime do exemplo; usar módulos inline/string
- **Verificação documental:** reviewer confere `spike.md` contra critérios de aceite antes de fechar story

## Tasks
- [ ] Adicionar dev-deps/features opcionais em `edger-isolation/Cargo.toml` com versões pinadas
- [ ] Implementar `embedding-spike-deno.rs` (boot, load module, fetch roundtrip, timing)
- [ ] Implementar `embedding-spike-wasm.rs` (compile, WASI, invoke, timing)
- [ ] Medir e registrar baseline spawn/exec em `spike.md`
- [ ] Documentar sharp edges e referências Edge Runtime (`deno_facade`, `cpu_timer`, `base_mem_check`)
- [ ] Recomendar split: `src/deno/mod.rs`, `src/wasm/mod.rs`, feature flags `deno`, `wasm`
- [ ] Atualizar status em `00-overview.md`
- [ ] Se spike alterar direção: patch mínimo em `design.md` Risks/Rollout

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