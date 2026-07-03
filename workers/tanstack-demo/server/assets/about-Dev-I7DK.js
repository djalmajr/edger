import { c as require_jsx_runtime } from "../server.js";
//#region src/routes/about.tsx?tsr-split=component
var import_jsx_runtime = require_jsx_runtime();
function About() {
	return /* @__PURE__ */ (0, import_jsx_runtime.jsx)("main", {
		className: "page-wrap px-4 py-12",
		children: /* @__PURE__ */ (0, import_jsx_runtime.jsxs)("section", {
			className: "island-shell rounded-2xl p-6 sm:p-8",
			children: [
				/* @__PURE__ */ (0, import_jsx_runtime.jsx)("p", {
					className: "island-kicker mb-2",
					children: "About"
				}),
				/* @__PURE__ */ (0, import_jsx_runtime.jsx)("h1", {
					className: "display-title mb-3 text-4xl font-bold text-[var(--sea-ink)] sm:text-5xl",
					children: "A small starter with room to grow."
				}),
				/* @__PURE__ */ (0, import_jsx_runtime.jsx)("p", {
					className: "m-0 max-w-3xl text-base leading-8 text-[var(--sea-ink-soft)]",
					children: "TanStack Start gives you type-safe routing, server functions, and modern SSR defaults. Use this as a clean foundation, then layer in your own routes, styling, and add-ons."
				})
			]
		})
	});
}
//#endregion
export { About as component };
