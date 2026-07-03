import { c as require_jsx_runtime } from "../server.js";
//#region src/routes/index.tsx?tsr-split=component
var import_jsx_runtime = require_jsx_runtime();
function App() {
	return /* @__PURE__ */ (0, import_jsx_runtime.jsxs)("main", {
		className: "page-wrap px-4 pb-8 pt-14",
		children: [
			/* @__PURE__ */ (0, import_jsx_runtime.jsxs)("section", {
				className: "island-shell rise-in relative overflow-hidden rounded-[2rem] px-6 py-10 sm:px-10 sm:py-14",
				children: [
					/* @__PURE__ */ (0, import_jsx_runtime.jsx)("div", { className: "pointer-events-none absolute -left-20 -top-24 h-56 w-56 rounded-full bg-[radial-gradient(circle,rgba(79,184,178,0.32),transparent_66%)]" }),
					/* @__PURE__ */ (0, import_jsx_runtime.jsx)("div", { className: "pointer-events-none absolute -bottom-20 -right-20 h-56 w-56 rounded-full bg-[radial-gradient(circle,rgba(47,106,74,0.18),transparent_66%)]" }),
					/* @__PURE__ */ (0, import_jsx_runtime.jsx)("p", {
						className: "island-kicker mb-3",
						children: "TanStack Start Base Template"
					}),
					/* @__PURE__ */ (0, import_jsx_runtime.jsx)("h1", {
						className: "display-title mb-5 max-w-3xl text-4xl leading-[1.02] font-bold tracking-tight text-[var(--sea-ink)] sm:text-6xl",
						children: "Start simple, ship quickly."
					}),
					/* @__PURE__ */ (0, import_jsx_runtime.jsx)("p", {
						className: "mb-8 max-w-2xl text-base text-[var(--sea-ink-soft)] sm:text-lg",
						children: "This base starter intentionally keeps things light: two routes, clean structure, and the essentials you need to build from scratch."
					}),
					/* @__PURE__ */ (0, import_jsx_runtime.jsxs)("div", {
						className: "flex flex-wrap gap-3",
						children: [/* @__PURE__ */ (0, import_jsx_runtime.jsx)("a", {
							href: "/about",
							className: "rounded-full border border-[rgba(50,143,151,0.3)] bg-[rgba(79,184,178,0.14)] px-5 py-2.5 text-sm font-semibold text-[var(--lagoon-deep)] no-underline transition hover:-translate-y-0.5 hover:bg-[rgba(79,184,178,0.24)]",
							children: "About This Starter"
						}), /* @__PURE__ */ (0, import_jsx_runtime.jsx)("a", {
							href: "https://tanstack.com/router",
							target: "_blank",
							rel: "noopener noreferrer",
							className: "rounded-full border border-[rgba(23,58,64,0.2)] bg-white/50 px-5 py-2.5 text-sm font-semibold text-[var(--sea-ink)] no-underline transition hover:-translate-y-0.5 hover:border-[rgba(23,58,64,0.35)]",
							children: "Router Guide"
						})]
					})
				]
			}),
			/* @__PURE__ */ (0, import_jsx_runtime.jsx)("section", {
				className: "mt-8 grid gap-4 sm:grid-cols-2 lg:grid-cols-4",
				children: [
					["Type-Safe Routing", "Routes and links stay in sync across every page."],
					["Server Functions", "Call server code from your UI without creating API boilerplate."],
					["Streaming by Default", "Ship progressively rendered responses for faster experiences."],
					["Tailwind Native", "Design quickly with utility-first styling and reusable tokens."]
				].map(([title, desc], index) => /* @__PURE__ */ (0, import_jsx_runtime.jsxs)("article", {
					className: "island-shell feature-card rise-in rounded-2xl p-5",
					style: { animationDelay: `${index * 90 + 80}ms` },
					children: [/* @__PURE__ */ (0, import_jsx_runtime.jsx)("h2", {
						className: "mb-2 text-base font-semibold text-[var(--sea-ink)]",
						children: title
					}), /* @__PURE__ */ (0, import_jsx_runtime.jsx)("p", {
						className: "m-0 text-sm text-[var(--sea-ink-soft)]",
						children: desc
					})]
				}, title))
			}),
			/* @__PURE__ */ (0, import_jsx_runtime.jsxs)("section", {
				className: "island-shell mt-8 rounded-2xl p-6",
				children: [/* @__PURE__ */ (0, import_jsx_runtime.jsx)("p", {
					className: "island-kicker mb-2",
					children: "Quick Start"
				}), /* @__PURE__ */ (0, import_jsx_runtime.jsxs)("ul", {
					className: "m-0 list-disc space-y-2 pl-5 text-sm text-[var(--sea-ink-soft)]",
					children: [
						/* @__PURE__ */ (0, import_jsx_runtime.jsxs)("li", { children: [
							"Edit ",
							/* @__PURE__ */ (0, import_jsx_runtime.jsx)("code", { children: "src/routes/index.tsx" }),
							" to customize the home page."
						] }),
						/* @__PURE__ */ (0, import_jsx_runtime.jsxs)("li", { children: [
							"Update ",
							/* @__PURE__ */ (0, import_jsx_runtime.jsx)("code", { children: "src/components/Header.tsx" }),
							" and",
							" ",
							/* @__PURE__ */ (0, import_jsx_runtime.jsx)("code", { children: "src/components/Footer.tsx" }),
							" for brand links."
						] }),
						/* @__PURE__ */ (0, import_jsx_runtime.jsxs)("li", { children: [
							"Add routes in ",
							/* @__PURE__ */ (0, import_jsx_runtime.jsx)("code", { children: "src/routes" }),
							" and tweak visual tokens in",
							" ",
							/* @__PURE__ */ (0, import_jsx_runtime.jsx)("code", { children: "src/styles.css" }),
							"."
						] })
					]
				})]
			})
		]
	});
}
//#endregion
export { App as component };
