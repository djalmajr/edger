# Evidência 2026-07-11: captura segura de console dos workers

## Contrato entregue

- stdout e stderr do processo Deno persistente são drenados continuamente por tasks separadas.
- Cada linha aceita até 4 KiB e cada stream até 100 linhas por segundo.
- O canal até o orquestrador possui 1.024 entradas e usa enqueue não bloqueante.
- Drops acumulam no próximo registro aceito; truncamentos e drops também aparecem nas estatísticas do store.
- Registros carregam namespace, worker, versão, process ID, stream e timestamp.
- ANSI, controles, UTF-8 inválido, tokens e caminhos locais conhecidos são sanitizados antes do store.
- `EDGER_CONSOLE_LOGS_ENABLED=false` restaura o caminho sem captura de stdout.

## TDD e carga

O teste real inicia o Deno, escreve um segredo, uma linha com 5 KiB e mais de 300 linhas em burst, atravessa uma nova janela de rate limit e encerra graciosamente. Depois cria um segundo processo para a mesma versão.

```text
cargo test -p edger-isolation --features multiproc --test console_capture
1 passed; 0 failed; finished in 2.32s

cargo test -p edger-orchestrator --test observability_api
5 passed; 0 failed
```

O worker continuou respondendo HTTP 200 sob flood. A prova também confirmou mensagem de `beforeunload`, dois process IDs distintos após recycle, segredo redigido, linha truncada e drop contabilizado.

## Achado de segurança ao vivo

O primeiro tráfego real do fixture `commonjs` revelou que `console.log(require)` serializava caminhos absolutos `file://` e `/Users/...`. A sanitização central foi ampliada para paths conhecidos antes de considerar a história concluída. Depois do restart, a resposta da Admin API e o DOM do Browser não continham `file://`, `/Users/`, `secret-value` ou `authorization`.

## Browser e tráfego real

Com o runtime em `127.0.0.1:19080`, a rota
`/cpanel/workers/commonjs/latest/logs?source=console` mostrou:

- fonte `Worker console` preservada na query string;
- 92 linhas reais na tabela;
- mensagens atribuídas ao mesmo process ID do worker aquecido;
- nenhum segredo ou caminho absoluto no DOM;
- aviso explícito de retenção em memória, reset no restart e independência de OTEL.

O filtro exato por request ID, a rota e a fonte sobrevivem a refresh pela URL.
