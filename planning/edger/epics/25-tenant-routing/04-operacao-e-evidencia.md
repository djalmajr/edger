# Story 25.04: operação, visibilidade e fechamento

**Origin:** `planning/edger/epics/25-tenant-routing/00-overview.md`.

## Context

O operador precisa configurar a restrição de tenant e o split, entender qual versão recebe tráfego e reverter com segurança. Esta história consome APIs e contratos testados nas histórias 25.01–25.03; não cria um segundo modelo de configuração no cPanel.

## Traceability

TR-04, TR-07, TR-08, TR-10 e TR-11 do [epic](00-overview.md); cPanel em `workers/core/cpanel/`; APIs administrativas em `crates/edger-orchestrator/src/admin_api.rs`; observabilidade em `crates/edger-orchestrator/src/{metrics,operational_log}.rs`; chart Rancher em `charts/edger/`. Não há protótipo; derivar UI do sistema existente e revisar estados vazios, carregamento, erro e confirmação de rollback.

## Files

| Caminho | Ação | Motivo |
|---|---|---|
| `workers/core/cpanel/src/lib/api.ts` | editar | Cliente tipado para ler/aplicar política. |
| `workers/core/cpanel/src/main.tsx` | editar | Configuração e estado da política no detalhe de app, após desenho de UX. |
| `crates/edger-orchestrator/src/metrics.rs` | editar | Contadores agregados do gate, sem slug/cookie/hostname como label. |
| `charts/edger/{values,questions}.yaml` e `charts/edger/templates/` | editar | Flags independentes no formulário Rancher e Secret Tenancit condicional. |
| `planning/edger/docs/tenant-routing.md` | criar | Contratos, exemplos, limites, recuperação e segurança. |
| `planning/edger/status/evidence/tenant-routing-2026-09-26.md` | criar | Comandos e resultados locais, estado externo não verificado. |

## Detail

Exibir política configurada e elegibilidade de versão. A UI não chama uma política de efetiva sem sinal real das flags no processo. Fluxo de edição valida pesos e slugs antes de enviar, mas o backend revalida tudo. Rollback remove split ou aplica 100% à versão de recuperação, deixando claro que sessões podem remapear. Métricas do pool já contabilizam requests por app/versão; novos contadores agregam allowed, denied e indisponibilidade do gate sem cardinalidade por tenant, cookie ou hostname. O formulário Rancher mostra duas flags false por padrão; endpoint e referência ao Secret existente só aparecem e são obrigatórios quando tenant é ligado. O guia cobre token `tenant:identify` de escopo mínimo, segredo entregue pelo ambiente, limites de `ETag`/rate limit, backup do arquivo de política, single-node vs réplica, teste local e remoção rápida de política. Um smoke real exige autorização específica para deploy; esta entrega só prova localmente.

### Acceptance criteria

- Operador consegue ler, aplicar e remover política no cPanel com erros e confirmação claros.
- Setup Rancher instala com as flags off sem configurar Tenancit; com tenant on exige endpoint e Secret existente sem guardar token em values.
- Métricas e logs permitem verificar o split e diagnosticar deny sem cookies/tokens ou labels de alta cardinalidade.
- Guia e evidência distinguem testes locais de publicação e oferecem rollback reproduzível.

## Test-first plan

1. Teste do cliente/UI para leitura, validação, erro e rollback.
2. Teste de métrica para ambos os resultados de split e negação, sem informação sensível.
3. Smoke local roteia duas versões, nega tenant diferente e reverte política.

## Tasks

- [x] Desenhar e revisar controles do cPanel com estados de erro e confirmação.
- [x] Integrar API tipada e testes de fluxo.
- [x] Expor métricas de baixa cardinalidade e documentação de alerta.
- [x] Registrar passo a passo reproduzível de configuração, rollback e restauração.
- [x] Executar gate Rust, UI aplicável, refinement Mode 1 e revisão de outra família.
- [x] Validar no browser local o formulário do cPanel com uma política real e sua reversão.
- [ ] Validar no formulário Rancher os campos condicionais e a instalação com as duas flags desligadas.
- [x] Medir latência local e limite de chamadas do identify com servidor Tenancit real, PostgreSQL e Valkey descartáveis.
- [ ] Validar latência e limite de chamadas sob carga no ambiente alvo antes de disponibilizar apps restritos.
- [ ] Conferir contadores e logs de allowed, denied e split após instalação em cluster autorizado, sem labels de tenant ou coorte.

## Verification

```bash
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
```

Rodar os testes específicos Rust/JS existentes, o gate de planejamento com relatório Mode 1 em `SCRATCH`, e smoke local com evidência de respostas e política. Browser interativo e produção são provas separadas, reportadas apenas se executadas.

**Estado:** implementada e revisada localmente, inclusive cPanel no browser e integração com Tenancit real local; formulário Rancher, carga e observabilidade no cluster e publicação ainda não verificados.
