import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  getCoreRowModel,
  getPaginationRowModel,
  getSortedRowModel,
  type ColumnDef,
  type SortingState,
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
import { RefreshCwIcon } from "@edger/ui/icons/lucide";
import * as React from "react";
import {
  DataGrid,
  DataGridColumnHeader,
  DEFAULT_PAGE_SIZE,
} from "./data-grid";
import { apiJson, isUserManager, type Principal } from "../lib/api";
import { useI18n } from "../lib/i18n";

// The backend contract serves only the connected sessions, epochs in
// seconds, and never a token or hash. The server is the real barrier
// (user managers only, 403 on self, 404 on unknown); the UI hides the
// management surface for any other principal before the panel mounts its
// hooks.
export interface AdminSession {
  createdAt: number;
  current: boolean;
  expiresAt: number;
  id: string;
  isRoot: boolean;
  revocable: boolean;
  role: string;
  userId: number;
  username: string;
}

export function ConsoleSessions({
  apiKey,
  principal,
}: {
  apiKey: string;
  principal: Principal;
}) {
  const { t } = useI18n();
  // The gate runs before the child, so a principal without the management
  // role never mounts the query hooks and never issues a single request.
  if (!isUserManager(principal)) {
    return (
      <p className="text-sm text-muted-foreground">
        {t("sessions.noManagement")}
      </p>
    );
  }
  return <SessionsPanel apiKey={apiKey} />;
}

function formatEpoch(seconds: number, locale: string) {
  return new Date(seconds * 1000).toLocaleString(locale);
}

