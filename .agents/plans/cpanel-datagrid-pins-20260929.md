# Plano: DataGrid de usuários e prévia atualizada

## Contexto
Pins do operador: http://127.0.0.1:17373/b/9aa1a887-753d-4a79-96e0-ac50d5a6d394.md.
Pin adicional de ajuda: http://127.0.0.1:17373/b/297176fd-58ba-40b2-ae13-a891a2787889.md (capture f23f95e6-6357-4f95-845f-4eeaf78910b3, pin f8c55d5f-a059-4940-81ce-95062f1f59d1). Apresentar keys.lead em tooltip no botão de ajuda junto ao título Chaves, conforme WikiHelp do ai-memory-ui, com hover e foco de teclado; remover o parágrafo do corpo.
Pin de ações: http://127.0.0.1:17373/b/3d6b6722-0936-480d-8765-306e543ac169.md (capture b4a4cc11-37f5-44f5-8f15-f432426ddb47, pin42e31af8-ed54-4cd2-8adf-4e27717c51ac). Trocar ações de Usuários por ícones compactos, com aria-label e tooltip traduzidos; preservar root e mutações.
Usuários em 19080 usa Table simples. Chaves da captura mostra texto inglês da versão anterior, embora a base rc.6 já use DataGrid. As portas 19080/19081 estavam offline no início da inspeção.

## Arquivos
- workers/core/cpanel/src/components/console-users.tsx: migrar a tabela.
- workers/core/cpanel/src/components/console-users.test.tsx e console-users.dom.ts: adaptar fixture e provar o comportamento nos runners reais.
- Relatórios de evidência locais, sem mudança de backend/contrato.

## Detalhes
Reutilizar DataGrid/DataGridColumnHeader/DEFAULT_PAGE_SIZE do cPanel e o padrão de columns de api-keys.tsx. Referência visual appliance/apps/apigate/web/src/routes/users.tsx. Ordenação por username, paginação 15/30/60, larguras compactas e dados longos sem expandir a página. Reutilizar PermissionBadges para no máximo duas linhas. Preservar root-only e todas as mutações/diálogos/permits existentes. Nenhuma mudança de identidade ou RBAC. Chaves usa DataGrid existente; atualizar a prévia e validar que o bundle servido é o construído, sem recriar a tabela.

## Tarefas
- [x] Implementer cpanel-qwen: DataGrid de usuários, ações por ícones, fixtures e testes.
- [x] Implementer release-qwen: prova de DataGrid/i18n de chaves, build isolado e ajuda do título com tooltip; sem editar runtime.
- [x] Integrar, revisão Sonnet 5.5 e gates pertinentes.
- [x] Build final na prévia própria 127.0.0.1:19081 com token sintético test-root, DB scratch. Preservar processo compartilhado19080 (cPanel rc.4).
- [x] Prova HTTP/asset e visual do usuário/grid, ajuda hover/foco/Escape e paginação real15/30.

## Verificação
Bun console-users e API Keys juntos e isolados, Vitest/typecheck cPanel, build. Preservar evidência Rust da rc.6 enquanto nenhum arquivo Rust/runtime/dependência é modificado. Conferir fingerprint servido x arquivo dist e token local via GET admin/session. Sem publicar, push/tag/PR/merge ou deploy remoto nesta fatia.

Gate Rust completo reexecutado nesta rodada com cargo +1.98.0 (test --workspace, clippy --workspace -- -D warnings, fmt -- --check): PASS, logs /tmp/edger-cpanel-pins-rust-{tests,clippy,fmt}-20260929.log. Sem delta Rust/chart/dependências.

Fechamento: planning/edger/status/evidence/cpanel-datagrid-tooltip-icons-2026-09-29.md. Revisão final PASS, zero achados novos. Sugestão P3 não bloqueante de teste versionado específico da ajuda, com comportamento já exercitado. Alterações locais não commitadas.

## Emenda do topo de Usuários (19:05)

Pin a402841f-3a83-4669-af4b-02443e9ed8e8, capture e1572821-ec85-44fa-8c71-c6fed2da46e7, batch a7065e09-afce-4eae-b445-6a9493c98943. Aplicar o padrão API Keys ao topo de Usuários: Novo usuário no slot PageActions junto de Atualizar; users.lead no tooltip PageTitleHelp junto ao título. Preservar root-only, DataGrid, ações e diálogos. Alterar apenas main.tsx e console-users.tsx.

- [x] Implementer pi/Qwen: callback de ação e ajuda por rota; checks específicos existentes.
- [x] Reviewer Sonnet 5.5: delta local, root gate e composição do slot; PASS0achados.
- [x] Build final e prova browser de toolbar, tooltip hover/foco/Escape e abrir/fechar diálogo sem criar conta.
- [x] Gate Rust e atualização de evidências locais, seção Emenda19:05 no fechamento acima.
