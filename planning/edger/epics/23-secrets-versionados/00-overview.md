# Epic 23: Secrets versionados e injeção segura

**Origin:** `planning/edger/roadmap.md`, follow-up aprovado pelo usuário durante
o refinamento da WebIDE em 2026-07-14.

## Context

O `manifest.yaml` aceita configuração de ambiente, mas não é um cofre: valores
incluídos no manifesto ou no ZIP ficam materializados no artefato implantado.
A WebIDE também não deve prometer secrets apenas com armazenamento no browser,
pois a API de instalação atual não possui um canal seguro para referências ou
overrides de ambiente.

## Objective

Definir e implementar uma fronteira substituível de secret provider, com
referências versionadas, resolução no runtime, rotação e auditoria. HashiCorp
Vault e GCP Secret Manager são integrações-alvo; o domínio EdgeR não deve
depender dos SDKs desses produtos.

## Architecture boundaries

- `manifest.yaml` declara referências, versões ou aliases; nunca o valor secreto.
- O ZIP de deploy não contém material secreto.
- A resolução ocorre em componente confiável antes do spawn do worker e injeta
  somente as chaves autorizadas no processo daquele worker.
- Versão implantada registra referências imutáveis suficientes para rollback
  reproduzível; rotação por alias exige política explícita e observável.
- Providers externos vivem atrás de contrato e composição, sem contaminar
  `edger-core` com I/O ou SDK específico.
- A WebIDE só recebe telas de secrets depois de existir API segura e testada.

## Story backlog

| Story | Arquivo | Objetivo | Status | Depende de |
|---|---|---|---|---|
| 23.01 Contrato e threat model | `01-contrato-provider-secrets.md` | Definir referências, versões, injeção, rotação, auditoria e integração de providers | planned | Epic 15, Epic 22 |

## Epic acceptance criteria

- [ ] Threat model cobre persistência, transporte, logs, memória de processo,
  rotação, rollback e indisponibilidade do provider.
- [ ] Contrato provider-agnostic suporta referência a versão imutável e alias.
- [ ] Valores não aparecem em manifesto, ZIP, API responses, logs ou eventos.
- [ ] Injeção preserva isolamento entre workers e versões.
- [ ] Ao menos um provider real e um provider de teste exercitam o mesmo contrato.
- [ ] WebIDE global/local secrets só é liberada após o fluxo backend E2E.

## Status

planned (2026-07-14).
