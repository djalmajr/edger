# Story 23.01: Contrato e threat model de secrets versionados

**Origin:** `planning/edger/epics/23-secrets-versionados/00-overview.md`.

## Context

Secrets globais e locais são úteis para reutilização e override por projeto,
mas o EdgeR ainda não possui persistência segura nem um contrato de injeção fora
do artefato. Copiar a interface da referência antes desse backend criaria uma
garantia falsa de segurança.

## Files

- `planning/edger/epics/23-secrets-versionados/00-overview.md`
- `planning/edger/docs/` (contrato e threat model a definir durante a story)
- `crates/edger-core/src/manifest.rs` (somente vocabulário puro, se necessário)
- `crates/edger-orchestrator/` (composição e API administrativa)
- `crates/edger-isolation/` (injeção no processo do worker)
- crate de provider a definir após o desenho do contrato

## Detail

Modelar identificador lógico, provider, versão imutável e alias de rotação.
Definir precedência entre defaults globais e referências por worker sem colocar
valores no manifesto. Especificar cache, revogação, falhas, redaction, auditoria,
rollback e o ponto exato em que o supervisor resolve e injeta o ambiente.

O desenho deve comparar pelo menos HashiCorp Vault e GCP Secret Manager, mas a
API pública permanece provider-agnostic. O provider de teste precisa permitir
provas determinísticas sem exigir serviço externo nos gates locais.

## Scope

- Contrato de referência/versionamento e provider.
- Threat model, redaction, auditoria, rotação e falhas.
- Pontos de integração com install, rollback, supervisor e WebIDE futura.

## Out of scope

- Persistir valores secretos no `manifest.yaml`, ZIP ou banco da WebIDE.
- Escolher um único fornecedor como dependência obrigatória do EdgeR.
- Implementar a tela global/local antes do contrato backend e do E2E seguro.

## Tasks

- [ ] Produzir threat model e matriz de exposição.
- [ ] Definir vocabulário puro de referências e versões.
- [ ] Definir trait de provider fora do core puro ou com I/O apenas na camada
  apropriada.
- [ ] Definir configuração, composição e política de precedência.
- [ ] Definir integração com install, rollback e spawn do worker.
- [ ] Prototipar provider de teste e uma integração externa.
- [ ] Criar testes de isolamento, redaction, rotação e indisponibilidade.
- [ ] Só então especificar APIs e UX global/local na WebIDE.

## Acceptance criteria

- [ ] Nenhum valor secreto é serializado no projeto ou artefato.
- [ ] Rollback para versão fixa resolve a mesma versão do secret.
- [ ] Alias rotativo possui semântica e evento auditável explícitos.
- [ ] Falha do provider não inicia worker com ambiente parcial silenciosamente.
- [ ] Logs e erros são redigidos por teste automatizado.
- [ ] Um worker nunca recebe referência destinada a outro worker.
- [ ] Gates Rust, planejamento e E2E do provider ficam verdes.

## Verification

Comandos mínimos previstos para a execução da story; testes específicos de
provider e redaction serão adicionados com o código:

```bash
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
SCRATCH=/tmp/edger-secrets-refinement planning/edger/scripts/run-gates.sh
```

## Status

planned (2026-07-14).
