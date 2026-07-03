# Story 19.E: Fullstack adapter

**Origin:** planning/edger/epics/19-runtime-completude/00-overview.md

## Context

- **Problema:** antes desta story, `kind: fullstack` existia no contrato, mas retornava stub `501`; apps rodavam apenas via `kind: fetch` com wrapper manual.
- **Objetivo:** tornar `kind: fullstack` declarativo com `adapter: hono|sveltekit|tanstack`.
- **Valor:** remove o stub e entrega um caminho oficial para apps fullstack suportados.
- **Dependência:** Story 19.B, porque esta story também toca o caminho de execução em `edger-worker/src/pool.rs`.

## Files

| Path | Action | Reason |
|---|---|---|
| `edger-core/src/execution.rs` | edit | Representar `kind: fullstack` e adapters suportados sem stub enganoso |
| `edger-worker/src/pool.rs` | edit | Remover resposta 501 e despachar para o adapter declarado |
| `workers/` | inspect/edit | Usar exemplos existentes como prova, sem criar wrapper manual obrigatório |
| `edger-worker/tests/` | edit | Cobrir dispatch por adapter suportado |

## Detail

### Critérios de aceite
- [x] Não há stub `501` para `kind: fullstack`.
- [x] Manifesto aceita `adapter: hono`, `adapter: sveltekit` ou `adapter: tanstack`.
- [x] Adapter desconhecido falha com erro claro e status estável.
- [x] Um app suportado serve sem wrapper manual em `kind: fullstack`.
- [x] O limite de body efetivo da Story 19.B continua aplicado.

## Tasks

- [x] Confirmar o contrato atual de `kind` e `adapter`.
- [x] Modelar adapters suportados no contrato de execução.
- [x] Remover o caminho que retorna 501.
- [x] Ligar dispatch fullstack aos adapters declarados.
- [x] Adicionar testes para adapter suportado e adapter inválido.

## Verification

```bash
cargo test -p edger-core execution
cargo test -p edger-worker
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
```

## Status

**completed** (2026-07-03) — `kind: fullstack` agora exige
`adapter: hono|sveltekit|tanstack` e usa `ssrEntrypoint` (com `entrypoint`
como alias) para delegar ao backend fetch existente. TanStack Start serve
`clientDir` estaticamente via Rust, restaura `x-base` antes do SSR e o fixture
`workers/tanstack-demo` não depende mais de wrapper manual.
