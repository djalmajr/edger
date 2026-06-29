# Story 03.04: Preparação dual-backend (módulo deno facade + wasmtime WASI)

**Origin:** `planning/edger/epics/03-isolacao-execucao/00-overview.md`

## Context
- **Problema:** Após o spike, é preciso estruturar o código para dois backends distintos (JS/TS via deno_core+facade; Wasm via wasmtime standalone) sem implementação de produção completa.
- **Objetivo:** Esqueletos de módulo `deno` (facade) e `wasm` (wasmtime+WASI), feature flags, traits internos e hooks de bundling/eszip documentados — compilando e testáveis com stubs.
- **Valor:** PR 10 (execução real) terá layout claro; evita misturar Wasm no isolate V8 (decisão do usuário).
- **Restrições:** Baseado em conclusões de `spike.md` (story 03.01); backends atrás de features `deno` e `wasm`; default build sem V8 para CI.

## Traceability
- **Source docs:** `planning/edger/design.md` (Resolved Decisions embedding, PR 5/10, eszip_trait), `planning/edger/epics/03-isolacao-execucao/spike.md` (output 03.01)
- **Depende de:** Stories 03.01, 03.02, 03.03

## Files

| Path | Ação | Motivo |
|---|---|---|
| `edger-isolation/Cargo.toml` | alterar | features: `deno`, `wasm`, `default = []` |
| `edger-isolation/src/deno/mod.rs` | criar | `DenoIsolate` skeleton, `DenoFacade` config |
| `edger-isolation/src/deno/facade.rs` | criar | registro ops stub, module loader trait |
| `edger-isolation/src/deno/bundle.rs` | criar | traits `ModuleBundler`, stub eszip/precomp hooks |
| `edger-isolation/src/wasm/mod.rs` | criar | `WasmIsolate` skeleton |
| `edger-isolation/src/wasm/wasi.rs` | criar | config WASI capabilities stub |
| `edger-isolation/src/backend.rs` | criar | enum `IsolationBackend { Mock, Deno, Wasm }` factory |
| `edger-isolation/tests/backend_factory.rs` | criar | factory retorna Mock por default |
| `planning/edger/epics/03-isolacao-execucao/spike.md` | referenciar | layout deve espelhar recomendação do spike |

## Detail

### AS-IS
- Apenas `MockIsolate` funcional
- Sem estrutura para deno_core ou wasmtime em `src/`
- Sem feature flags de backend

### TO-BE
- Feature `deno`: compila `deno/mod.rs` com `DenoIsolate` struct e impl `Isolate` que retorna `IsolationError::NotImplemented` em todos os métodos (ou delega ao mock em dev)
- Feature `wasm`: compila `wasm/mod.rs` com `WasmIsolate` + `WasiConfig { allow_env: false, ... }` stub
- `backend.rs`: `create_isolate(backend: IsolationBackend, config: &WorkerConfig) -> Box<dyn Isolate>` — Mock sempre disponível
- `bundle.rs`: trait `ModuleBundler { fn load_eszip(&self, path) -> Result<...> }` com impl `StubBundler`
- Documentação inline referenciando Edge Runtime `deno_facade`, `eszip_trait`, decisão Wasm standalone
- README fragment em doc comment do crate sobre como habilitar `--features deno,wasm`

### Escopo
- **In:** módulos skeleton, features, factory, traits de bundling, testes de compilação
- **Out:** execução V8/Wasm real, registro completo de ops Deno, eszip parser real

### Critérios de aceite
- [ ] `cargo test -p edger-isolation` passa sem features (mock only)
- [ ] `cargo check -p edger-isolation --features deno` compila (pode ignorar link V8 em CI com cfg)
- [ ] `cargo check -p edger-isolation --features wasm` compila
- [ ] `DenoIsolate` e `WasmIsolate` existem e implementam trait `Isolate` (stubs NotImplemented)
- [ ] `ModuleBundler` trait documentado com path para eszip/precomp (PR 10)
- [ ] Factory test: `IsolationBackend::Mock` retorna isolate funcional
- [ ] Layout alinhado com seção "Recomendação de módulos" em `spike.md`

### Dependências
- Story 03.01 (`spike.md` publicado)
- Story 03.02 (trait + mock)
- Story 03.03 (limits wrapper reutilizável pelos backends futuros)

## Test-first plan
- **Primeiro teste falhando:** `factory_mock_backend_executes_fetch` — `create_isolate(Mock, ...)` retorna 200
- **Nível:** `backend_factory.rs`; compile tests com `#[cfg(feature = "deno")]` para struct exists
- **Evitar:** Exigir V8 no CI default; usar `cfg` guards

## Tasks
- [ ] Ler `spike.md` e alinhar nomes de módulos
- [ ] Adicionar features `deno` / `wasm` no Cargo.toml com deps opcionais pinadas
- [ ] Criar `deno/mod.rs`, `facade.rs`, `bundle.rs` (stubs)
- [ ] Criar `wasm/mod.rs`, `wasi.rs` (stubs)
- [ ] Criar `backend.rs` factory + enum `IsolationBackend`
- [ ] Impl `Isolate` stub para `DenoIsolate` e `WasmIsolate`
- [ ] Testes factory + compile tests por feature
- [ ] Atualizar `00-overview.md` status epic → ready-for-development quando todas stories planejadas

## Verification
```bash
cargo test -p edger-isolation
cargo check -p edger-isolation --features deno
cargo check -p edger-isolation --features wasm
cargo clippy -p edger-isolation -- -D warnings
cargo fmt -- --check
bun test

# Alinhamento com spike
grep -q "deno" planning/edger/epics/03-isolacao-execucao/spike.md || echo "WARN: spike.md ainda não publicado (bloqueio soft até 03.01)"
```