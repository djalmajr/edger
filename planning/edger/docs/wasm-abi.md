# Wasm ABI (edger)

**Status:** ABI v1 mínima implementada na story `07.05`
**Origin:** `planning/edger/design.md` (Wasm standalone wasmtime + WASI)

## Modelo

- Wasm **standalone** via wasmtime + WASI (não co-localizado no isolate JS)
- Entrypoint via `manifest.yaml` (`entrypoint: index.wasm` ou `index.wat`) ou bytes pré-carregados em teste
- ABI v1 não recebe request em linear memory; retorna uma resposta estática do módulo
- Imports de host e WASI são negados por default no ABI v1

## ABI / exports

Exports obrigatórios para resposta com body:

| Export | Tipo | Descrição |
|---|---|---|
| `memory` | memory | Linear memory lida a partir do offset `0` |
| `http_status` | `() -> i32` | Status HTTP; default `200` se ausente |
| `http_body_len` | `() -> i32` | Número de bytes do body no offset `0`; default `0` se ausente |

Limites e validações:

- Módulo deve começar com magic bytes Wasm (`\0asm`).
- Entrypoint `.wat` é compilado para Wasm antes da validação, para fixtures e exemplos de desenvolvimento.
- Módulo máximo: 4 MiB.
- Body máximo: 64 KiB.
- Qualquer import externo retorna `WASM_IMPORT_DENIED`.
- Qualquer import WASI (`wasi_snapshot_preview1` ou `wasi:*`) retorna `WASI_IMPORT_DENIED` quando o sandbox está `deny_all`.

## WASI / env

- `WasiConfig::deny_all()` é o default.
- `WasiConfig::from_worker_config` filtra env sensível antes de qualquer futura injeção.
- Padrões bloqueados nesta fase: `AWS_*`, `DB_*`, `*_KEY`, `*_SECRET`.
- Host WASI real com preopen de worker root ainda é pendência; até lá, imports WASI são bloqueados.

## Versionamento

_v0.1 — foundation_
