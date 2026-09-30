# Fechamento local: avisos do cPanel e preparação da 0.3.2

Data: 2026-09-29. Base: `17ec351` (main após a entrega rc.6). Branch local:
`fix/cpanel-act-warnings`. A final 0.3.2 não foi publicada.

## Entrega e plano

- [x] Corrigir os avisos React `act(...)` no teste isolado de API Keys.
- [x] Consolidar as notas rc.1–rc.6 em `CHANGELOG.md`, seção Unreleased.
- [x] Registrar a preparação final em `.agents/plans/0.3.2-final-preparation.md`.
- [x] Integrar localmente as duas fatias e obter revisão independente PASS.
- [ ] Validação visual autenticada do lab-dev após rc.6.
- [ ] Prova remota de PATCH em chave descartável, aguardando autorização específica.
- [ ] Preparar metadata/versões da final e executar gates de release.
- [ ] Publicação da final por PR, merge, tag e CI, mediante autorização.

## Arquivos e escopo

`workers/core/cpanel/src/components/api-keys.test.tsx`: o helper assíncrono
mantém `act` aberto durante a notificação inicial de React Query, agendada
por `setTimeout(0)`. Não muda produto, mocks ou assertions. Antes, o commit
inicial caía fora de `act` e disparava avisos de atualização do layout das
badges e dos estados vazios. A centralização no helper cobre os consumidores
atuais; mudanças futuras no agendamento da biblioteca exigem nova validação.

`CHANGELOG.md`, `.agents/plans/0.3.2.md` e o novo plano consolidam a entrega
com flags off por padrão, limites multirréplica, compatibilidade de métricas e
MSRV. Nenhuma versão Cargo/chart/manifest foi alterada. Este relatório é
registro de fechamento, adicional aos quatro arquivos funcionais planejados.
Sem mudança de banco, dependências, schema ou impacto de performance de produção.

## Provas executadas

| Verificação | Resultado |
|---|---|
| Bun isolado baseline/correção, reviewer | 8 testes, 49 assertions nos dois; avisos 18 → 0 |
| Bun isolado, orquestrador | 8 pass, 0 fail, 49 assertions, zero avisos act |
| Bun workspace, implementer e reviewer | 157 pass, 0 fail; reviewer zero avisos act |
| Vitest cPanel, implementer e reviewer | 13 arquivos, 140 testes passando |
| Typecheck cPanel, implementer e reviewer | `tsc --noEmit`, exit 0 |
| `git diff --check`, orquestrador/reviewer | PASS |
| Path preflight, orquestrador/reviewer | 310 referências, 0 ausentes |
| Refinement lint, orquestrador | PASS, 0 RED, 46 WARN existentes |

O Rust gate da rc.6 não foi repetido nesta fatia: Rust, dependências, chart e
runtime permanecem idênticos à base verificada. A publicação final terá seu
gate próprio. Não se alega inspeção visual, PATCH remoto ou nova CI nesta rodada.

Logs locais: `/tmp/edger-act-before-20260929.log`,
`/tmp/edger-act-after-20260929.log`, `/tmp/edger-act-orchestrator-20260929.log`,
`/tmp/rev5-before.log`, `/tmp/rev5-after.log`, `/tmp/rev5-all.log`,
`/tmp/rev5-vitest.log`, `/tmp/rev5-tc.log` e
`/tmp/edger-final-preparation-refinement-20260929.log`.

## Revisão e coordenação

Dois implementadores pi `applianceai01/qwen3.8-27b` high: `cpanel-qwen`
corrigiu o teste; `release-qwen` consolidou a documentação. O orquestrador
integrou as fatias e ajustou redação. Claude `claude-sonnet-5-5` high,
`review-cpanel-claude`, executou baseline/correção e suites: PASS, 0 P1/P2,
1 P3 de redação no plano. O P3 foi corrigido após a revisão; a descrição do
cookie também foi precisada para distinguir coorte opaca e bucket por app.

Relatórios scratch (não são artefatos versionados):
`/var/folders/f2/r857c16x45z6p82wsq_0d_v00000gp/T/herdr-soho/wF/reports/`:
`cpanel-qwen-20260929T162132.md`, `cpanel-qwen-20260929T171455.md`,
`release-qwen-20260929T163025.md`, `review-cpanel-claude-20260929T171650.md`.

O brief inicial citou `permission-badges.test.tsx`, que não existe: os testes
Badge vivem em `api-keys.test.tsx`. As tentativas em duas ordens executaram o
mesmo arquivo; não constituem prova de isolamento entre dois arquivos.
Nenhum teste foi removido para contornar isso.

## Pendências e limites

A validação visual depende de navegador autenticado. A prova com chave
remota descartável foi bloqueada pela revisão automática por criar e remover
uma credencial no lab-dev; aguarda autorização específica. A publicação da
0.3.2 final e qualquer upgrade adicional aguardam autorização do operador.
Nesta rodada não houve commit, push, PR, merge, tag ou deploy.
