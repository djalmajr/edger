# Evidência — resiliência a refresh e streams (2026-07-11)

## Escopo validado

- Retry único de `GET`/`HEAD` após falha transitória `UDS_IO` ou
  `UDS_POISONED`; métodos não idempotentes não são repetidos.
- Cancelamento durante espera na fila remove imediatamente o waiter das
  métricas globais e por worker.
- `hello-world` e `read-body` aceitam refresh do navegador (`GET` sem body).
- `stream` e `sse` enviam o primeiro chunk imediatamente e declaram capacidade
  dedicada para conexões longas (`maxProcesses: 4`, fila limitada).
- O wrapper do `sveltekit-demo` serve os assets compilados a partir do diretório
  raiz real do worker, inclusive quando o entrypoint Node é empacotado em uma
  pasta temporária.

## Provas automatizadas

- Red/green: a regressão UDS retornava `500` antes do retry e passou a `200`;
  POST permaneceu com uma única tentativa.
- Red/green: o waiter cancelado aparecia como `queued: 1` por worker e passou a
  `queued: 0`.
- `cargo test -p edger-orchestrator --test streaming_e2e -- --ignored
  --nocapture`: 3/3 verdes, incluindo desconexão no meio do stream.
- Gate integral verde:
  `cargo test --workspace && cargo clippy --workspace -- -D warnings && cargo
  fmt -- --check`.

## Provas com tráfego real

- 50 refreshes sequenciais em cada endpoint (`express-demo`, `hello-world`,
  `read-body`): 150 respostas HTTP 200.
- 10 ciclos de abrir/cancelar em cada endpoint (`stream`, `sse`): 20 respostas
  HTTP 200, todas com body recebido.
- 8 streams concorrentes: 8 respostas HTTP 200; o pool abriu processos sob
  demanda.
- 4 streams ativos + 1 request cancelado na fila: `activeProcesses: 4`,
  `totalProcesses: 4`, `maxProcesses: 4`, `queued: 0`.
- Todos os 12 arquivos públicos compilados do `sveltekit-demo` responderam 200.
- Refresh real do cPanel após reinício do runtime preservou a sessão autenticada
  e exibiu o Overview sem redirecionar para login.

## Nota operacional

Uma resposta SSE/stream infinita permanece `pending` no painel de rede enquanto
a conexão estiver aberta; isso é o comportamento HTTP esperado. O sinal de
saúde é receber chunks incrementalmente e liberar/reciclar a capacidade quando
o cliente cancela — ambos foram verificados acima.
