# Story 24.02: Lista oficial completa

## Context

A referência oficial do Deno Deploy passou a explicitar nove frameworks. Após
a primeira entrega do Epic 24 ainda faltavam Fresh, Lume e Remix, além de uma
rodada consolidada que evitasse declarar suporte apenas por inferência.

## Files

| Path | Action | Reason |
|---|---|---|
| `crates/edger-core/src/{config,execution,worker_ref}.rs` | edit | Normalizar adapters e defaults |
| `crates/edger-isolation/src/{fullstack,multiproc}.rs` | edit | Base path, assets e processos |
| `crates/edger-isolation/src/deno/cli.rs` | edit | Preservar o caminho do build Fresh no fallback |
| `planning/edger/docs/` | edit | Receita e evidência canônicas |

## Detail

Fresh usa seu `server-entry.mjs` no local original: o cache de produção calcula
os caminhos de `_fresh/client` a partir de `import.meta.dirname`, portanto um
rebundle relocável quebra somente os assets. Lume é um fullstack declarativo de
saída estática e não cria isolate. Remix usa React Router framework mode, entry
server Web Streams e servidor Node autocontido no socket Unix privado.

### Acceptance criteria

- [x] `fresh`, `lume` e `remix` são adapters canônicos e validados.
- [x] Fresh entrega SSR, API e arquivos do island no subpath.
- [x] Lume entrega raiz, directory index, rota HTML limpa e assets.
- [x] Remix entrega SSR, loader/API e asset hidratável sem porta TCP privada.
- [x] Os nove frameworks oficiais são exercitados na mesma versão do runtime.
- [x] O status experimental de Remix e a natureza estática de Lume ficam explícitos.

## Tasks

- [x] Construir aplicações mínimas reais com as versões atuais dos frameworks.
- [x] Adicionar adapters, aliases, defaults e resolução de assets.
- [x] Preservar o diretório do entrypoint de produção Fresh.
- [x] Instalar Fresh, Lume e Remix por ZIP no Admin API.
- [x] Revalidar SSR/API/assets dos nove caminhos.
- [x] Atualizar Epic, roadmap e matrizes canônicas.

## Verification

```bash
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
SCRATCH=<dir> ./planning/edger/scripts/run-gates.sh
```

Validação live em `127.0.0.1:19080`: HTTP 200 nas superfícies previstas dos
nove apps, install HTTP 201 nos três novos pacotes e Browser sem erros de
console nos caminhos hidratáveis exercitados.

## Status

**completed** (2026-07-17).
