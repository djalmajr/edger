# SSE mantido pelo host (EdgeR)

**Status:** implementado no PR #179 (EDG-55), ainda sem release.
Este documento descreve o comportamento do código, com os limites reais. Onde o
contrato de desenho e o código divergem, vale o código.
**Prova:** `crates/edger-orchestrator/tests/sse_binary_e2e.rs` (binário `edger`
real, Deno real e Go/WASM real, `#[ignore]`, WSL Linux) e
`crates/edger-orchestrator/tests/sse_pipeline.rs` (pipeline, sem processo).

## Modelo de execução

O guest **autoriza** e **publica** em execuções curtas. O host mantém a conexão
do navegador. Nenhuma execução, vaga de processo, `Store` WASM ou task fica
retida por assinante: entre eventos, `activeRequest` é `null` em todo processo
de `/metrics/stats`.

- Assinar: o navegador faz `GET` em uma rota `subscribe`. O guest roda, confere
  a identidade pelos headers do pedido e responde `200` com o header
  `x-edger-sse-subscribe`. O host descarta o corpo do guest e responde com um
  fluxo `text/event-stream` que ele mesmo mantém.
- Publicar: qualquer método em uma rota `publish`. O guest roda, faz o seu
  trabalho e inclui headers `x-edger-sse-publish` na resposta. Depois que a
  execução termina de forma limpa com status 2xx, o host entrega cada evento aos
  assinantes do tópico.

O SSE passa por `WorkerPool::fetch_worker_completed`: o corpo inteiro é lido, a
produção termina e a vaga e a guarda são liberadas antes de o host decidir. Não
é o streaming do Deno (`ReadableStream`), que continua existindo e segura a vaga
até o fim da produção.

## Manifest

```yaml
sse:
  subscribe: ["/api/events"]   # obrigatório, 1 a 16 rotas
  publish: ["/api/rooms/*"]    # opcional, 0 a 16 rotas
  ttl: 5m                      # padrão 5m; de 5s a 1h; mesmas formas de `ttl`
  maxConnections: 256          # padrão 256; mínimo 1
```

| Campo | Regra |
|---|---|
| `subscribe` | Obrigatório, de 1 a 16 rotas. |
| `publish` | Opcional, no máximo 16 rotas. |
| `ttl` | Vida de cada assinatura, não do worker. Padrão 5 minutos, entre 5 s e 1 h. O TTL só é conferido no tick do heartbeat (`EDGER_SSE_HEARTBEAT_MS`, padrão 15 s): um `ttl` curto perto ou abaixo do heartbeat dura até o próximo tick, por exemplo `ttl: 5s` com o padrão pode chegar a cerca de 20 s. |
| `maxConnections` | Padrão 256. Zero é recusado. Sem teto no manifest; o host limita por `EDGER_SSE_MAX_CONNECTIONS_PER_APP`. |

Rotas são relativas ao worker e começam com `/`. Sem `*`, a comparação é de
igualdade exata (barra final não casa). Com `/*` no fim, casa por segmento:
`/a/*` casa `/a/b` e `/a/b/c`, não `/a`, `/a/` nem `/ab`. São recusados `:param`,
`*` fora do fim, caractere de controle, `%`, `?` e `#`. O caminho comparado não
passa por decodificação de percent-encoding: um caminho codificado não casa. Uma
rota pode estar nas duas listas: `GET` assina; os demais métodos seguem a regra
de `publish`. `HEAD` nunca assina nem publica.

Onde vale:

- `kind: fetch`, `routes`, `wasm` e `fullstack` sem WASM aceitam `sse`.
- `kind: static-spa` (inclusive o inferido) recusa a instalação.
- `kind: fullstack` com `adapter: wasm` exige `wasmBackend.apiPrefix`, e toda
  rota precisa estar sob ele, por segmento inteiro (`/api` e `/api/x` estão sob
  `/api`; `/apix` não).
- O parser do bloco `sse:` **não recusa chaves desconhecidas**: um erro de
  digitação como `maxConnection` é ignorado em silêncio e vale o padrão.

Manifest inválido recusa a instalação com mensagem estática, sem eco da
entrada.

## Assinatura

