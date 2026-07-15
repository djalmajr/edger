# Story 14.06: Arquivos seguros no cPanel

**Origin:** `planning/edger/epics/14-deploy-apps/00-overview.md`

## Context

- **Problema:** o explorador lista e envia arquivos, mas não permite baixar um
  arquivo ou uma pasta e hoje a escrita alcança inclusive workers core. Mutações
  in-place também não reciclam com segurança o processo Deno persistente.
- **Objetivo:** entregar a primeira fatia segura do file manager: download
  autenticado de arquivo/pasta e proteção contra escrita em workers core.
- **Valor:** o operador consegue inspecionar e exportar conteúdo sem comprometer
  rollback, identidade da versão ou isolamento dos workers mantidos pelo EdgeR.
- **Restrições:** mudanças de conteúdo continuam versionadas; nenhuma operação
  desta story edita, renomeia ou remove arquivos de uma versão ativa.

## Files

- `crates/edger-orchestrator/src/admin_api.rs`
- `crates/edger-orchestrator/tests/deploy_install.rs`
- `workers/core/cpanel/src/main.tsx`
- `workers/core/cpanel/src/lib/api.ts`
- Buntime: file manager usado apenas como referência de UX; o EdgeR preserva
  versões imutáveis e o boundary core/user.

## Detail

- **In:** download autenticado de arquivo; pasta empacotada como ZIP; paths
  confinados ao diretório da versão; limite de 4 MiB para resposta em memória;
  upload permitido somente para origin `user`; ocultar upload em core; remover o
  breadcrumb redundante na raiz.
- **Out:** editar, renomear, mover, remover, batch operations e hotfix in-place;
  fluxo de draft/nova versão; streaming de downloads maiores.

## Acceptance criteria

- [x] Arquivo baixado preserva bytes e nome sugerido.
- [x] Pasta baixada vira ZIP com paths relativos, sem seguir symlinks.
- [x] Download exige `workers:read`, respeita namespace e rejeita traversal.
- [x] Download acima de 4 MiB falha de forma tipada, sem alocação ilimitada.
- [x] Upload em `core_bundled` e `core_overlay` retorna `403`; a UI não oferece
  o botão nesses origins.
- [x] Na raiz, o explorador não mostra o nome do worker como breadcrumb vazio;
  em subpastas, mantém navegação até a raiz.
- [x] Rust gate, build do cPanel e validação Browser verdes.

## Test-first plan

- **Behavior:** API real devolve arquivo/ZIP e rejeita escrita em core.
- **Level:** integração em `deploy_install.rs`, usando roots temporários e o
  router completo; Browser para affordances e navegação visual.
- **Avoid:** snapshots de HTML/JSON e testes de classe CSS.

## Tasks

- [x] Mapear lifecycle, origins, autenticação e confinamento de paths.
- [x] Escrever e observar testes falhando para download e proteção de core.
- [x] Implementar endpoint bounded e proteção de origin.
- [x] Implementar download autenticado e affordances no cPanel.
- [x] Executar gates e registrar evidência Browser.

## Verification

- Integração Rust cobre download de arquivo/pasta e bloqueio de escrita em core.
- Testes e build do cPanel cobrem a UI e seus contratos de API.
- Browser confirma navegação, downloads, paginação, data-grid e detalhes dos
  eventos; evidência consolidada em
  `planning/edger/status/evidence/files-safe-cpanel-2026-07-15.txt`.
- Gate Rust, `cpanel-ui-gate.sh`, refinamento e `git diff --check` devem passar.

## Fluxo recomendado para atualização de conteúdo

1. Criar draft em staging a partir da versão escolhida.
2. Editar arquivos e `manifest.yaml` no draft, atribuindo nova versão semver.
3. Validar manifesto, entrypoint, paths, permissões e configuração.
4. Publicar atomicamente como nova versão; executar release/health e ativar.
5. Manter a versão anterior disponível para rollback.

Workers core publicam uma nova `core_overlay`; nunca alteram o bundle nem a
versão ativa in-place.

## Status

**completed** (2026-07-15) — endpoint autenticado baixa arquivo ou pasta ZIP
com cap de 4 MiB, traversal bloqueado e symlinks ignorados; upload in-place
retorna `403` para origins core. O cPanel baixa via fetch autenticado, oculta
upload em core e só mostra breadcrumb dentro de subpastas. TDD observado:
download iniciou em `404` e core upload em `200`; ambos ficaram verdes após a
implementação. Rust gate, cPanel gate e validação Browser concluídos.

**Ajuste UX (2026-07-15):** ações de download usam ícone com tooltip e nome
acessível por arquivo; o cabeçalho redundante "Files"/descrição foi removido.
Breadcrumb e upload de workers `user` compartilham uma toolbar compacta.

**Invariante do cPanel (2026-07-15):** apenas uma versão do worker `cpanel`
permanece habilitada. Inserir ou habilitar uma versão desabilita atomicamente as
demais; na lista, `Open URL` fica sempre desabilitado para o próprio cPanel,
inclusive na versão ativa, pois essa já é a superfície administrativa atual.

**Paginação (2026-07-15):** a lista de workers reutiliza o controle dos logs,
com primeira/anterior/próxima/última, indicador de página e seletor de
25/50/100 aplicações. Busca, filtro de kind e mudança de page size retornam à
primeira página.

**Data-grid de logs (2026-07-15):** os logs usam o padrão TanStack do appliance,
com cabeçalhos ordenáveis, estado vazio, paginação completa e seletor de
25/50/100 linhas. Busca e nível continuam na toolbar do contexto. Cada linha
abre um painel lateral com o payload allowlisted completo e ação `Copy JSON`.
