import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  getCoreRowModel,
  getPaginationRowModel,
  getSortedRowModel,
  type ColumnDef,
  type SortingState,
  type VisibilityState,
  useReactTable,
} from "@tanstack/react-table";
import { Badge } from "@edger/ui/components/ui/badge";
import { Button } from "@edger/ui/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@edger/ui/components/ui/dialog";
import { Input } from "@edger/ui/components/ui/input";
import { Label } from "@edger/ui/components/ui/label";
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@edger/ui/components/ui/tooltip";
import {
  CheckIcon,
  CopyIcon,
  PencilIcon,
  PlusIcon,
  Trash2Icon,
} from "@edger/ui/icons/lucide";
import * as React from "react";
import {
  ColumnVisibilityMenu,
  columnVisibilityStorageKey,
  useColumnVisibility,
} from "./column-visibility";
import {
  DataGrid,
  DataGridColumnHeader,
  DEFAULT_PAGE_SIZE,
} from "./data-grid";
import { EditKeyDialog } from "./edit-key-dialog";
import { PermissionBadges } from "./permission-badges";
import {
  apiJson,
  canManageKeys,
  PERMISSION_CATALOG,
  type ApiKey,
  type CreatedKey,
  type CreateKeyRequest,
  type Principal,
} from "../lib/api";
import { useI18n } from "../lib/i18n";

// A gestão segue o padrão tenancit/Studio: escopos por checkbox, o segredo
// aparece UMA vez na criação, revogação é terminal e o delete só existe para
// key já revogada. O servidor aplica a anti-escalada (subconjunto do criador)
// — a UI só desabilita o que o principal visivelmente não pode conceder.

function formatEpoch(seconds: number | null | undefined, locale: string) {
  if (!seconds) return "—";
  return new Date(seconds * 1000).toLocaleString(locale);
}

type KeyStatus = "revoked" | "expired" | "active";

function keyStatus(key: ApiKey): KeyStatus {
  if (key.revokedAt) return "revoked";
  if (key.expiresAt && key.expiresAt * 1000 < Date.now()) return "expired";
  return "active";
}

const EXPIRY_CHOICES = [
  { days: 0, key: "keys.create.never" },
  { days: 30, key: "keys.create.days30" },
  { days: 90, key: "keys.create.days90" },
  { days: 365, key: "keys.create.days365" },
] as const;

// Columns the user can hide from the view menu; the actions column is always
// present. The ids mirror the column definitions below.
const HIDEABLE_KEY_COLUMNS = [
  "name",
  "keyPrefix",
  "permissions",
  "workers",
  "status",
  "lastUsedAt",
  "expiresAt",
] as const;
const DEFAULT_KEY_COLUMN_VISIBILITY: VisibilityState = { permissions: false };

