---
name: EdgeR console
status: operator-confirmed-layout
source: workers/ui/src/theme.css
---

# EdgeR console design

## Scope

The existing cPanel is the visual and interaction reference for proposals and
implementation. Preserve its shell and extend the relevant existing surface.
Tokens and component variants come from `workers/ui/src/theme.css` and
`workers/ui/src/components/ui/`. This file does not introduce a second theme.

## Operator-confirmed layout rules

Confirmed by the operator on 2026-11-04:

- Preserve the existing navigation label **Workers**. Do not introduce an
  **Applications** or **Aplicações** menu or a replacement application shell.
- Put **Secrets inside the details of a worker**, next to the existing Files,
  Observability and Logs views. It is not a global navigation entry.
- Preserve the sidebar groups, order, brand, header and preference/account
  controls appropriate to the current principal and locale.
- Inspect the real UI before drawing. Source inspection supplements the visual
  reference; it does not authorize inventing a different shell.

## Existing layout anchors

Measured from the current rc.30 frontend, rendered with synthetic local API data:

- At 1440 × 960, sidebar width 216 px; header starts at x=216 and is 64 px high.
- Worker details are a page, with back-to-workers control, worker name and
  `Version <version> · <view>` subtitle. They are not an event-detail sheet.
- Content has 16 px padding. Existing tab band starts at x=232, y=80 and is
  32 px high. Tables and toolbar actions retain the existing compact density.
- On a 390 px viewport the sidebar is hidden; preserve the existing worker
  header. Reflow the new content without adding a different mobile navigation.
- Use Noto Sans and the existing light/dark theme. Any proposed token adjustment
  must be called out explicitly and approved before changing production tokens.

## Secrets semantics

- Secrets belong to the worker name and are shared across its versions, even
  when the surrounding detail page carries a selected version.
- Display names and dates. Values are input-only and are never retrieved for
  revealing, copying or pre-filling a replacement.
- Saving and deleting do not recycle processes. Immediate adoption is a separate,
  confirmed operation for an explicitly selected version and its own permission.
- Distinguish empty, loading, unavailable, forbidden and failed operations.

## Proposal reference

`edger-secrets.fig` and `planning/edger/design/README.md` document the proposal.
They are not an implementation or permission to publish or change production.
