# Story 26.03: configuração da senha inicial e prova local

**Origin:** `planning/edger/epics/26-console-auth/00-overview.md`, formulário de setup Rancher e preferência por Secret existente.

## Context

O binário recebe caminho de arquivo para o seed; o chart precisa montar esse arquivo sem salvar senha em values ou ConfigMap. A entrega precisa de prova integrada no servidor local antes de release.

## Files

`charts/edger/values.yaml`, `questions.yaml`, `templates/_helpers.tpl`, `templates/deployment.yaml`, `README.md`, documentação e evidência sob `planning/edger/`.

## Detail

`consoleAuth.rootPasswordSecret.name` é opcional e aponta para Secret existente; `key` seleciona o campo. O chart projeta esse campo em `/var/run/secrets/edger-console-root/password`, read-only, e define `EDGER_ROOT_PASSWORD_FILE` apenas quando há nome. Sem Secret, nenhum mount/var adicional. O root token mantém seu Secret separado. Provas: template default, configurado e inválido, testes Rust/JS, refinamento, login/logout no binário e Browser; publicação e deploy ficam fora desta história.

### Acceptance criteria

- A senha não aparece em values, ConfigMap, respostas ou relatório.
- Default não exige Secret de senha; chave vazia com nome configurado falha no render.
- Smoke local prova sessão por senha e fallback por token.

## Tasks

- [x] Implementar campos de chart/Rancher e docs sem senha inline.
- [x] Validar `helm lint/template` com configuração ausente, válida e inválida.
- [x] Integrar o binário e registrar gate/refinamento/Browser/HTTP em evidência.

## Verification

```bash
helm lint charts/edger --set rootKey.existingSecret=existing-root
helm template edger charts/edger --set rootKey.existingSecret=existing-root
cargo test --workspace
```

Prova completa em `planning/edger/status/evidence/console-auth-2026-09-27.md`.