O guest autoriza com `200` e um único header
`x-edger-sse-subscribe: <topico>[,<topico>...]`.

| Item | Valor |
|---|---|
| Tópicos por assinatura | 1 a 8 |
| Formato do tópico | `[A-Za-z0-9._:-]{1,128}` |
| Nome do header | comparado sem diferenciar caixa |
| Espaço ou tab em volta do tópico | aparado; item vazio (`a,,b`) é inválido |
| Tópico repetido | aceito; o broker deduplica |
| Corpo do guest | descartado; até 64 KiB |

Qualquer outra resposta do guest (401, 403, 204, redirect, 200 sem o header) volta
ao cliente como resposta comum, sem assinatura, com os headers `x-edger-sse-*`
removidos. A autorização vem antes da capacidade: um token errado com o app cheio
recebe 401, não 503.

A resposta do host é `200`, `content-type: text/event-stream`,
`cache-control: no-store` e `x-accel-buffering: no`. Os demais headers do guest
são preservados, inclusive `set-cookie`, exceto `cache-control`,
`content-encoding`, `content-length`, `content-type`, `etag`, `last-modified`,
`transfer-encoding` e `x-edger-sse-*`. Nunca há `304` e o corpo não é comprimido.

O limite de 64 KiB do corpo vale para **qualquer** resposta do guest em rota
`subscribe`, antes de o status ser conhecido: um 401 com corpo maior que isso
vira `502 SSE_INVALID_DIRECTIVE`, em vez de passar como 401.

### Header repetido: Deno, WASM e bridge

- **Deno persistente:** a API `Headers` funde valores repetidos em um, separados
  por vírgula. Repetir `x-edger-sse-subscribe` vira, portanto, uma lista (mesmo
  teto de 8 tópicos, 9 dá 502).
- **WASM:** os pares chegam intactos; um segundo `x-edger-sse-subscribe` dá
  `502 SSE_INVALID_DIRECTIVE`.
- **Fallback `EDGER_JS_RUNTIME=bridge`:** recusa workers com `sse:` antes de
  executar o guest, com `SSE_RUNTIME_UNSUPPORTED` na mensagem de um
  `500 WORKER_ERROR`. Nenhuma diretiva chega ao broker.

O harness Deno separa `x-edger-sse-publish` em um par por diretiva antes de
enviar a cabeça ao host (base64url não contém vírgula). Ele não conhece o bloco
`sse` do worker, então faz isso para qualquer worker: sem `sse`, um
`x-edger-sse-publish` repetido sai em duas linhas em vez de uma com vírgula, o
que em HTTP é equivalente.

## Publicação

O guest inclui de 1 a 8 headers `x-edger-sse-publish` por resposta. Cada valor é
um JSON em base64url **sem padding** (`+`, `/` e `=` são recusados):

```json
{ "topic": "room.1", "data": "texto", "event": "msg", "id": "42" }
```

| Campo | Regra |
|---|---|
| `topic` | Obrigatório, mesmo formato dos tópicos de assinatura. |
| `data` | Obrigatório, UTF-8, sem NUL. CR e LF são permitidos e viram linhas `data:`. |
| `event`, `id` | Opcionais, até 128 bytes, sem CR, LF ou NUL. |
| Outros campos | Recusados, assim como campo duplicado. O JSON precisa abrir com `{`. |

A publicação só ocorre depois de a execução terminar de forma limpa **e** com
status 2xx. Status fora de 2xx descarta as diretivas sem erro. Todas as diretivas
são validadas antes da primeira entrega: uma inválida troca a resposta inteira
por `502 SSE_INVALID_DIRECTIVE` e nada é publicado. Os headers nunca chegam ao
cliente. O corpo da resposta do guest em rota `publish` pode ter até o
`maxBodySize` do worker; acima disso, `502 SSE_RESPONSE_TOO_LARGE`.

Se o broker está desligado ou em shutdown, a resposta do guest sai normal e as
diretivas **nem são analisadas** (uma diretiva inválida passa despercebida); os
headers `x-edger-sse-*` continuam removidos.

### Limite de cabeça e tamanho útil por diretiva

