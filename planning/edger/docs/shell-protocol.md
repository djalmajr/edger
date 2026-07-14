# Shell / micro-frontend protocol

> OBSOLETO desde o Epic 17: shell routing, `shellExcludes` e gateway interno
> foram removidos do runtime. Composição/AppShell e políticas de borda ficam em
> serviços externos ou nos próprios workers. Este arquivo fica como registro
> histórico da arquitetura pré-Epic 17. Ver
> `planning/edger/epics/17-edger-minimalista/`.

**Status:** histórico/obsoleto (removido pelo Epic 17)
**Origin:** `planning/edger/epics/08-valor-buntime/05-shell-gateway-e-experiencia-de-apps.md`, `planning/edger/epics/07-avancado/02-shell-routing.md`

## Objetivo

O edger precisa entregar o mesmo valor observável do app-shell do Buntime sem
copiar o CPanel ou o protocolo legado. O contrato v1 é: um worker shell serve
document navigations e embute apps isolados por iframe; apps montados continuam
workers normais e recebem seu próprio `x-base`.

## Configuração

Um worker é tratado como shell quando o manifesto declara:

```yaml
name: shell-demo
entrypoint: index.html
base: "/"
visibility: protected
shellExcludes:
  - todos-shell-demo
  - platform
```

Campos:

| Campo | Descrição |
|---|---|
| `base: "/"` | Marca o worker como shell central. |
| `shellExcludes` | Basenames que bypassam o shell e abrem diretamente como app/iframe. |
| `injectBase` | Continua controlando injeção de `<base href>`. Para shell, o base efetivo é `/`. |

`shellExcludes` aceita apenas basenames estáticos no manifesto v1. Excludes
dinâmicos persistidos em storage ficam para evolução de gateway/providers.

## Decisão de roteamento

O shell é candidato quando existe um worker com `base: "/"` e o path não é
reservado (`/api`, `/health`, `/ready`, `/.well-known`). A decisão v1 segue as
mesmas intenções operacionais do Buntime:

| Requisição | Resultado |
|---|---|
| `GET /todos-shell-demo` com `Sec-Fetch-Dest: document` | Serve o shell, salvo se `todos-shell-demo` estiver em `shellExcludes`. |
| `GET /todos-shell-demo` com `Sec-Fetch-Dest: iframe` | Bypassa o shell e serve o worker `todos-shell-demo`. |
| `GET /chunk.js` sem iframe | Pode ser servido pelo shell para assets single-segment. |
| `GET /assets/main.js` | Não é roteado ao shell por ser multi-segment asset. |
| `GET /api/admin/session` | Nunca é roteado ao shell. |

Basename excluído sempre bypassa o shell. Isso evita shell-in-shell loop quando
o shell embute apps em iframes.

## Headers e base path

| Destino | `x-base` | `base_href` |
|---|---|---|
| Shell | `/` | `/` |
| App montado em `/todos-shell-demo` | `/todos-shell-demo` | `/todos-shell-demo/` |
| Worker namespaced `/@team/app` | `/@team/app` | `/@team/app/` |

O runtime injeta `<base href>` em Static SPA quando `injectBase` está ativo. O
shell deve usar URL/path atual para decidir qual iframe renderizar.

## Protocolo iframe v1

O shell demo usa iframe HTML padrão:

```html
<iframe src="/todos-shell-demo" title="todos-shell-demo"></iframe>
```

O protocolo avançado de mensagens (`z-frame`, `MessageChannel`, props, eventos
e RPC) é compatível como direção arquitetural, mas não é requisito de execução
do v1. A fronteira de segurança principal é o isolamento do iframe e a
autorização normal do worker montado.

## Evolução planejada

- **Compat z-frame/MessageChannel (legado Buntime):** apps que dependem do
  protocolo de mensagens legado continuam funcionando dentro do iframe sem
  intervenção do runtime; o edger não intercepta `postMessage`.
- **WebTransport:** direção para o protocolo shell<->app evoluído (streams
  bidirecionais para eventos/RPC no lugar de `MessageChannel`). Não faz parte
  do v1; requer decisão de contrato próprio (autenticação por sessão de
  transporte e multiplexação por app montado) antes de implementação.
- **`base_href` para handlers dinâmicos:** o orchestrator propaga
  `SerializedRequest.base_href` (equivalente ao `X-Base` Buntime) em todo
  dispatch, então workers fetch/routes podem gerar URLs absolutas sem ler
  headers.

## Gateway v1

O gateway middleware cobre CORS/preflight mínimo:

- `OPTIONS` com `Origin` retorna `204`.
- Respostas normais recebem `Access-Control-Allow-Origin`.
- `credentials: true` com `*` não é permitido quando a configuração evoluir
  para múltiplas opções.

O gateway também cobre proxy loopback explícito, cache durável opcional e
rate-limit persistente opcional por `DurableSqlProvider`. Proxy externo amplo
continua fora do v1 e exigirá allowlist explícita para não introduzir SSRF.

## Evidência

- `crates/edger-orchestrator/tests/shell_gateway.rs`
- `crates/edger-orchestrator/tests/shell_routing_test.rs` (SPA namespaced com
  `<base href="/@team/panel/">`, asset relativo pela mesma rota e
  `injectBase: false` servindo HTML intocado)
- `edger-ext-gateway/tests/gateway_middleware.rs`
- `workers/shell-demo`
- `workers/todos-shell-demo`
