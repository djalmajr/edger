// EdgeR WebIDE shadcn primitives.
//
// shadcn is source-owned: applications compose these primitives while the
// primitive itself owns the native element, data-slot contract, variants and
// accessibility attributes. The API mirrors the cPanel preset without tying
// the WebIDE renderer to a framework runtime.

function escapeAttribute(value) {
  return String(value ?? "")
    .replaceAll("&", "&amp;")
    .replaceAll('"', "&quot;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;");
}

const attributeNames = {
  className: "class",
  htmlFor: "for",
  readOnly: "readonly",
  tabIndex: "tabindex",
};

function attributes(props = {}) {
  return Object.entries(props)
    .filter(([name, value]) => value !== undefined && value !== null && (value !== false || name.startsWith("aria-") || name.startsWith("data-")))
    .map(([name, value]) => {
      const attribute = attributeNames[name] || name;
      if (typeof value === "boolean" && (name.startsWith("aria-") || name.startsWith("data-"))) return `${attribute}="${value}"`;
      return value === true ? attribute : `${attribute}="${escapeAttribute(value)}"`;
    })
    .join(" ");
}

function element(tag, slot, { children = "", ...props } = {}) {
  return `<${tag} data-slot="${slot}" ${attributes(props)}>${children}</${tag}>`;
}

export function Button({ variant = "default", size = "default", ...props } = {}) {
  return element("button", "button", { type: "button", "data-variant": variant, "data-size": size, ...props });
}

export function ButtonLink({ variant = "ghost", size = "default", ...props } = {}) {
  return element("a", "button", { "data-variant": variant, "data-size": size, ...props });
}

export function Input(props = {}) {
  return `<input data-slot="input" ${attributes(props)}>`;
}

export function Textarea({ children = "", ...props } = {}) {
  return element("textarea", "textarea", { ...props, children });
}

export function Checkbox(props = {}) {
  return `<input data-slot="checkbox" type="checkbox" ${attributes(props)}>`;
}

export function InputGroup({ as = "div", children = "", ...props } = {}) {
  return element(as, "input-group", { role: as === "div" ? "group" : undefined, ...props, children });
}

export function InputGroupAddon({ children = "", align = "inline-start", ...props } = {}) {
  return element("span", "input-group-addon", { "data-align": align, ...props, children });
}

export function Field({ children = "", ...props } = {}) {
  return element("div", "field", { ...props, children });
}

export function FieldLabel({ children = "", ...props } = {}) {
  return element("label", "field-label", { ...props, children });
}

export function FieldError({ children = "", ...props } = {}) {
  return element("p", "field-error", { ...props, children });
}

export function Card({ children = "", ...props } = {}) {
  return element("article", "card", { ...props, children });
}

export function CardContent({ children = "", ...props } = {}) {
  return element("div", "card-content", { ...props, children });
}

export function CardTitle({ children = "", ...props } = {}) {
  return element("strong", "card-title", { ...props, children });
}

export function CardDescription({ children = "", ...props } = {}) {
  return element("small", "card-description", { ...props, children });
}

export function Badge({ children = "", variant = "secondary", ...props } = {}) {
  return element("span", "badge", { "data-variant": variant, ...props, children });
}

export function TabsList({ children = "", ...props } = {}) {
  return element("div", "tabs-list", { role: "tablist", ...props, children });
}

export function TabsTrigger({ children = "", active = false, ...props } = {}) {
  return element("button", "tabs-trigger", {
    type: "button",
    role: "tab",
    "aria-selected": active,
    "data-state": active ? "active" : "inactive",
    ...props,
    children,
  });
}

export function TabsContent({ children = "", ...props } = {}) {
  return element("div", "tabs-content", { role: "tabpanel", ...props, children });
}

