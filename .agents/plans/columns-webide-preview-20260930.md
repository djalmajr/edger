# Plano — visibilidade de colunas e prévia WebIDE

## Contexto
Operador pediu ocultar Permissões por padrão (não remover), com exibir/ocultar
colunas como appliance, em API Keys e Usuários. Pins
26b70e9f-9bd9-4ac6-8077-ad8e2e3b708f e e0799322-d93f-4208-8c3a-cacb8f08581a,
capturas c8d2f1de-a4ff-4baf-97da-d2906fc1ebdc e
6a0354ed-c9b4-47bf-84a9-d8f4622cd1a1.
Também reportou SPA_ENTRYPOINT_INVALID no WebIDE local.

## Escopo
cPanel: controle acessível traduzido, visibilidade por tabela persistida,
Permissões false por default, ações sempre visíveis. Preservar RBAC e diálogos.
WebIDE: gerar dist no worktree servido; nenhuma mudança source por default.
Dois pi/Qwen high separados, reviewer Sonnet independente.

## Tarefas
- [x] Implementar visibilidade de colunas e testes de comportamento.
- [x] Gerar build WebIDE e verificar entrada/assets.
- [x] Revisão Sonnet com evidência executada.
- [x] Build cPanel e browser: defaults, menu, show/hide/reload, independência.
- [x] Browser WebIDE: carregar interface e assets (dashboard, sem pageerror).
- [x] Rust test/clippy/fmt e relatório final.

## Limites
Preservar alterações locais acumuladas. Sem commit, push, publicação ou deploy
remoto. Preview 19081 somente; servidor 19080 preservado. Nenhum secret novo.

## Fechamento

Concluído localmente: visibilidade, layout compacto das colunas finais de API
Keys, tooltip do badge de Workers e build WebIDE. Revisão final Sonnet
review-cpanel-claude-20260930T074933.md: PASS, zero achados; P2 de mocks
resolvido. Evidências em planning/edger/status/evidence/columns-webide-preview-2026-09-30.md.
