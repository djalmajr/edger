# Story 08.29: Logs acionáveis de erros operacionais

**Origin:** `planning/edger/epics/08-valor-buntime/00-overview.md`

## Context
- Problema atual: a matriz ainda marca `Logging e warnings acionáveis` como `must partial`; request IDs, diagnósticos de gateway e logs recentes existem, mas falta uma prova global de que erros operacionais emitem logs estruturados sem segredos.
- Objetivo de entrega: emitir log operacional estruturado para falhas do Admin API, com `surface`, `request_id`, `status` e `code`, sem serializar headers, corpo ou mensagem potencialmente sensível.
- Restrições: não criar stack de observabilidade externa, não depender de SaaS/OTel, não vazar `Authorization`, API keys, body ou mensagens com valores de usuário no log.
- Referências: `crates/edger-orchestrator/src/admin_api.rs`, `crates/edger-orchestrator/src/server.rs`, `crates/edger-orchestrator/tests/security_operational.rs`, `planning/edger/docs/value-parity-matrix.md`.

## Traceability
- Protótipos/telas: não aplicável.
- Regras de negócio: operador precisa correlacionar falhas administrativas por request ID e código sem expor segredos.
- Source docs: `planning/edger/epics/08-valor-buntime/07-observabilidade-operacao-e-deploy.md`, `planning/edger/epics/08-valor-buntime/17-gateway-operational-diagnostics.md`, `planning/edger/docs/value-parity-matrix.md`.

## Files

| Arquivo | Ação | Motivo | Confiança |
|---|---|---|---|
| `crates/edger-orchestrator/src/operational_log.rs` | Criar | Centralizar log estruturado e redaction por omissão | core |
| `crates/edger-orchestrator/src/lib.rs` | Alterar | Expor módulo interno do orchestrator | core |
| `crates/edger-orchestrator/src/admin_api.rs` | Alterar | Emitir log em respostas de erro admin | core |
| `crates/edger-orchestrator/tests/security_operational.rs` | Alterar | Capturar log real de request admin e provar campos/redaction | core |
| `planning/edger/docs/value-parity-matrix.md` | Alterar | Marcar logging como testado quando houver evidência | core |
| `planning/edger/epics/08-valor-buntime/00-overview.md` | Alterar | Registrar Story 08.29 no backlog/status | core |
| `planning/edger/roadmap.md` | Alterar | Atualizar contagem da Fase 8 | core |
| `planning/edger/status/checkpoint-2026-06-29-epic-08-value-parity.md` | Alterar | Atualizar checkpoint de valor e lacunas restantes | core |
| `planning/edger/status/evidence/story-08-29-runtime.txt` | Criar | Registrar comandos reais e resultado da entrega | core |

## Detail

### Estado atual (AS-IS)
- Admin API retorna erro tipado e preserva `x-request-id` na resposta.
- Gateway mantém diagnósticos e logs recentes sem segredos.
- Não há teste que capture um evento `tracing` de erro administrativo e prove campos estruturados/redaction.

### Estado alvo (TO-BE)
- Erros do Admin API emitem um `warn` com target estável `edger.operational`.
- O log contém `surface="admin_api"`, `request_id`, `status` e `code`.
- O log não contém `Authorization`, API key bruta, corpo da request ou mensagem de erro.
- A matriz passa a tratar `Logging e warnings acionáveis` como `tested` para o escopo local atual.

### Escopo
- Inclui Admin API e helper compartilhável.
- Não inclui SSE, retenção histórica global, OTel exporter ou logs de todas as rotas runtime.
- Não altera semântica de resposta HTTP.

### Approach
- Criar helper `log_operational_error` que recebe superfície, status, código e request ID.
- Chamar o helper em `admin_error` usando `x-request-id` da request quando presente.
- Testar com subscriber local capturando o log de uma request admin não autenticada com header Authorization falso, garantindo campos úteis e ausência do segredo.

### Risks and dependencies
- Risco: log virar vazamento de dados. Mitigação: helper não recebe headers/body/mensagem; teste verifica ausência de segredo.
- Risco: múltiplos subscribers globais quebrarem testes. Mitigação: usar subscriber scoped com `tracing::subscriber::with_default`.

## Acceptance criteria
- [x] Erro do Admin API emite log `warn` com `surface`, `request_id`, `status` e `code`.
- [x] Log não contém header `Authorization`, token bruto, body ou mensagem de erro.
- [x] Teste captura log real de request HTTP pelo pipeline.
- [x] `Logging e warnings acionáveis` fica `tested` na matriz para o escopo local.

## Test-first plan
- Comportamento a provar: request admin sem credencial válida gera erro HTTP e log operacional correlacionável sem segredo.
- Primeiro teste falhando: capturar tracing em `admin_errors_preserve_request_id` esperando `edger.operational`, `request_id`, `status=401` e `code=UNAUTHORIZED`.
- Nível preferido: integração do orchestrator com `build_pipeline`.
- Valor do teste: contrato operacional e segurança de redaction.
- Testes de baixo valor a evitar: chamar helper diretamente sem passar por request real.

## Tasks
- [x] Criar helper de log operacional. **Done when:** campos estruturados ficam centralizados e não recebem dados sensíveis.
- [x] Integrar Admin API. **Done when:** todos os erros admin passam pelo helper.
- [x] Cobrir com teste de captura de tracing. **Done when:** teste falha se remover request ID/status/code ou vazar token.
- [x] Atualizar artefatos de paridade. **Done when:** matriz, overview, roadmap e checkpoint apontam Story 08.29.
- [x] Registrar evidência e closure. **Done when:** evidence/closure citam comandos reais.
- [x] Rodar verificação. **Done when:** Rust gate e planning gate passam.

## Verification
- [x] `cargo test -p edger-orchestrator --test security_operational admin_errors_preserve_request_id -- --exact`
- [x] `cargo test --workspace`
- [x] `cargo clippy --workspace -- -D warnings`
- [x] `cargo fmt -- --check`
- [x] `SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh`

## Recommended next step
- Continuar a próxima linha `must partial` da matriz.
