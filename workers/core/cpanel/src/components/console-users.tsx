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
import { PlusIcon } from "@edger/ui/icons/lucide";
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@edger/ui/components/ui/tooltip";
import * as React from "react";

// Row actions are compact icon buttons (the global TooltipProvider is
// mounted by main.tsx). Icons come from the virtual unplugin-icons modules
// — not the @edger/ui barrel — because the barrel's frozen export surface
// would not carry the new icon names under the bun test runner.
import KeyRound from "~icons/lucide/key-round";
import Pencil from "~icons/lucide/pencil";
import Trash2 from "~icons/lucide/trash-2";
import UserCheck from "~icons/lucide/user-check";
import UserX from "~icons/lucide/user-x";

import {
  ApiError,
  changeMyPassword,
  createUser,
  deleteUser,
  isValidUsername,
  listUsers,
  passwordPolicyIssues,
  PERMISSION_CATALOG,
  resetUserPassword,
  updateUser,
  type AdminUser,
  type CreateUserRequest,
  type Principal,
} from "../lib/api";
import { useI18n } from "../lib/i18n";
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
import { PermissionBadges } from "./permission-badges";

// Story 26.04: additional password users over the `/api/admin/users`
// backend. Root-only on the server; the UI hides what the principal cannot
// do and never substitutes the server-side validation. Passwords ride the
// request body only — never URLs, errors, or logs.

function formatEpoch(seconds?: number | null, locale = "en-US") {
  if (!seconds) return "—";
  return new Date(seconds * 1000).toLocaleString(locale);
}

function describeError(reason: unknown): string {
  return reason instanceof Error ? reason.message : String(reason);
}

// Columns the user can hide from the view menu; the actions column is always
// present. The ids mirror the column definitions below.
const HIDEABLE_USER_COLUMNS = [
  "username",
  "status",
  "permissions",
  "namespaces",
  "workers",
  "createdAt",
] as const;
const DEFAULT_USER_COLUMN_VISIBILITY: VisibilityState = { permissions: false };

const csv = (value: string) =>
  value
    .split(",")
    .map((entry) => entry.trim())
    .filter(Boolean);

