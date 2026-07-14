# Wasm ABI (edger)

**Status:** ABI v2 request/response em linear memory implementada no
`edger-isolation`  
**Origin:** `planning/edger/design.md` (Wasm standalone wasmtime + WASI)

## Modelo

- Wasm **standalone** via wasmtime; não é co-localizado no isolate JS.
- Entrypoint via `manifest.yaml` (`entrypoint: index.wasm` ou `index.wat`) ou
  bytes pré-carregados em teste.
- O host serializa a request HTTP para a memória linear do guest, chama um
  export do módulo e decodifica a response devolvida também pela memória linear.
- Imports de host arbitrários continuam negados. Imports WASIp1
  `wasi_snapshot_preview1` são linkados por `wasmtime-wasi` com contexto mínimo:
  sem preopens de filesystem, sem rede por default, env filtrado quando
  permitido e stdout/stderr apenas quando `WasiConfig::allow_stdio` pedir.

## Exports obrigatórios

| Export | Tipo | Descrição |
|---|---|---|
| `memory` | memory | Memória linear usada para request e response |
| `edger_alloc` | `(len: i32) -> i32` | Aloca `len` bytes no guest e retorna o ponteiro |
| `edger_handle` | `(ptr: i32, len: i32) -> i64` | Processa a request em `ptr..ptr+len` e retorna `(response_len << 32) | response_ptr` |

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
- Módulo máximo: 4 MiB.
- Frame máximo request/response: 256 KiB.
- Body máximo de response: 64 KiB.
- Qualquer import externo fora de `wasi_snapshot_preview1` retorna
  `WASM_IMPORT_DENIED`.
- Imports `wasi:*` de component model/WASIp2 retornam `WASI_IMPORT_UNSUPPORTED`
  no caminho atual de core modules.

## Fixture local

`workers/examples/wasm-hello/index.wat` é a fonte versionada do fixture. O runtime
compila `.wat` para bytes Wasm antes da validação, o que mantém o exemplo
auditável sem exigir toolchain Wasm no checkout. O fixture ecoa a URI recebida
como `wasm path: <uri>`, provando que a request chegou ao guest.

Para materializar `index.wasm` manualmente, veja
`workers/examples/wasm-hello/README.md`.

## Versionamento

_v0.2 — request/response em linear memory_