export function ApiKeys({
  apiKey,
  principal,
  renderPageAction,
}: {
  apiKey: string;
  principal: Principal;
  renderPageAction?: (action: React.ReactNode) => React.ReactNode;
}) {
  const { locale, t } = useI18n();
  const queryClient = useQueryClient();
  const manageable = canManageKeys(principal);
  const keysQuery = useQuery({
    queryKey: ["cpanel", "keys", apiKey],
    queryFn: () =>
      apiJson<{ keys: ApiKey[] }>(apiKey, "/api/admin/keys").then(
        (data) => data.keys ?? [],
      ),
    enabled: manageable,
  });

  const [createOpen, setCreateOpen] = React.useState(false);
  const [revealed, setRevealed] = React.useState<CreatedKey | null>(null);
  const [confirmDelete, setConfirmDelete] = React.useState<ApiKey | null>(null);
  const [editingKey, setEditingKey] = React.useState<ApiKey | null>(null);
  const [sorting, setSorting] = React.useState<SortingState>([]);

  const invalidate = () =>
    queryClient.invalidateQueries({ queryKey: ["cpanel", "keys"] });

  // Uma ação só na UI: deletar revoga primeiro (o servidor exige — revoke é
  // terminal e delete só remove key revogada) e então remove o registro. O
  // revoke isolado continua existindo na API/MCP para quem quer matar a
  // credencial mantendo a linha de auditoria.
  const remove = useMutation({
    mutationFn: async (key: ApiKey) => {
      if (!key.revokedAt) {
        await apiJson(apiKey, `/api/admin/keys/${key.id}/revoke`, {
          method: "POST",
        });
      }
      await apiJson(apiKey, `/api/admin/keys/${key.id}`, { method: "DELETE" });
    },
    onSettled: invalidate,
  });

  const columns = React.useMemo<ColumnDef<ApiKey>[]>(
    () => [
      {
        accessorKey: "name",
        header: ({ column }) => (
          <DataGridColumnHeader column={column} label={t("keys.name")} />
        ),
        cell: ({ row }) => (
          <span className="block truncate" title={row.original.name}>
            {row.original.name}
          </span>
        ),
        size: 140,
      },
      {
        accessorKey: "keyPrefix",
        header: ({ column }) => (
          <DataGridColumnHeader column={column} label={t("keys.key")} />
        ),
        cell: ({ row }) => (
          <code className="block truncate text-xs" title={row.original.keyPrefix}>
            {row.original.keyPrefix}…
          </code>
        ),
        size: 140,
      },
      {
        accessorFn: (key) => key.permissions.join(", "),
        id: "permissions",
        header: ({ column }) => (
          <DataGridColumnHeader column={column} label={t("keys.permissions")} />
        ),
        cell: ({ row }) => (
          <PermissionBadges permissions={row.original.permissions} />
        ),
        size: 256,
      },
      {
        accessorFn: (key) => key.workers.join(", "),
        id: "workers",
        header: ({ column }) => (
          <DataGridColumnHeader column={column} label={t("keys.workers")} />
        ),
        cell: ({ row }) => {
          const workers = row.original.workers.join(", ");
          return (
            <code className="block truncate text-xs" title={workers}>
              {workers}
            </code>
          );
        },
        size: 140,
      },
      {
        accessorFn: keyStatus,
        id: "status",
        header: ({ column }) => (
          <DataGridColumnHeader column={column} label={t("keys.status")} />
        ),
        cell: ({ row }) => {
          const status = keyStatus(row.original);
          return (
            <Badge variant={status === "active" ? "default" : "outline"}>
              {status === "active"
                ? t("keys.active")
                : status === "revoked"
                  ? t("keys.revoked")
                  : t("keys.expired")}
            </Badge>
          );
        },
        size: 96,
      },
      {
        accessorKey: "lastUsedAt",
        header: ({ column }) => (
          <DataGridColumnHeader column={column} label={t("keys.lastUsed")} />
        ),
        cell: ({ row }) => {
          const value = formatEpoch(row.original.lastUsedAt, locale);
          return (
            <span className="block truncate text-xs text-muted-foreground" title={value}>
              {value}
            </span>
          );
        },
        size: 154,
      },
      {
        accessorKey: "expiresAt",
        header: ({ column }) => (
          <DataGridColumnHeader column={column} label={t("keys.expires")} />
        ),
        cell: ({ row }) => {
          const value = formatEpoch(row.original.expiresAt, locale);
          return (
            <span className="block truncate text-xs text-muted-foreground" title={value}>
              {value}
            </span>
          );
        },
        size: 154,
      },
      {
        id: "actions",
        enableSorting: false,
        header: () => <span className="sr-only">{t("keys.actions")}</span>,
        cell: ({ row }) => {
          const key = row.original;
          const status = keyStatus(key);
          return (
            <div className="flex min-w-20 justify-end gap-1">
              {status === "active" && (
                <Tooltip>
                  <TooltipTrigger
                    render={
                      <Button
                        aria-label={t("keys.editPermissions").replace(
                          "{name}",
                          key.name,
                        )}
                        onClick={() => setEditingKey(key)}
                        size="icon"
                        variant="ghost"
                      />
                    }
                  >
                    <PencilIcon />
                  </TooltipTrigger>
                  <TooltipContent>{t("keys.edit.title")}</TooltipContent>
                </Tooltip>
              )}
              <Tooltip>
                <TooltipTrigger
                  render={
                    <Button
                      aria-label={t("keys.deleteAccessible").replace(
                        "{name}",
                        key.name,
                      )}
                      onClick={() => setConfirmDelete(key)}
                      size="icon"
                      variant="ghost"
                    />
                  }
                >
                  <Trash2Icon />
                </TooltipTrigger>
                <TooltipContent>{t("keys.deleteTooltip")}</TooltipContent>
              </Tooltip>
            </div>
          );
        },
        size: 104,
      },
    ],
    [locale, t],
  );

  const { columnVisibility, onColumnVisibilityChange } = useColumnVisibility({
    columnIds: HIDEABLE_KEY_COLUMNS,
    defaultVisibility: DEFAULT_KEY_COLUMN_VISIBILITY,
    storageKey: columnVisibilityStorageKey("keys"),
  });
  const visibilityColumns = React.useMemo(
    () => [
      { id: "name", label: t("keys.name") },
      { id: "keyPrefix", label: t("keys.key") },
      { id: "permissions", label: t("keys.permissions") },
      { id: "workers", label: t("keys.workers") },
      { id: "status", label: t("keys.status") },
      { id: "lastUsedAt", label: t("keys.lastUsed") },
      { id: "expiresAt", label: t("keys.expires") },
    ],
    [t],
  );

  const table = useReactTable({
    columns,
    data: keysQuery.data ?? [],
    getCoreRowModel: getCoreRowModel(),
    getPaginationRowModel: getPaginationRowModel(),
    getSortedRowModel: getSortedRowModel(),
    initialState: { pagination: { pageIndex: 0, pageSize: DEFAULT_PAGE_SIZE } },
    onColumnVisibilityChange,
    onSortingChange: setSorting,
    state: { columnVisibility, sorting },
  });

  const pageActions = (
    <>
      <ColumnVisibilityMenu columns={visibilityColumns} table={table} />
      <Button onClick={() => setCreateOpen(true)}>
        <PlusIcon /> {t("keys.new")}
      </Button>
    </>
  );

  if (!manageable) {
    return (
      <p className="text-sm text-muted-foreground">
        {t("keys.noManagement")}
      </p>
    );
  }

  return (
    <div className="space-y-4">
      {renderPageAction?.(pageActions)}
      {!renderPageAction && (
        <div className="flex flex-wrap items-center justify-between gap-2">
          {pageActions}
        </div>
      )}

      {keysQuery.error ? (
        <p className="text-sm text-destructive">
          {t("keys.loadError")}
        </p>
      ) : (
        <DataGrid
          elasticColumnId="name"
          emptyText={keysQuery.isLoading ? t("keys.loading") : t("keys.empty")}
          fixedLayout
          table={table}
        />
      )}

      <CreateKeyDialog
        apiKey={apiKey}
        onClose={() => setCreateOpen(false)}
        onCreated={(created) => {
          setCreateOpen(false);
          setRevealed(created);
          void invalidate();
        }}
        open={createOpen}
        principal={principal}
      />

      <RevealDialog created={revealed} onClose={() => setRevealed(null)} />

      {editingKey && (
        <EditKeyDialog
          apiKey={apiKey}
          keyInfo={editingKey}
          onClose={() => setEditingKey(null)}
          onSaved={() => void invalidate()}
          principal={principal}
        />
      )}

      <Dialog
        onOpenChange={(open) => !open && setConfirmDelete(null)}
        open={Boolean(confirmDelete)}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{t("keys.delete.title")}</DialogTitle>
            <DialogDescription>
              {t("keys.delete.description").replace(
                "{name}",
                confirmDelete?.name ?? "",
              )}
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button onClick={() => setConfirmDelete(null)} variant="outline">
              {t("keys.cancel")}
            </Button>
            <Button
              onClick={() => {
                if (confirmDelete) remove.mutate(confirmDelete);
                setConfirmDelete(null);
              }}
              variant="destructive"
            >
              {t("keys.delete.confirm")}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}

function CreateKeyDialog({
  apiKey,
  open,
  onClose,
  onCreated,
  principal,
}: {
  apiKey: string;
  open: boolean;
  onClose: () => void;
  onCreated: (created: CreatedKey) => void;
  principal: Principal;
}) {
  const { t } = useI18n();
  const [name, setName] = React.useState("");
  const [permissions, setPermissions] = React.useState<string[]>([
    "workers:read",
  ]);
  const [namespaces, setNamespaces] = React.useState("*");
  const [workers, setWorkers] = React.useState("*");
  const [expiryDays, setExpiryDays] = React.useState(0);

  const create = useMutation({
    mutationFn: (request: CreateKeyRequest) =>
      apiJson<CreatedKey>(apiKey, "/api/admin/keys", {
        body: JSON.stringify(request),
        headers: { "content-type": "application/json" },
        method: "POST",
      }),
    onSuccess: (created) => {
      setName("");
      setPermissions(["workers:read"]);
      setNamespaces("*");
      setWorkers("*");
      setExpiryDays(0);
      onCreated(created);
    },
  });

  const creatorPermissions = principal.isRoot
    ? null
    : (principal.permissions ?? []);
  const csv = (value: string) =>
    value
      .split(",")
      .map((entry) => entry.trim())
      .filter(Boolean);

  return (
    <Dialog onOpenChange={(next) => !next && onClose()} open={open}>
      {/* O default do DialogContent é `sm:max-w-sm`, e nele o rótulo do campo
          de workers quebra em duas linhas: o input desce e desalinha do par ao
          lado. Este formulário tem duas colunas de verdade — onze permissions
          e os dois escopos —, então pede a largura maior. */}
      <DialogContent className="sm:max-w-xl">
        <DialogHeader>
          <DialogTitle>{t("keys.create.title")}</DialogTitle>
          <DialogDescription>{t("keys.create.description")}</DialogDescription>
        </DialogHeader>
        <div className="space-y-4">
          <div className="space-y-1.5">
            <Label htmlFor="key-name">{t("keys.create.name")}</Label>
            <Input
              id="key-name"
              onChange={(event) => setName(event.target.value)}
              placeholder="studio-labdev"
              value={name}
            />
          </div>
          <fieldset className="space-y-1.5">
            <legend className="text-sm font-medium">
              {t("keys.create.permissions")}
            </legend>
            <div className="grid grid-cols-2 gap-1.5">
              {PERMISSION_CATALOG.map((permission) => {
                const grantable =
                  !creatorPermissions ||
                  creatorPermissions.includes("*") ||
                  creatorPermissions.includes(permission);
                const checked = permissions.includes(permission);
                return (
                  <label
                    className={`flex items-center gap-2 text-sm ${grantable ? "" : "opacity-40"}`}
                    key={permission}
                  >
                    <input
                      checked={checked}
                      disabled={!grantable}
                      onChange={(event) =>
                        setPermissions((current) =>
                          event.target.checked
                            ? [...current, permission]
                            : current.filter((entry) => entry !== permission),
                        )
                      }
                      type="checkbox"
                    />
                    <span className="text-xs">{permission}</span>
                  </label>
                );
              })}
            </div>
          </fieldset>
          {/* `items-end` mantém os dois inputs na mesma linha de base mesmo se
              um rótulo quebrar — em tela estreita a largura acima não salva. */}
          <div className="grid grid-cols-2 items-end gap-3">
            <div className="space-y-1.5">
              <Label htmlFor="key-namespaces">
                {t("keys.create.namespaces")}
              </Label>
              <Input
                id="key-namespaces"
                onChange={(event) => setNamespaces(event.target.value)}
                placeholder="*"
                value={namespaces}
              />
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="key-workers">{t("keys.create.workers")}</Label>
              <Input
                id="key-workers"
                onChange={(event) => setWorkers(event.target.value)}
                placeholder={t("keys.create.workersPlaceholder")}
                value={workers}
              />
            </div>
          </div>
          <div className="space-y-1.5">
            <Label>{t("keys.create.expiry")}</Label>
            <div className="flex gap-1.5">
              {EXPIRY_CHOICES.map((choice) => (
                <Button
                  key={choice.days}
                  onClick={() => setExpiryDays(choice.days)}
                  size="sm"
                  type="button"
                  variant={expiryDays === choice.days ? "default" : "outline"}
                >
                  {t(choice.key)}
                </Button>
              ))}
            </div>
          </div>
          {create.error && (
            <p className="text-sm text-destructive">
              {t("keys.create.error")}
            </p>
          )}
        </div>
        <DialogFooter>
          <Button onClick={onClose} variant="outline">
            {t("keys.cancel")}
          </Button>
          <Button
            disabled={
              create.isPending || !name.trim() || permissions.length === 0
            }
            onClick={() =>
              create.mutate({
                name: name.trim(),
                permissions,
                namespaces: csv(namespaces).length ? csv(namespaces) : ["*"],
                workers: csv(workers).length ? csv(workers) : ["*"],
                ...(expiryDays > 0 && {
                  expiresAt:
                    Math.floor(Date.now() / 1000) + expiryDays * 86_400,
                }),
              })
            }
          >
            {t("keys.create.submit")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

function RevealDialog({
  created,
  onClose,
}: {
  created: CreatedKey | null;
  onClose: () => void;
}) {
  const { t } = useI18n();
  const [copied, setCopied] = React.useState(false);
  React.useEffect(() => {
    if (created) setCopied(false);
  }, [created]);
  return (
    <Dialog onOpenChange={(open) => !open && onClose()} open={Boolean(created)}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{t("keys.reveal.title")}</DialogTitle>
          <DialogDescription>{t("keys.reveal.description")}</DialogDescription>
        </DialogHeader>
        <div className="flex items-center gap-2">
          <code className="min-w-0 flex-1 break-all rounded bg-muted px-2 py-1.5 text-xs">
            {created?.rawKey}
          </code>
          <Button
            aria-label={t("keys.reveal.copy")}
            onClick={() => {
              if (created)
                void navigator.clipboard
                  .writeText(created.rawKey)
                  .then(() => setCopied(true));
            }}
            size="icon"
            variant="outline"
          >
            {copied ? <CheckIcon /> : <CopyIcon />}
          </Button>
        </div>
        <DialogFooter>
          <Button onClick={onClose}>{t("keys.reveal.done")}</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