function PermissionsField({
  name,
  value,
  onChange,
}: {
  name: string;
  onChange(value: string[]): void;
  value: string[];
}) {
  return (
    <fieldset className="space-y-1.5">
      <legend className="text-sm font-medium">{name}</legend>
      <div className="grid grid-cols-2 gap-1.5">
        {PERMISSION_CATALOG.map((permission) => {
          const checked = value.includes(permission);
          return (
            <label
              className="flex items-center gap-2 text-sm"
              key={permission}
            >
              <input
                checked={checked}
                onChange={(event) =>
                  onChange(
                    event.target.checked
                      ? [...value, permission]
                      : value.filter((entry) => entry !== permission),
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
  );
}

export function ConsoleUsers({
  apiKey,
  principal,
  renderPageAction,
}: {
  apiKey: string;
  principal: Principal;
  renderPageAction?: (action: React.ReactNode) => React.ReactNode;
}) {
  const { t } = useI18n();
  // The server is the real barrier: a non-root principal sees only the
  // notice, and the routes answer 401/403 for anything but root. The page
  // action slot is only passed down once that root gate has passed.
  if (!principal.isRoot) {
    return (
      <p className="text-sm text-muted-foreground">{t("users.noManagement")}</p>
    );
  }
  return <UsersPanel apiKey={apiKey} renderPageAction={renderPageAction} />;
}

function UsersPanel({
  apiKey,
  renderPageAction,
}: {
  apiKey: string;
  renderPageAction?: (action: React.ReactNode) => React.ReactNode;
}) {
  const { locale, t } = useI18n();
  const queryClient = useQueryClient();
  const usersQuery = useQuery({
    queryFn: () => listUsers(apiKey),
    queryKey: ["cpanel", "users"],
  });
  const [createOpen, setCreateOpen] = React.useState(false);
  const [editTarget, setEditTarget] = React.useState<AdminUser | null>(null);
  const [resetTarget, setResetTarget] = React.useState<AdminUser | null>(null);
  const [deleteTarget, setDeleteTarget] = React.useState<AdminUser | null>(null);
  const [disableTarget, setDisableTarget] = React.useState<AdminUser | null>(
    null,
  );
  const [sorting, setSorting] = React.useState<SortingState>([]);
  const [search, setSearch] = React.useState("");
  const invalidate = () =>
    queryClient.invalidateQueries({ queryKey: ["cpanel", "users"] });
  const disableMutation = useMutation({
    mutationFn: (user: AdminUser) => updateUser(apiKey, user.id, { disabled: true }),
    onSuccess: () => {
      setDisableTarget(null);
      void invalidate();
    },
  });
  const enableMutation = useMutation({
    mutationFn: (user: AdminUser) => updateUser(apiKey, user.id, { disabled: false }),
    onSuccess: () => {
      setDisableTarget(null);
      void invalidate();
    },
  });
  const deleteMutation = useMutation({
    mutationFn: (user: AdminUser) => deleteUser(apiKey, user.id),
    onSuccess: () => {
      setDeleteTarget(null);
      void invalidate();
    },
  });

  // Shared DataGrid, the same shape as the API keys screen: 15 rows per page
  // (15/30/60 options), bounded fixed-layout columns, long values truncated
  // with the full value in the title, and compact permission badges.
  const columns = React.useMemo<ColumnDef<AdminUser>[]>(
    () => [
      {
        accessorKey: "username",
        header: ({ column }) => (
          <DataGridColumnHeader column={column} label={t("users.username")} />
        ),
        cell: ({ row }) => {
          const user = row.original;
          return (
            <span className="flex min-w-0 items-center gap-2">
              <span className="truncate text-sm" title={user.username}>
                {user.username}
              </span>
              {user.isRoot && <Badge variant="secondary">root</Badge>}
            </span>
          );
        },
        size: 180,
      },
      {
        accessorFn: (user) => (user.disabled ? "disabled" : "active"),
        id: "status",
        header: ({ column }) => (
          <DataGridColumnHeader column={column} label={t("users.status")} />
        ),
        cell: ({ row }) => (
          <Badge variant={row.original.disabled ? "outline" : "default"}>
            {row.original.disabled ? t("users.disabled") : t("users.active")}
          </Badge>
        ),
        size: 100,
      },
      {
        accessorFn: (user) => user.permissions.join(", "),
        id: "permissions",
        header: ({ column }) => (
          <DataGridColumnHeader column={column} label={t("users.permissions")} />
        ),
        cell: ({ row }) => (
          <PermissionBadges permissions={row.original.permissions} />
        ),
        size: 240,
      },
      {
        accessorFn: (user) => user.namespaces.join(", "),
        id: "namespaces",
        header: ({ column }) => (
          <DataGridColumnHeader column={column} label={t("account.namespaces")} />
        ),
        cell: ({ row }) => {
          const namespaces = row.original.namespaces.join(", ");
          return (
            <code className="block truncate text-xs" title={namespaces}>
              {namespaces}
            </code>
          );
        },
        size: 120,
      },
      {
        accessorFn: (user) => user.workers.join(", "),
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
        size: 120,
      },
      {
        accessorKey: "createdAt",
        header: ({ column }) => (
          <DataGridColumnHeader column={column} label={t("users.created")} />
        ),
        cell: ({ row }) => {
          const value = formatEpoch(row.original.createdAt, locale);
          return (
            <span
              className="block truncate text-sm text-muted-foreground"
              title={value}
            >
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
          const user = row.original;
          // The root row is immutable: no mutable buttons, as before the
          // DataGrid migration.
          if (user.isRoot) {
            return <span className="sr-only">{t("users.noManagement")}</span>;
          }
          // Compact icon actions on one row: the accessible name stays the
          // existing translated label, the tooltip explains each action, and
          // the icons are decorative (aria-hidden).
          return (
            <div className="flex justify-end gap-1">
              <Tooltip>
                <TooltipTrigger
                  render={
                    <Button
                      aria-label={t("users.edit")}
                      onClick={() => setEditTarget(user)}
                      size="icon-sm"
                      variant="ghost"
                    />
                  }
                >
                  <Pencil aria-hidden />
                </TooltipTrigger>
                <TooltipContent>{t("users.edit.title")}</TooltipContent>
              </Tooltip>
              {user.disabled ? (
                <Tooltip>
                  <TooltipTrigger
                    render={
                      <Button
                        aria-label={t("users.enable")}
                        disabled={enableMutation.isPending}
                        onClick={() => enableMutation.mutate(user)}
                        size="icon-sm"
                        variant="ghost"
                      />
                    }
                  >
                    <UserCheck aria-hidden />
                  </TooltipTrigger>
                  <TooltipContent>{t("users.enable")}</TooltipContent>
                </Tooltip>
              ) : (
                <Tooltip>
                  <TooltipTrigger
                    render={
                      <Button
                        aria-label={t("users.disable")}
                        onClick={() => setDisableTarget(user)}
                        size="icon-sm"
                        variant="ghost"
                      />
                    }
                  >
                    <UserX aria-hidden />
                  </TooltipTrigger>
                  <TooltipContent>{t("users.disable.title")}</TooltipContent>
                </Tooltip>
              )}
              <Tooltip>
                <TooltipTrigger
                  render={
                    <Button
                      aria-label={t("users.resetPassword")}
                      onClick={() => setResetTarget(user)}
                      size="icon-sm"
                      variant="ghost"
                    />
                  }
                >
                  <KeyRound aria-hidden />
                </TooltipTrigger>
                <TooltipContent>{t("users.reset.title")}</TooltipContent>
              </Tooltip>
              <Tooltip>
                <TooltipTrigger
                  render={
                    <Button
                      aria-label={t("users.delete")}
                      onClick={() => setDeleteTarget(user)}
                      size="icon-sm"
                      variant="ghost"
                    />
                  }
                >
                  <Trash2 aria-hidden />
                </TooltipTrigger>
                <TooltipContent>{t("users.delete.title")}</TooltipContent>
              </Tooltip>
            </div>
          );
        },
        size: 160,
      },
    ],
    [enableMutation, locale, t],
  );

  const { columnVisibility, onColumnVisibilityChange } = useColumnVisibility({
    columnIds: HIDEABLE_USER_COLUMNS,
    defaultVisibility: DEFAULT_USER_COLUMN_VISIBILITY,
    storageKey: columnVisibilityStorageKey("users"),
  });
  const visibilityColumns = React.useMemo(
    () => [
      { id: "username", label: t("users.username") },
      { id: "status", label: t("users.status") },
      { id: "permissions", label: t("users.permissions") },
      { id: "namespaces", label: t("account.namespaces") },
      { id: "workers", label: t("keys.workers") },
      { id: "createdAt", label: t("users.created") },
    ],
    [t],
  );

  // Local, case-insensitive filter over username, namespaces and workers.
  // An empty term (or only whitespace) shows every user.
  const filteredUsers = React.useMemo(() => {
    const term = search.trim().toLowerCase();
    const users = usersQuery.data ?? [];
    if (!term) return users;
    return users.filter((user) =>
      [
        user.username,
        user.namespaces.join(", "),
        user.workers.join(", "),
      ].some((field) => field.toLowerCase().includes(term)),
    );
  }, [usersQuery.data, search]);

  const table = useReactTable({
    columns,
    data: filteredUsers,
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
        <PlusIcon aria-hidden /> {t("users.new")}
      </Button>
    </>
  );

  return (
    <div className="space-y-4">
      {renderPageAction?.(pageActions)}
      {!renderPageAction && (
        <div className="flex flex-wrap items-center justify-between gap-2">
          {pageActions}
        </div>
      )}
      <Input
        aria-label={t("users.search")}
        className="max-w-xs"
        onChange={(event) => {
          setSearch(event.target.value);
          table.setPageIndex(0);
        }}
        placeholder={t("users.search")}
        value={search}
      />
      {usersQuery.isLoading && (
        <p className="text-sm text-foreground" role="status">
          {t("users.loading")}
        </p>
      )}
      {usersQuery.error && (
        <div className="flex items-center justify-between gap-3">
          <p className="text-sm text-destructive" role="alert">
            {describeError(usersQuery.error)}
          </p>
          <Button onClick={() => void usersQuery.refetch()} size="sm" variant="outline">
            {t("users.retry")}
          </Button>
        </div>
      )}
      {enableMutation.error && (
        <p className="text-sm text-destructive" role="alert">
          {describeError(enableMutation.error)}
        </p>
      )}
      {!usersQuery.isLoading && !usersQuery.error && (
        <DataGrid
          emptyText={search.trim() ? t("users.noResults") : t("users.empty")}
          fixedLayout
          table={table}
        />
      )}
      <CreateUserDialog
        apiKey={apiKey}
        onClose={() => setCreateOpen(false)}
        onCreated={invalidate}
        open={createOpen}
      />
      {editTarget && (
        <EditUserDialog
          apiKey={apiKey}
          onSaved={invalidate}
          user={editTarget}
          onClose={() => setEditTarget(null)}
        />
      )}
      {resetTarget && (
        <ResetPasswordDialog
          apiKey={apiKey}
          onClose={() => setResetTarget(null)}
          onReset={invalidate}
          user={resetTarget}
        />
      )}
      {deleteTarget && (
        <Dialog
          onOpenChange={(open) => !open && setDeleteTarget(null)}
          open
        >
          <DialogContent>
            <DialogHeader>
              <DialogTitle>{t("users.delete.title")}</DialogTitle>
              <DialogDescription>{t("users.delete.description")}</DialogDescription>
            </DialogHeader>
            {deleteMutation.error && (
              <p className="text-sm text-destructive" role="alert">
                {describeError(deleteMutation.error)}
              </p>
            )}
            <DialogFooter>
              <Button onClick={() => setDeleteTarget(null)} variant="outline">
                {t("users.cancel")}
              </Button>
              <Button
                disabled={deleteMutation.isPending}
                onClick={() => deleteMutation.mutate(deleteTarget)}
                variant="destructive"
              >
                {deleteMutation.isPending
                  ? t("users.deleting")
                  : t("users.confirm")}
              </Button>
            </DialogFooter>
          </DialogContent>
        </Dialog>
      )}
      {disableTarget && (
        <Dialog
          onOpenChange={(open) => !open && setDisableTarget(null)}
          open
        >
          <DialogContent>
            <DialogHeader>
              <DialogTitle>{t("users.disable.title")}</DialogTitle>
              <DialogDescription>{t("users.disable.description")}</DialogDescription>
            </DialogHeader>
            {disableMutation.error && (
              <p className="text-sm text-destructive" role="alert">
                {describeError(disableMutation.error)}
              </p>
            )}
            <DialogFooter>
              <Button onClick={() => setDisableTarget(null)} variant="outline">
                {t("users.cancel")}
              </Button>
              <Button
                disabled={disableMutation.isPending}
                onClick={() => disableMutation.mutate(disableTarget)}
                variant="destructive"
              >
                {t("users.confirm")}
              </Button>
            </DialogFooter>
          </DialogContent>
        </Dialog>
      )}
    </div>
  );
}

function CreateUserDialog({
  apiKey,
  open,
  onClose,
  onCreated,
}: {
  apiKey: string;
  onClose(): void;
  onCreated(): void;
  open: boolean;
}) {
  const { t } = useI18n();
  const [username, setUsername] = React.useState("");
  const [password, setPassword] = React.useState("");
  const [permissions, setPermissions] = React.useState<string[]>([
    "workers:read",
  ]);
  const [namespaces, setNamespaces] = React.useState("*");
  const [workers, setWorkers] = React.useState("*");
  const create = useMutation({
    mutationFn: (request: CreateUserRequest) =>
      createUser(apiKey, request),
    onSuccess: () => {
      setUsername("");
      setPassword("");
      setPermissions(["workers:read"]);
      setNamespaces("*");
      setWorkers("*");
      onClose();
      onCreated();
    },
  });
  React.useEffect(() => {
    if (!open) {
      // Close always clears the form: no password-shaped value outlives the
      // dialog, whether it succeeded or was cancelled.
      setUsername("");
      setPassword("");
    }
  }, [open]);
  const usernameOk = username === "" || isValidUsername(username);
  const passwordOk = password === "" || passwordPolicyIssues(password).length === 0;
  const submit = () => {
    if (!isValidUsername(username)) return;
    if (passwordPolicyIssues(password).length > 0) return;
    if (!permissions.length) return;
    create.mutate({
      namespaces: csv(namespaces).length ? csv(namespaces) : ["*"],
      password,
      permissions,
      username,
      workers: csv(workers).length ? csv(workers) : ["*"],
    });
  };
  return (
    <Dialog onOpenChange={(next) => !next && onClose()} open={open}>
      <DialogContent className="sm:max-w-xl">
        <DialogHeader>
          <DialogTitle>{t("users.new")}</DialogTitle>
          <DialogDescription>{t("users.lead")}</DialogDescription>
        </DialogHeader>
        <div className="space-y-4">
          <div className="space-y-1.5">
            <Label htmlFor="user-username">{t("users.username")}</Label>
            <Input
              autoComplete="off"
              id="user-username"
              onChange={(event) => setUsername(event.target.value)}
              placeholder="analyst-01"
              spellCheck={false}
              value={username}
            />
            <p className="text-xs text-muted-foreground">{t("users.usernameHint")}</p>
            {!usernameOk && (
              <p className="text-sm text-destructive" role="alert">
                {t("users.invalidUsername")}
              </p>
            )}
          </div>
          <div className="space-y-1.5">
            <Label htmlFor="user-password">{t("users.password")}</Label>
            <Input
              autoComplete="new-password"
              id="user-password"
              onChange={(event) => setPassword(event.target.value)}
              type="password"
              value={password}
            />
            <p className="text-xs text-muted-foreground">{t("users.passwordHint")}</p>
            {!passwordOk && (
              <p className="text-sm text-destructive" role="alert">
                {t("users.invalidPassword")}
              </p>
            )}
          </div>
          <PermissionsField name={t("users.permissions")} value={permissions} onChange={setPermissions} />
          <div className="grid grid-cols-2 items-end gap-3">
            <div className="space-y-1.5">
              <Label htmlFor="user-namespaces">{t("users.namespaces")}</Label>
              <Input
                id="user-namespaces"
                onChange={(event) => setNamespaces(event.target.value)}
                placeholder="*"
                value={namespaces}
              />
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="user-workers">{t("users.workers")}</Label>
              <Input
                id="user-workers"
                onChange={(event) => setWorkers(event.target.value)}
                placeholder="* or p-abc*"
                value={workers}
              />
            </div>
          </div>
          {create.error && (
            <p className="text-sm text-destructive" role="alert">
              {describeError(create.error)}
            </p>
          )}
        </div>
        <DialogFooter>
          <Button onClick={onClose} variant="outline">
            {t("users.cancel")}
          </Button>
          <Button
            disabled={
              create.isPending ||
              !usernameOk ||
              !passwordOk ||
              permissions.length === 0
            }
            onClick={submit}
          >
            {create.isPending ? t("users.saving") : t("users.new")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

function EditUserDialog({
  apiKey,
  user,
  onClose,
  onSaved,
}: {
  apiKey: string;
  onClose(): void;
  onSaved(): void;
  user: AdminUser;
}) {
  const { t } = useI18n();
  const [permissions, setPermissions] = React.useState<string[]>(user.permissions);
  const [namespaces, setNamespaces] = React.useState(user.namespaces.join(", "));
  const [workers, setWorkers] = React.useState(user.workers.join(", "));
  const save = useMutation({
    mutationFn: () =>
      updateUser(apiKey, user.id, {
        namespaces: csv(namespaces).length ? csv(namespaces) : ["*"],
        permissions,
        workers: csv(workers).length ? csv(workers) : ["*"],
      }),
    onSuccess: () => {
      onClose();
      onSaved();
    },
  });
  return (
    <Dialog onOpenChange={(open) => !open && onClose()} open>
      <DialogContent className="sm:max-w-xl">
        <DialogHeader>
          <DialogTitle>{t("users.edit.title")}</DialogTitle>
          <DialogDescription>{t("users.edit.description")}</DialogDescription>
        </DialogHeader>
        <div className="space-y-4">
          <div className="space-y-1.5">
            <Label>{t("users.username")}</Label>
            <span className="text-sm">{user.username}</span>
          </div>
          <PermissionsField name={t("users.permissions")} value={permissions} onChange={setPermissions} />
          <div className="grid grid-cols-2 items-end gap-3">
            <div className="space-y-1.5">
              <Label htmlFor="edit-namespaces">{t("users.namespaces")}</Label>
              <Input
                id="edit-namespaces"
                onChange={(event) => setNamespaces(event.target.value)}
                value={namespaces}
              />
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="edit-workers">{t("users.workers")}</Label>
              <Input
                id="edit-workers"
                onChange={(event) => setWorkers(event.target.value)}
                value={workers}
              />
            </div>
          </div>
          {save.error && (
            <p className="text-sm text-destructive" role="alert">
              {describeError(save.error)}
            </p>
          )}
        </div>
        <DialogFooter>
          <Button onClick={onClose} variant="outline">
            {t("users.cancel")}
          </Button>
          <Button
            disabled={save.isPending || permissions.length === 0}
            onClick={() => save.mutate()}
          >
            {save.isPending ? t("users.saving") : t("users.save")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

function ResetPasswordDialog({
  apiKey,
  user,
  onClose,
  onReset,
}: {
  apiKey: string;
  onClose(): void;
  onReset(): void;
  user: AdminUser;
}) {
  const { t } = useI18n();
  const [password, setPassword] = React.useState("");
  const reset = useMutation({
    mutationFn: () => resetUserPassword(apiKey, user.id, password),
    onSuccess: () => {
      setPassword("");
      onClose();
      onReset();
    },
  });
  const passwordOk = password === "" || passwordPolicyIssues(password).length === 0;
  return (
    <Dialog onOpenChange={(open) => !open && onClose()} open>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{t("users.reset.title")}</DialogTitle>
          <DialogDescription>{t("users.reset.description")}</DialogDescription>
        </DialogHeader>
        <div className="space-y-4">
          <div className="space-y-1.5">
            <Label htmlFor="reset-password">{t("users.password")}</Label>
            <Input
              autoComplete="new-password"
              id="reset-password"
              onChange={(event) => setPassword(event.target.value)}
              type="password"
              value={password}
            />
            <p className="text-xs text-muted-foreground">{t("users.passwordHint")}</p>
            {!passwordOk && (
              <p className="text-sm text-destructive" role="alert">
                {t("users.invalidPassword")}
              </p>
            )}
          </div>
          {reset.error && (
            <p className="text-sm text-destructive" role="alert">
              {describeError(reset.error)}
            </p>
          )}
        </div>
        <DialogFooter>
          <Button onClick={onClose} variant="outline">
            {t("users.cancel")}
          </Button>
          <Button
            disabled={reset.isPending || !passwordOk}
            onClick={() => {
              if (passwordPolicyIssues(password).length === 0) reset.mutate();
            }}
          >
            {reset.isPending ? t("users.resetting") : t("users.reset")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

// Own-password change for the account menu: only `ses-` session credentials
// can reach it (the server refuses anything else). The request never keeps a
// password-shaped value in the error surface, and the form clears both on
// success and on close.
export function ChangePasswordDialog({
  apiKey,
  open,
  onNewSession,
  onOpenChange,
  onRequireLogin,
}: {
  apiKey: string;
  onNewSession(token: string): void;
  onOpenChange(open: boolean): void;
  onRequireLogin(): void;
  open: boolean;
}) {
  const { t } = useI18n();
  const [current, setCurrent] = React.useState("");
  const [next, setNext] = React.useState("");
  const change = useMutation({
    mutationFn: () => changeMyPassword(apiKey, current, next),
    onSuccess: (token) => {
      setCurrent("");
      setNext("");
      onOpenChange(false);
      if (token) onNewSession(token);
      else onRequireLogin();
    },
  });
  React.useEffect(() => {
    if (!open) {
      setCurrent("");
      setNext("");
    }
  }, [open]);
  const nextOk = next === "" || passwordPolicyIssues(next).length === 0;
  function describeChangeError(reason: unknown): string {
    if (reason instanceof ApiError) {
      if (reason.status === 401) return t("account.changePassword.invalidCurrent");
      if (reason.status === 429) return t("auth.rateLimited");
    }
    return t("users.networkError");
  }
  return (
    <Dialog onOpenChange={(next) => !next && onOpenChange(false)} open={open}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{t("account.changePassword.title")}</DialogTitle>
          <DialogDescription>
            {t("account.changePassword.description")}
          </DialogDescription>
        </DialogHeader>
        <div className="space-y-4">
          <div className="space-y-1.5">
            <Label htmlFor="current-password">
              {t("account.changePassword.current")}
            </Label>
            <Input
              autoComplete="current-password"
              id="current-password"
              onChange={(event) => setCurrent(event.target.value)}
              type="password"
              value={current}
            />
          </div>
          <div className="space-y-1.5">
            <Label htmlFor="new-password">
              {t("account.changePassword.new")}
            </Label>
            <Input
              autoComplete="new-password"
              id="new-password"
              onChange={(event) => setNext(event.target.value)}
              type="password"
              value={next}
            />
            <p className="text-xs text-muted-foreground">{t("users.passwordHint")}</p>
            {!nextOk && (
              <p className="text-sm text-destructive" role="alert">
                {t("users.invalidPassword")}
              </p>
            )}
          </div>
          {change.error && (
            <p className="text-sm text-destructive" role="alert">
              {describeChangeError(change.error)}
            </p>
          )}
        </div>
        <DialogFooter>
          <Button onClick={() => onOpenChange(false)} variant="outline">
            {t("users.cancel")}
          </Button>
          <Button
            disabled={change.isPending || !current || !nextOk}
            onClick={() => {
              if (nextOk) change.mutate();
            }}
          >
            {change.isPending
              ? t("account.changePassword.changing")
              : t("account.changePassword.submit")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
