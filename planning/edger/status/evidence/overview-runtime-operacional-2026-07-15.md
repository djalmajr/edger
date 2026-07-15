# Evidência — Overview operacional compreensivo

Data: 2026-07-15

## TDD

- Red: `bun test src/lib/overview.test.ts` falhou com
  `Cannot find module './overview'` antes da implementação.
- Green: `bun test` passou com 6 testes, incluindo 3 casos do resumo
  operacional (Critical, Degraded sem penalizar Unobserved e ranking).
- Build: `bun run build` passou com TypeScript e Vite.

## Gates

- `planning/edger/scripts/cpanel-ui-gate.sh`: passou (`cpanel-ui-gate ok`).
- `cargo test --workspace`: passou.
- `cargo clippy --workspace -- -D warnings`: passou.
- `cargo fmt -- --check`: passou.

## Browser

URL: `http://127.0.0.1:19080/cpanel/`

- Overview renderizou status `Critical` na headline com dados reais do runtime
  e polling de cinco segundos; os metadados redundantes de freshness foram
  removidos após validação visual.
- Headline exibiu 32 apps, 37 versões, 17 requests em cinco minutos, 0 erros e
  p95 de 340 ms no snapshot observado.
- Distribuição exibiu 2 Healthy, 1 Failing e 2 Unobserved.
- O item acionável de `cron-worker@1.0.0` navegou para
  `/cpanel/workers/cron-worker/1.0.0/observability`.
- Os itens acionáveis de `Needs attention` exibem um chevron alinhado à direita
  e centralizado verticalmente para comunicar a navegação contextual.
- `View logs` navegou para `/cpanel/observability/logs`.
- Refinamento final confirmou cinco atividades recentes, seis workers na
  tabela (com fallback `Unobserved` para catálogo sem métricas) e ausência do
  CTA redundante `Open Observability`.

Os valores são uma fotografia da execução local e podem mudar no próximo
polling; a prova relevante é a composição, a janela declarada e os destinos das
ações.
