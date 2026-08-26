import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
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
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@edger/ui/components/ui/table";
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@edger/ui/components/ui/tooltip";
import {
  BanIcon,
  CheckIcon,
  CopyIcon,
  PlusIcon,
  Trash2Icon,
} from "@edger/ui/icons/lucide";
import * as React from "react";
import {
  apiJson,
  canManageKeys,
  PERMISSION_CATALOG,
  type ApiKey,
  type CreatedKey,
  type CreateKeyRequest,
  type Principal,
} from "../lib/api";

// A gestão segue o padrão tenancit/Studio: escopos por checkbox, o segredo
// aparece UMA vez na criação, revogação é terminal e o delete só existe para
// key já revogada. O servidor aplica a anti-escalada (subconjunto do criador)
// — a UI só desabilita o que o principal visivelmente não pode conceder.

function formatEpoch(seconds?: number | null) {
  if (!seconds) return "—";
  return new Date(seconds * 1000).toLocaleString();
}

function keyStatus(key: ApiKey): "revoked" | "expired" | "active" {
  if (key.revokedAt) return "revoked";
  if (key.expiresAt && key.expiresAt * 1000 < Date.now()) return "expired";
  return "active";
}

const EXPIRY_CHOICES = [
  { days: 0, label: "Never" },
  { days: 30, label: "30 days" },
  { days: 90, label: "90 days" },
  { days: 365, label: "365 days" },
] as const;

