# Story 08.05: Shell, gateway e experiência de apps

**Origin:** `planning/edger/epics/08-valor-buntime/00-overview.md`

## Context
- **Problema:** Buntime entrega uma experiência composta por shell, apps montados, gateway/proxy, base path e navegação. edger precisa provar esse valor sem copiar detalhes de UI ou protocolo legado.
- **Objetivo:** Definir e implementar o contrato edger para composição de apps, roteamento shell/gateway e proxy controlado.
- **Valor:** Apps migrados não são apenas endpoints isolados; eles aparecem como experiência navegável e composta.
- **Restrições:** UI final não é escopo desta story; o shell deve ser compatível por valor, com protocolo documentado e testável.

## Status
completed (2026-06-29) — shell routing Rust-native por worker com `base: "/"`, `shellExcludes` no manifesto, bypass para iframes/assets/API/reserved paths, `x-base: /` para o shell, app iframe recebendo seu próprio base path e gateway middleware com CORS/preflight mínimo. Proxy externo/cache/rate-limit persistente ficam documentados como evolução para evitar SSRF e abstração prematura.

## Traceability
- **Source docs:** `planning/edger/docs/shell-protocol.md`, `planning/edger/epics/07-avancado/02-shell-routing.md`, `planning/edger/docs/value-parity-matrix.md`
- **Buntime refs:** runtime docs, manifests de apps `shell`, `todos`, `platform` e plugin gateway em `/Users/djalmajr/Developer/djalmajr/buntime/`
- **Prototype refs:** browser validation pode usar `http://127.0.0.1:<port>/todos` e shell local quando disponível.
- **Business rules:** gateway não pode bypassar auth/namespace.

## Files

| Path | Action | Reason |
|---|---|---|
| `planning/edger/docs/shell-protocol.md` | edit | Contrato de composição e navegação |
| `edger-core/src/manifest.rs` | edit | Declarar `shellExcludes` no manifesto puro |
| `edger-core/src/config.rs` | edit | Preservar excludes no `WorkerConfig` |
| `edger-core/tests/models_mapping.rs` | edit | Cobrir parsing de `shellExcludes` |
| `edger-orchestrator/src/shell_gateway.rs` | create | Decisão pura de shell routing |
| `edger-orchestrator/src/manifest_index_stub.rs` | edit | Identificar worker shell por `base: "/"` |
| `edger-orchestrator/src/lib.rs` | edit | Exportar shell gateway |
| `edger-ext-gateway/src/lib.rs` | edit | Regras gateway/proxy como extensão |
| `edger-orchestrator/src/pipeline.rs` | edit | Roteamento shell/gateway antes de dispatch final |
| `edger-orchestrator/tests/shell_gateway.rs` | create | E2E de shell, base injection, proxy e auth |
| `edger-ext-gateway/tests/gateway_middleware.rs` | edit | Provar CORS/preflight mínimo |
| `workers/shell-demo/manifest.yaml` | create | Fixture de shell |
| `workers/shell-demo/index.html` | create | Fixture SPA/shell mínima |
| `workers/todos-shell-demo/manifest.yaml` | create | Fixture de app montado |
| `workers/todos-shell-demo/index.html` | create | Fixture iframe/app mínima |
| `docs/developers/06-operacao-e-testes.adoc` | edit | Documentar operação shell/gateway v1 |
| `planning/edger/docs/value-parity-matrix.md` | edit | Evidência para shell/gateway |

## Detail

### AS-IS
- Epic 07 prevê shell routing e `inject_base`.
- Static SPA e path semantics já foram validados parcialmente com `todos`.
- Gateway existe como extensão inicial, mas ainda não cobre valor composto de shell/proxy.

### TO-BE
- Shell protocol define app catalog mínimo, base path, navegação por iframe e headers internos.
- Worker com `base: "/"` vira shell configurado. `shellExcludes` decide quais basenames bypassam o shell para abrir o app direto no iframe.
- Gateway extension aplica CORS/preflight mínimo como primeira regra de borda; proxy externo fica fora do v1 por risco de SSRF sem política de allowlist.
- Teste E2E prova shell-hosted navigation, app montado via iframe, base path correto e reserved paths sem bypass.
- Browser validation cobre fluxo visual mínimo quando o servidor local estiver rodando.

