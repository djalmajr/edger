import type {
  Table as TanstackTable,
  Updater,
  VisibilityState,
} from "@tanstack/react-table";

import { Button } from "@edger/ui/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuCheckboxItem,
  DropdownMenuContent,
  DropdownMenuGroup,
  DropdownMenuLabel,
  DropdownMenuTrigger,
} from "@edger/ui/components/ui/dropdown-menu";
import { SettingsIcon } from "@edger/ui/icons/lucide";
import * as React from "react";

import { useI18n } from "../lib/i18n";

// Appliance-style local preference: each table persists its column
// visibility under its own key (keys/users never share the choice). Only the
// booleans of the table's own column ids ever reach the storage — no row data.
const COLUMN_VISIBILITY_PREFIX = "edger.cpanel.columns.";

export function columnVisibilityStorageKey(table: "keys" | "users"): string {
  return `${COLUMN_VISIBILITY_PREFIX}${table}`;
}

// The storage is local but user-editable (devtools, other apps on the origin):
// accept only plain objects, only ids this table actually has, and only
// boolean flags. Everything else is dropped, never thrown away with the page.
export function sanitizeVisibility(
  raw: unknown,
  knownIds: ReadonlySet<string>,
): VisibilityState {
  if (typeof raw !== "object" || raw === null || Array.isArray(raw)) return {};
  const clean: VisibilityState = {};
  for (const [id, value] of Object.entries(raw as Record<string, unknown>)) {
    if (knownIds.has(id) && typeof value === "boolean") {
      clean[id] = value;
    }
  }
  return clean;
}

// The localStorage getter itself can throw (blocked storage, private
// contexts): every access happens inside the try, never in a guard.
export function readStoredVisibility(
  storageKey: string,
  knownIds: ReadonlySet<string>,
  fallback: VisibilityState,
): VisibilityState {
  try {
    if (typeof window === "undefined") return fallback;
    const raw = window.localStorage.getItem(storageKey);
    if (!raw) return fallback;
    return {
      ...fallback,
      ...sanitizeVisibility(JSON.parse(raw), knownIds),
    };
  } catch {
    // blocked storage, invalid JSON or unavailable storage: fall back to the
    // defaults
    return fallback;
  }
}

export function writeStoredVisibility(
  storageKey: string,
  value: VisibilityState,
): void {
  try {
    if (typeof window === "undefined") return;
    window.localStorage.setItem(storageKey, JSON.stringify(value));
  } catch {
    // blocked, full or unavailable storage: the preference simply does not
    // persist
  }
}

export function useColumnVisibility(options: {
  columnIds: readonly string[];
  defaultVisibility?: VisibilityState;
  storageKey: string;
}) {
  const { columnIds, defaultVisibility = {}, storageKey } = options;
  const knownIds = React.useMemo(() => new Set(columnIds), [columnIds]);
  const [columnVisibility, setColumnVisibility] = React.useState<
    VisibilityState
  >(() => readStoredVisibility(storageKey, knownIds, defaultVisibility));
  const onColumnVisibilityChange = React.useCallback(
    (updater: Updater<VisibilityState>) => {
      setColumnVisibility((previous) =>
        typeof updater === "function" ? updater(previous) : updater,
      );
    },
    [],
  );
  React.useEffect(() => {
    writeStoredVisibility(
      storageKey,
      sanitizeVisibility(columnVisibility, knownIds),
    );
  }, [columnVisibility, knownIds, storageKey]);
  return { columnVisibility, onColumnVisibilityChange };
}

export function ColumnVisibilityMenu<TData>({
  columns,
  table,
}: {
  columns: ReadonlyArray<{ id: string; label: string }>;
  table: TanstackTable<TData>;
}) {
  const { t } = useI18n();
  return (
    <DropdownMenu>
      <DropdownMenuTrigger
        render={
          <Button
            aria-label={t("grid.columns.aria")}
            className="h-8 font-normal"
            variant="outline"
          />
        }
      >
        <SettingsIcon />
        {t("grid.columns")}
      </DropdownMenuTrigger>
      <DropdownMenuContent aria-label={t("grid.columns")} className="min-w-44">
        {/* base-ui: the group label must live inside a Menu.Group */}
        <DropdownMenuGroup>
          <DropdownMenuLabel>{t("grid.columns")}</DropdownMenuLabel>
          {columns.map(({ id, label }) => {
            const column = table.getColumn(id);
            if (!column) return null;
            return (
              <DropdownMenuCheckboxItem
                checked={column.getIsVisible()}
                key={id}
                onCheckedChange={(next) => column.toggleVisibility(next)}
              >
                {label}
              </DropdownMenuCheckboxItem>
            );
          })}
        </DropdownMenuGroup>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
