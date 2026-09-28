# EdgeR 0.3.2-rc.6 — publicação e lab-dev (2026-09-28)

## Entrega

- PR [#69](https://github.com/djalmajr/edger/pull/69) mesclado por squash em `main` (`54d1b6e53bc700f090ab394c7d9e89bff38ef206`). Tag anotada `v0.3.2-rc.6` nesse commit.
- A rc.6 traduz a visão geral do cPanel, corrige o progresso dos dois modos de login, trunca nomes de workers com valor completo acessível, mostra o estado efetivo das flags de roteamento e desabilita a política quando ambas estão desligadas.
- A edição de permissões de API keys usa `PATCH /api/admin/keys/{id}` com validação de catálogo, anti-escalada, rejeição de chave revogada e invalidação imediata de cache. A tela usa DataGrid, ações na mesma linha e badges limitados a duas linhas com `+N`.

## Provas antes do deploy

- O [CI final do PR](https://github.com/djalmajr/edger/actions/runs/36490864421) passou Rust workspace, integração OTLP, contratos de cPanel e planejamento, Helm/Rancher, container, dependências e Secret scan. A primeira tentativa do PR falhou no scanner por um prefixo sintético de fixture; o único commit da branch foi amendado para removê-lo do histórico do PR. Gitleaks redacted no intervalo `30bfde4..f2113cb` retornou 0 e o novo CI passou.
- Local: `cargo +1.98.0 test --workspace`, `cargo +1.98.0 clippy --workspace -- -D warnings` e `cargo +1.98.0 fmt -- --check` passaram. `bun test` em `workers`: 157 passaram, 0 falharam; Vitest cPanel: 140 passaram, 0 falharam. `planning/edger/scripts/run-gates.sh` terminou em `ALL PLANNING GATES PASS`. `helm lint` com o overlay lab-dev passou.
- Revisão independente Claude Sonnet 5.5 high: os três achados executados de fixture/gate foram corrigidos e reproduzidos como corrigidos; recheck com 0 P1/P2 e veredito PASS. O último delta da fixture recebeu novo PASS com 0 achados e Gitleaks 0. A execução isolada do arquivo API Keys em Bun ainda emite avisos React `act(...)` (P3); as suítes de cPanel/workspace e o Vitest terminam sem esses avisos. Tentativa de ajuste simples foi testada e revertida por não resolver.
- Prévia local em navegador: ações de chave na mesma linha, coluna de permissões de 256 px, badges em duas linhas e indicador `+N`. O diálogo de edição abriu em pt-BR. Servidor de prévia em `127.0.0.1:19081` respondeu 200 em `/health`.

## Publicação GHCR

- O [workflow da tag](https://github.com/djalmajr/edger/actions/runs/36491786759) terminou com todos os jobs verdes e publicou imagem e chart OCI.
- Chart `oci://ghcr.io/djalmajr/charts/edger --version 0.3.2-rc.6`, digest `sha256:81f1c4f2868962a0a3daeda7f201af8b09978fa2d02fd0613b3a91cf3dbe7cbb`. `Chart.yaml` publicado informa `version` e `appVersion` iguais a `0.3.2-rc.6`.
- A renderização do chart publicado com `values-labdev.yaml` fixou a imagem em `ghcr.io/djalmajr/edger@sha256:c0294e8182586bd83c2431c61a79040f864ed9d30733e00e140b0fec53aba225` e manteve `EDGER_TENANT_ROUTING_ENABLED=false` e `EDGER_WEIGHTED_ROUTING_ENABLED=false`.

## Upgrade e prova externa no lab-dev

- Antes do upgrade, backup do PVC em `/tmp/edger-labdev-rc6-deploy-20260928/workers-before-rc6.tar`, modo `0600`, 126 entradas e 7.270.400 bytes. A cópia do SQLite foi inspecionada localmente com `PRAGMA integrity_check=ok`; o arquivo temporário extraído foi apagado. O tar ao vivo não é snapshot transacional do conjunto inteiro.
- Helm `edger` no namespace `hyper` atualizado da revisão 16 (`0.3.2-rc.5`) para 17 (`0.3.2-rc.6`) com chart OCI publicado, `--reset-values --atomic --wait --timeout 10m --history-max 5` e overlay lab-dev. Status `deployed`, deployment 1/1 pronto, imagem efetiva igual ao digest renderizado acima.
- Externo por HTTPS com TLS válido: `/apps/health`, `/apps/cpanel/`, `/apps/api/admin/login-options`, JS, CSS e favicon do cPanel responderam 200. `login-options` informou `passwordEnabled=true`, `rootSeeded=true`.
- `GET /apps/api/admin/session` com a credencial existente respondeu 200, retornou principal válido e flags efetivas `tenantRoutingEnabled=false`, `weightedRoutingEnabled=false`. Nenhum valor de credencial foi salvo no relatório.
- Uma prova adicional de `PATCH` em chave descartável no lab-dev não foi executada: a revisão automática rejeitou a criação/edição/revogação de credencial remota sem autorização específica para esse teste. Nenhuma chave de teste foi criada; o contrato `PATCH` tem testes de integração locais e passou no CI.

## Limites

- A rc.6 altera o EdgeR no lab-dev; não atualiza VPS nem serviços Planner/celld.
- As flags de tenant e A/B permanecem desligadas no overlay lab-dev. O Tenancit não é configurado para essa instalação.
- O cPanel foi verificado por HTTP/TLS e assets externos; não houve nova inspeção visual autenticada em navegador após o upgrade. A prévia local foi inspecionada antes da publicação.
