# Follow-up: enforcement de casca (AppShell) para apps front-end

**Origem:** discussão de arquitetura (2026-07-02).

## Requisito

Apps de front-end (que **não podem ser modificados** para embutir a casca) devem
sempre renderizar **dentro** do AppShell; ninguém deveria acessar o app "pelado"
pela URL. AppShell é **opcional** (se não configurado, apps renderizam direto).

## Verdade sobre enforcement (limite físico)

Casca client-side = chrome ao redor de conteúdo servido independentemente:

- O iframe **precisa** carregar `/app` via HTTP real → o app é inerentemente carregável.
- Distinguir iframe-legítimo de acesso-direto só via headers (`Sec-Fetch-Dest`,
  `Referer`, `Sec-Fetch-Site`) → **browser honra, não-browser forja** → é **guardrail
  de UX, não fronteira de segurança**.
- Mesmo com isolamento de rede, uma vez que o browser recebeu o HTML/JS (iframe legítimo),
  o conteúdo vazou — não dá para obrigar o browser a manter a casca.

**Conclusão:** "nunca renderizar sem a casca" não é perfeitamente forçável sem tocar no
app. Melhor viável:
1. **Isolar por rede** os workers de front-end (só o AppShell os alcança; nunca públicos).
2. **Redirect top-level → shell** (`Sec-Fetch-Dest: document` → shell; `iframe`/asset → app).

Enforcement forte só se a casca injetar algo que o app **precise** (token/config) — mas
isso acopla o app (colide com "não posso modificar os apps").

## Decisão: NÃO colocar no edger

Interceptar rotas de front-end / forçar casca é **composição de front-end = opinião** que
o Epic 17 está removendo do runtime. Colocar no edger (mesmo atrás de flag) reabre o
`shell_gateway`. Manter o edger burro.

## Arquitetura-alvo: AppShell service (reverse proxy) na frente do edger

```
Ingress ─► API GW ─┬─ front-end ─► AppShell service (proxy + casca) ─► edger (app workers internos/isolados)
                   └─ API/backend ───────────────────────────────────► edger (workers) direto, sem casca
```

- AppShell service (same-origin, reverse proxy): top-level → HTML da casca (com iframes);
  iframe/asset → proxy pro edger. Dono do registry de apps, política e chrome.
- Workers de front-end **internos** (network policy) → "não acessa direto" real (nível rede).
- edger não conhece "casca"/"frontend" — só serve workers. Zero regra nova no runtime.
- Multi-shell/por-tenant trivial (instâncias/config do serviço).

## Stopgap (se não quiser operar outro serviço já)

Flag `frontend: true` por worker + shell configurado + redirect top-level→shell no edger
(~50 linhas). É re-adicionar a opinião do `shell_gateway` (opt-in). Tratar como muleta;
alvo é o serviço separado.

## Relacionado
- [appshell-iframe-composition](appshell-iframe-composition.md)
- [fallback-worker-routing](fallback-worker-routing.md)
