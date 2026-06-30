# Closure — Story 08.05 Shell e gateway

**Data:** 2026-06-29  
**Story:** `planning/edger/epics/08-valor-buntime/05-shell-gateway-e-experiencia-de-apps.md`  
**Epic:** `planning/edger/epics/08-valor-buntime/00-overview.md`

## Resultado

Story 08.05 concluída como shell/gateway v1. O edger agora reconhece um worker shell por `base: "/"`, roteia navegações de documento para esse shell, respeita `shellExcludes` para apps embutidos, preserva paths reservados e entrega CORS/preflight mínimo no `edger-ext-gateway`.

## Entregue

- `planning/edger/docs/shell-protocol.md` descreve o contrato v1 de shell, iframe, `x-base`, reserved paths e lacunas.
- `edger-core` parseia `shellExcludes` e preserva o campo em `WorkerConfig`.
- `ManifestIndex` registra worker shell/homepage quando `manifest.base == "/"`, sem transformar `/` em plugin wildcard.
- `edger-orchestrator/src/shell_gateway.rs` centraliza decisão pura de shell routing.
- `pipeline.rs` despacha o shell antes do routing normal quando a requisição é documento/root/single-segment e não está excluída.
- `edger-ext-gateway` responde preflight CORS e adiciona `Access-Control-Allow-Origin` em respostas.
- `workers/shell-demo` e `workers/todos-shell-demo` são fixtures de shell e app iframe.
- `planning/edger/docs/value-parity-matrix.md` marca shell/gateway como `partial` com evidência e lacunas explícitas.

## Drift de escopo

- Proxy externo não foi implementado no v1 para não introduzir SSRF sem allowlist e política de destino.
- Cache e rate-limit persistente continuam planejados; o gateway v1 cobre CORS/preflight mínimo.
- O protocolo avançado `z-frame`/`MessageChannel` fica como direção documentada; o fixture usa iframe HTML simples.
- `shellExcludes` v1 é estático em manifesto, não dinâmico via storage.

## Verificação

- `cargo test -p edger-core` — passou.
- `cargo test -p edger-ext-gateway` — passou; 5 testes.
- `cargo test -p edger-orchestrator shell_gateway` — passou; testes puros de shell routing.
- `cargo test -p edger-orchestrator --test shell_gateway` — passou; 5 testes E2E.
- `cargo test -p edger-orchestrator --test kind_dispatch_integration repository_js_examples_dispatch_through_deno_backend` — passou, confirmando que `workers/shell-demo` não quebrou exemplos existentes.
- `cargo test --workspace` — passou.
- `cargo clippy --workspace -- -D warnings` — passou.
- `cargo fmt -- --check` — passou.
- `SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh` — passou; 8 epics, 39 stories, 98 refs, 0 missing.
- Launch local: `ROOT_API_KEY=test-root PORT=19084 RUNTIME_WORKER_DIRS=workers cargo run -p edger-orchestrator --bin edger` — servidor subiu em `127.0.0.1:19084`.
- Evidência runtime: `planning/edger/status/evidence/story-08-05-runtime.txt`.
  - Navegação documento com auth em `/reports/list` retornou shell HTML com `<base href="/" />`, `edger shell demo` e iframe `/todos-shell-demo`.
  - Asset raiz `/shell.js` retornou `globalThis.edgerShellDemo = true;`.
  - Requisição iframe em `/todos-shell-demo/list` retornou `{"base":"/todos-shell-demo","path":"/list"}`.
  - Navegação documento sem auth em `/reports/list` retornou `401 Unauthorized`.
  - Caminho reservado `/api/admin/session` não foi interceptado pelo shell e retornou sessão admin.
  - Preflight `OPTIONS /hello-world` com auth retornou `204 No Content` e headers `Access-Control-Allow-*`.
- Browser embutido: navegação para `http://127.0.0.1:19084/reports/list` carregou o JSON `UNAUTHORIZED` sem auth, confirmando o gate. A superfície CDP disponível no Browser não aceitou `Network.setExtraHTTPHeaders` (`This method is not supported through raw CDP`), então a validação autenticada do shell ficou coberta por curl/testes automatizados, não pelo Browser.

## Riscos restantes

- Proxy externo deve exigir allowlist e testes de SSRF antes de existir.
- Excludes dinâmicos devem usar o modelo de providers/bindings da 08.06, não estado solto no gateway.
- Menu/catalog de apps ainda precisa de provider/registry explícito.

## Próximo

Executar 08.06 `planning/edger/epics/08-valor-buntime/06-modelo-de-extensoes-e-bindings.md`, consolidando providers, menus/catalog e lookup de serviços para que shell/gateway e state services não fiquem acoplados a detalhes de implementação.
