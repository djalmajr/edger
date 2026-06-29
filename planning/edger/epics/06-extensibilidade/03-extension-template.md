# Story 06.03: Template de extensão — edger-ext-gateway (skeleton + wiring)

**Origin:** `planning/edger/epics/06-extensibilidade/00-overview.md`

## Context
- **Problema:** Após `edger-ext-auth`, autores não têm skeleton copiável para novas extensões Middleware.
- **Objetivo:** Crate template `edger-ext-gateway` com implementação mínima de `Middleware` e exemplo de wiring no bin.
- **Valor:** Acelera criação de `edger-ext-metrics`, `edger-ext-*` futuras; demonstra choose ONE (gateway ≠ auth).
- **Restrições:** Template não precisa proxy HTTP real; pass-through + log suficiente; marcado como example no workspace.

## Traceability
- **Source docs:** `planning/edger/design.md` (Main Binary sketch — `edger_ext_gateway::GatewayExtension`, PR 9 pattern)
- **Design PR:** PR 9 (extension pattern); complementa PR 8 registry
- **Depende de:** Stories 06.01, 06.02

## Files

| Path | Ação | Motivo |
|---|---|---|
| `edger-ext-gateway/Cargo.toml` | criar | template crate |
| `edger-ext-gateway/src/lib.rs` | criar | `GatewayExtension` + `Middleware` |
| `edger-ext-gateway/README.md` | criar | instruções copy-paste para nova ext |
| `edger-ext-gateway/tests/gateway_middleware.rs` | criar | on_request pass-through |
| `Cargo.toml` (workspace) | alterar | member (opcional feature `examples`) |
| `edger-orchestrator/src/bin/edger.rs` | alterar | register gateway (feature ou always) |
| `planning/edger/docs/extensions.md` | criar/atualizar (nesta story) | seção template |

## Detail

### AS-IS
Apenas `edger-ext-auth` (após 06.02); sem template reutilizável.

### TO-BE
- `edger-ext-gateway`:
  ```rust
  pub struct GatewayExtension { prefix: String }
  impl Extension for GatewayExtension { fn name(&self) -> &'static str { "gateway" } ... }
  impl Middleware for GatewayExtension {
      fn on_request(&self, req: &mut SerializedRequest, ctx: &RequestContext)
          -> Result<Option<SerializedResponse>> {
          // log + optional path strip; None = continue
      }
  }
  ```
- README com:
  1. Copiar diretório → renomear crate
  2. Implementar trait(s) — **uma responsabilidade**
  3. Adicionar ao workspace `Cargo.toml`
  4. Registrar no bin via padrão 06.01
  5. Escrever testes
  6. Rodar gate cargo
- Bin `edger`: registrar auth + gateway (ordem priority documentada: auth antes gateway)
- Feature flag `default-extensions` no orchestrator para incluir gateway em dev

### Escopo
- **In:** skeleton crate, README, teste, wiring exemplo, docs
- **Out:** Proxy reverso real, rate limiting, TLS termination

### Critérios de aceite
- [ ] `cargo test -p edger-ext-gateway` verde
- [ ] README permite criar nova ext em <30 min seguindo passos
- [ ] Gateway registrado e `on_request` invocado (trace log verificável em teste)
- [ ] Crate não implementa `AuthProvider` (choose ONE demonstrado)
- [ ] `extensions.md` referencia template como ponto de partida

### Dependências
- Stories 06.01, 06.02

## Test-first plan
1. **Red:** `GatewayExtension::on_request` retorna `None` (continue)
2. **Red:** com header `X-Gateway-Test`, extension loga/conta invocação
3. **Red:** registry com auth + gateway executa auth priority menor primeiro
4. **Green:** implementar skeleton
5. **Refactor:** extrair `extension-template/` script ou documentar copy via README only

**Nível:** unit + integração leve

## Tasks
- [ ] Scaffold `edger-ext-gateway` copiando estrutura de `edger-ext-auth` (sem lógica auth)
- [ ] Implementar `Middleware` pass-through mínimo
- [ ] Escrever README template (português)
- [ ] Testes unitários + integração registry
- [ ] Registrar no bin com priority > auth
- [ ] Atualizar `planning/edger/docs/extensions.md` com diagrama de wiring
- [ ] Opcional: `cargo generate` ou script `scripts/new-extension.sh` (fora de escopo se atrasar — README basta)

## Verification
```bash
cargo test -p edger-ext-gateway
cargo test -p edger-orchestrator
cargo test --workspace
cargo clippy --workspace -- -D warnings
bun test
```