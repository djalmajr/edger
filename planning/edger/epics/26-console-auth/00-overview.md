# Epic 26: autenticação por senha no cPanel

**Origin:** `planning/edger/roadmap.md`; pedido do operador em 2026-09-27, tela de referência do appliance em `apps/apigate/web/src/components/app-shell.tsx`. **Estado:** rc.5 publicada e implantada no lab-dev, com fluxo de senha validado externamente.

## Contexto

O cPanel atual exige um token no navegador. O control plane aceita root key, chaves `egk_` e OIDC, mas não mantém usuários ou sessões de console. O operador quer login por usuário e senha com a apresentação do appliance e entrada por token disponível no mesmo card.

## Objetivo e limites

Implementar o usuário `root` da console, sem senha default. Um Secret opcional com senha inicial semeia root somente quando a conta `root` ainda não existe, mesmo que operadores tenham sido criados antes pelo root token. O root pode criar e administrar usuários adicionais com permissões e escopos explícitos; esses usuários não viram root e não administram outros usuários nesta entrega. Login emite token de sessão aleatório com prazo fixo, persistido só pelo hash; logout e troca de senha revogam sessões. O root token continua disponível como acesso de emergência, e `egk_`/OIDC preservam seus contratos. Sem Secret de senha, o modo por token continua funcionando. O cPanel usa os componentes compartilhados e reproduz a hierarquia visual do login do appliance: senha principal e token recolhido.

O mesmo banco SQLite persistente de API keys recebe usuários e sessões da console; `edger-core` permanece livre de I/O. `EDGER_ROOT_PASSWORD_FILE` contém apenas o caminho do Secret no Pod. O chart nunca aceita a senha em values. A senha inicial não substitui uma senha já alterada. A API pública de login tem corpo limitado, erro genérico, rate limit e checagem de origem. O token de sessão é enviado em header, não em cookie.

**Fora desta entrega:** papéis delegáveis entre root e operador, auditoria completa do Apigate, login por SSO na tela e deploy remoto. A gestão de usuários adicionais foi confirmada pelo operador durante a implementação e está na história 26.04.

## Story backlog

| História | Entrega | Estado |
|---|---|---|
| [26.01 Sessão root](01-sessao-root.md) | Seed seguro, senha lenta, sessão revogável, API, testes negativos | verificada localmente |
| [26.02 Tela de acesso](02-tela-acesso.md) | Card semelhante ao appliance, idioma/tema, senha e token, favicon | verificada localmente |
| [26.03 Configuração e prova](03-configuracao-prova.md) | Secret existente no Rancher, docs, gate Rust/JS/Helm, browser e smoke local | verificada localmente |
| [26.04 Gestão de usuários](04-gestao-usuarios.md) | Root cria, lista, altera permissões e escopos, desativa, redefine senha e exclui usuários adicionais; cPanel expõe essas ações | verificada localmente |

## Epic acceptance criteria

- Sem Secret de senha, a entrada por token continua acessível e nenhuma senha default é criada.
- Com Secret de senha e conta `root` ausente, `root` é criado sem alterar operadores existentes; usuário/senha errados não revelam a existência do usuário; sessão expirada/revogada é negada.
- `POST /api/admin/logout` revoga a sessão e o cPanel limpa o armazenamento local. O root token continua aceito.
- Apenas root gerencia usuários; usuários adicionais recebem permissões e escopos válidos, nunca root implícito. Desativar, excluir ou redefinir senha revoga sessões ativas. Usuário desativado não autentica. Ações não-root falham sem alteração no banco.
- A UI pt-BR acompanha a referência, incluindo língua/tema, mostrar senha e token recolhido; en-US/es-ES preservam traduções.
- O chart monta somente Secret existente de senha e só injeta seu caminho; default não monta nem exige o Secret.
- Testes Rust, clippy, fmt, testes/build cPanel, UI gate, Helm, refinamento e smoke no servidor local passam; evidências separam teste local de deploy.

## Riscos

| Risco | Controle |
|---|---|
| Roubo de token de sessão por script no mesmo origin | Armazenamento em `sessionStorage`, CSP existente e sessão curta/revogável; revisão do bundle e de headers antes de release. |
| Ataque de força bruta | Rate limit por IP verificado no servidor, senha lenta e mensagens genéricas. |
| Perda do SQLite | PVC persistente e export de estado coerente; root token mantém recuperação enquanto o store está indisponível. |
| Seed substituir senha trocada | Seed só no banco sem root; teste de restart com Secret alterado. |
| Usuário adicional ganhar capacidades do root | Role de operador, permissão de gestão root-only, validação do catálogo e dos escopos na escrita e no read-path da sessão. |
| Supor que o cartão visual prova auth real | Teste HTTP de login, revogação e autenticação admin no binário; Browser cobre somente o fluxo visível. |

## Evidência

Registrar em `planning/edger/status/evidence/console-auth-2026-09-27.md` após integração. O mapeamento read-only do Apigate está em `.herdr-agents/wF/reports/scout-apigate-auth-20260927T130138.md`.

## Status

Implementação local verificada por testes, revisão independente e smoke HTTP no binário. A rc.5 foi publicada e implantada no lab-dev em 2026-09-28; login root por senha, listagem de usuários, logout e revogação da sessão passaram por HTTPS. A root key anterior continuou válida. O Browser confirmou a tela de login e o favicon; o fluxo visual autenticado de gestão ainda será validado pelo operador. Evidência: `.herdr-agents/wF/reports/labdev-rc5-deploy-20260928.md`.