function SessionsPanel({ apiKey }: { apiKey: string }) {
  const { locale, t } = useI18n();
  const queryClient = useQueryClient();
  const sessionsQuery = useQuery({
    queryFn: () =>
      apiJson<{ sessions?: AdminSession[]; truncated?: boolean }>(
        apiKey,
        "/api/admin/sessions",
      ),
    queryKey: ["cpanel", "sessions"],
    refetchInterval: 30_000,
  });
  const [search, setSearch] = React.useState("");
  const [sorting, setSorting] = React.useState<SortingState>([]);
  const [revoking, setRevoking] = React.useState<AdminSession | null>(null);

  const sessions = sessionsQuery.data?.sessions ?? [];
  const truncated = sessionsQuery.data?.truncated === true;
  const invalidate = () =>
    queryClient.invalidateQueries({ queryKey: ["cpanel", "sessions"] });

  const revoke = useMutation({
    mutationFn: (session: AdminSession) =>
      apiJson(
        apiKey,
        `/api/admin/sessions/${encodeURIComponent(session.id)}`,
        { method: "DELETE" },
      ),
    onSuccess: () => {
      setRevoking(null);
      void invalidate();
    },
  });

  const columns = React.useMemo<ColumnDef<AdminSession>[]>(
    () => [
      {
        accessorKey: "username",
        header: ({ column }) => (
          <DataGridColumnHeader
            column={column}
            label={t("sessions.username")}
          />
        ),
        cell: ({ row }) => {
          const session = row.original;
          return (
            <span className="flex min-w-0 items-center gap-2">
              <span className="truncate text-sm" title={session.username}>
                {session.username}
              </span>
              {session.current && (
                <Badge variant="secondary">
                  {t("sessions.current")}
                </Badge>
              )}
            </span>
          );
        },
        size: 180,
      },
      {
        accessorKey: "role",
        header: ({ column }) => (
          <DataGridColumnHeader column={column} label={t("sessions.role")} />
        ),
        cell: ({ row }) => (
          <span className="block truncate text-sm" title={row.original.role}>
            {row.original.role}
          </span>
        ),
        size: 120,
      },
      {
        accessorKey: "createdAt",
        header: ({ column }) => (
          <DataGridColumnHeader
            column={column}
            label={t("sessions.createdAt")}
          />
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
        accessorKey: "expiresAt",
        header: ({ column }) => (
          <DataGridColumnHeader
            column={column}
            label={t("sessions.expiresAt")}
          />
        ),
        cell: ({ row }) => {
          const value = formatEpoch(row.original.expiresAt, locale);
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
        header: () => (
          <span className="block text-right">{t("sessions.actions")}</span>
        ),
        cell: ({ row }) => {
          const session = row.original;
          // The caller's current session cannot end from here: the server
          // would 403, and logging out is the only supported way to close
          // it — it keeps its disabled rendering with the logout hint.
          // A session the caller cannot revoke renders no action at all.
          if (!session.current && !session.revocable) return null;
          const disabled = session.current || revoke.isPending;
          return (
            <div className="flex justify-end">
              <Button
                disabled={disabled}
                size="sm"
                title={session.current ? t("sessions.currentHint") : undefined}
                variant="outline"
                onClick={() => setRevoking(session)}
              >
                {revoking?.id === session.id && revoke.isPending
                  ? t("sessions.revoking")
                  : t("sessions.revoke")}
              </Button>
            </div>
          );
        },
        size: 112,
      },
    ],
    [locale, revoke, revoking, t],
  );

  // Local, case-insensitive filter over username and role; an empty term
  // (or only whitespace) shows every session.
  const filteredSessions = React.useMemo(() => {
    const term = search.trim().toLowerCase();
    if (!term) return sessions;
    return sessions.filter((session) =>
      [session.username, session.role].some((field) =>
        field.toLowerCase().includes(term),
      ),
    );
  }, [search, sessions]);

  const table = useReactTable({
    columns,
    data: filteredSessions,
    getCoreRowModel: getCoreRowModel(),
    getPaginationRowModel: getPaginationRowModel(),
    getSortedRowModel: getSortedRowModel(),
    initialState: { pagination: { pageIndex: 0, pageSize: DEFAULT_PAGE_SIZE } },
    onSortingChange: setSorting,
    state: { sorting },
  });

  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-center gap-2">
        <Input
          aria-label={t("sessions.search")}
          className="max-w-xs"
          onChange={(event) => {
            setSearch(event.target.value);
            table.setPageIndex(0);
          }}
          placeholder={t("sessions.search")}
          value={search}
        />
        <div className="ml-auto">
          <Button
            onClick={() => void sessionsQuery.refetch()}
            variant="outline"
          >
            <RefreshCwIcon />
            {t("overview.refresh")}
          </Button>
        </div>
      </div>

      {sessionsQuery.isLoading && (
        <p className="text-sm text-foreground" role="status">
          {t("sessions.loading")}
        </p>
      )}
      {sessionsQuery.error && (
        <div className="flex items-center justify-between gap-3">
          <p className="text-sm text-destructive" role="alert">
            {t("sessions.loadError")}
          </p>
          <Button
            onClick={() => void sessionsQuery.refetch()}
            size="sm"
            variant="outline"
          >
            {t("sessions.retry")}
          </Button>
        </div>
      )}
      {!sessionsQuery.isLoading && !sessionsQuery.error && truncated && (
        <p className="text-sm text-muted-foreground">
          {t("sessions.truncated")}
        </p>
      )}
      {!sessionsQuery.isLoading && !sessionsQuery.error && (
        <DataGrid
          elasticColumnId="username"
          emptyText={
            search.trim() ? t("sessions.noResults") : t("sessions.empty")
          }
          fixedLayout
          table={table}
        />
      )}

      {revoking && (
        <Dialog
          onOpenChange={(open) => !open && setRevoking(null)}
          open
        >
          <DialogContent>
            <DialogHeader>
              <DialogTitle>{t("sessions.revokeTitle")}</DialogTitle>
              <DialogDescription>
                {t("sessions.revokeDescription").replace(
                  "{username}",
                  revoking.username,
                )}
              </DialogDescription>
            </DialogHeader>
            {revoke.error && (
              <p className="text-sm text-destructive" role="alert">
                {revoke.error instanceof Error && revoke.error.message
                  ? revoke.error.message
                  : t("sessions.revokeError")}
              </p>
            )}
            <DialogFooter>
              <Button onClick={() => setRevoking(null)} variant="outline">
                {t("sessions.cancel")}
              </Button>
              <Button
                disabled={revoke.isPending}
                onClick={() => revoke.mutate(revoking)}
                variant="destructive"
              >
                {revoke.isPending
                  ? t("sessions.revoking")
                  : t("sessions.revoke")}
              </Button>
            </DialogFooter>
          </DialogContent>
        </Dialog>
      )}
    </div>
  );
}
