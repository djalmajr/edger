// shadcn/select — composed, styled select using a details-backed popup.
import { createContext } from "preact";
import { useContext, useEffect, useRef } from "preact/hooks";
import { html } from "htm/preact";
import { cn } from "./utils.js";

const SelectContext = createContext(null);

export function Select({ value, onValueChange, multiple = false, className = "", children, ...props }) {
  const rootRef = useRef(null);
  const close = () => rootRef.current?.removeAttribute("open");
  useEffect(() => {
    const handlePointerDown = (event) => {
      if (!rootRef.current?.contains(event.target)) close();
    };
    document.addEventListener("pointerdown", handlePointerDown);
    return () => document.removeEventListener("pointerdown", handlePointerDown);
  }, []);
  return html`
    <${SelectContext.Provider} value=${{ close, multiple, onValueChange, value }}>
      <details ref=${rootRef} data-slot="select" class=${cn("group/select relative", className)} ...${props}>
        ${children}
      </details>
    <//>
  `;
}

export function SelectTrigger({ className = "", children, ...props }) {
  return html`
    <summary
      data-slot="select-trigger"
      class=${cn(
        "border-input bg-background flex h-9 w-fit cursor-pointer list-none items-center justify-between gap-2 rounded-md border px-3 py-1 text-sm shadow-xs outline-none transition-colors hover:bg-accent focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50 [&::-webkit-details-marker]:hidden",
        className,
      )}
      ...${props}
    >
      ${children}
      <iconify-icon icon="lucide:chevron-down" width="16" class="text-muted-foreground transition-transform group-open/select:rotate-180"></iconify-icon>
    </summary>
  `;
}

export function SelectValue({ placeholder = "Select…", className = "", children, ...props }) {
  const context = useContext(SelectContext);
  return html`<span data-slot="select-value" class=${cn("truncate", className)} ...${props}>${children ?? context?.value ?? placeholder}</span>`;
}

export function SelectContent({ align = "start", className = "", children, ...props }) {
  return html`
    <div
      data-slot="select-content"
      class=${cn(
        "bg-popover text-popover-foreground absolute top-[calc(100%+0.25rem)] z-50 min-w-full overflow-hidden rounded-md border p-1 shadow-md",
        align === "end" ? "right-0" : "left-0",
        className,
      )}
      ...${props}
    >
      ${children}
    </div>
  `;
}

export function SelectGroup({ className = "", children, ...props }) {
  return html`<div data-slot="select-group" class=${cn("grid gap-0.5", className)} ...${props}>${children}</div>`;
}

export function SelectItem({ value, className = "", children, ...props }) {
  const context = useContext(SelectContext);
  const selected = context?.multiple ? context?.value?.includes(value) : context?.value === value;
  return html`
    <button
      type="button"
      data-slot="select-item"
      data-selected=${selected ? "true" : "false"}
      class=${cn(
        "hover:bg-accent hover:text-accent-foreground flex w-full cursor-pointer items-center gap-2 rounded-sm px-2 py-1.5 text-left text-sm outline-none",
        className,
      )}
      onClick=${() => {
        if (context?.multiple) {
          const current = context.value || [];
          context?.onValueChange?.(selected ? current.filter((item) => item !== value) : [...current, value]);
        } else {
          context?.onValueChange?.(value);
          context?.close?.();
        }
      }}
      ...${props}
    >
      <span class=${cn("flex size-4 items-center justify-center", selected ? "opacity-100" : "opacity-0")}><iconify-icon icon="lucide:check" width="14"></iconify-icon></span>
      <span class="flex-1 truncate">${children}</span>
    </button>
  `;
}
