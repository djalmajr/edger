# Story 13.02: MCP server control plane

**Origin:** `planning/edger/epics/13-mcp-authoring-ai-native/00-overview.md`

## Context

O MCP server e a primeira interface AI-native funcional do edger. Ele deve expor ferramentas locais pequenas e seguras, com descoberta e dry-run, em vez de entregar um wrapper generico que execute comandos arbitrarios.

**Depende de:** Story 13.01

## Files

| Path | Action | Reason |
|---|---|---|
| `Cargo.toml` | edit | Registrar crate `edger-mcp` no workspace |
| `crates/edger-mcp/Cargo.toml` | create | Definir crate/binario MCP sem contaminar core |
| `crates/edger-mcp/src/lib.rs` | create | Expor handler testavel de protocolo MCP |
| `crates/edger-mcp/src/main.rs` | create | Rodar servidor stdio local |
| `crates/edger-mcp/tests/protocol.rs` | create | Provar initialize, tools/list e tools/call |
| `docs/developers/06-operacao-e-testes.adoc` | edit | Documentar execucao local do MCP |

## Detail

### AS-IS

- Nao existe MCP server do edger.
- Admin API e dados operacionais existem, mas agentes precisam invocar o runtime diretamente.

### TO-BE

- MCP server local inicia sem rede externa e lista tools.
- Tools iniciais: list_workers, list_capabilities, inspect_worker, validate_local.
- Auth e workspace root sao configuraveis localmente.
- Respostas sao pequenas, estruturadas e sem segredos.
- A primeira versao usa transporte stdio, JSON-RPC 2.0 newline-delimited, com `initialize`, `tools/list` e `tools/call`.

### Scope

- **In:** servidor MCP local stdio, tools de discovery, redaction, testes.
- **Out:** deploy remoto, execucao arbitraria de shell, editor completo.

### Approach

- Criar crate `edger-mcp` como modulo de produto/automacao, nao como parte do core.
- Implementar um handler JSON-RPC pequeno com metodos `initialize`, `tools/list` e `tools/call`.
- Tools iniciais:
  - `edger.list_workers`: carrega manifests de `workspaceRoot`/`workerDirs` e retorna inventario seguro.
  - `edger.inspect_worker`: retorna um worker especifico por nome e versao opcional.
  - `edger.list_capabilities`: retorna contrato versionado das capacidades AI-native iniciais.
  - `edger.validate_local`: primeira versao dry-run retorna checks locais disponiveis e deixa execucao completa para Story 13.04.
- O binario stdio le uma request JSON por linha e escreve uma response JSON por linha.
- `workspaceRoot` e `workerDirs` passam por normalizacao para impedir path traversal fora do workspace.

### Test-first plan

- **Behavior:** um agente consegue inicializar o MCP, listar tools e chamar `edger.list_workers` contra fixtures locais.
- **First failing test:** enviar JSON-RPC `tools/list` ao handler e assertar que as quatro tools existem com input schemas.
- **Level:** integration test do crate `edger-mcp` chamando o handler sem processo externo.
- **Low-value tests to avoid:** teste que apenas verifica que uma string contem "tools" sem validar shape JSON-RPC.

### Critérios de aceite

- [x] MCP server inicia localmente e responde initialize/list tools.
- [x] Tools de discovery retornam JSON estruturado.
- [x] Respostas nao incluem tokens, headers sensiveis ou env bruto.
- [x] Teste local cobre pelo menos uma chamada de discovery.

## Tasks

- [x] Escolher fronteira de crate/binario MCP sem contaminar core.
  - Done when: `edger-mcp` entra no workspace e `edger-core` continua sem dependencias novas de I/O.
- [x] Implementar inicializacao e listagem de tools.
  - Done when: `initialize` e `tools/list` retornam respostas JSON-RPC validas.
- [x] Implementar discovery de workers/capabilities.
  - Done when: `edger.list_workers`, `edger.inspect_worker` e `edger.list_capabilities` retornam dados reais/seguros.
- [x] Adicionar testes e docs de execucao local.
  - Done when: testes cobrem protocolo e docs mostram como rodar `cargo run -p edger-mcp --bin edger-mcp`.

## Verification

```bash
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh
```

## Status

completed (2026-06-29) - `edger-mcp` entrou no workspace como crate/binario separado com handler stdio JSON-RPC para `initialize`, `tools/list` e `tools/call`; testes cobrem tools de discovery e contrato sem vazar env sensivel.
