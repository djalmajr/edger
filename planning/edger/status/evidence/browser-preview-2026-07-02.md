# Evidência — validação no browser/preview builtin (2026-07-02)

**Launch:** `.claude/launch.json` → `env ROOT_API_KEY=test-root PORT=19080
RUNTIME_WORKER_DIRS=workers:workers/value-parity cargo run -p
edger-orchestrator --bin edger` (multi-root exercitado de propósito).

## APIs (fetch no browser, Bearer test-root)

| Chamada | Resultado |
|---|---|
| `POST /hello-world` `{"name":"Browser"}` | 200 `{"message":"Hello Browser from foo!"}` |
| `GET /routes-demo/users/42` | 200 `{"user":"42"}` (rota `:param`) |
| `POST /routes-demo/admin` | 405 (method map) |
| `GET /wasm-hello` | 200 `wasm-hello` (wasmtime) |
| `POST /read-body` (8 bytes) | 200 `{"totalSize":8}` |

## SPA (navegação document real)

- `GET /todos` (public, descoberto via multi-root `workers/value-parity`):
  200, `<base href="/todos/">` injetado, `styles.css`/`app.js` resolvem por
  path relativo sob `/todos/`, JS executa (`valueParityTodosReady === true`),
  deep link `/todos/some/deep/route` devolve o index (fallback SPA).
  Screenshot renderizado com estilos aplicados.
- `GET /cpanel` (public): admin UI carrega, conexão com root key pelo form
  dispara o admin API real — Overview mostra 21 workers / 4 módulos /
  principal `root · admin` / namespaces `*` / gateway history `local`; a view
  Workers lista o inventário completo incluindo `routes-demo` (RoutesTable) e
  `fullstack-demo` (Fullstack, public).

## Fullstack (contrato v1)

- `GET /fullstack-demo` (public, sem auth): **501** `fullstack not
  implemented`, header `x-adapter: default` — contrato adapter-required
  observável no browser. Fixture nova `workers/fullstack-demo/`.

## Shell

- `GET /todos-shell-demo` com Bearer: 200 `{"base":"/todos-shell-demo","path":"/"}`
  (app montado, excluded do shell → dispatch direto).
- `GET /todos-shell-demo` sem key: 401 (shell/worker protegidos — contrato).

## Achado corrigido durante a validação

Shell interception vs workers públicos: com `shell-demo` (base `/`,
protegido) registrado, GET single-segment **não listado** em `shellExcludes`
era tratado como candidato a shell e exigia auth **antes** de resolver o
worker público de destino (`/todos` e `/fullstack-demo` retornavam 401 no
browser). Corrigido na fixture adicionando os novos workers a
`workers/shell-demo/manifest.yaml::shellExcludes`. Comportamento do runtime é
o desenhado (shell decide antes do routing), mas fica registrado que **um
shell protegido oculta workers públicos não excluídos** — candidato a
refinamento futuro (ex.: fallback para worker público quando o shell nega).

## cPanel redesign (adendo 2026-07-02)

`workers/cpanel` reescrito para espelhar o cPanel do Buntime, ainda Static SPA
sem build (Preact + htm + Tailwind v4 + catálogo shadcn em `components/ui/`).
Validado no preview:

- **Login gated**: `/cpanel` mostra só o form de login; ~40 módulos JS carregam
  sem erro; o painel (sidebar + views) só aparece após `Connect` com root key.
- **Widgets só no Overview**: cards de métrica movidos do topo global para
  dentro da view Overview (feedback da anotação).
- **shadcn real**: `Select` com chevron, `Table`, `Card`, `Badge`, `Sidebar`,
  `Alert`, `Button` do catálogo. Fluxo de keys real: criar `ci-deploy`
  (role via `Select`) → `rawKey` mostrado uma vez → tabela com `active` +
  revoke. Console e network sem erros; key de validação revogada ao final.

### Correções de runtime exigidas pela UI

1. **Static SPA persistente** — `edger-core::parse_worker_config` dá TTL default
   (`STATIC_SPA_DEFAULT_TTL_MS = 300_000`) a `StaticSpa`; antes era efêmero
   (ttl=0), o que sob ~40 imports concorrentes do mesmo worker quebrava.
2. **Pool re-resolve sob concorrência** — `WorkerPool::fetch_worker_inner`
   re-resolve uma instância nova quando a instância efêmera compartilhada é
   terminada por um dispatch concorrente enquanto o request esperava o
   dispatch lock (antes: `worker not ready for dispatch`). Regressão:
   `edger-worker/tests/pool_ephemeral_concurrency.rs` (mutação provada).

## Gates pós-validação

`cargo test -p edger-orchestrator` completo verde com os novos workers no
root `workers/` (nenhuma contagem/asserção quebrada).
