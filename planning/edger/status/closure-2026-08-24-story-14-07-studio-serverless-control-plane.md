# Fechamento da Story 14.07: Control plane serverless para o Studio

## Resumo

A Story 14.07 está concluída. O EdgeR agora separa drafts internos mutáveis de releases públicas imutáveis, oferece ciclo de deploy/rollback persistente e expõe o mesmo control plane por tools MCP sem aceitar origem ou credencial em argumentos.

## Entregue

- `visibility: public|internal` no manifest, com default público e campo presente nas respostas de install/list.
- Data plane devolve 404 para worker interno; marcador `x-edger-internal` só é aceito com credencial root válida.
- Install forçado somente para versão interna existente, com staging, swap, rollback e reciclagem do grupo no pool.
- CAS do draft: install devolve `revision` (persistida em `.edger-revision` dentro da versão, atravessa restart/rescan); `force` exige `x-edger-expected-revision` (`409 DEPLOY_REVISION_REQUIRED` sem ela, `409 DEPLOY_REVISION_STALE` defasada) e upload de files avança a revisão no retorno.
- `WorkerMutationSlot` serializa install/delete/files por `name@version` pela transação inteira (até commit/rollback, claim antes de qualquer mutação): concorrente recebe `409 DEPLOY_IN_PROGRESS`; regressões cobrem barreira de dois writers, janela de release com delete/files recusados e rollback restaurando o draft sem diretório órfão.
- Releases públicas recusam force e upload de arquivos; upload interno recicla o processo antes de responder.
- Delete por versão ou worker completo remove arquivos, índice e processos.
- Promote somente de versão pública, com ponteiro persistido por temp+rename e restaurado no startup/rescan.
- Install `staged=true` aceita somente release pública imutável, persiste o marker em `.edger-revision`, mantém a versão pinada/health-checkável sem alterar o fallback sem versão e limpa o marker no promote; install/list expõem `staged`.
- Invoke admin autenticado preserva método, headers, body e streaming; `x-edger-worker-version` seleciona o worker sem consumir a query da aplicação. A credencial de controle usa `x-edger-control-authorization`, é removida antes do dispatch e não conflita com `Authorization`/`x-api-key` da aplicação.
- Method-map do harness responde 405 com `Allow` ordenado, preserva handlers diretos e resolve exato > parâmetro > wildcard (`/prefix/*` antes de `/*`).
- MCP cobre install (path/base64, inclusive `staged`), list, enable, disable, delete, promote, invoke e eventos filtráveis por worker.
- MCP liga `EDGER_URL` e `EDGER_ROOT_KEY` uma vez no contexto, exige HTTPS fora de loopback, redige Debug e exige `version` ou `allVersions: true` para delete total.

## Desvios de escopo

- O requisito de segurança do Studio ampliou a fatia original com `visibility`, invoke e hardening MCP; todos mapeiam aos critérios adicionados à Story 14.07.
- Nenhuma alteração foi feita em charts, CI ou README.

## Evidência

- `cargo fmt -- --check`: limpo.
- `cargo clippy --workspace --all-targets --locked -- -D warnings`: limpo.
- `cargo test --workspace --locked` (Deno real no PATH): 55 suítes verdes, 0 falhas — inclui as regressões de CAS/slot (barreira, janela de release, rollback) em `deploy_install.rs` (24 testes) e o protocolo MCP (12).
- Regressão staged + restart/rescan/promote: `cargo test -p edger-orchestrator` (201 pass); UDS wildcard real: `cargo test -p edger-isolation --features multiproc` (61 pass, 1 ignored); MCP: `cargo test -p edger-mcp` (12 pass); `cargo deny check advisories`: ok.
- `SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh`: todos os planning gates passaram, incluindo refinement sem red flags, lint, deploy layout, builds/testes dos frontends e `cargo check`.

## Riscos remanescentes

- O transporte MCP retorna invoke bufferizado; streams infinitos continuam responsabilidade do endpoint admin HTTP, não do resultado JSON-RPC da tool.
- Ponteiro persistido inválido é ignorado com warning e fallback para maior semver pública, evitando indisponibilidade silenciosa.

## Handoff

- O lowcode-studio deve criar draft com `visibility: internal`, usar force apenas nessa versão (SEMPRE com `x-edger-expected-revision` da última resposta — install, list ou files), instalar a nova release pública com `staged=true`, validar o pathname versionado e então promover.
- `409 DEPLOY_IN_PROGRESS` significa outra mutação do mesmo alvo em voo: repetir após ela assentar, sem retry cego.
- A seleção de versão no invoke usa `x-edger-worker-version`; a query inteira pertence à aplicação.
- O invoke autentica o controle em `x-edger-control-authorization`; `Authorization` e `x-api-key` pertencem à aplicação.
- Delete MCP total exige `allVersions: true` explícito.
