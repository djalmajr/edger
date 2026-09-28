import * as React from "react";
import { createRequire } from "node:module";
import { GlobalRegistrator } from "@happy-dom/global-registrator";

const nodeRequire = createRequire(import.meta.url);

// Bun's test runner has no Vite icon plugin, and a module's export surface is
// frozen process-wide by whichever sibling test file loads it first (the
// routing-policy tests mock the icon barrel with only their three icons).
// The login screen's production code is therefore barrel-free (see
// glyphs.tsx); the one remaining barrel edge is dropdown-menu.tsx itself, so
// under bun the whole module is replaced with a DOM stub that keeps the menu
// triggers accessible without touching the frozen barrel.
if ((process.versions as { bun?: string }).bun) {
  const bunTest = nodeRequire("bun:test") as {
    mock: {
      module: (id: string, factory: () => Record<string, unknown>) => void;
    };
  };
  const DropdownMenu = ({ children }: { children?: unknown }) =>
    React.createElement("div", null, children as React.ReactNode);
  const DropdownMenuContent = ({ children }: { children?: unknown }) =>
    React.createElement(
      "div",
      { role: "menu" },
      children as React.ReactNode,
    );
  const DropdownMenuTrigger = ({
    render,
    children,
  }: {
    render?: React.ReactElement<Record<string, unknown>>;
    children?: unknown;
  }) =>
    render
      ? React.cloneElement(
          render,
          { type: "button" } as Partial<Record<string, unknown>>,
          children as React.ReactNode,
        )
      : React.createElement("button", { type: "button" }, children as React.ReactNode);
  const DropdownMenuRadioGroup = ({ children }: { children?: unknown }) =>
    React.createElement(
      "div",
      { role: "radiogroup" },
      children as React.ReactNode,
    );
  const DropdownMenuRadioItem = ({ children }: { children?: unknown }) =>
    React.createElement(
      "button",
      { role: "radio", type: "button" },
      children as React.ReactNode,
    );
  bunTest.mock.module("@edger/ui/components/ui/dropdown-menu", () => ({
    DropdownMenu,
    DropdownMenuContent,
    DropdownMenuRadioGroup,
    DropdownMenuRadioItem,
    DropdownMenuTrigger,
  }));
}

// Exported so the test file can re-assert it in beforeAll: bun evaluates
// every test file's modules before any test runs, so a sibling file's
// afterAll can unregister the shared globals in between. Never unregister
// here — bun runs the whole suite in one process.
export function ensureLoginDom(): void {
  if (!("happyDOM" in globalThis)) {
    GlobalRegistrator.register({ url: "http://localhost/cpanel/" });
  }
}

ensureLoginDom();
