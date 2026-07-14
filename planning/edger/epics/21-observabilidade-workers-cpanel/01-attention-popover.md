# Story 21.01: Popover de atenção acionável

**Origin:** `planning/edger/epics/21-observabilidade-workers-cpanel/00-overview.md`

## Context

- **Problem:** o badge `Needs attention` pode representar várias versões desabilitadas e erros; tooltip não escala para conteúdo longo ou ações.
- **Objective:** abrir um popover estruturado com resumo, motivos e destinos de resolução.
- **Value:** o operador entende o problema sem expandir tabelas ou adivinhar a causa.
- **Constraints:** conteúdo derivado de dados reais; popover fecha ao clicar fora; teclado e leitores de tela suportados.

## Traceability

- **Prototype:** `ATT-01 Attention popover` no Paper.
- **Business rules:** desabilitada e erro recente são motivos distintos; cada item mostra fonte e ação; nada sensível entra na mensagem.

## Files

| Path | Action | Reason |
|---|---|---|
| `workers/core/cpanel/index.js` | edit | Substituir tooltip por popover e ações |
| `workers/core/cpanel/components/ui/popover.js` | edit/reuse | Garantir fechamento externo e acessibilidade |
| `planning/edger/scripts/cpanel-ui-gate.sh` | edit | Proteger contrato do popover |
| `planning/edger/status/evidence/` | create | Evidência Browser dos cenários curto e longo |

## Detail

### AS-IS

- Badge usa texto agregado em tooltip.
- Motivos podem exceder a largura útil e não possuem ação contextual.

### TO-BE

- Clique no badge abre popover com título `Needs attention`, contagem e lista de motivos.
- Versão desabilitada oferece `View version`; erro oferece `View errors` e request ID mais recente.
- Até três itens aparecem diretamente; excedente usa área rolável limitada e `View all in Observability`.

### Scope

- **In:** popover, motivos, ações, foco, fechamento externo, estados vazio/1/muitos.
- **Out:** alterar regras que geram atenção ou persistir erros.

### Acceptance criteria

- [x] Popover permanece legível com 1, 3 e 20 motivos por meio de altura limitada e scroll interno.
- [x] Badge não mostra tooltip longo simultaneamente.
- [x] Ações navegam para versão, erros ou Observability com filtros preservados.
- [x] Escape e clique externo fecham o conteúdo controlado e o trigger expõe `aria-expanded`/`aria-controls`.

### Dependencies

- APIs atuais de error summary e inventário; sem dependência de 21.03 para o MVP.

## Tasks

- [x] Implementar estados do protótipo ATT-01 sobre dados reais.
- [x] Compor trigger e conteúdo controlado sem depender da API nativa de popover.
- [x] Adicionar ações e navegação filtrada.
- [x] Cobrir overflow, teclado e clique externo no Browser.
- [x] Atualizar gate/evidência.

## Status

completed (2026-07-12) — validado no Browser com tráfego real, posicionamento dentro do viewport e fechamento por clique externo; fallback controlado funciona mesmo sem suporte à API HTML Popover.

## Verification

```bash
planning/edger/scripts/cpanel-ui-gate.sh
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
```
