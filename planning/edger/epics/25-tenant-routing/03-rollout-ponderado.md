# Story 25.03: escolha ponderada e estável de versão

**Origin:** `planning/edger/epics/25-tenant-routing/00-overview.md`.

## Context

O operador quer servir versões A/B de um app, por exemplo 80/20. Hoje o EdgeR usa um único ponteiro de versão. A escolha deve ser feita por request, sem gravar `defaultVersion`, e a mesma sessão precisa permanecer na mesma versão enquanto a política não muda.

## Traceability

TR-01, TR-04, TR-05, TR-06, TR-07 e TR-09 do [epic](00-overview.md). Fonte: `manifest_index_stub.rs` (`resolve_public_worker`, `worker_for_host`, `served_entry`, `resolve_plugin_worker`, `homepage`), `router.rs`, `pipeline.rs` e testes `routing_resolution.rs`/`deploy_install.rs`. Sem protótipo de tela.

## Files

| Caminho | Ação | Motivo |
|---|---|---|
| `crates/edger-orchestrator/src/manifest_index_stub.rs` | editar | Escolher versão elegível do snapshot de política. |
| `crates/edger-orchestrator/src/router.rs` | editar | Diferenciar URL com e sem pin de versão na resolução. |
| `crates/edger-orchestrator/src/pipeline.rs` | editar | Criar/ler cookie de coorte por sessão; anexar ao response sem apagar cookies do worker. |
| `crates/edger-orchestrator/src/routing_policy.rs` | editar | Hash estável, cálculo de faixas e validação de pesos. |
| `crates/edger-orchestrator/tests/weighted_routing.rs` | criar | Prova de host/path, coorte, distribuição e rollback. |

## Detail

**TO-BE:** `EDGER_WEIGHTED_ROUTING_ENABLED` inicia desligada. Off ignora `traffic` armazenado, usa a seleção atual e não emite cookie de coorte. On permite cookie opaco de sessão criado pelo EdgeR (UUID aleatório, HttpOnly, SameSite=Lax, Path=/; nenhuma informação de usuário/versão). `SHA-256(nome do app || cookie)` gera bucket 0..99; a política atribui faixas aos pesos. Mesmo cookie + mesmo app + política inalterada resulta na mesma versão; apps distintos não compartilham a mesma decisão apesar de poderem usar o mesmo cookie. O cookie não é credencial, e um cliente que o forja só escolhe seu próprio coorte. Versões ponderadas devem ser públicas, habilitadas, não staged; host route exige que a versão declare o host. Seleção explícita `@versão` preserva testes e rollback e nunca entra no split. Política removida volta ao default corrente imediatamente. Resposta do worker preserva seus `Set-Cookie` existentes.

**Limites:** distribuição 80/20 é estatística sobre muitos **coortes de sessão**, não promessa exata em janela pequena nem em volume de requisições (uma sessão muito ativa pode distorcer a contagem). Mudança de pesos pode remapear uma sessão; rollback operacional prevalece sobre stickiness. Sem cookie (cliente que não aceita cookies), escolha pode variar entre requests. Políticas inconsistentes com versões instaladas retornam erro de disponibilidade e sinal operacional; não redirecionam silenciosamente para outra versão.

### Acceptance criteria

- Coorte estável mantém versão com política inalterada, e amostra determinística cobre ambas as versões na proporção configurada.
- Flag off preserva `defaultVersion` e não emite cookie, mesmo com `traffic` persistido.
- URL com `@versão` ignora pesos; host, path, plugin base e homepage sem versão obedecem à política.
- Versão inelegível ou sem host não recebe tráfego, e DELETE volta ao default imediatamente.

## Test-first plan

1. Teste falhando: 10 mil coortes fixos observam versões A/B e contagens próximas de 80/20; mesmo cookie sempre resolve a mesma.
2. Teste HTTP falhando: primeira resposta emite cookie de sessão e segunda request com ele mantém versão; `Set-Cookie` do worker permanece.
3. Teste falhando: path, host, plugin base e homepage usam a mesma política; pinned `@versão` ignora split; tenant gate continua aplicável.
4. Testes negativos: staged, disabled, internal, host ausente e versão removida não recebem tráfego; rollback com uma única versão em 100% ou DELETE tem efeito imediato.

## Tasks

- [x] Implementar escolha pura por coorte e teste estatístico determinístico.
- [x] Passar o coorte pelos caminhos de host, path, plugin base e homepage sem mudar a precedência das rotas.
- [x] Criar/validar cookie de sessão com limite de tamanho e caracteres; não logar valor.
- [x] Exigir elegibilidade no momento do dispatch para evitar versão retirada em runtime.
- [x] Testar compatibilidade com route explícita, default/promote e cookies do worker.

## Verification

```bash
cargo test -p edger-orchestrator --test weighted_routing
cargo test -p edger-orchestrator --test routing_resolution
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
```

Teste HTTP local de 80/20 com versões reais e cookie. Registrar contagem e condições, sem afirmar garantia probabilística exata nem resultado de produção.

**Estado:** implementada e testada localmente; 9 testes dedicados e smoke HTTP com duas versões. O custo da verificação de staged por request e o comportamento sob carga ainda exigem medição antes de tráfego elevado.