Assim que há uma diretiva presente, o host aplica à cabeça inteira da resposta
os limites de `validate_headers`: no máximo **100 headers**, **8192 bytes por
valor** e **65536 bytes** somando nomes e valores. Estouro dá
`502 SSE_INVALID_DIRECTIVE` e nada é publicado. Sem diretiva, o parser não
recusa nada.

Com 8192 caracteres por valor, o JSON decodificado tem no máximo 6144 bytes.
Medido nos testes:

- Tópico `t`, sem `event` nem `id`: o envelope tem 23 bytes e `data` chega a
  6121 bytes; 8196 é recusado (`sse_directive.rs`, testes unitários).
- Tópico `room.1`, sem `event` nem `id`: o envelope tem 28 bytes e `data` de
  6116 bytes é entregue por Go e por Deno (`sse_binary_e2e.rs`).
- Oito diretivas de cerca de 4 KB de `data` são entregues íntegras. Oito valores
  de 8192 bytes somam 65688 e estouram o limite total; sete passam.

`event`, `id`, o nome do tópico e o escape JSON (aspas, barras, acentos) reduzem
o tamanho útil. Evento maior não é suportado: publique um aviso e deixe o
cliente buscar o detalhe.

No **WASM**, o backend rejeita a cabeça acima do limite ao decodificar a
resposta do guest, antes do parser de SSE. O resultado é `500 WORKER_ERROR`, não
`502 SSE_INVALID_DIRECTIVE`; só o Deno chega ao parser (o teste de Go confere
apenas `status >= 400`).

## Escopo

`SseScope = (nome integral do worker, namespace RBAC)`, do `WorkerRef` final. A
**versão não entra**: durante um rollout o publicador de uma versão alcança
assinantes conectados por outra. Apps diferentes nunca compartilham tópico,
mesmo com nomes iguais. O guest não escolhe o escopo (a diretiva não tem esse
campo). Tenant e usuário ficam por conta da app, codificados no tópico que ela
autoriza. Headers `x-edger-sse-*` do pedido do visitante são removidos antes do
guest em worker com `sse`. O controle (`invoke_worker`) nunca assina nem publica.

## Broker

Uma instância por processo do `edger`, com estado em memória.

