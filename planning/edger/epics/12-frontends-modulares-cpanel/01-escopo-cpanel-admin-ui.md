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
- Telas minimas: dashboard operacional, workers, modulos/extensoes, gateway e chaves operacionais.
- Cada tela tem contrato de API, criterio de auth e criterio de Browser validation.

### Scope

- **In:** escopo, contrato de telas, primeira rota local quando implementada.
- **Out:** marketplace, IDE completa, deploy remoto.

### Critérios de aceite

- [ ] Escopo inicial referencia apenas APIs existentes ou stories donas.
- [ ] Cada tela tem estados de loading, vazio, erro e acesso negado.
- [ ] Nenhuma tela exige acesso direto a arquivos internos do runtime.
- [ ] A matriz aponta Epic 12 como owner da UI administrativa.

## Tasks

- [ ] Mapear telas e endpoints minimos.
- [ ] Definir estados de UI e limites de auth.
- [ ] Identificar lacunas de API que pertencem aos Epics 10, 11 ou 13.
- [ ] Atualizar matriz e docs.

## Verification

```bash
SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
```

