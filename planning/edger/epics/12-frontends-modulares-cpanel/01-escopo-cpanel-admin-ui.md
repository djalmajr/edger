# Story 12.01: Escopo cPanel/admin UI

**Origin:** `planning/edger/epics/12-frontends-modulares-cpanel/00-overview.md`

## Context

O Buntime tem cPanel e outras superficies de operacao. O edger precisa de uma UI administrativa, mas ela deve nascer dos contratos de Admin API, gateway e extensoes, nao como um app isolado que inventa estado paralelo.

**Depende de:** Epic 08.02, Epic 10.01

## Files

| Path | Action | Reason |
|---|---|---|
| `workers/` | edit | Adicionar ou ajustar app worker de admin UI quando a implementacao comecar |
| `edger-orchestrator/tests/value_parity.rs` | edit | Provar rota local do frontend quando existir |
| `docs/developers/06-operacao-e-testes.adoc` | edit | Documentar como abrir e validar localmente |
| `planning/edger/docs/value-parity-matrix.md` | edit | Atualizar status da UI administrativa |

## Detail

### AS-IS

- A operacao existe por API e docs.
- `/todos` e shell demo provam static SPA/document routing.
- Nao ha escopo de cPanel/admin UI do edger.

### TO-BE

- Escopo inicial lista telas, dados, endpoints e estados vazios/erro.
- Telas minimas entregues: dashboard operacional, workers, modulos/extensoes, gateway e chaves operacionais.
- Cada tela usa Admin API existente, credencial root em memoria e criterio de Browser validation.

### Scope

- **In:** escopo, contrato de telas, primeira rota local quando implementada.
- **Out:** marketplace, IDE completa, deploy remoto.

### Critérios de aceite

- [x] Escopo inicial referencia apenas APIs existentes ou stories donas.
- [x] Cada tela tem estados de loading, vazio, erro e acesso negado.
- [x] Nenhuma tela exige acesso direto a arquivos internos do runtime.
- [x] A matriz aponta Epic 12 como owner da UI administrativa.

## Tasks

- [x] Mapear telas e endpoints minimos.
- [x] Definir estados de UI e limites de auth.
- [x] Identificar lacunas de API que pertencem aos Epics 10, 11 ou 13.
- [x] Atualizar matriz e docs.

## Verification

```bash
SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
```

## Status

completed (2026-06-30) - `workers/cpanel` entrega a primeira UI administrativa modular: overview operacional, workers, modulos/extensoes, gateway e chaves. A UI consome apenas Admin APIs root existentes, mantem a root key em memoria, nao acessa arquivos internos e deixa lacunas de catalogo/shell para 12.02 e operacao dinamica para Epics 10/11.
