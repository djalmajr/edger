// shadcn/native-select — explicit browser-native select for cases that require it.
import { html } from "htm/preact";
import { cn } from "./utils.js";

export function NativeSelect({ className = "", children, ...props }) {
  return html`<select data-slot="native-select" class=${cn("border-input bg-background h-9 rounded-md border px-3 text-sm", className)} ...${props}>${children}</select>`;
}

export function NativeSelectGroup({ label, children, ...props }) {
  return html`<optgroup label=${label} ...${props}>${children}</optgroup>`;
}

export function NativeSelectItem({ value, children, ...props }) {
  return html`<option value=${value} ...${props}>${children}</option>`;
}
