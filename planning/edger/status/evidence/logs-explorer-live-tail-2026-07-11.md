# Evidência — explorador de logs e live tail (2026-07-11)

## Gates automatizados

- `cargo test -p edger-orchestrator --test observability_api`: paginação, filtros, autorização e sanitização.
- `cargo test -p edger-orchestrator --test observability_sse`: autorização root-only, resume e filtros SSE.
- `planning/edger/scripts/cpanel-ui-gate.sh`: aprovado após explorador global, diálogo e correção de z-index do Select.

## Browser real

- `/cpanel/observability/logs` exibiu a navegação global e 100 eventos iniciais.
- `Load older` expandiu 100 para 194 eventos sem IDs duplicados.
- Filtro `worker=cpanel` persistiu na URL e retornou somente essa identidade.
- Abrir o evento `227` gerou `?event=227`; após refresh o diálogo foi restaurado com dados allowlisted.
- Live tail scoped recebeu tráfego real sem refresh, pausou e retomou pelo cursor sem duplicar eventos.
- O Select usa `z-index: 50`; hit-test na área antes encoberta retornou `select-item` (`Error`), acima do alert.

## Limites confirmados

- Store: 2.000 global / 200 por identidade.
- Página: 100 eventos; live view: 200 linhas.
- Broadcast: 256 notificações; perda de wake-up não perde a autoridade do store e atraso vira gap explícito.
- Nenhum Collector, endpoint OTLP ou backend externo esteve presente durante a jornada.
