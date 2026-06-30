# Story 08.26: Persistência de status de extensões

**Origin:** `planning/edger/epics/08-valor-buntime/00-overview.md`

## Context
- Problema atual: a matriz ainda marca `APIs de plugins/extensões` como `partial`; enable/disable já afeta hooks e providers em runtime, mas o overlay operacional se perde após reconstruir o registry.
- Objetivo de entrega: persistir o status operacional de extensões habilitadas/desabilitadas em arquivo JSON opcional, sem transformar isso em loader dinâmico de plugins.
- Restrições: não copiar o desenho interno do Buntime, não abandonar registro explícito v1, não misturar reload/rescan dinâmico nesta fatia e não acoplar o mecanismo a Turso remoto/sync.
- Referências: `planning/edger/docs/value-parity-matrix.md`, `planning/edger/epics/08-valor-buntime/13-extension-enable-disable-runtime.md`, `planning/edger/epics/09-providers-duraveis-externos/00-overview.md`.

## Traceability
- Rotas/telas de protótipo: não aplicável; superfície é Admin API.
- Regras de negócio: mutações de extensão continuam root-only e passam pelo mesmo guard CSRF/internal já existente.
- Source docs: `planning/edger/docs/value-parity-matrix.md`, `planning/edger/docs/extensions.md`, `docs/developers/06-operacao-e-testes.adoc`.

## Files

| Arquivo | Ação | Motivo | Confiança |
|---|---|---|---|
| `edger-orchestrator/src/registry.rs` | Alterar | Armazenar e recarregar overlay de status por extensão registrada | core |
| `edger-orchestrator/src/bin/edger.rs` | Alterar | Carregar o arquivo opcional via env no composition root | core |
| `edger-orchestrator/tests/admin_workers_plugins.rs` | Alterar | Provar o fluxo observável pela Admin API | core |
| `planning/edger/docs/value-parity-matrix.md` | Alterar | Atualizar evidência e lacuna remanescente | core |
| `planning/edger/epics/08-valor-buntime/00-overview.md` | Alterar | Registrar a nova story no backlog/status | core |
| `planning/edger/status/checkpoint-2026-06-29-epic-08-value-parity.md` | Alterar | Atualizar checkpoint e evidência | core |

## Detail

### Estado atual (AS-IS)
- `ExtensionRegistry::set_extension_enabled` atualiza um mapa em memória.
- O inventário de extensões mostra `enabled`/`disabled`.
- Recriar o registry perde o status desabilitado.

### Estado alvo (TO-BE)
- O registry pode carregar um status store JSON opcional.
- Uma mutação pela Admin API grava o status no store.
- Um registry reconstruído com o mesmo store preserva o status operacional.

### Escopo
- Inclui persistência de enable/disable de extensões registradas.
- Inclui configuração por `EDGER_EXTENSION_STATUS_FILE` e fallback via `EDGER_STATE_DIR/extension-status.json`.
- Não inclui discovery dinâmico, reload/rescan de crates, persistência de manifesto completo, UI, SSE ou marketplace.

### Abordagem
- Manter o registro explícito como fonte de verdade das extensões conhecidas.
- Persistir apenas nomes registrados no documento JSON.
- Ignorar entradas desconhecidas ao carregar, para não quebrar startup após remover uma extensão do composition root.
- Gravar o arquivo apenas quando um status store estiver configurado.

#### Decisões story-time

| Decisão | Escolha | Racional |
|---|---|---|
| Formato | JSON `{ "extensions": { "<name>": true/false } }` | Simples de auditar e suficiente para overlay operacional |
| Configuração | `EDGER_EXTENSION_STATUS_FILE`, com fallback em `EDGER_STATE_DIR` | Permite opt-in explícito e reaproveita diretório de estado local |
| Reload/rescan | Fora desta story | É uma capacidade maior e deve continuar como lacuna explícita |

### Riscos e dependências
- Se o arquivo não puder ser escrito, a mutação deve falhar com erro operacional em vez de fingir persistência.
- Se a extensão sumir do composition root, o status salvo deve ser ignorado no próximo boot.

## Acceptance criteria
- [x] Desabilitar extensão pela Admin API grava o status no JSON configurado.
- [x] Recriar o registry com o mesmo JSON mostra a extensão como `disabled`.
- [x] Sem status store configurado, o comportamento em memória atual permanece compatível.
- [x] A matriz registra que persistência de status está testada e que reload/rescan dinâmico ainda fica fora da fatia.

## Test-first plan
- Comportamento a provar antes da implementação: `POST /api/admin/extensions/gateway/disable` persiste status e o inventário reconstruído lê `disabled`.
- Primeiro teste falhando: teste de integração em `edger-orchestrator/tests/admin_workers_plugins.rs`.
- Nível preferido: integração de Admin API + unitário do registry.
- Valor do teste: contrato de API operacional e persistência local observável.
- Testes de baixo valor a evitar: snapshot grande do JSON, teste que só verifica que arquivo existe sem provar reload.

## Tasks
- [x] Mapear impacto e validar arquivos. **Done when:** registry, Admin API e binário foram lidos antes da edição.
- [x] Escrever o primeiro teste para o comportamento principal. **Done when:** teste cobre mutação Admin API e rebuild do registry.
- [x] Implementar persistência opcional. **Done when:** registry grava/carrega status store sem afetar modo em memória.
- [x] Atualizar matriz, overview, checkpoint e evidência. **Done when:** Story 08.26 aparece nos artefatos e lacunas remanescentes estão explícitas.
- [x] Rodar verificação. **Done when:** gates relevantes passam ou falhas ficam registradas com causa.

## Verification
- [x] `cargo test -p edger-orchestrator registry::tests::extension_status_store_survives_registry_rebuild --lib`
- [x] `cargo test -p edger-orchestrator --test admin_workers_plugins extension_enable_disable_status_persists_for_rebuilt_registry -- --exact`
- [x] `cargo test --workspace`
- [x] `cargo clippy --workspace -- -D warnings`
- [x] `cargo fmt -- --check`
- [x] `SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh`

## Recommended next step
- Após implementação: usar `/agile-status` em modo closure para registrar resultado e evidência.
