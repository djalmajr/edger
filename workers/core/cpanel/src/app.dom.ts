import { GlobalRegistrator } from "@happy-dom/global-registrator";

// Registers the shared happy-dom for the root-app tests BEFORE ./main
// evaluates: main.tsx freezes the runtime root from document.baseURI at
// module load (via lib/api) and its mount code renders into the #root
// element it finds. The #root must exist before the import so the app's own
// mount code can run; the tests then drive that mounted instance — the real
// router, query client and providers — instead of rendering a second one.
//
// Exported so the test file can re-assert it in beforeAll: bun evaluates
// every test file's modules before any test runs, so a sibling file's
// afterAll can unregister the shared globals in between. Never unregister
// here — bun runs the whole suite in one process.
export function ensureAppDom(): void {
  if (!("happyDOM" in globalThis)) {
    GlobalRegistrator.register({ url: "http://localhost/cpanel/" });
  }
  if (!document.getElementById("root")) {
    const root = document.createElement("div");
    root.id = "root";
    document.body.append(root);
  }
  // happy-dom has no Web Animations API: base-ui's ScrollArea viewport
  // (the Table the Shell renders) calls getAnimations shortly after mount
  // and would otherwise crash with an unhandled error.
  if (
    typeof Element !== "undefined" &&
    !("getAnimations" in Element.prototype)
  ) {
    (Element.prototype as unknown as {
      getAnimations(options?: unknown): unknown[];
    }).getAnimations = () => [];
  }
}

ensureAppDom();