| Aspecto | Comportamento |
|---|---|
| Fila por assinante | Até 64 eventos e até 256 KiB. Estourar qualquer um retira o assinante (`lagged`), esvazia a fila e encerra o stream sem entregar o restante. |
| Heartbeat | Comentário `: ping` a cada `EDGER_SSE_HEARTBEAT_MS` (padrão 15 s). Um cliente parado enche a fila com os pings e cai por `lagged`. |
| TTL | Verificado **só no tick** do heartbeat. Ao vencer, o assinante recebe `retry: <1000..5000>` (jitter uniforme, em ms) e o stream termina. O `EventSource` reconecta e o guest reautoriza. |
| Ordem | Por tópico, na ordem de chamada no processo. |
| Entrega | No máximo uma vez. Sem durabilidade nem ordenação global. |
| Shutdown | Encerra todos os streams e recusa novos. A drenagem HTTP do encerramento é limitada por `EDGER_HTTP_DRAIN_MS` (veja [Encerramento](#encerramento)). |

## Limites que o operador precisa conhecer

1. **Cliente que parou de ler segura a vaga.** A vaga de capacidade só volta
   quando o hyper descarta o corpo, isto é, quando o socket acaba. Um cliente
   parado é retirado por `lagged` e deixa de consumir fila, mas o TTL **não**
   libera a vaga enquanto o TCP durar (um cliente parado continua segurando a vaga
   enquanto a conexão existir, mas deixou de impedir o encerramento do processo
   além de `EDGER_HTTP_DRAIN_MS`). A fila é liberada na hora (`lagged`,
   shutdown, descarte). Quando o TTL vence com a fila cheia, o `retry:` não cabe e
   a saída conta como `lagged`, não como `ttl`. A aceitação deste limite é uma
   decisão de desenho: um `TCP_USER_TIMEOUT` mudaria todas as conexões do data
   plane. O custo é limitado pelo teto por app; a prova é
   `go_wasm_stalled_client` e `deno_stalled_client`, em que a vaga volta até 30 s
   depois de o TCP do cliente acabar.
2. **Revogação vale na próxima reconexão.** Desabilitar ou apagar o worker,
   rotacionar um segredo e reciclar processos **não** fecham assinaturas abertas.
   Uma assinatura só cai pelo próprio TTL. A latência de revogação é o `ttl` mais
   o intervalo do tick; com os padrões, um `ttl: 5s` pode durar até cerca de 20 s.
3. **Broker por processo.** Sem fan-out entre réplicas: um pod só alcança os
   assinantes do próprio pod. Quem publica no pod A não chega a quem assinou no
   pod B.
4. **Sem histórico nem replay.** `Last-Event-ID` chega ao guest como header comum
   e o host não o interpreta; nada é retomado. A app deve reconstruir o estado ao
   reconectar.
5. **Cabeça com diretiva presente:** 100 headers, 8192 bytes por valor e 65536 no
   total; estouro dá 502 (Deno) ou 500 (WASM). Veja a seção acima.
6. **`x-edger-sse-subscribe` repetido:** no Deno vira lista; no WASM dá 502. O
   fallback `EDGER_JS_RUNTIME=bridge` recusa workers que declaram `sse:`.
7. **Respostas de erro do SSE não emitem o evento `dispatch`.** As quatro
   respostas próprias do SSE (`SSE_INVALID_DIRECTIVE`, `SSE_RESPONSE_TOO_LARGE`,
   `SSE_CAPACITY`, `SSE_UNAVAILABLE`) retornam antes do evento operacional
   `dispatch`, da linha de log `log_dispatch_event` e do registro em
   `worker_errors`. Só resta o log `info` "worker dispatch" e o `warn` da
   diretiva inválida. Uma assinatura bem-sucedida emite `dispatch` (status 200,
   `text/event-stream`), e as respostas comuns do guest (401, 200 sem header)
   também. Falhas de execução (`CompletedTimeout`, `CompletedStreamFailed`)
   emitem o `dispatch` de erro e respondem `500 WORKER_ERROR`.
8. **O ciclo de vida do worker é independente.** O `ttl` do worker (WASM) e o
   `sse.ttl` são coisas distintas; veja
   [wasm-abi.md](wasm-abi.md#custo-do-wasm-efêmero) sobre `ttl` em guest Go.

## Erros

Corpo JSON `{"code","message"}`.

| Código | Status | Quando |
|---|---|---|
| `SSE_INVALID_DIRECTIVE` | 502 | Header de assinatura inválido ou repetido (WASM); diretiva de publicação inválida; cabeça acima do limite com diretiva presente; corpo de guest em rota `subscribe` acima de 64 KiB. |
| `SSE_RESPONSE_TOO_LARGE` | 502 | Corpo em rota `publish` acima do `maxBodySize`. |
| `SSE_CAPACITY` | 503 | Teto por app ou do processo cheio. Inclui `Retry-After: 5`. |
| `SSE_UNAVAILABLE` | 503 | Broker desligado (`EDGER_SSE_MAX_CONNECTIONS=0`) ou em shutdown, depois de o guest já ter autorizado. |
| `WORKER_ERROR` | 500 | Prazo, falha de stream ou qualquer outro erro de execução do guest. |

Se a execução de uma assinatura ou de uma publicação por `GET` falha por
`UDS_IO` ou `UDS_POISONED`, ela é repetida uma vez; a entrega ocorre uma única
vez, a partir da execução que completou.

## Variáveis

Lidas uma vez no boot. Valor inválido (não numérico, abaixo do mínimo, texto) gera
`warn` "invalid SSE limit; using the default" e vale o padrão daquela variável.
Não há teto superior.

| Variável | Padrão | Mínimo | Observação |
|---|---|---|---|
| `EDGER_SSE_MAX_CONNECTIONS` | 1024 | 0 | Total no processo. `0` mantém o broker registrado, mas desligado: toda assinatura autorizada dá `503 SSE_UNAVAILABLE` e as diretivas de publicação são descartadas. |
| `EDGER_SSE_MAX_CONNECTIONS_PER_APP` | 256 | 1 | Teto sobre o `maxConnections` do manifest: vale o menor dos dois. |
| `EDGER_SSE_HEARTBEAT_MS` | 15000 | 1000 | Também é a granularidade do TTL. |

## Métricas e logs

Em `/metrics`, sempre registradas (mesmo com o broker em `0`); a linha de um
worker aparece depois do primeiro `subscribe` ou `publish` dele. O rótulo
`worker` é o nome do worker, sem namespace.

- `edger_sse_connections{worker}` (gauge; cai quando o stream é descartado).
- `edger_sse_events_published_total{worker}` (um por evento, não por assinante;
  conta mesmo sem assinantes).
- `edger_sse_disconnects_total{worker,reason="client|ttl|lagged|shutdown"}`.
- `edger_sse_rejected_total{worker,reason="capacity|unavailable"}`.

Logs: `info` "sse subscriber lagged" (só `worker`), `warn` para evento com CR/LF
em `event`/`id` (descartado), `warn` para evento maior que a fila (descartado,
inalcançável por diretivas) e `warn` "invalid SSE publish/subscribe directive"
(`request_id`, `worker_name`). Nenhum log ou evento operacional carrega tópico ou
`data`.

## Exemplos

Os três exemplos estão em `workers/examples/`: `sse-deno` (`kind: fetch`),
`sse-go-wasm` (`kind: wasm`) e, como contraste, `sse` (streaming Deno comum, sem
bloco `sse:`). Os dois primeiros autorizam com o segredo `SSE_ACCESS_TOKEN`.

### Go (`kind: wasm`)

Manifest (`workers/examples/sse-go-wasm/manifest.yaml`, trecho; o par
`allowNet: []` + `egress.enabled` só é necessário sem o banco gerenciado
configurado e **concede egress HTTPS geral** pelo guard, veja
[wasm-abi.md](wasm-abi.md#import-http-edger_http_v1fetch)). O `ttl` do
worker mantém uma instância e o módulo compilado em cache; sem ele, cada pedido
recompila o módulo (veja [wasm-abi.md](wasm-abi.md#custo-do-wasm-efêmero)):

```yaml
kind: wasm
entrypoint: index.wasm
allowNet: []
egress:
  enabled: true
secrets:
  - SSE_ACCESS_TOKEN
ttl: 10m
maxProcesses: 1
sse:
  subscribe: ["/events"]
  publish: ["/publish"]
  ttl: 5m
  maxConnections: 256
```

Autorizar e publicar (`main.go`, trecho):

```go
func eventsHandler(w http.ResponseWriter, r *http.Request) {
	if !authorized(r) {
		writeStatic(w, http.StatusUnauthorized, staticBody401)
		return
	}
	w.Header().Set("x-edger-sse-subscribe", "room.1")
	writeStatic(w, http.StatusOK, []byte(`{"subscribed":true}`))
}

// Em uma rota publish, depois do trabalho da app:
directive, _ := json.Marshal(controlEvent{Topic: "room.1", Data: "oi"})
w.Header().Add("x-edger-sse-publish", base64.RawURLEncoding.EncodeToString(directive))
```

Build do guest (a partir de `workers/examples/sse-go-wasm`, só stdlib, sem rede):

```sh
GOOS=wasip1 GOARCH=wasm GOTOOLCHAIN=local GOWORK=off GOPROXY=off \
  go build -trimpath -buildmode=c-shared -ldflags='-s -w' -o index.wasm .
```

O `index.wasm` não é versionado. A prova no binário exige um arquivo
`<módulo>.provenance` ao lado do módulo (SHA-256 de `abi.go`, `main.go`, `go.mod`
e do módulo, mais a linha `build:` igual à constante do teste, que usa
`-o sse-go-wasm.wasm`); a variável é `EDGER_GO_WASM_SSE_FIXTURE`.

### Deno (`kind: fetch`)

```ts
// workers/examples/sse-deno/index.ts (trecho)
function base64Url(text: string): string {
  const bytes = new TextEncoder().encode(text);
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

export default {
  async fetch(request: Request): Promise<Response> {
    const url = new URL(request.url);
    if (url.pathname === "/events" && request.method === "GET") {
      if (!authorized(request)) return json(401, { error: "unauthorized" });
      return new Response('{"subscribed":true}', {
        headers: { "content-type": "application/json", "x-edger-sse-subscribe": "room.1" },
        status: 200,
      });
    }
    if (url.pathname === "/publish") {
      if (!authorized(request)) return json(401, { error: "unauthorized" });
      const headers = new Headers({ "content-type": "application/json" });
      headers.append("x-edger-sse-publish", base64Url(JSON.stringify({ topic: "room.1", data: "oi" })));
      return new Response("{}", { headers, status: 200 });
    }
    return json(404, { error: "not found" });
  },
};
```

Cliente (navegador):

```js
const source = new EventSource("/meu-app/events");
source.onmessage = (e) => console.log(e.data);
```

O `EventSource` reconecta sozinho quando o TTL encerra o stream; cada reconexão
roda o guest de novo e reautoriza.

## Encerramento

Na parada do processo (SIGTERM ou SIGINT), o broker encerra todos os streams e
recusa assinantes novos, e o servidor drena as conexões HTTP restantes. Essa
drenagem tem limite de tempo, contado a partir do sinal:

- `EDGER_HTTP_DRAIN_MS` (padrão `10000`): `0` desliga o limite e a espera passa a
  ser sem fim, como antes. Valor inválido gera `warn` e vale o padrão.
- Ao expirar, as conexões que sobraram morrem com o processo, depois de um `warn`;
  o cron e o pool encerram normalmente e a saída é com sucesso.
- Vale para **todas** as conexões HTTP, não só as do SSE: um cliente parado que
  segura uma conexão não impede mais o encerramento além do limite.

Os prazos padrão do encerramento somam a drenagem HTTP (até 10 s), o cron (7 s: 5
de graça, 1 de abort e 1 de margem) e o pool (até 15 s): 32 s. Essa soma não é um
teto garantido: depois de abortar o supervisor do cron, o processo espera ele
terminar sem prazo. Veja
[deployment-k8s.md](deployment-k8s.md#sse-wasm-e-banco-gerenciado) sobre o
`terminationGracePeriodSeconds`.

## Execução

Um timeout, uma falha de stream ou qualquer outro erro de execução curta do guest
(assinatura ou publicação) responde `500 WORKER_ERROR` e emite o `dispatch` de
erro; nada é assinado nem publicado.

No caminho curto, uma resposta do guest cortada por `EDGER_STREAM_MAX_BYTES`
conta como execução **sem sucesso**: responde `500 WORKER_ERROR`, não assina e não
publica, e um corpo com exatamente esse tamanho também é recusado. O caminho comum
(resposta que o host repassa ao cliente) continua com o corte limpo: o corpo é
truncado e o frame de fim ainda é escrito.

O fallback `EDGER_JS_RUNTIME=bridge` recusa qualquer execução de guest de um
worker que declara `sse:`, inclusive em rotas comuns do mesmo worker, antes de
ler arquivos, empacotar ou iniciar o guest. A resposta é `500 WORKER_ERROR`, com
`SSE_RUNTIME_UNSUPPORTED` e a mensagem estática `SSE is not supported by this runtime`
na mensagem do erro de isolamento, no mesmo formato do banco gerenciado quando
o runtime não é suportado. Nada é assinado ou publicado e o broker não muda.
Workers sem `sse:` mantêm o comportamento do bridge, inclusive a captura parcial
de corpos `text/plain` e `text/event-stream`; Deno persistente e WASM seguem
inalterados. Prova: os testes `bridge_*` em `tests/sse_pipeline.rs` (os dois que
executam Deno real são `#[ignore]`, usam somente loopback e devem ser rodados com
`--include-ignored`).

A produção é bufferizada nos backends WASM e em `ttl: 0`; nesses casos o teto de
memória do caminho é o `maxBodySize` do worker, e o limite da rota decide só se
a resposta é aceita. No backend de processo (Deno), o corpo é lido por inteiro
com limite (64 KiB em `subscribe`, `maxBodySize` em `publish`) e o prazo é o
`timeout` do worker, contado da chegada da cabeça. Assets servidos pelo host
(static-spa, assets de fullstack) nunca contam como execução do guest: o pedido
segue o caminho comum.
