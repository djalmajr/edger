# Story 08.14: Autodiscovery de `index.html` sem manifesto

**Status:** completed (2026-06-29)

**Origin:** `planning/edger/epics/08-valor-buntime/00-overview.md`

## Context
- **Problema:** A matriz promete autodiscovery de `index.html`, `index.ts` e `index.js`, mas o loader atual só reconhece diretórios sem manifesto quando encontra `index.ts`, `index.js`, `index.mjs`, `index.wasm` ou `index.wat`.
- **Objetivo:** Fazer um app SPA mínimo com apenas `index.html` ser descoberto como worker `StaticSpa`, preservando a prioridade Buntime de entrypoint.
- **Valor:** Operadores conseguem migrar apps estáticos simples sem escrever `manifest.yaml` só para apontar o arquivo HTML padrão.
- **Restrições:** Não alterar semver routing, upload/install de worker, hot reload, package manager ou execução JS.

## Traceability
- **Source docs:** `planning/edger/docs/value-parity-matrix.md`, `planning/edger/docs/compat-matrix.md`, `planning/edger/epics/07-avancado/01-full-manifests-kinds.md`
- **Buntime refs:** `wiki/apps/runtime.md` em `workspace: zommehq`, `project: buntime`, seção Entrypoint Detection: `manifest.yaml` antes de autodiscovery `index.html` -> `index.ts` -> `index.js` -> `index.mjs`.
- **Prototype refs:** none; this is runtime loader behavior.
- **Business rules:** Paridade é por comportamento observável, não por copiar o loader Buntime.

## Files

| Path | Action | Reason |
|---|---|---|
| `edger-orchestrator/src/manifest_loader.rs` | edit | Incluir `index.html` na lista de entrypoints inferidos e na detecção de worker dir |
| `edger-orchestrator/tests/manifest_loader.rs` | edit | Provar autodiscovery de SPA sem manifesto e prioridade sobre JS |
| `planning/edger/docs/value-parity-matrix.md` | edit | Atualizar linha de manifests/entrypoint detection |
| `planning/edger/docs/compat-matrix.md` | edit | Registrar compatibilidade técnica do `index.html` manifest-less |
| `planning/edger/epics/08-valor-buntime/00-overview.md` | edit | Adicionar 08.14 no backlog e status |
| `planning/edger/status/evidence/story-08-14-runtime.txt` | create | Capturar comandos e resultados |

## Detail

### AS-IS
- `ENTRYPOINT_CANDIDATES` não contém `index.html`.
- Um diretório com apenas `index.html` não é tratado como worker dir.
- Um diretório com `index.html` e `index.ts`, sem manifesto ou package metadata, infere `index.ts` porque HTML nem entra na lista.

### TO-BE
- `index.html` é o primeiro candidato de autodiscovery.
- Um diretório sem manifesto/package com apenas `index.html` entra no `ManifestIndex`.
- Quando `index.html` e `index.ts/js/mjs` coexistem sem manifesto, o loader escolhe `index.html` e `ExecutionKind::StaticSpa`.
- Manifesto explícito e `package.json` continuam com a precedência atual do edger.

### Scope
- **In:** autodiscovery local no loader, testes de contrato e atualização de matrizes.
- **Out:** semver ranges, upload/install, refresh incremental, watch mode, package manager, Browser manual.

### Approach
- Inserir `index.html` como primeiro item de `ENTRYPOINT_CANDIDATES`.
- Adicionar teste que cria um diretório com `index.html` + `index.ts` sem manifesto e valida `StaticSpa`.
- Adicionar teste direto para caminho passado como worker dir com apenas `index.html`.

### Risks
- **Mudança de prioridade:** Se um app tinha `index.html` e `index.ts` sem manifesto, passará a ser tratado como SPA. Isso alinha com Buntime; se quiser JS, deve declarar manifesto/package.
- **Overclaiming:** A story fecha autodiscovery de HTML, não semver ranges nem loader dinâmico.

### Acceptance criteria
- [x] `load_manifests_from_dirs` descobre um diretório com apenas `index.html`.
- [x] `index.html` vence `index.ts` em autodiscovery sem manifesto.
- [x] O worker inferido usa `ExecutionKind::StaticSpa`.
- [x] Matriz de valor registra `Worker manifests e entrypoint detection` como testado para autodiscovery HTML/JS.
- [x] Gates Rust e planejamento passam.

## Test-first plan
- **Behavior:** app sem manifesto com `index.html` é roteável como `StaticSpa`.
- **First failing test:** criar tempdir `landing/index.html` + `landing/index.ts`; loader deve escolher `index.html` e `ExecutionKind::StaticSpa`.
- **Preferred level:** integração em `edger-orchestrator/tests/manifest_loader.rs`, porque o contrato é filesystem -> `ManifestIndex`.
- **Mutation captured:** remover `index.html` da lista de candidatos ou colocá-lo depois de `index.ts` deve quebrar o teste.
- **Avoid:** teste unitário privado de `infer_entrypoint`; a prova precisa passar pelo loader público.

## Tasks
- [x] Fase 1 — Testes de autodiscovery HTML.
  - Done when: `manifest_loader.rs` cobre `index.html` sem manifesto e prioridade sobre JS.
- [x] Fase 2 — Implementação do loader.
  - Done when: `ENTRYPOINT_CANDIDATES` inclui `index.html` em primeiro lugar.
- [x] Fase 3 — Atualizar artefatos de valor.
  - Done when: overview, matriz de valor, compat matrix, README/roadmap/evidência refletem 08.14.
- [x] Fase 4 — Rodar gates.
  - Done when: teste focado, Rust gate completo e planning gate passam.

## Verification
```bash
cargo test -p edger-orchestrator --test manifest_loader
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh
```
