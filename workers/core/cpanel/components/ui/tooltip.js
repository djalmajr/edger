// Tooltip — isolated hover/focus state. No JS positioning.
// Usage:
//   <Tooltip content="Add to library">
//     <Button>+<//>
//   </Tooltip>

import { html } from "htm/preact";
import { useState } from "preact/hooks";
import { cn } from "./utils.js";

export function Tooltip({ content, side = "top", align = "center", className = "", contentClassName = "", children }) {
  const [open, setOpen] = useState(false);
  const sideClass =
    side === "bottom"
      ? "top-full mt-1.5"
      : side === "right"
        ? "left-full ml-1.5 top-1/2 -translate-y-1/2"
        : side === "left"
          ? "right-full mr-1.5 top-1/2 -translate-y-1/2"
          : "bottom-full mb-1.5";
  const alignClass =
    side === "left" || side === "right"
      ? ""
      : align === "start"
        ? "left-0"
        : align === "end"
          ? "right-0"
          : "left-1/2 -translate-x-1/2";

  return html`
    <span
      data-slot="tooltip"
      class=${cn("relative inline-flex", className)}
      onBlur=${(event) => {
        if (!event.currentTarget.contains(event.relatedTarget)) setOpen(false);
      }}
      onFocus=${() => setOpen(true)}
      onMouseEnter=${() => setOpen(true)}
      onMouseLeave=${() => setOpen(false)}
    >
      ${children}
      <span
        aria-hidden=${!open}
        data-slot="tooltip-content"
        role="tooltip"
        class=${cn(
          "pointer-events-none absolute z-50 w-max max-w-[min(20rem,calc(100vw-2rem))] break-words whitespace-normal",
          "rounded-md bg-foreground px-2.5 py-1.5 text-xs leading-relaxed text-background shadow-md",
          open ? "visible opacity-100" : "hidden",
          "transition-opacity",
          sideClass,
          alignClass,
          contentClassName,
        )}
      >
        ${content}
      </span>
    </span>
  `;
}