export function ApiKeys({
  apiKey,
  principal,
}: {
  apiKey: string;
  principal: Principal;
}) {
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

  const invalidate = () =>
    queryClient.invalidateQueries({ queryKey: ["cpanel", "keys"] });

  const revoke = useMutation({
    mutationFn: (id: number) =>
      apiJson(apiKey, `/api/admin/keys/${id}/revoke`, { method: "POST" }),
    onSettled: invalidate,
  });
  const remove = useMutation({
    mutationFn: (id: number) =>
      apiJson(apiKey, `/api/admin/keys/${id}`, { method: "DELETE" }),
    onSettled: invalidate,
  });

  if (!manageable) {
    return (
      <p className="text-sm text-muted-foreground">
        This account has no keys:manage permission.
      </p>
    );
  }

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between gap-2">
        <p className="text-sm text-muted-foreground">
          Control-plane API keys. The secret is shown once at creation;
          revocation is permanent.
        </p>
        <Button onClick={() => setCreateOpen(true)}>
          <PlusIcon /> New key
        </Button>
      </div>

      {keysQuery.error ? (
        <p className="text-sm text-destructive">
          {(keysQuery.error as Error).message}
        </p>
      ) : (
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>Name</TableHead>
              <TableHead>Key</TableHead>
              <TableHead>Permissions</TableHead>
              <TableHead>Workers</TableHead>
              <TableHead>Status</TableHead>
              <TableHead>Last used</TableHead>
              <TableHead>Expires</TableHead>
              <TableHead aria-label="Actions" />
            </TableRow>
          </TableHeader>
          <TableBody>
            {(keysQuery.data ?? []).map((key) => {
              const status = keyStatus(key);
              return (
                <TableRow key={key.id}>
                  <TableCell className="font-medium">{key.name}</TableCell>
                  <TableCell>
                    <code className="text-xs">{key.keyPrefix}…</code>
                  </TableCell>
                  <TableCell>
                    <div className="flex max-w-64 flex-wrap gap-1">
                      {key.permissions.map((permission) => (
                        <Badge key={permission} variant="secondary">
                          {permission}
                        </Badge>
                      ))}
                    </div>
                  </TableCell>
                  <TableCell>
                    <code className="text-xs">{key.workers.join(", ")}</code>
                  </TableCell>
                  <TableCell>
                    <Badge
                      variant={status === "active" ? "default" : "outline"}
                    >
                      {status}
                    </Badge>
                  </TableCell>
                  <TableCell className="text-xs text-muted-foreground">
                    {formatEpoch(key.lastUsedAt)}
                  </TableCell>
                  <TableCell className="text-xs text-muted-foreground">
                    {formatEpoch(key.expiresAt)}
                  </TableCell>
                  <TableCell>
                    <div className="flex justify-end gap-1">
                      {status !== "revoked" ? (
                        <Tooltip>
                          <TooltipTrigger
                            render={
                              <Button
                                aria-label={`Revoke ${key.name}`}
                                onClick={() => revoke.mutate(key.id)}
                                size="icon"
                                variant="ghost"
                              />
                            }
                          >
                            <BanIcon />
                          </TooltipTrigger>
                          <TooltipContent>Revoke (permanent)</TooltipContent>
                        </Tooltip>
                      ) : (
                        <Tooltip>
                          <TooltipTrigger
                            render={
                              <Button
                                aria-label={`Delete ${key.name}`}
                                onClick={() => setConfirmDelete(key)}
                                size="icon"
                                variant="ghost"
                              />
                            }
                          >
                            <Trash2Icon />
                          </TooltipTrigger>
                          <TooltipContent>Delete</TooltipContent>
                        </Tooltip>
                      )}
                    </div>
                  </TableCell>
                </TableRow>
              );
            })}
            {keysQuery.data?.length === 0 && (
              <TableRow>
                <TableCell
                  className="text-center text-sm text-muted-foreground"
                  colSpan={8}
                >
                  No API keys yet.
                </TableCell>
              </TableRow>
            )}
          </TableBody>
        </Table>
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

      <Dialog
        onOpenChange={(open) => !open && setConfirmDelete(null)}
        open={Boolean(confirmDelete)}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Delete key</DialogTitle>
            <DialogDescription>
              Permanently remove “{confirmDelete?.name}” from the store? The
              key is already revoked; this only clears the record.
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button onClick={() => setConfirmDelete(null)} variant="outline">
              Cancel
            </Button>
            <Button
              onClick={() => {
                if (confirmDelete) remove.mutate(confirmDelete.id);
                setConfirmDelete(null);
              }}
              variant="destructive"
            >
              Delete
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
      <DialogContent>
        <DialogHeader>
          <DialogTitle>New API key</DialogTitle>
          <DialogDescription>
            A non-root creator can only grant a subset of its own permissions
            and scopes — the server enforces it.
          </DialogDescription>
        </DialogHeader>
        <div className="space-y-4">
          <div className="space-y-1.5">
            <Label htmlFor="key-name">Name</Label>
            <Input
              id="key-name"
              onChange={(event) => setName(event.target.value)}
              placeholder="studio-labdev"
              value={name}
            />
          </div>
          <fieldset className="space-y-1.5">
            <legend className="text-sm font-medium">Permissions</legend>
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
                    <code className="text-xs">{permission}</code>
                  </label>
                );
              })}
            </div>
          </fieldset>
          <div className="grid grid-cols-2 gap-3">
            <div className="space-y-1.5">
              <Label htmlFor="key-namespaces">Namespaces (CSV)</Label>
              <Input
                id="key-namespaces"
                onChange={(event) => setNamespaces(event.target.value)}
                placeholder="*"
                value={namespaces}
              />
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="key-workers">Workers (CSV, glob de sufixo)</Label>
              <Input
                id="key-workers"
                onChange={(event) => setWorkers(event.target.value)}
                placeholder="* ou p-abc*"
                value={workers}
              />
            </div>
          </div>
          <div className="space-y-1.5">
            <Label>Expiry</Label>
            <div className="flex gap-1.5">
              {EXPIRY_CHOICES.map((choice) => (
                <Button
                  key={choice.days}
                  onClick={() => setExpiryDays(choice.days)}
                  size="sm"
                  type="button"
                  variant={expiryDays === choice.days ? "default" : "outline"}
                >
                  {choice.label}
                </Button>
              ))}
            </div>
          </div>
          {create.error && (
            <p className="text-sm text-destructive">
              {(create.error as Error).message}
            </p>
          )}
        </div>
        <DialogFooter>
          <Button onClick={onClose} variant="outline">
            Cancel
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
            Create
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
  const [copied, setCopied] = React.useState(false);
  React.useEffect(() => {
    if (created) setCopied(false);
  }, [created]);
  return (
    <Dialog onOpenChange={(open) => !open && onClose()} open={Boolean(created)}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Key created</DialogTitle>
          <DialogDescription>
            Copy it now — the secret is not stored and will never be shown
            again.
          </DialogDescription>
        </DialogHeader>
        <div className="flex items-center gap-2">
          <code className="min-w-0 flex-1 break-all rounded bg-muted px-2 py-1.5 text-xs">
            {created?.rawKey}
          </code>
          <Button
            aria-label="Copy key"
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
          <Button onClick={onClose}>Done</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
