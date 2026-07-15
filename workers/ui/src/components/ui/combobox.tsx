"use client";

import { Check, ChevronsUpDown, X } from "@edger/ui/icons/lucide";
import * as React from "react";
import { Badge } from "@edger/ui/components/ui/badge";
import { Button } from "@edger/ui/components/ui/button";
import {
  Command,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
} from "@edger/ui/components/ui/command";
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@edger/ui/components/ui/popover";
import { cn } from "@edger/ui/lib/utils";

export type ComboboxOption = {
  value: string;
  label: string;
  /** Extra tokens for cmdk search (ids, etc.). */
  keywords?: string;
  disabled?: boolean;
};

type ComboboxProps = {
  options: ComboboxOption[];
  value: string;
  onValueChange: (value: string) => void;
  /** Search input — only when the list can grow large. */
  searchable?: boolean;
  searchPlaceholder?: string;
  emptyText?: string;
  placeholder?: string;
  disabled?: boolean;
  className?: string;
  contentClassName?: string;
  triggerClassName?: string;
  align?: "start" | "center" | "end";
  side?: "top" | "bottom" | "left" | "right";
  /** Accessible name for the trigger. */
  "aria-label"?: string;
  id?: string;
};

/**
 * Popover + Command combobox (Base UI / shadcn pattern).
 * Prefer over Select for consistent UX. Pass `searchable` only when needed.
 */
export function Combobox({
  options,
  value,
  onValueChange,
  searchable = false,
  searchPlaceholder,
  emptyText,
  placeholder = "…",
  disabled,
  className,
  contentClassName,
  triggerClassName,
  align = "start",
  side = "bottom",
  "aria-label": ariaLabel,
  id,
}: ComboboxProps) {
  const [open, setOpen] = React.useState(false);
  const selected = options.find((o) => o.value === value);
  const label = selected?.label ?? placeholder;

  return (
    <div className={cn("inline-flex", className)}>
      <Popover open={open} onOpenChange={setOpen}>
        <PopoverTrigger
          render={
            <Button
              aria-expanded={open}
              aria-label={ariaLabel}
              className={cn(
                "h-9 w-full min-w-0 justify-between border-input font-normal dark:bg-input/30",
                !selected && "text-muted-foreground",
                triggerClassName,
              )}
              disabled={disabled}
              id={id}
              role="combobox"
              type="button"
              variant="outline"
            />
          }
        >
          <span className="truncate">{label}</span>
          <ChevronsUpDown className="size-4 shrink-0 opacity-50" />
        </PopoverTrigger>
        <PopoverContent
          align={align}
          className={cn(
            // Match trigger width (Base UI sets --anchor-width on the positioner).
            "w-(--anchor-width)! min-w-(--anchor-width)! max-w-(--anchor-width)! p-0",
            contentClassName,
          )}
          side={side}
        >
          <Command>
            {searchable ? (
              <CommandInput placeholder={searchPlaceholder} />
            ) : null}
            <CommandList>
              {searchable ? (
                <CommandEmpty>{emptyText ?? "—"}</CommandEmpty>
              ) : null}
              <CommandGroup>
                {options.map((opt) => (
                  <CommandItem
                    key={opt.value}
                    data-checked={value === opt.value || undefined}
                    disabled={opt.disabled}
                    value={
                      opt.keywords
                        ? `${opt.label} ${opt.keywords}`
                        : `${opt.label} ${opt.value}`
                    }
                    onSelect={() => {
                      onValueChange(opt.value);
                      setOpen(false);
                    }}
                  >
                    <span className="truncate">{opt.label}</span>
                  </CommandItem>
                ))}
              </CommandGroup>
            </CommandList>
          </Command>
        </PopoverContent>
      </Popover>
    </div>
  );
}

type MultiComboboxProps = Omit<ComboboxProps, "value" | "onValueChange"> & {
  /** Selected values. */
  values: string[];
  onValuesChange: (values: string[]) => void;
};

/**
 * Multi-select combobox (Popover + Command). Selected options show as removable
 * tags in the trigger; picking an item toggles it and keeps the list open.
 */
export function MultiCombobox({
  options,
  values,
  onValuesChange,
  searchable = true,
  searchPlaceholder,
  emptyText,
  placeholder = "…",
  disabled,
  className,
  contentClassName,
  triggerClassName,
  align = "start",
  side = "bottom",
  "aria-label": ariaLabel,
  id,
}: MultiComboboxProps) {
  const [open, setOpen] = React.useState(false);
  const selected = options.filter((o) => values.includes(o.value));
  const toggle = (v: string) =>
    onValuesChange(
      values.includes(v) ? values.filter((x) => x !== v) : [...values, v],
    );

  return (
    <div className={cn("inline-flex w-full", className)}>
      <Popover open={open} onOpenChange={setOpen}>
        <PopoverTrigger
          render={
            <Button
              aria-expanded={open}
              aria-label={ariaLabel}
              className={cn(
                "h-auto min-h-9 w-full min-w-0 justify-between border-input font-normal dark:bg-input/30",
                triggerClassName,
              )}
              disabled={disabled}
              id={id}
              role="combobox"
              type="button"
              variant="outline"
            />
          }
        >
          <span className="flex flex-1 flex-wrap items-center gap-1 py-0.5">
            {selected.length === 0 ? (
              <span className="text-muted-foreground">{placeholder}</span>
            ) : (
              selected.map((o) => (
                <Badge key={o.value} variant="secondary" className="gap-1 pr-1">
                  {o.label}
                  <span
                    aria-label={`remover ${o.label}`}
                    className="inline-flex cursor-pointer items-center rounded-sm opacity-70 hover:opacity-100"
                    onClick={(e) => {
                      e.stopPropagation();
                      e.preventDefault();
                      toggle(o.value);
                    }}
                    onPointerDown={(e) => e.stopPropagation()}
                    role="button"
                    tabIndex={-1}
                  >
                    <X className="size-3" />
                  </span>
                </Badge>
              ))
            )}
          </span>
          <ChevronsUpDown className="size-4 shrink-0 self-center opacity-50" />
        </PopoverTrigger>
        <PopoverContent
          align={align}
          className={cn(
            "w-(--anchor-width)! min-w-(--anchor-width)! max-w-(--anchor-width)! p-0",
            contentClassName,
          )}
          side={side}
        >
          <Command>
            {searchable ? (
              <CommandInput placeholder={searchPlaceholder} />
            ) : null}
            <CommandList>
              {searchable ? (
                <CommandEmpty>{emptyText ?? "—"}</CommandEmpty>
              ) : null}
              <CommandGroup>
                {options.map((opt) => {
                  const on = values.includes(opt.value);
                  return (
                    <CommandItem
                      key={opt.value}
                      data-checked={on || undefined}
                      disabled={opt.disabled}
                      value={
                        opt.keywords
                          ? `${opt.label} ${opt.keywords}`
                          : `${opt.label} ${opt.value}`
                      }
                      onSelect={() => toggle(opt.value)}
                    >
                      <Check
                        className={cn(
                          "size-4 shrink-0",
                          on ? "opacity-100" : "opacity-0",
                        )}
                      />
                      <span className="truncate">{opt.label}</span>
                    </CommandItem>
                  );
                })}
              </CommandGroup>
            </CommandList>
          </Command>
        </PopoverContent>
      </Popover>
    </div>
  );
}
