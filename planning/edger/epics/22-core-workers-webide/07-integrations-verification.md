# Story 22.07: Integrações, documentação e gates finais

**Origin:** `planning/edger/epics/22-core-workers-webide/00-overview.md`

## Context

Integrar o workspace segmentado com Files, Preview, Deployments, Observability e
Logs; fechar documentação e evidência executável.

## Files

- `README.md`
- `CLAUDE.md`
- `docs/`
- `planning/edger/`
- Evidências geradas pelos gates

## Detail

A entrega só pode ser fechada depois dos gates Rust, UI, planejamento, imagem e
Browser; a documentação deve distinguir estado validado de roadmap.

## Tasks

- [x] Integrar os contratos administrativos existentes.
- [x] Atualizar documentação de roots, volumes e WebIDE.
- [x] Executar gates Rust, cPanel e refinement.
- [x] Registrar inspeção da imagem e Browser E2E.

## Acceptance criteria

- [x] WebIDE reutiliza auth, install, lifecycle e observabilidade atuais.
- [x] Interfaces administrativas informam origem e resultado do deploy.
- [x] Docker/Helm e documentação descrevem roots e volumes.
- [x] Cargo, clippy, fmt, cPanel gate e refinement gate passam.
- [x] Browser E2E e inspeção da imagem estão registrados em status/evidence.

## Verification

```bash
cargo test --workspace && cargo clippy --workspace -- -D warnings && cargo fmt -- --check
planning/edger/scripts/cpanel-ui-gate.sh
SCRATCH=/tmp/edger-refinement planning/edger/scripts/run-gates.sh
```

## Status

completed (2026-07-13).
