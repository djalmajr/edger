# Static SPA contract

How EdgeR serves a `kind: static` (or `spa`) worker, and the rules a prebuilt
app must follow to work at `/<name>/` — including behind a URL prefix.

## How EdgeR serves the SPA

- Pure Rust file server (`StaticSpa`); no Deno process is started.
- The package is a zip with `manifest.yaml` and `index.html` at the root, plus
  the assets. A zip with a single top-level directory is unwrapped. Limits:
  64 MiB compressed, 256 MiB expanded, 50,000 entries; absolute or `..` paths
  are rejected.
- Addressed at `/<name>/...`. The version-pinned address
  `/<name>@<version>/...` also serves the SPA, and the `<base href>` there
  keeps the version segment (for example `<base href="/my-spa@1.2.3/" />`);
  that fix shipped in EdgeR 0.3.1-rc.2 (the 0.3.1-rc.1 tag was never
  published). The public base is also available in the `x-base` response
  header.
- **`<base href>` rewriting.** With `injectBase: true` (the default) EdgeR
  replaces the existing `<base>` tag in `index.html` — or inserts one right
  after `<head>` when there is none — with the public base. Behind a
  prefix-stripping proxy the `X-Forwarded-Prefix` header (for example
  `/apps`) is included, so the tag becomes `<base href="/apps/my-spa/" />`
  and `<base href="/apps/my-spa@1.2.3/" />` on the version-pinned address.
- **SPA fallback.** Any path that is not an existing file returns
  `index.html` with status 200 — including a missing asset. A missing JS
  chunk comes back as HTML, so the browser logs a MIME error (`text/html`),
  not a 404.
- **`publicEnv`.** Keys listed in `publicEnv` that exist in `env` are
  injected into `index.html` before `</head>` as
  `<script>window.__env__={...};</script>`. Keys that look like secrets are
  dropped (uppercased prefix `AWS_`, `GITHUB_`, `OPENAI_`, `ANTHROPIC_`,
  `STRIPE_`, `DATABASE_`, `DB_`, `API_KEY`, `AUTH_KEY`, `SECRET_KEY`,
  `PRIVATE_KEY`, or suffix `_KEY`, `_TOKEN`, `_SECRET`, `_PASSWORD`).
  Whatever is injected is visible to any visitor: never put a secret in
  `publicEnv`.
- **Cache.** HTML: `no-cache`. Files under `assets/` whose name ends in a
  hash-like tail (Vite's `name-<hash>` shape): `max-age=31536000, immutable`.
  Everything else: `max-age=300`.
- **Content-Type.** Recognized extensions: `css`, `html`/`htm`, `ico`,
  `js`/`mjs`, `json`, `png`, `svg`, `wasm`. Anything else — `woff2`, `jpg`,
  `webp`, `txt`, `map`, `webmanifest` — is served as
  `application/octet-stream`.

## Rules every app must follow

- **R1.** The source `index.html` has `<base href="/" />` as the first child
  of `<head>`. EdgeR replaces that tag; without EdgeR (local dev) it keeps
  the app working on deep links.
- **R2.** No absolute URLs for the app's own files.
  Right: `./assets/index-abc123.js`, `./logo.svg`. Wrong:
  `/assets/index-abc123.js`. In Vite this is `base: './'` in the Vite config.
- **R3.** The router basename is `new URL(document.baseURI).pathname` minus
  the trailing slash. `/apps/my-spa/` yields `/apps/my-spa`; `/` yields `""`.
  Never hard-code the prefix or the worker name in app code.
- **R4.** Internal links go through the router (which prefixes the basename)
  or are relative without a leading slash. Right: `<a href="about">`.
  Wrong: `<a href="/about">`.
- **R5.** Assets use only recognized Content-Types (list above): `svg`,
  `png`, `css`, `js`, `json`, `ico`, `wasm`.
- **R6.** `manifest.yaml` sits at the zip root. In a Vite project it lives in
  `public/manifest.yaml`, and `vite build` copies it into `dist/`.

## Basename recipes

Shared helper (same shape as `tmp/spa-examples/vite-react/src/basename.ts`):

```ts
export function getBasename(): string {
  return new URL(document.baseURI).pathname.replace(/\/$/, "");
}
```

- **React Router** (`tmp/spa-examples/vite-react`):
  `<BrowserRouter basename={getBasename()} />`
- **Vue Router** (`tmp/spa-examples/vite-vue`):
  `createWebHistory(`${getBasename()}/`)` — note the trailing slash.
- **@solidjs/router** (`tmp/spa-examples/vite-solid`):
  `<Router base={getBasename()} ... />`
- **Vanilla** (`tmp/spa-examples/vanilla`):
  `const basename = new URL(document.baseURI).pathname.replace(/\/$/, "");`
  then strip the basename from `location.pathname` to get the internal route
  (`pathname.slice(basename.length)` when the pathname starts with it).

Build config: `base: './'` in the Vite config so the emitted asset URLs are
relative.
