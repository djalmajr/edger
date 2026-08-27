# ADR 0006 — MCP em dois transportes com vocabulário no `edger-core`

- **Status:** Aceito
- **Data:** 2026-08-27

## Contexto

O control plane precisa falar MCP por HTTP em `/api/mcp`, o mesmo protocolo
que o `edger-mcp` fala por stdio. A dependência entre os crates é inversa ao
que o endpoint exige: `edger-mcp` depende do `edger-orchestrator` para o
discovery local, e importar de lá os descritores de tool fecharia um ciclo.

Alternativas consideradas:

- duplicar descritores, schemas e framing JSON-RPC no orquestrador;
- criar um crate novo só para o protocolo MCP;
- chamar a API admin a partir do endpoint por um cliente HTTP interno;
- reimplementar permissão, escopo e auditoria no transporte novo.

Duplicar contrato garante divergência silenciosa entre transportes, e
reimplementar política multiplica os pontos onde uma regra pode faltar.

## Decisão

Descer o vocabulário do MCP para o `edger-core`: descritores de tool, schemas
de entrada, contrato de capabilities e framing JSON-RPC. Tudo é dado puro, sem
I/O. O store de API keys fica fora — trait no core, SQLite no orquestrador — e
o transporte fica fora, em cada ponta.

O catálogo é único, com dois recortes: o stdio expõe todas as tools, o HTTP
expõe só o subconjunto de control plane. No servidor, as tools locais de
authoring leriam o filesystem do runtime, um trust domain alheio ao chamador.

Despachar as tools do HTTP no próprio router do admin, in-process, por
`oneshot`, com a credencial original do chamador. Permissão, CSRF, escopo por
worker e contratos de deploy valem uma vez só. O CSRF de browser é avaliado na
porta, com os headers originais, porque a requisição interna não tem `Origin`.

Este ADR não supersede o 0001 nem o 0002 — preenche os dois: o auth gate do
0001 ganha estado persistente e o core do 0002 segue puro, pois só desceram
tipos sem I/O.

## Consequências

Positivas:

- um contrato de tools para os dois transportes, sem cópia para envelhecer;
- o transporte novo não carrega política própria de autorização;
- endpoint HTTP sem cliente interno, sem sessão e sem porta extra.

Custos:

- mudança BREAKING no stdio: falha de tool virou resultado `isError`, no
  lugar do erro JSON-RPC `-32603`; o `_meta.status` é só do HTTP, onde a
  chamada admin tem um status para carregar;
- o recorte HTTP é manual: catálogo e dispatch são duas listas escritas à
  mão, e tool nova só chega ao transporte quando entra nas duas;
- o self-dispatch amarra os nomes de tool às rotas do admin.

## Status

Aceito em 2026-08-27. Fonte de verdade: `crates/edger-core/src/mcp.rs`,
`crates/edger-orchestrator/src/mcp_http.rs`, `crates/edger-mcp/src/lib.rs` e
`CHANGELOG.md` (0.3.0).