### Approach

| Decisão story-time | Escolha | Motivo |
|---|---|---|
| Configuração do shell | `manifest.base: "/"` no worker shell | Reaproveita campo já compatível com Buntime e evita env global invisível |
| Bypass de apps montados | `shellExcludes` como lista de basenames no manifesto do shell | Equivale ao valor de `GATEWAY_SHELL_EXCLUDES` sem persistência dinâmica prematura |
| Detecção de navegação | `Sec-Fetch-Dest: document` ou root/single-segment fora de iframe | Preserva o comportamento útil do Buntime para documentos e assets root-level |
| Base path | Shell recebe `x-base: /`; apps excluídos recebem `x-base: /app` | Mantém SPAs e iframes independentes |
| Gateway v1 | CORS/preflight mínimo no middleware | Entrega política de borda testável antes de proxy/cache/rate-limit |
| Proxy externo | Documentar como out-of-scope v1 | Evita SSRF e semântica insegura sem allowlist/config persistente |

### Risks
- `shellExcludes` v1 é estático no manifesto; excludes dinâmicos via storage ficam para uma evolução de gateway/providers.
- O shell demo usa iframe simples; protocolo `z-frame`/MessageChannel fica documentado, não reimplementado nesta fatia.
- CORS v1 cobre preflight e headers básicos; rate-limit/cache/proxy externo seguem planejados na matriz.

### Scope
- **In:** protocolo, fixtures, roteamento shell, base injection, bypass de app montado, CORS/preflight mínimo, testes.
- **Out:** design final de CPanel, microfrontend framework específico, marketplace UI, proxy externo, cache, rate-limit persistente, excludes dinâmicos.

### Acceptance criteria
- [x] Shell demo serve HTML e assets sob base path correto.
- [x] App montado recebe `x-base` ou contrato edger equivalente.
- [x] Gateway shell respeita auth e namespace do worker shell.
- [x] Teste impede bypass de paths reservados por regra gateway.
- [x] Matriz de valor marca shell/gateway com evidência.

### Dependencies
- Story 08.01 para matriz.
- Epic 07.02 para shell routing foundation.
- Story 08.03 para segurança, se houver mutação ou proxy protegido.

## Tasks
- [x] Fase 1 — Detalhar o contrato shell/gateway.
  - Done when: story e `planning/edger/docs/shell-protocol.md` descreverem `base: "/"`, `shellExcludes`, headers e lacunas v1.
- [x] Fase 2 — Modelar configuração shell no core/index.
  - Done when: `shellExcludes` for parseado no manifesto, preservado no config e o index identificar shell por `base: "/"`.
- [x] Fase 3 — Implementar shell routing no orchestrator.
  - Done when: document/root navigation servir o shell, iframe/app excluído bypassar para o worker real e reserved paths permanecerem reservados.
- [x] Fase 4 — Expandir gateway middleware com CORS/preflight mínimo.
  - Done when: `OPTIONS` com `Origin` retornar `204` com CORS e respostas normais receberem headers permitidos.
- [x] Fase 5 — Criar fixtures e testes E2E.
  - Done when: `shell_gateway.rs` provar shell HTML/asset, app iframe com `x-base`, auth do shell protegido e reserved path não interceptado.
- [x] Fase 6 — Atualizar matriz, docs e closure.
  - Done when: docs operacionais e matriz registrarem `partial/tested` com proxy/cache/rate-limit fora do v1.

## Verification
```bash
cargo test -p edger-orchestrator --test shell_gateway
curl -H 'sec-fetch-dest: document' -H 'authorization: Bearer test-root' http://127.0.0.1:19084/todos-shell-demo
ROOT_API_KEY=test-root PORT=19084 RUNTIME_WORKER_DIRS=workers cargo run -p edger-orchestrator --bin edger
curl -H 'authorization: Bearer test-root' http://127.0.0.1:19084/shell-demo
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh
```
