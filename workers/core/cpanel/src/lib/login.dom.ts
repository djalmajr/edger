import { GlobalRegistrator } from "@happy-dom/global-registrator";

// The cpanel derives the runtime root from the injected <base href>. Under
// vitest no DOM exists yet: register a deep proxy prefix so the tests prove
// login routes resolve against the base's PARENT, never the SPA prefix.
// Under bun test the workers preload already registered a DOM (single-segment
// /cpanel/ base) — or none, if a sibling file's afterAll unregistered it —
// so register when absent. The assertions compute the expected root from
// document.baseURI, so either base is checked the same way.
//
// Never unregister here: bun runs the whole suite in one process, and dropping
// the globals would break every later test file that still needs a DOM.
// Exported so the test file can re-assert it in beforeAll — bun evaluates
// every test file's modules before any test runs, so a sibling file's
// afterAll can kill the shared globals in between.
export function ensureLoginDom(): void {
  if (!("happyDOM" in globalThis)) {
    GlobalRegistrator.register({ url: "http://localhost/apps/cpanel/" });
  }
}

ensureLoginDom();
