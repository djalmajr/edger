# Follow-up: worker fallback (catch-all de roteamento)

**Origem:** discussão de arquitetura (2026-07-02), como substituto explícito do `shell_gateway` removido no Epic 17.

## Ideia

Quando um request não casa com nenhum worker, o edger pode despachar para um
**worker fallback** designado (ex.: o appshell) em vez de 404. É a generalização
honesta do `HomepageFallback` (`/` → homepage) para catch-all — **sem opinião**
(nada de sniff de `sec-fetch-dest` nem heurística de path, que era o `shell_gateway`).

## Duas formas

1. **Ingress faz (funciona hoje, sem mexer no edger):** `defaultBackend`/catch-all do
   ingress (nginx `defaultBackend`, Traefik router `PathPrefix('/')` de baixa prioridade,
   APISIX/Kong rota catch-all) aponta para o edger reescrevendo para `/appshell` e passando
   o path original num header (`X-Original-URI`).
2. **edger faz (recomendado, minimalista):** config explícita "unmatched → worker fallback".
   Ingress não precisa de regra; edger é auto-suficiente; uma fonte de verdade de roteamento.

## Detalhe de implementação (Forma 2)

- Diferente do `HomepageFallback` (que reescreve para `/`), o catch-all deve **preservar o
  path original** ao despachar (unmatched `/foo/bar` → worker fallback recebe `/foo/bar`),
  para o worker decidir (render shell, 404 bonito, redirect, proxy...).
- Config: preferir flag no manifest (`fallback: true` num worker) a env — fica versionado
  com o worker, sem env-mágica. (`EDGER_FALLBACK_WORKER=appshell` como alternativa.)
- Router: em `resolve_route`, quando nada casa e há um worker fallback configurado, resolver
  para `ResolvedRoute::Worker { worker: fallback, rewritten_path: <path original> }` em vez
  de `NOT_FOUND`.

## Relação

- Substitui o `shell_gateway` (removido) de forma explícita/sem-opinião.
- Combina com [appshell-iframe-composition](appshell-iframe-composition.md): o worker
  fallback costuma ser o appshell (que então compõe via iframes).

## Ação (pós-Epic 17)

Implementar o worker fallback configurável (flag no manifest) no `router.rs`/`pipeline.rs`,
preservando o path original; documentar como o padrão de catch-all no lugar do shell routing.
