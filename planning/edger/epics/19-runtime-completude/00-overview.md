# Epic 19 — Completude do Runtime (features deferidas)

**Origin:** investigação de features deferidas (sessão de tech-debt pós Epic 17/18)

## Contexto

**Problema:** cinco features ficaram deferidas ao longo dos Epics 15–18, deixando
stubs `501`, enums meio-implementados e código vestigial que sugerem capacidades
inexistentes. O runtime funciona, mas carrega dívida que confunde autores de
workers e mantenedores.

**AS-IS:**
- `kind: fullstack` agora possui adapters declarativos `hono`, `sveltekit` e
  `tanstack`; o stub `501` foi removido na Story 19.05. TanStack Start usa
  `ssrEntrypoint` + `clientDir` no manifest, sem wrapper manual.
- `UdsTransport` (`edger-isolation/src/transport.rs`) é código morto — o UDS real é
  `multiproc.rs`.
- `StubBundler` (`edger-isolation/src/deno/bundle.rs`) não empacota dependências;
  multi-file workers pagam cold-start desnecessário.
- WASI host (`edger-isolation/src/wasm/`) tem ABI estática e nunca passa o request ao
  módulo wasm.
- Limite de body é global fixo (`MAX_BODY_BYTES` 4 MiB); override por-worker existe na
  config mas não é ligado ao caminho de execução (`edger-worker/src/pool.rs`).

**TO-BE:** as cinco features entregues de forma real e validada, sem stubs `501`
remanescentes e sem código vestigial.

**Fora de escopo:** adapter de Next.js (alto custo/churn) — só Hono/SvelteKit/TanStack;
OTLP export; Cloudflare Tunnel/CD.

## Story backlog

| # | Story | Objetivo | Tam. | Depende | Status |
|---|---|---|---|---|---|
| 01 | Remover UdsTransport vestigial | Deletar código morto e re-exports | S | — | pending |
| 02 | Per-worker body override | Ligar `max_body_bytes` da config ao execute path | S/M | — | pending |
| 03 | eszip bundler real | Substituir StubBundler por bundling real (deps) | M | — | pending |
| 04 | WASI host real | Passar request ao wasm, ABI real | M/L | — | pending |
| 05 | Fullstack adapter | `kind: fullstack, adapter: X` declarativo | L | 02 | completed |

## Roadmap

- **Fase 1 (paralela):** 01, 02, 03, 04 — arquivos disjuntos (isolation/transport,
  worker+core, isolation/deno, isolation/wasm). Worktrees isoladas.
- **Fase 2:** 05 (fullstack) após 02 fazer merge — ambos tocam `edger-worker/src/pool.rs`.
  Design read-only roda em paralelo à Fase 1.

Caminho crítico: 02 → 05.

## Critérios de aceite do epic

- [x] Nenhum stub `501` para `kind: fullstack` (ou variante removida com decisão registrada)
- [ ] `UdsTransport` removido; workspace compila sem re-exports órfãos
- [ ] Bundler produz artefato real para multi-file worker (teste)
- [ ] Módulo wasm recebe request e responde (teste ponta-a-ponta)
- [ ] Worker com `max_body_bytes` custom rejeita/aceita conforme limite próprio (teste)
- [x] `kind: fullstack, adapter: tanstack|hono|sveltekit` serve app sem wrapper manual
- [ ] `cargo fmt --check`, `clippy -D warnings`, `cargo test --workspace` verdes
- [ ] Validação viva fora do sandbox por feature (coordenador)

## Riscos

- **body-override / config normalization:** mesma área que causou 2 regressões no 18.B —
  rodar suite inteira após cada iteração.
- **eszip:** dependência pesada; se inviável, entregar o menor avanço real sobre o stub
  e registrar bloqueio (não deixar build quebrado).
- **fullstack:** churn de framework; manter adapters restritos a Hono/SvelteKit/TanStack.
