# Story 22.04: Imagem mínima e persistência operacional

**Origin:** `planning/edger/epics/22-core-workers-webide/00-overview.md`

## Context

Distribuir binário, Deno, cPanel e WebIDE runtime-ready, mantendo overlays e
workers de usuário em volumes documentados.

## Files

- `Dockerfile`
- `charts/edger/`
- Documentação Docker e de operação

## Detail

O estágio final não carrega toolchains nem exemplos. Os roots mutáveis são
montagens separadas para permitir persistência explícita no container e no Helm.

## Tasks

- [x] Produzir imagem multi-stage non-root.
- [x] Copiar somente os dois apps core runtime-ready.
- [x] Configurar envs, mounts, PVCs e Rancher questions.
- [x] Construir e inspecionar a imagem final.

## Acceptance criteria

- [x] Dockerfile usa estágios de build e usuário non-root.
- [x] Imagem copia somente cPanel runtime-ready e `webide/dist`.
- [x] Helm/Rancher configura core bundled, overlay e user roots.
- [x] Overlay e workers de usuário possuem PVCs opcionais.
- [x] Build e inspeção da imagem final provam conteúdo e usuário.

## Verification

- `helm lint charts/edger` e `helm template edger charts/edger`.
- Build e inspeção do usuário e conteúdo da imagem.

## Status

completed (2026-07-13).
