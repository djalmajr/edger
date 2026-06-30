# Epic 13: MCP e Authoring AI-native Local

**Origin:** `planning/edger/roadmap.md`

**Depends on epic:** `planning/edger/epics/10-operacao-extensoes-plugins/00-overview.md`

## Context

### Macro problem

O edger deve ser amigavel a agentes desde o desenho. O objetivo nao e apenas documentar APIs: um agente precisa descobrir capabilities, criar ou modificar workers, validar localmente e preparar commit/PR sem deploy remoto. Esse valor se aproxima de ferramentas como lovable e v0, mas orientado ao runtime edger e com limites locais fortes.

### Initiative objective

Entregar uma primeira versao funcional, testada localmente, de control plane MCP/AI-native. O servidor MCP deve expor ferramentas seguras para descoberta, authoring de worker, validacao local e preparacao de mudanca, usando contratos machine-readable e mantendo side effects explicitos.

### AS-IS

- Admin API, gateway, workers e extensoes ja tem parte dos contratos operacionais.
- Nao ha servidor MCP do edger.
- OpenAPI/JSON Schema e catalogo de capabilities ainda nao existem como contrato formal.
- Fluxos de authoring por agente dependem de leitura humana do repo.

### TO-BE

- Contratos machine-readable descrevem workers, capabilities, admin endpoints e validacoes locais.
- MCP server local expõe tools com auth, dry-run e redaction.
- Agente consegue criar ou modificar worker em workspace controlado.
- Agente consegue rodar validacao local e preparar diff/commit/PR sem deploy remoto.

### Out of scope

- Deploy remoto de workers.
- Publicacao de extensoes.
- Edicao fora do workspace autorizado.
- Escrita de segredos em manifests, logs ou respostas MCP.

## Story backlog

| Story | Arquivo | Objetivo | Tamanho | Status | Depende de |
|---|---|---|---|---|---|
| 13.01 Contratos machine-readable | `01-contratos-machine-readable.md` | Gerar ou manter schemas de API/capabilities para agentes | medium | completed | Epic 08.02, Epic 10.01 |
| 13.02 MCP server control plane | `02-mcp-server-control-plane.md` | Criar primeiro MCP server local do edger com tools seguras | large | completed | 13.01 |
| 13.03 Authoring local de worker | `03-authoring-worker-local.md` | Permitir criar/modificar worker em workspace controlado via tool | large | completed | 13.02 |
| 13.04 Validacao local de worker | `04-validacao-local-worker.md` | Rodar validacao local de worker e registrar evidencia | medium | completed | 13.03 |
| 13.05 Preparacao de commit/PR | `05-preparacao-commit-pr.md` | Preparar diff, commit local e PR metadata sem deploy remoto | medium | completed | 13.04 |

## Epic acceptance criteria

- [x] Pelo menos um servidor MCP local do edger inicia e lista tools.
- [x] Tools de descoberta retornam workers, capabilities e contracts sem segredos.
- [x] Tool de authoring cria ou modifica worker em workspace permitido com dry-run.
- [x] Tool de validacao roda checks locais e retorna evidencia objetiva.
- [x] Fluxo completo e testado localmente: descobrir, criar/modificar worker, validar e preparar commit/PR.
- [x] Nenhuma tool realiza deploy remoto nesta fase.
- [x] Gate de planejamento fica verde: `SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh`.

## Status

completed (2026-06-29) - primeira versao funcional local entregue em `edger-mcp`: stdio JSON-RPC com `initialize`, `tools/list`, `tools/call` e tools para capabilities, workers, inspect, authoring local sob `workers/`, validacao local de manifests e preparacao de commit/PR metadata sem deploy remoto. `cargo test --workspace`, `cargo clippy --workspace -- -D warnings`, `cargo fmt -- --check`, validacao stdio manual e `SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh` passaram.
