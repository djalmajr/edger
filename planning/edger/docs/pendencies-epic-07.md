# Pendências Epic 07 — Fase 7 Avançado

**Origin:** `planning/edger/epics/07-avancado/00-overview.md`  
**Atualizado:** 2026-06-29

Documento dedicado para itens não resolvidos durante execução da Fase 7.

## Bloqueadores cross-cutting

| ID | Item | Bloqueia | Destino |
|---|---|---|---|
| E07-B01 | deno_core V8 platform boot | 07.04 completo | Story 03.04 carry-over; pin `deno_core` + toolchain |
| E07-B02 | `load_manifests_from_dirs` | 07.01, 07.02, 07.03 | Story 07.01 após backends parciais |

## Por story

### 07.04 Real JS execution — **not started**

- V8 singleton + op registration (facade Edge Runtime)
- `execute_fetch` / `execute_routes` / `serve_static_spa` production
- Fixtures `workers/js-*`
- Ver `spike.md` — go condicional, wire OK, boot pendente

### 07.05 Wasm execution — **in progress (v1)**

- [x] ABI mínima `http_status` + `http_body_len` + testes
- [ ] Load from worker dir, pool E2E, WASI sandbox, env filter
- Ver `status/checkpoint-2026-06-29-story-07-05-wip.md`

### 07.01 Manifests + kinds — **not started**

- Depende 07.04+07.05 integração ou gate explícito com mock

### 07.02 Shell routing — **not started**

### 07.03 Cron nativo — **not started**

### 07.06 OTEL — **not started**

### 07.07 Hardening + compat matrix — **not started**

- Turso auth, argon2 keys (carry from 06.02)
- Harness performance baselines