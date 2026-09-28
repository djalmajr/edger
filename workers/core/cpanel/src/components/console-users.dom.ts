import * as React from "react";
import { createRequire } from "node:module";
import { GlobalRegistrator } from "@happy-dom/global-registrator";

const nodeRequire = createRequire(import.meta.url);

// Bun's test runner has no Vite plugins, and the @edger/ui icon barrel's
// export surface is frozen process-wide by whichever sibling test file loads
// it first. The users screen's production code is barrel-free (text action
// buttons, no icons); the one remaining edge is dialog.tsx itself, so under
// bun the whole module is replaced with a DOM stub that keeps the dialog
// content reachable without touching the frozen barrel.
if ((process.versions as { bun?: string }).bun) {
  const bunTest = nodeRequire("bun:test") as {
    mock: {
      module: (id: string, factory: () => Record<string, unknown>) => void;
    };
  };
  const Dialog = ({
    children,
    open,
  }: {
    children?: unknown;
    open?: boolean;
  }) =>
    open
      ? React.createElement(
          React.Fragment,
          null,
          children as React.ReactNode,
        )
      : null;
  const passthrough =
    (tag: "footer" | "h2" | "header" | "p") =>
    ({ children }: { children?: unknown }) =>
      React.createElement(tag, null, children as React.ReactNode);
  bunTest.mock.module("@edger/ui/components/ui/dialog", () => ({
    Dialog,
    DialogContent: ({ children }: { children?: unknown }) =>
      React.createElement("div", { role: "dialog" }, children as React.ReactNode),
    DialogDescription: passthrough("p"),
    DialogFooter: passthrough("footer"),
    DialogHeader: passthrough("header"),
    DialogTitle: passthrough("h2"),
  }));
}

// Exported so the test file can re-assert it in beforeAll: bun evaluates
// every test file's modules before any test runs, so a sibling file's
// afterAll can unregister the shared globals in between. Never unregister
// here — bun runs the whole suite in one process.
export function ensureConsoleUsersDom(): void {
  if (!("happyDOM" in globalThis)) {
    GlobalRegistrator.register({ url: "http://localhost/cpanel/" });
  }
  // happy-dom has no Web Animations API: base-ui's ScrollArea viewport (the
  // one Table uses) calls getAnimations shortly after mount and would
  // otherwise crash the next test with an unhandled error.
  if (
    typeof Element !== "undefined" &&
    !("getAnimations" in Element.prototype)
  ) {
    (Element.prototype as unknown as {
      getAnimations(options?: unknown): unknown[];
    }).getAnimations = () => [];
  }
}

ensureConsoleUsersDom();
