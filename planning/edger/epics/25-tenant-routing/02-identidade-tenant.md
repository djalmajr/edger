# Story 25.02: identidade Tenancit e gate de tenant

**Origin:** `planning/edger/epics/25-tenant-routing/00-overview.md`.

## Context

Apps restritos precisam saber em qual tenant um hostname está cadastrado, sem fazer o EdgeR ler recursos ou segredos. Tenancit já fornece `GET /v1/identify?hostname=...`; a credencial `tenant:identify` é de serviço. O EdgeR deve aplicar a allowlist antes de iniciar/usar o worker. A identidade de domínio não autoriza pessoas nem substitui a auth da aplicação.

## Traceability

TR-02, TR-03, TR-08 e TR-09 do [epic](00-overview.md); Tenancit `docs/developers/{03-contratos-http,04-seguranca-e-criptografia}.adoc`; alinhamento solicitado em `.herdr-agents/wF/to-tenancit-tenant-routing-design-20260926.md`. Sem protótipo de tela.

## Files

| Caminho | Ação | Motivo |
|---|---|---|
| `crates/edger-orchestrator/src/tenant_identity.rs` | criar | Cliente identify com timeout, limites, erros e revalidação. |
| `crates/edger-orchestrator/src/bin/edger.rs` | editar | Composição opt-in de URL e token de arquivo/secret, sem embutir valores. |
| `crates/edger-orchestrator/src/pipeline.rs` | editar | Gate após resolver app e antes de dispatch, em host/path/`@versão`. |
| `crates/edger-orchestrator/src/wire.rs` | editar se necessário | Substituir o header de tenant antes da entrega ao worker. |
| `crates/edger-orchestrator/tests/tenant_routing.rs` | criar | Fake Tenancit + worker contador; sucesso, negação e falhas. |

## Detail

**AS-IS:** o roteamento usa `Host` recebido, e `Authorization` do visitante vai para o worker. **TO-BE:** `EDGER_TENANT_ROUTING_ENABLED` inicia desligada. Off não pede configuração do Tenancit, não consulta identify e não aplica allowlists persistidas. On exige `EDGER_TENANCIT_IDENTIFY_URL` e `EDGER_TENANCIT_TOKEN_FILE` no startup. Com a flag on, política `allowlist` aciona identify com hostname canonizado e token de serviço sob escopo mínimo. Resposta 200 com `tenantSlug` válido é comparada à lista. O header `x-tenant-id` do cliente é removido sempre; para app restrito permitido, o EdgeR injeta o slug confirmado. `Authorization` do visitante continua pertencendo ao worker. Host ausente, 404, 401/403, 429, timeout, 5xx, JSON fora do contrato e slug fora da lista negam a execução. Resposta não inclui tenant alheio nem token. Em app aberto, não há dependência do Tenancit e não se injeta identidade presumida.

**Cache/indisponibilidade:** a documentação do Tenancit permite guardar `ETag`, mas exige revalidar a cada uso para não manter hostname reatribuído. `304` só pode reutilizar slug guardado para **o mesmo hostname canônico**; sem entrada anterior é falha fechada. Não assumir TTL positivo. O feed/eventos atual não inclui dados suficientes para espelho de domínios; limite de chamadas e latência precisam ser medidos antes de liberar política restrita de alto volume. `404` representa domínio/tenant ausente; `401/403` são falha da credencial de serviço, não login do visitante; `429`, timeout e `5xx` são falha da dependência. A associação hostname→tenant é uma regra de disponibilidade de domínio; para controle de usuário é necessária prova adicional emitida/validada pela aplicação ou por IdP confiável.

### Acceptance criteria

- Tenant listado executa o app, com slug confirmado propagado ao worker sem token de serviço.
- Flag off preserva o dispatch legado com ou sem política armazenada, sem exigir URL/token.
- Tenant diferente ou falha do identify não invoca o worker, também com URL `@versão`.
- Header `x-tenant-id` do visitante não altera a decisão nem chega como identidade confiável.
- Cron interno com marcador e credencial root autenticada continua executando sem domínio de visitante; marcador forjado sem root não contorna a allowlist.

## Test-first plan

1. Teste HTTP falhando: tenant permitido executa worker e recebe slug injetado, mesmo que o cliente envie outro `x-tenant-id`.
2. Teste falhando: tenant diferente, host sem cadastro ou ausência de host retornam deny e contador do worker segue zero.
3. Testes falhando para 401/403/429/5xx/timeout/JSON inválido; nunca faz fallback para header bruto ou worker default.
4. Teste de reatribuição: duas requisições ao mesmo hostname com slugs diferentes não usam identidade velha; `@versão` também passa pelo gate.

## Tasks

- [x] Receber resposta do Tenancit e fixar semântica de status/cache; orçamento de latência ainda exige medição operacional.
- [x] Configurar cliente com endpoint permitido, TLS de produção, token vindo de secret/file e timeout/limites de corpo.
- [x] Tratar identidade como contexto autenticado pelo serviço, com retirada de header do cliente.
- [x] Aplicar gate no caminho compartilhado de worker, incluindo plugin/homepage se expostos sob política.
- [x] Testar negação antes de pool/worker e ausência de secrets no smoke local.
- [x] Documentar operação e a diferença entre tenant de domínio e pessoa autenticada.

## Verification

```bash
cargo test -p edger-orchestrator --test tenant_routing
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
```

Mock HTTP local do Tenancit com mudança de slug; smoke local com token efêmero de teste, sem publicar credenciais. Validar configuração negativa (URL/token ausentes) ao ativar política restrita.

**Estado:** contrato Tenancit recebido e conferido em `.herdr-agents/wF/from-tenancit-tenant-routing-design-20260926.md`; implementação, smoke local e integração com servidor Tenancit real local concluídos, incluindo latência e rate limit. A validação sob carga no ambiente publicado continua pendente.
