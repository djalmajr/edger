# Story 26.05: sessões ativas e configurações da console

**Origin:** `planning/edger/epics/26-console-auth/00-overview.md`; pedido visual do operador em 2026-09-30 e autorização explícita das APIs e páginas root-only. Depende de 26.01 e 26.04. Estado: portada ao main em EDG-23 (base rc.17); verificada localmente; sem publicação ou deploy.

## Context

O operador pediu a navegação agrupada como no appliance e acesso a Sessões ativas e Configurações. A entrega usa as sessões persistentes existentes, preservando autenticação por senha, root key e permissões dos operadores.

## Files

Backend: `crates/edger-orchestrator/src/console_auth.rs`, `admin_api.rs`, `tests/console_system.rs`. Frontend: `workers/core/cpanel/src/components/console-sessions.tsx`, `console-settings.tsx`, seus testes, `lib/i18n.tsx`, `lib/route.ts`, `lib/route.test.ts` e `main.tsx`.

## Detail

- GET `/api/admin/sessions`: root-only, no-store; até 1000 sessões não expiradas de usuários ativos, ordenadas por criação/id, com sinalização `truncated`. Somente id opaco, userId, username, role, isRoot, createdAt, expiresAt e current. Nenhum token/hash.
- DELETE `/api/admin/sessions/{id}`: root-only e gate de origem; id de 32 hex minúsculos, SQL parametrizado. Revoga sessão alheia e invalida sua credencial imediatamente. Própria sessão recebe 403, com guarda no DELETE atômico; encerramento próprio usa logout. Id inválido 400 e inexistente 404.
- GET `/api/admin/settings`: root-only, no-store; allowlist de versão, flags tenant/weighted, disponibilidade de senha e políticas já existentes de sessão/senha/login. Sem URLs, caminhos, secrets ou escrita.
- Sistema fica visível apenas ao root. Sessões usa DataGrid, busca, refresh a cada 30s, confirmação para revogar e bloqueio da sessão atual. Configurações mostra valores reais em cards somente leitura. pt-BR/en-US/es-ES.
- Rotas preservam prefixos de proxy e versão. Non-root não monta queries das páginas. Servidor continua sendo a barreira de autorização.

## Acceptance criteria

- Root pode listar sessões e configurações; credencial ausente recebe401 e operador/egk recebe403.
- JSON só contém metadados permitidos, nunca tokens/hashes; GET usa no-store.
- Revogar própria sessão recebe403 e ela permanece válida; sessão alheia revogada200 autentica como401 imediatamente.
- Origens hostis e ids inválidos não alteram o banco; a listagem omite sessões expiradas/inativas e sinaliza o teto1000.
- UI non-root não faz fetch; cancelar não revoga; confirmar revoga e remove a linha; configurações não oferece escrita.
- Gates Rust/frontend, revisão independente e smoke/browser local passam antes de marcar verificada.

## Tasks

- [x] Fechar contrato e registrar autorização local do operador.
- [x] Implementar componentes, traduções, rotas e navegação.
- [x] Implementar store/API e testes HTTP de segurança.
- [x] Executar typecheck, suítes frontend, builds e smoke HTTP/browser local com fixtures removidas.
- [x] Encerrar gate Rust completo e revisão independente (PASS, sem P0–P2).

## Verification

Gate Rust canônico:

```bash
cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt -- --check
```

Frontend: typecheck, Vitest, Bun e build. Provas locais: no credential401, operador/egk403, root aceito, JSON allowlist/no-store, expiradas/inativas omitidas, limite1000, origem hostil403, self403 preservada, revoke200 seguido de token401, unknown404/invalid400 e flags off/on. Browser: cancelar mantém sessão, confirmar remove linha e credencial revogada falha; fixtures temporárias removidas.

Evidência original (base rc.6) em `planning/edger/status/evidence/console-system-2026-09-30.md` do worktree `cpanel-act-warnings` — não copiada por descrever outra base; na portação EDG-23 a prova local é a suíte `crates/edger-orchestrator/tests/console_system.rs`. Nenhuma prova local implica merge, publicação ou deploy remoto.
