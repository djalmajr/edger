# Story 26.04: gestão de usuários da console

**Origin:** `planning/edger/epics/26-console-auth/00-overview.md`; confirmação explícita do operador em 2026-09-27: criar e administrar outros usuários com senha nesta entrega. Depende da sessão root da 26.01.

## Context

A primeira fatia da console autentica root, mas não permite delegar acesso humano. Esta história acrescenta usuários de senha sem transformá-los em root, usando as mesmas permissões e escopos do control plane existente.

## Files

`crates/edger-orchestrator/src/console_auth.rs`, `auth.rs`, `admin_api.rs` e testes HTTP/store para o backend; `workers/core/cpanel/src/components/console-users.tsx`, `main.tsx`, `lib/api.ts`, `lib/i18n.tsx` e testes correspondentes para a UI. Ajustar a lista aos arquivos efetivamente tocados na revisão.

## Detail

O root cria usuários adicionais com username normalizado (`[a-z0-9._-]`, 2–32 caracteres, sem pontuação inicial), senha forte e um conjunto explícito de `permissions`, `namespaces` e `workers` compatível com o `PERMISSION_CATALOG` e com `validate_key_grant`. Não há papel root delegável pela API; `root` é reservado e imutável. Os usuários adicionais têm `role=operator`, `isRoot=false` e só as permissões/escopos gravados. A permissão `keys:manage` pode ser atribuída explicitamente, mas nenhum operador ganha gestão de usuários. O root token existente e uma sessão root podem gerir usuários.

| Rota | Contrato |
|---|---|
| `GET /api/admin/users` | Root apenas. Lista metadados sem hash/senha/sessões. |
| `POST /api/admin/users` | Root apenas. Cria com username, senha, permissões e escopos; 409 no username repetido. Nunca devolve a senha. |
| `PATCH /api/admin/users/{id}` | Root apenas. Altera `disabled`, permissões e escopos de usuário não-root. Desativação ou redução de capacidades revoga suas sessões na mesma transação. |
| `POST /api/admin/users/{id}/reset-password` | Root apenas. Define nova senha forte e revoga todas as sessões desse usuário na mesma transação. |
| `DELETE /api/admin/users/{id}` | Root apenas. Remove usuário não-root e sessões na mesma transação. |
| `POST /api/admin/me/password` | Usuário de sessão troca a própria senha após validar a atual; recebe nova sessão ou precisa de novo login. Token API/root não é uma sessão de usuário. |

Todas as mutações passam pelo gate de origem e corpo limitado. Erros de login não distinguem usuário ausente, desativado e senha errada. A sessão consulta o usuário atual para negar disabled/expirado e aplicar escopos atuais; mudanças não podem deixar principal antigo com permissões ampliadas. Falha de banco nega a operação. A UI oferece lista e formulário de usuário apenas a root; o servidor é a barreira real. Senhas só entram pelo formulário de criação/reset/troca, nunca em logs ou respostas.

## Tasks

- [x] Ampliar schema/store com usuários adicionais e operações transacionais; testar persistência e classes hostis.
- [x] Integrar o principal de sessão e rotas admin com autorização root-only; testar 401/403, desativação, reset e revogação.
- [x] Entregar gestão no cPanel e alteração da própria senha; testar DOM/API. O fluxo Browser autenticado fica para a validação do operador no servidor local.
- [x] Rodar Rust gate, testes/build do cPanel, Helm, revisão de outra família, refinamento e smoke no binário.

## Verification

- Username com caixa/Unicode/controle/duplicado; senha fraca/muito longa; permissões desconhecidas, `*` indevido, scopes vazios ou inválidos.
- Operador tenta criar/editar/excluir usuário e alterar escopo próprio por API: 403, banco intacto.
- Usuário desativado/expirado, sessão revogada e reset de senha: não acessam `/api/admin/session` nem mutações.
- Falha forçada no SQLite durante mutação: não deixa permissões/sessões em estado parcial.
- Root token e sessão root continuam gerindo usuários, sem transformar `egk_` com `keys:manage` em root.

## Aceite

Criação, uso, alteração e revogação de um usuário adicional funcionam no HTTP e no cPanel. Evidências em `planning/edger/status/evidence/console-auth-2026-09-27.md`.

### Acceptance criteria

- Usuário adicional só recebe as permissões/escopos gravados e nunca `isRoot=true`.
- Root gerencia usuários; API key ou usuário não-root não passa pelo endpoint de gestão.
- Reset, desativação e exclusão revogam sessões sem estado parcial.

```bash
cargo test -p edger-orchestrator --test console_users
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
```
