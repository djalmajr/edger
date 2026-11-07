# ABI Wasm (EdgeR)

**Status:** ABI de request/response em linear memory e import HTTP
`edger_http_v1.fetch` implementados no `edger-isolation` no PR #179 (EDG-52 a
EDG-56), ainda sem release

**Origin:** `planning/edger/design.md` (Wasm standalone wasmtime + WASI)

**Veja também:** [sse.md](sse.md) (SSE mantido pelo host),
[sqld-managed.md](sqld-managed.md) (banco gerenciado),
[egress-byok.md](egress-byok.md)

## Modelo

- Wasm **standalone** via wasmtime; não é co-localizado no isolate JS.
- Entrypoint via `manifest.yaml` (`entrypoint: index.wasm` ou `index.wat`) ou
  bytes pré-carregados em teste.
- O host serializa a request HTTP para a memória linear do guest, chama um
  export do módulo e decodifica a response devolvida também pela memória linear.
- Cada request cria um `Store` e uma instância novos (`_initialize` roda de novo
  a cada vez). Memória e globals do guest não são reutilizados entre requests.
- O módulo compilado é reutilizado por um cache de conteúdo (SHA-256 dos bytes)
  que mora **dentro do isolate** (`WasmIsolate`/`WasmHttpHandler`), isto é, de
  uma instância do pool. Não há `Engine` global nem cache do wasmtime em disco.
  Veja [Custo do WASM efêmero](#custo-do-wasm-efêmero): o `ttl` decide se esse
  cache sobrevive entre requests.
- A resposta é bufferizada: o frame precisa estar completo na memória do guest
  quando `edger_handle` retorna. O guest não faz streaming. O SSE do EdgeR não
  mantém o `Store` vivo: o guest autoriza e publica em execuções curtas e o host
  segura a conexão (veja [sse.md](sse.md)).
- Imports de host: `wasi_snapshot_preview1` (linkado por `wasmtime-wasi` com
  contexto mínimo) e `edger_http_v1.fetch` (só com concessão por execução, veja
  [Import HTTP](#import-http-edger_http_v1fetch)). Qualquer outro import é negado.
- Em produção (fábrica do binário `edger`), o guest não tem: rede WASI (TCP, UDP
  e DNS desligados), stdio (stdout e stderr não são herdados), nem filesystem
  (nenhum preopen é adicionado; `allow_fs_read`/`allow_fs_write` não são lidos).
  Tem o `env` do manifest (todo o `env` não vazio entra, sem filtro por prefixo)
  e os `secrets:` declarados, por execução. Valores do cofre são privados ao app.

## Segredos do cofre

A fábrica vincula o resolvedor ao nome do `WorkerRef`. A cada execução, só os
nomes declarados são consultados; os valores entram diretamente no contexto
WASI do `Store` novo, sem alterar `WorkerConfig.env`, `publicEnv`, `WasiConfig`,
seu `Debug` ou o cache de módulos. Um valor resolvido substitui o `env` explícito
de mesmo nome, com uma única entrada no ambiente. Rotação e remoção são vistas
na requisição seguinte, mesmo com o módulo compilado em cache.

Um segredo ausente ou um registro que não decifra não injeta valor do cofre;
o worker continua com o `env` explicitamente configurado. O aviso usa só o
nome do worker e do segredo e sai uma vez por nome **por isolate**: com `ttl` 0,
cada requisição cria um isolate e repete o aviso. Sem cofre configurado, o aviso
sai uma vez por nome declarado, na criação do isolate. Nome inválido ou valor com
NUL retorna `WASM_SECRET_INVALID`; a mensagem cita o nome do segredo, nunca o
valor. Os valores entram no `env` WASI mesmo quando o `env` do manifest está
vazio, e prevalecem sobre ele. O módulo autorizado pode ler seus
segredos; quem pode instalar sua versão possui essa capacidade.

A resolução compartilha o prazo absoluto da execução. O resolvedor SQLite
usa trabalho bloqueante fora do executor assíncrono, com uma leitura por cofre
em voo. O cancelamento da requisição não interrompe uma leitura já iniciada:
ela retém a permissão até terminar. A cifra, os nomes, a chave e as permissões
do cofre seguem o contrato existente.

## Exports obrigatórios

| Export | Tipo | Descrição |
|---|---|---|
| `memory` | memory | Memória linear usada para request e response |
| `edger_alloc` | `(len: i32) -> i32` | Aloca `len` bytes no guest e retorna o ponteiro |
| `edger_handle` | `(ptr: i32, len: i32) -> i64` | Processa a request em `ptr..ptr+len` e retorna `(response_len << 32) | response_ptr` |

## Inicialização opcional de reactor

Um módulo reactor, como o gerado por Go com `-buildmode=c-shared`, pode exportar
`_initialize: () -> ()`. Após instanciar o módulo, o host chama esse export uma
vez, antes da primeira chamada a `edger_alloc` ou `edger_handle`, no mesmo
`Store` e com o mesmo orçamento de memória e epoch.

Módulos sem `_initialize` mantêm o caminho legado. Um export com esse nome que
não seja uma função com a assinatura esperada é recusado com `WASM_ABI`; trap
durante a inicialização retorna `WASM_EXEC`. O host não chama o export `_start`.

## Request frame

Todos os inteiros são little-endian. O host escreve:

| Offset | Tipo | Campo |
|---|---|---|
| `0` | `u32` | tamanho do método |
| `4` | `u32` | tamanho da URI/path recebido pelo worker |
| `8` | `u32` | tamanho dos headers serializados |
| `12` | `u32` | tamanho do body |
| `16..` | bytes | método, URI, headers JSON e body, nessa ordem |

Headers usam JSON `Vec<(String, String)>`, o mesmo formato lógico do
`SerializedRequest`. O body é omitido como zero bytes quando a request não tem
body.

## Response frame

O guest devolve um ponteiro e tamanho empacotados em `i64`. O host lê:

| Offset | Tipo | Campo |
|---|---|---|
| `0` | `u16` | status HTTP, de `100` a `599` |
| `2` | 2 bytes | reservado/padding |
| `4` | `u32` | tamanho dos headers serializados |
| `8` | `u32` | tamanho do body |
| `12..` | bytes | headers JSON e body, nessa ordem |

Headers usam JSON `Vec<(String, String)>` e passam por `validate_headers`. Body
zero vira `None`.

## Limites e validações

- Módulo deve começar com magic bytes Wasm (`\0asm`).
- Entrypoint `.wat` é compilado para Wasm antes da validação, para fixtures e
  exemplos de desenvolvimento.
- Módulo máximo: 16 MiB (caminho síncrono e assíncrono).
- Frame máximo de request: 256 KiB, incluindo o prefixo de 16 bytes, método,
  URI, headers JSON e body; acima disso, `WASM_ABI_ENCODE`. O `maxBodySize`
  (padrão 4 MiB) é validado antes, então um body de request só chega ao guest se
  couber nos 256 KiB: na prática, o teto do body de entrada é cerca de 256 KiB
  menos método, URI e headers.
- Headers serializados de response: no máximo 64 KiB, além das validações de
  nomes, valores e quantidade feitas por `validate_headers`.
- Body máximo de response: o `maxBodySize` efetivo do worker; padrão de 4 MiB
  (os 64 KiB antigos não existem mais). O limite do frame completo deriva de
  `12 + 64 KiB + maxBodySize` efetivo: com o padrão de 4 MiB, 12 + 65.536 +
  4.194.304 = 4.259.852 bytes. Frame acima
  disso dá `WASM_ABI`; body acima do cap, status fora de 100 a 599 ou tamanho do
  frame diferente de `12 + headers + body` dão `WASM_ABI_DECODE`.
- O host confere tamanhos, overflow, status, ponteiros e limites da memória
  linear antes de copiar e decodificar o frame de resposta.
- Memória linear: 512 MiB por instância, ou 128 MiB com `lowMemory`.
- Tabela funcref: 16.384 elementos por Store (uma tabela e uma memória por
  Store). Os campos `memoryMb`, `cpu*Ms` e `rssSoftMb` do manifest não afetam o
  WASM.
- Admissão: um semáforo único do processo, com `available_parallelism()`
  permissões (1 se desconhecido), compartilhado por todos os workers WASM e
  retido durante a compilação e a execução. Dentro de uma instância do pool, uma
  requisição por vez (`maxProcesses` padrão 1, `queueLimit` 8, `queueTimeout`
  1000 ms).
- Tempo de execução: interrupção por epoch, com uma thread `edger-wasm-epoch`
  por handler incrementando o epoch a cada 10 ms, e orçamento de
  `ceil(timeout / 10 ms)` ticks (mínimo 1) do `timeout` do worker (padrão 30 s).
  Inicialização, chamadas do ABI, resolução de segredos, admissão, compilação e
  chamadas ao import HTTP compartilham um único prazo absoluto, nunca renovado.
  **Fuel não é usado**: o engine só habilita epoch. A compilação de `Module` não
  é interrompida por esse mecanismo.
- Qualquer import fora de `wasi_snapshot_preview1` e de `edger_http_v1.fetch`
  (este só com a assinatura `(i32, i32, i32, i32) -> i64` e a concessão da
  execução) retorna `WASM_IMPORT_DENIED`. A checagem roda depois da compilação,
  também em acerto de cache. Uma função `wasi_snapshot_preview1` que o linker
  p1 não fornece falha na instanciação (`WASM_INSTANTIATE`).
- Imports `wasi:*` de component model/WASIp2 retornam `WASI_IMPORT_UNSUPPORTED`
  no caminho atual de core modules.

Runtime assíncrono (`execute_wasm` da feature `wasm`): um prazo absoluto, fixado
na entrada da chamada, cobre admissão, compilação, execução e awaits; o runtime
cede por epoch em checkpoints (`yield_now`) enquanto o prazo não passa, e um
timeout de `0` conserva o watchdog de um tick (10 ms). A compilação de
`Module::new` não é cancelável: roda offloadada (`spawn_blocking`) com a
permissão de admissão retida até terminar, e não há preempção prometida para
ela; um timeout descarta o `await`, mas a thread de compilação segue e segura a
permissão. Se o prazo total estoura (admissão, compilação ou execução), a chamada
do isolate responde `WASM_TIMEOUT`; traps de guest mantêm os códigos anteriores
(`WASM_INSTANTIATE`, `WASM_ABI`, `WASM_EXEC`). O embedding síncrono segue o
budget por epoch descrito acima.

Pelo pool, o timeout externo do worker (`execute_with_limits`) dispara antes do
prazo interno, que é arredondado para cima: o erro visto pelo chamador é
`TIMEOUT`, não `WASM_TIMEOUT`. Qualquer erro do isolate (trap, timeout,
`WASM_IMPORT_DENIED`, frame inválido) é tratado como crítico: a instância é
terminada e removida, e o cache de módulo dessa instância se perde. Na superfície
HTTP, um `WorkerError` vira `500 WORKER_ERROR`; o código do WASM aparece na
mensagem e no evento `dispatch`.

### Códigos de erro do backend WASM

`WASM_NOT_LOADED` (sem bytes), `WASM_PATH` (caminho absoluto ou com `..`),
`WASM_READ`, `WAT_COMPILE`, `WASM_INVALID` (sem magic `\0asm`), `WASM_TOO_LARGE`
(módulo acima de 16 MiB), `WASM_COMPILE`, `WASM_CACHE`, `WASM_IMPORT_DENIED`,
`WASI_IMPORT_UNSUPPORTED`, `WASI_LINK`, `WASM_HTTP_LINK`, `WASM_INSTANTIATE`,
`WASM_ABI`, `WASM_ABI_ENCODE`, `WASM_ABI_DECODE`, `WASM_EXEC`, `WASM_TIMEOUT` e
`WASM_SECRET_INVALID`.

## Import HTTP: `edger_http_v1.fetch`

Único caminho de saída de rede do guest. O módulo `edger_http_v1` exporta
`fetch(req_ptr: i32, req_len: i32, out_ptr: i32, out_cap: i32) -> i64`; em Go,
`//go:wasmimport edger_http_v1 fetch` (`workers/examples/go-wasm-http/abi.go`).

- Request: o mesmo formato do request frame, com URL absoluta no lugar da URI
  (prefixo de 16 bytes, método, URL, headers JSON, body). `req_len` entre 16 B e
  256 KiB; headers JSON até 64 KiB.
- Response: o mesmo formato do response frame (prefixo de 12 bytes, headers JSON,
  body), escrito em `out_ptr..out_ptr+out_cap`. `out_cap` mínimo de 14.
- Retorno: `>= 14` é o número de bytes escritos. Negativo, sem escrever nada:
  `-2` requisição inválida (limites de tamanho, faixa fora da memória viva, alvo
  ruim), `-3` resposta grande demais (acima de `min(maxBodySize, cap do frame -
  14)` ou headers acima de 64 KiB), `-4` falha de transporte (inclui destino
  bloqueado, DNS, upstream, timeout, redirects demais, limite de concorrência e
  acesso ao banco negado). `-1` existe no vocabulário, mas nenhum caminho de
  produção o devolve. Quando o orçamento da execução estoura durante a chamada,
  o import faz trap em vez de retornar.
- Sem contador de chamadas por execução; o limite efetivo é o `maxConcurrent` do
  guard (padrão 8 por worker e versão, recusa imediata, sem fila).

Concessão, avaliada **a cada execução** e de novo em cada chamada (inclusive com
o módulo em cache):

| Situação | Resultado |
|---|---|
| `egress` no manifest (`enabled: true`, com `allowNet` declarado, mesmo `[]`) e o guard do processo | Transporte geral: o guard do egress (HTTPS apenas, portas de `egress.ports`, padrão `[443]`, todo IP resolvido classificado, até 5 redirects revalidados). A lista `allowNet` não é consultada pelo guard; o manifest só exige que ela exista. |
| Sem `egress`, com o banco gerenciado configurado no processo (as sete `EDGER_SQLD_*`) | A fábrica do binário envolve **todo** worker WASM em `ManagedDatabaseIsolate::wasm`, que liga o transporte privado do banco: o import é admitido, mas só alcança a origem do banco (veja [sqld-managed.md](sqld-managed.md)). O geral nunca é selecionado. |
| Os dois | A origem do banco vai só ao transporte do banco; as demais URLs só ao geral. Sem fallback depois de erro. |
| Nenhuma concessão | `WASM_IMPORT_DENIED` ("host import edger_http_v1::fetch is not allowed"). |

Consequência prática: o módulo Go de `workers/examples/go-wasm-http` **importa
`edger_http_v1.fetch` sempre**, mesmo para `/inspect`. O que o manifest precisa
depende do modo do processo:

- **Com o banco gerenciado configurado** (`EDGER_SQLD_*`): o manifest do exemplo
  funciona como está, sem `allowNet` nem `egress`. A fábrica
  (`crates/edger-orchestrator/src/bin/edger.rs`, ramo `uses_wasm_assembly`) usa
  `ManagedDatabaseIsolate::wasm`, que admite o import pelo transporte privado. O
  E2E com o binário real (`tests/sqld_managed_cli_e2e.rs`,
  `go_wasm_app_database_works_end_to_end_and_survives_recycle_and_restart`) roda
  esse formato de manifest: sem vínculo, o guest responde `503` "configuration
  missing"; com vínculo, consulta o banco. O import só alcança a origem do
  banco; qualquer outro destino precisa de `egress` declarado.
- **Sem `EDGER_SQLD_*`**: o worker cai em `assemble_wasm_isolate`, que só admite
  o import com `egress` e o guard do processo. Um módulo que importa
  `edger_http_v1.fetch` precisa de `allowNet: []` e `egress.enabled: true` no
  manifest, como faz `go-wasm-fullstack`. Atenção: isso **concede egress HTTPS
  geral** ao guest. O guard não consulta `allowNet`; o guest alcança qualquer
  destino HTTPS que o guard aceite (portas de `egress.ports`, padrão `[443]`;
  todo IP resolvido classificado e os CIDRs bloqueados pelo operador; até 5
  redirects revalidados; os limites de tempo, tamanho e concorrência do guard).
  `allowNet: []` só significa que o Deno não tem rede direta; não restringe o
  guard. Sem o par, toda requisição falha com `WASM_IMPORT_DENIED` e, como o erro
  é crítico, a instância e seu cache de módulo se perdem a cada falha.

Sockets brutos seguem negados em ambos os modos. O guard também recusa a porta 9090 e qualquer redirect no caminho do
banco; detalhes do egress em [egress-byok.md](egress-byok.md).

## Custo do WASM efêmero

`kind: wasm` sem `ttl` no manifest recebe `ttl` 0 (o padrão de 300 s só vale para
`static-spa`, `fetch`, `routes` e `fullstack` com kind ou entrypoint declarado).
Com `ttl` 0, o pool cria uma instância e um isolate novos **a cada pedido**, com
`Engine` novo e cache de módulo vazio, e os descarta ao fim: o arquivo é relido e
o módulo é **recompilado a cada pedido**.

Com `ttl` explícito maior que zero, a instância fica ociosa com timer deslizante;
o primeiro pedido compila e os seguintes acertam o cache. O cache se perde por
expiração do `ttl`, `maxRequests`, `isolation: oneshot`, despejo LRU (capacidade
32), reciclagem ou qualquer erro do isolate. O prewarm (`minProcesses`) não
compila: a compilação acontece no primeiro pedido.

O que foi **medido** (perfil `dev`, sem release; módulo do exemplo `sse-go-wasm`
de 6,6 MB, 6.653.087 bytes no build de conferência, binário `edger` real, durante a
prova do SSE):

| Configuração | Pedido | Tempo |
|---|---|---|
| `ttl` 0 (padrão do kind) | cada um | cerca de 9 s (uniforme) |
| `ttl` explícito | primeiro | cerca de 9 s |
| `ttl` explícito | seguintes | cerca de 80 ms |

O custo em **release não foi medido** e não deve ser extrapolado dos números do
perfil `dev`. O módulo de `go-wasm-http` é maior (11.676.592 bytes no build de
conferência, ainda abaixo do limite de 16 MiB) e seu tempo **não foi medido**. A recomendação é declarar `ttl` em todo guest Go (por exemplo
`ttl: 10m`, como em `workers/examples/sse-go-wasm/manifest.yaml`). O `ttl` do
worker não é o `ttl` das assinaturas SSE (`sse.ttl`). O fullstack com
`adapter: wasm` tem `ttl` padrão de 300 s, porque seu kind é explícito.

## WASM no mesmo app da SPA (`kind: fullstack`, `adapter: wasm`)

```yaml
kind: fullstack
adapter: wasm
clientDir: dist
wasmBackend:
  entrypoint: backend/index.wasm
  apiPrefix: /api
```

- `adapter: wasm` vale só com `kind: fullstack` (ou `ssr`) e exige `clientDir`,
  `wasmBackend.entrypoint` (caminho relativo portável terminado em `.wasm` ou
  `.wat`) e `wasmBackend.apiPrefix`. `ssrEntrypoint` e `entrypoint` de topo não
  são usados. `apiPrefix` é absoluto, não é a raiz, sem barra final ou repetida,
  sem `.`/`..`, espaço, controle nem `%`, `?`, `#` ou `\`.
- O pacote tem `manifest.yaml`, `clientDir/` (com `index.html`) e o módulo. A
  instalação confere que o módulo e `clientDir/index.html` são arquivos regulares
  (symlink é recusado).
- Roteamento sobre o pathname bruto: `apiPrefix` e `apiPrefix/...` (por segmento;
  `/apix` não é API) vão ao WASM **com o prefixo removido** (`/api` vira `/`,
  `/api/inspect` vira `/inspect`; query, método, headers e body intactos). Um
  caminho fora do prefixo que, decodificado, cairia dentro dele (`/%61pi/x`,
  `/api%2Fx`) responde 400 e nunca serve a SPA.
- O resto é servido pelo host sem isolate, só `GET` e `HEAD` (outro método:
  `405` com `allow: GET, HEAD`). Componente de caminho começando por `.` dá 404.
  Arquivo ausente cai em `clientDir/index.html` com 200, **inclusive um `.js`
  ausente**; o `<base href>` é injetado. Há negociação `br`/`gzip` dos assets
  pré-comprimidos. O módulo `.wasm` nunca é servido como asset.
- É sempre bufferizado, com os mesmos limites de módulo, memória, tempo e HTTP
  desta página. `sse` em fullstack WASM exige que toda rota esteja sob
  `apiPrefix`.
- Exemplo: `workers/examples/go-wasm-fullstack` (Vite + o mesmo módulo Go; veja o
  README do exemplo para o build e o ZIP).

## Exemplo Go: build

`workers/examples/go-wasm-http`, a partir do diretório do exemplo, só stdlib, sem
rede e sem `go.sum` (Go 1.27.3 em `go.mod`):

```sh
GOOS=wasip1 GOARCH=wasm GOTOOLCHAIN=local GOWORK=off GOPROXY=off \
  go build -trimpath -buildmode=c-shared -ldflags='-s -w' -o index.wasm .
```

O build `c-shared` é um reactor: exporta `memory`, `edger_alloc`, `edger_handle`
e `_initialize`, e importa `edger_http_v1.fetch`. `index.wasm` não é versionado.
O artefato revisado dessa fonte tem SHA-256
`8062d9cbed8f48dd8f167d7805b2f83e30e401c0d3a072c9d82dc5f810f535b0`; um build novo
é outro build. O manifest do exemplo `go-wasm-http` declara `secrets` e `env` mas
**não** `allowNet`/`egress`: isso funciona com o banco gerenciado configurado no
processo e falha com `WASM_IMPORT_DENIED` sem as `EDGER_SQLD_*` (veja
"Import HTTP" acima). O `go-wasm-fullstack` e o `sse-go-wasm` já trazem
`allowNet: []` e `egress.enabled: true`, que funcionam nos dois modos e, por
isso mesmo, concedem egress HTTPS geral pelo guard (veja "Import HTTP").

## Fixture local

`workers/examples/wasm-hello/index.wat` é a fonte versionada do fixture. O runtime
compila `.wat` para bytes Wasm antes da validação, o que mantém o exemplo
auditável sem exigir toolchain Wasm no checkout. O fixture ecoa a URI recebida
como `wasm path: <uri>`, provando que a request chegou ao guest.

Para materializar `index.wasm` manualmente, veja
`workers/examples/wasm-hello/README.md`.

## Versionamento

_v0.3 — request/response em linear memory, import HTTP e fullstack WASM_
