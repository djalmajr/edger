# Evidência 2026-07-11: health passivo por worker/versão

## Contrato entregue

- Identidade: namespace, worker e versão no agrupamento já usado pelo pool.
- Janela: cinco minutos, declarada por `windowMs: 300000`.
- Retenção: até 64 outcomes por identidade, em memória.
- Estados: `unobserved`, `healthy`, `degraded` e `failing`.
- Falhas: HTTP 5xx, erro de isolate, rejeição e timeout; HTTP 4xx não degrada health.
- Freshness: `observedAtMs`, `lastSuccessAtMs`, `lastFailureAtMs`, contagens e falhas consecutivas.
- Reset: a janela é reconstruída somente com tráfego observado no processo atual; restart e expiração não reutilizam amostras.

## TDD e gates

O primeiro teste do agregador falhou por ausência dos tipos/API de health. Após a implementação:

```text
cargo test -p edger-worker --lib
cargo test -p edger-worker --test integration_pool
cargo test -p edger-orchestrator --test metrics_endpoint
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
planning/edger/scripts/cpanel-ui-gate.sh
python3 planning/edger/scripts/refinement-lint.py --scope planning/edger/epics/21-observabilidade-workers-cpanel --round worker-passive-health
```

Resultados: testes e clippy/fmt passaram; `cpanel-ui-gate ok`; refinement com 0 RED, 0 WARN e veredito PASS.

## Tráfego real e API

Com o runtime local em `127.0.0.1:19080`, foram exercitados quatro cenários:

| Worker/versão | Tráfego real | Resultado em `/metrics/stats` |
|---|---|---|
| Sem requisição recente | nenhum | `unobserved` |
| `commonjs` | duas respostas 200 | `healthy`, 2 sucessos |
| `cpanel-scenario@1.2.0` | uma resposta 200 e uma 500 | `degraded` |
| `boom-ui@1.1.0` | três respostas 500 | `failing`, 3 falhas consecutivas |

O endpoint também foi validado por integração para o contrato JSON nested de health e para uma sequência de requests reais no pool.

## Browser

O cPanel atualizado foi aberto e autenticado no Browser embutido. A interface exibiu:

- resumo `Routable`, sem uso de `Serving`;
- colunas independentes `Routing`, `Health` e `Processes`;
- `Default + Failing` para `boom-ui` após as falhas reais;
- `Default + Degraded` para `cpanel-scenario`;
- `Default + Healthy` para `commonjs`;
- `Unobserved` para versões sem tráfego na janela;
- filtros multiselect separados para routing e health;
- tooltip de health com janela de cinco minutos, amostras, freshness e aviso de reset no restart.

A responsividade sem scroll horizontal interno já possui prova dedicada em `planning/edger/status/evidence/cpanel-responsive-2026-07-11.md`, incluindo 1155, 768 e 390 px nas rotas Overview, Workers e Files.
