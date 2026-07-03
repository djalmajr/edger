# Follow-up: AppShell como worker (composição via iframes)

**Origem:** discussão de arquitetura (2026-07-02), ao remover o `shell_gateway` no Epic 17.

## Ideia

Em vez de o edger ter shell routing embutido, o **AppShell vira um worker soberano**
montado na raiz (`base: "/"`). O ingress (ou o próprio roteamento nativo do edger)
manda `/` → worker appshell. Quando renderizado, o appshell controla os **iframes**
que embute (cada app é outro worker no seu path).

## Viabilidade: alta — o edger já suporta nativamente

- `router.rs` resolve `/` → `ResolvedRoute::HomepageFallback { worker }` via
  `index.homepage()`. Isso é **roteamento de worker montado na raiz**, separado do
  `shell_gateway` removido. **Não remover o HomepageFallback** — é o que faz o
  appshell-na-raiz funcionar sem mágica.
- Alternativa: o API Gateway externo reescreve `/` → `/appshell` (se quiser controlar no ingress).

## Modelo

- AppShell = worker normal (deployável, versionável, substituível). edger não sabe que é um "shell".
- Renderiza `<iframe src="/todos">`, `<iframe src="/dashboard">` — cada app é seu worker.
- Composição 100% no worker (client-side). edger = multiplexador burro. Multi-shell é trivial (workers diferentes).

## Benefícios

- Isolamento forte (iframe: contextos JS/CSS separados, deploy independente).
- **Same-origin**: todos os iframes no mesmo host do edger → cookies/sessão fluem; se houver
  auth no API Gateway na frente, uma vez logado todos os iframes herdam.
- Controle pelo appshell via `postMessage` (navegação, estado, resize, notificações) + deep-linking (history API → src do iframe).

## Tradeoffs (iframe)

- UX: sizing (postMessage de altura ou CSS), scroll aninhado, foco, back-button, deep-linking manual.
- SEO: conteúdo em iframe não indexa junto (irrelevante para cPanel/admin).
- Cross-origin exigiria CORS/postMessage; same-origin dispensa.
- Alternativas se incomodar: Web Components / module federation / server-side fragment stitching. Iframe é o mais isolado e simples — ótimo para shell operacional/cPanel.

## Relação com o cPanel

O cPanel atual já é praticamente um appshell (worker servindo UI). No modelo novo ele
absorve o papel de launcher (que o `shell_gateway` fazia): lista workers (`/api/admin/catalog`
ou endpoint próprio) e os iframeia.

## Ação (pós-Epic 17)

Transformar/definir um worker `appshell` (ou promover o cPanel) montado em `base: "/"`
que faz a composição via iframes; documentar o padrão no lugar do shell routing removido.
