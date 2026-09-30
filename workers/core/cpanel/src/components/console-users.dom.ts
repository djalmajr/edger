import * as React from "react";
import { createRequire } from "node:module";
import { GlobalRegistrator } from "@happy-dom/global-registrator";
// Registers the shared bun-only union mocks (icon barrel + dropdown-menu)
// before any consumer loads; inert under the vite-backed runner.
import "./bun-ui-mocks";

const nodeRequire = createRequire(import.meta.url);

// Bun's test runner has no Vite plugins, and the @edger/ui icon barrel's
// export surface is frozen process-wide by whichever sibling test file loads
// it first. The users screen imports its row-action icons from the virtual
// unplugin-icons modules (mocked below, one specifier per icon) instead of
// the frozen barrel; the one remaining edge for the barrel is dialog.tsx
// itself, so under bun the whole dialog module is replaced with a DOM stub
// that keeps the dialog content reachable.
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

  // The users table now renders the shared DataGrid, which pulls the frozen
  // icon barrel (covered by the shared ./bun-ui-mocks fixture) and the
  // base-ui combobox; the combobox is replaced under bun with the same DOM
  // stub the API Keys fixture uses, so pagination stays testable (the
  // page-size control is a native select).
  bunTest.mock.module("@edger/ui/components/ui/combobox", () => ({
    Combobox: ({
      "aria-label": ariaLabel,
      onValueChange,
      options,
      value,
    }: {
      "aria-label"?: string;
      onValueChange(value: string): void;
      options: Array<{ label: string; value: string }>;
      value: string;
    }) =>
      React.createElement(
        "select",
        {
          "aria-label": ariaLabel,
          onChange: (event: React.ChangeEvent<HTMLSelectElement>) =>
            onValueChange(event.target.value),
          value,
        },
        options.map((option) =>
          React.createElement(
            "option",
            { key: option.value, value: option.value },
            option.label,
          ),
        ),
      ),
  }));

  // base-ui's menu is registered by the shared ./bun-ui-mocks fixture
  // (stateful DOM stub, content inline once the trigger is clicked).

  // The row actions use compact icons imported from the virtual
  // unplugin-icons modules (vite resolves them; bun cannot). One mock per
  // specifier, mirroring the barrel stubs: a decorative aria-hidden element.
  const virtualIcon = () => ({
    default: () =>
      React.createElement("svg", {
        "aria-hidden": true,
        height: "1.2em",
        width: "1.2em",
        viewBox: "0 0 24 24",
      }),
  });
  for (const name of [
    "~icons/lucide/key-round",
    "~icons/lucide/pencil",
    "~icons/lucide/trash-2",
    "~icons/lucide/user-check",
    "~icons/lucide/user-x",
  ]) {
    bunTest.mock.module(name, virtualIcon);
  }

  // base-ui's tooltip only loads fine under the vite-backed runner; the same
  // trigger passthrough the API Keys fixture uses keeps the icon buttons
  // (with their aria-labels) reachable under bun.
  bunTest.mock.module("@edger/ui/components/ui/tooltip", () => ({
    Tooltip: ({ children }: { children?: unknown }) =>
      React.createElement(React.Fragment, null, children as React.ReactNode),
    TooltipContent: () => null,
    TooltipTrigger: ({
      children,
      render,
    }: {
      children?: unknown;
      render?: React.ReactElement;
    }) =>
      render
        ? React.cloneElement(render, undefined, children as React.ReactNode)
        : React.createElement(
            React.Fragment,
            null,
            children as React.ReactNode,
          ),
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