function dialogParts(slot) {
  return {
    root({ children = "", ...props } = {}) {
      return element("div", `${slot}-overlay`, { ...props, children });
    },
    content({ as = "form", children = "", ...props } = {}) {
      return element(as, `${slot}-content`, {
        role: slot === "alert-dialog" ? "alertdialog" : "dialog",
        "aria-modal": true,
        ...props,
        children,
      });
    },
    header({ children = "", ...props } = {}) { return element("header", `${slot}-header`, { ...props, children }); },
    title({ children = "", ...props } = {}) { return element("h2", `${slot}-title`, { ...props, children }); },
    description({ children = "", ...props } = {}) { return element("p", `${slot}-description`, { ...props, children }); },
    footer({ children = "", ...props } = {}) { return element("footer", `${slot}-footer`, { ...props, children }); },
  };
}

const dialog = dialogParts("dialog");
const alertDialog = dialogParts("alert-dialog");
export const Dialog = dialog.root;
export const DialogContent = dialog.content;
export const DialogHeader = dialog.header;
export const DialogTitle = dialog.title;
export const DialogDescription = dialog.description;
export const DialogFooter = dialog.footer;
export const AlertDialog = alertDialog.root;
export const AlertDialogContent = alertDialog.content;
export const AlertDialogHeader = alertDialog.header;
export const AlertDialogTitle = alertDialog.title;
export const AlertDialogDescription = alertDialog.description;
export const AlertDialogFooter = alertDialog.footer;

export function Empty({ children = "", ...props } = {}) {
  return element("div", "empty", { ...props, children });
}

export function EmptyTitle({ children = "", ...props } = {}) {
  return element("h3", "empty-title", { ...props, children });
}

export function EmptyDescription({ children = "", ...props } = {}) {
  return element("p", "empty-description", { ...props, children });
}

export function Table({ children = "", ...props } = {}) {
  return `<div data-slot="table-container" class="project-table"><table data-slot="table" ${attributes(props)}>${children}</table></div>`;
}

export function TableHeader({ children = "", ...props } = {}) { return element("thead", "table-header", { ...props, children }); }
export function TableBody({ children = "", ...props } = {}) { return element("tbody", "table-body", { ...props, children }); }
export function TableRow({ children = "", ...props } = {}) { return element("tr", "table-row", { ...props, children }); }
export function TableHead({ children = "", ...props } = {}) { return element("th", "table-head", { scope: "col", ...props, children }); }
export function TableCell({ children = "", ...props } = {}) { return element("td", "table-cell", { ...props, children }); }

export function ContextMenuContent({ children = "", ...props } = {}) {
  return element("div", "context-menu-content", { role: "menu", ...props, children });
}

export function ContextMenuItem({ children = "", destructive = false, ...props } = {}) {
  return element("button", "context-menu-item", {
    type: "button",
    role: "menuitem",
    "data-variant": destructive ? "destructive" : "default",
    ...props,
    children,
  });
}

export function ContextMenuSeparator(props = {}) {
  return `<div data-slot="context-menu-separator" role="separator" ${attributes(props)}></div>`;
}

export function ResizableHandle({ orientation = "vertical", ...props } = {}) {
  return `<div data-slot="resizable-handle" role="separator" aria-orientation="${orientation}" tabindex="0" ${attributes(props)}></div>`;
}

export function Toaster({ children = "", ...props } = {}) {
  return element("div", "toaster", { "aria-live": "polite", "aria-atomic": true, ...props, children });
}

export function Toast({ title, description = "", variant = "default", children = "", ...props } = {}) {
  return element("section", "toast", {
    role: variant === "error" ? "alert" : "status",
    "data-variant": variant,
    ...props,
    children: `<div><strong>${title}</strong>${description ? `<p>${description}</p>` : ""}</div>${children}`,
  });
}

export function Tooltip({ content, children, ...props } = {}) {
  return element("span", "tooltip", {
    ...props,
    children: `${children}<span data-slot="tooltip-content" role="tooltip">${content}</span>`,
  });
}
