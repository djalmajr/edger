# Story 24.01: Adapters e deploy real

## Context

Os builds oficiais de Next.js, Nuxt, Astro e SolidStart seguiam contratos
diferentes (`node:http`, `Deno.serve` e `handle`) e os pacotes Next standalone
ultrapassavam o limite de 4 MiB do data plane.

## Files

| Path | Action | Reason |
|---|---|---|
| `crates/edger-core/src/execution.rs` | edit | Normalizar os adapters declarativos |
| `crates/edger-isolation/src/multiproc*` | edit | Servidor Node privado e contratos Deno |
| `crates/edger-isolation/src/fullstack.rs` | edit | Base path e assets por framework |
| `crates/edger-orchestrator/src/{admin_api,deploy}.rs` | edit | Pacotes administrativos grandes e seguros |
| `planning/edger/docs/` | edit | Receitas e matriz de compatibilidade |

## Detail

### Delivered

- aliases e validação declarativa para `astro`, `nextjs`, `nuxt` e
  `solidstart`;
- proxy `node:http` por Unix socket para Next standalone, mantendo a captura
  leve anterior para Express/SvelteKit;
- suporte a `Deno.ServeHandlerInfo` para Nitro e a `handle` para o adapter Astro;
- restauração correta do base path, incluindo a raiz sem barra extra;
- prefixes de assets para Astro, Nuxt e SolidStart;
- pacote administrativo de 64 MiB com limites de expansão ZIP;
- testes unitários/integrados e deploy ZIP ao vivo dos quatro builds.

### Acceptance criteria

- [x] Os quatro adapters são validados e normalizados pelo core.
- [x] SSR e API respondem 200 em artefatos de produção reais.
- [x] Next não abre TCP privado e preserva semântica Node real.
- [x] Pacote Next acima de 4 MiB instala e baixa pelo Admin API.
- [x] Limites de expansão protegem a extração de ZIP.

### Live evidence

Em 2026-07-17, no runtime local `127.0.0.1:19080`:

| Framework | SSR | API | Pacote |
|---|---:|---:|---:|
| Next.js 16.2.10 standalone | 200 | 200 | 11 MiB ZIP, install 201 |
| Astro 7.1.1 + adapter Deno | 200 | 200 | install 201 |
| Nuxt 4.4.8 + Nitro 2.13.4 | 200 | 200 | install 201 |
| SolidStart 1.3.2 + Vinxi 0.5.11 | 200 | 200 | asset JS 200, install 201 |

O download do projeto Next instalado retornou `200`, 11.459.238 bytes e passou
em `unzip -t`.

## Tasks

- [x] Levantar os entrypoints e presets oficiais.
- [x] Implementar o proxy Node privado e os contratos Deno faltantes.
- [x] Configurar base path e assets dos quatro builds.
- [x] Separar o cap administrativo do limite normal de requests.
- [x] Instalar e revalidar os quatro ZIPs no runtime local.
- [x] Documentar escopo, receitas, riscos e evidência.

## Verification

```bash
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
SCRATCH=<dir> ./planning/edger/scripts/run-gates.sh
```

Validação live: SSR/API HTTP 200 para os quatro apps, asset hidratável
SolidStart HTTP 200, install HTTP 201 e Browser sem console errors.

## Status

**completed** (2026-07-17).
