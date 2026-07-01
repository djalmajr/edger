# Shell / micro-frontend protocol

**Status:** v1 em implementação pela Story 08.05
**Origin:** `planning/edger/epics/08-valor-buntime/05-shell-gateway-e-experiencia-de-apps.md`

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

- `edger-orchestrator/tests/shell_gateway.rs`
- `edger-ext-gateway/tests/gateway_middleware.rs`
- `workers/shell-demo`
- `workers/todos-shell-demo`
