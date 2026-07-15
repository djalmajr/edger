# Evidência — ações primárias no topo do conteúdo do cPanel

Data: 2026-07-15

## Implementação

- O Shell renderiza Refresh e expõe um slot por Context + portal React na faixa
  superior do conteúdo.
- Workers mantém o estado do DeployDialog e envia `Deploy app` à faixa.
- Files mantém input, mutation e ref locais e envia `Upload files` à faixa
  somente quando o worker é mutável.
- Tabs compartilham a faixa à esquerda; filtros, breadcrumbs, ações de
  linha/card, dialogs e painéis continuam contextuais.
- O header fica reservado para idioma, tema e conta.

## Browser

Validação em `http://127.0.0.1:19080`:

| Rota validada | Header | Topo do conteúdo |
|---|---|---|
| `/cpanel/workers` | Idioma, tema, conta | Refresh, Deploy app |
| `/cpanel/observability/logs` | Idioma, tema, conta | Tabs, Refresh |

Em Logs, as tabs ficam à esquerda e `Refresh` à direita na mesma faixa. Em
Workers, `Refresh` e `Deploy app` aparecem apenas uma vez nessa faixa.

## Gates

- `bun test`: 9 testes passaram.
- `bun run build`: passou.
- `planning/edger/scripts/cpanel-ui-gate.sh`: passou.
- Refinamento: `0 RED`.
- `cargo test --workspace`: passou.
- `cargo clippy --workspace -- -D warnings`: passou.
- `cargo fmt -- --check`: passou.
- `git diff --check`: passou.
