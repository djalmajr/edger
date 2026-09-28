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
import * as React from "react";

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

// Barrel-free on purpose: the component joins the DOM test graph, and the
// @edger/ui icon barrel cannot be imported there under the bun runner. Row
// actions are text buttons, so no icons are needed.

// Story 26.04: additional password users over the `/api/admin/users`
// backend. Root-only on the server; the UI hides what the principal cannot
// do and never substitutes the server-side validation. Passwords ride the
// request body only — never URLs, errors, or logs.

function formatEpoch(seconds?: number | null) {
  if (!seconds) return "—";
  return new Date(seconds * 1000).toLocaleString();
}

function describeError(reason: unknown): string {
  return reason instanceof Error ? reason.message : String(reason);
}

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
              <code className="text-xs">{permission}</code>
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
}: {
  apiKey: string;
  principal: Principal;
}) {
  const { t } = useI18n();
  // The server is the real barrier: a non-root principal sees only the
  // notice, and the routes answer 401/403 for anything but root.
  if (!principal.isRoot) {
    return (
      <p className="text-sm text-muted-foreground">{t("users.noManagement")}</p>
    );
  }
  return <UsersPanel apiKey={apiKey} />;
}

function UsersPanel({ apiKey }: { apiKey: string }) {
  const { t } = useI18n();
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
  const users = usersQuery.data ?? [];
  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between gap-2">
        <p className="text-sm text-muted-foreground">{t("users.lead")}</p>
        <Button onClick={() => setCreateOpen(true)}>
          {t("users.new")}
        </Button>
      </div>
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
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>{t("users.username")}</TableHead>
              <TableHead>{t("users.status")}</TableHead>
              <TableHead>{t("users.permissions")}</TableHead>
              <TableHead>{t("users.namespaces")}</TableHead>
              <TableHead>{t("users.workers")}</TableHead>
              <TableHead>{t("users.created")}</TableHead>
              <TableHead aria-label="Actions" />
            </TableRow>
          </TableHeader>
          <TableBody>
            {users.map((user) => (
              <TableRow key={user.id}>
                <TableCell className="font-medium">
                  <span className="flex items-center gap-2">
                    {user.username}
                    {user.isRoot && (
                      <Badge variant="secondary">root</Badge>
                    )}
                  </span>
                </TableCell>
                <TableCell>
                  <Badge variant={user.disabled ? "outline" : "default"}>
                    {user.disabled ? t("users.disabled") : t("users.active")}
                  </Badge>
                </TableCell>
                <TableCell>
                  <div className="flex max-w-64 flex-wrap gap-1">
                    {user.permissions.map((permission) => (
                      <Badge key={permission} variant="secondary">
                        {permission}
                      </Badge>
                    ))}
                  </div>
                </TableCell>
                <TableCell>
                  <code className="text-xs">{user.namespaces.join(", ")}</code>
                </TableCell>
                <TableCell>
                  <code className="text-xs">{user.workers.join(", ")}</code>
                </TableCell>
                <TableCell className="text-xs text-muted-foreground">
                  {formatEpoch(user.createdAt)}
                </TableCell>
                <TableCell>
                  {user.isRoot ? (
                    <span className="sr-only">{t("users.noManagement")}</span>
                  ) : (
                    <div className="flex justify-end gap-1">
                      <Button
                        onClick={() => setEditTarget(user)}
                        size="sm"
                        variant="outline"
                      >
                        {t("users.edit")}
                      </Button>
                      {user.disabled ? (
                        <Button
                          disabled={enableMutation.isPending}
                          onClick={() => enableMutation.mutate(user)}
                          size="sm"
                          variant="outline"
                        >
                          {t("users.enable")}
                        </Button>
                      ) : (
                        <Button
                          onClick={() => setDisableTarget(user)}
                          size="sm"
                          variant="outline"
                        >
                          {t("users.disable")}
                        </Button>
                      )}
                      <Button
                        onClick={() => setResetTarget(user)}
                        size="sm"
                        variant="outline"
                      >
                        {t("users.resetPassword")}
                      </Button>
                      <Button
                        onClick={() => setDeleteTarget(user)}
                        size="sm"
                        variant="destructive"
                      >
                        {t("users.delete")}
                      </Button>
                    </div>
                  )}
                </TableCell>
              </TableRow>
            ))}
            {users.length === 0 && (
              <TableRow>
                <TableCell
                  className="text-center text-sm text-muted-foreground"
                  colSpan={7}
                >
                  {t("users.empty")}
                </TableCell>
              </TableRow>
            )}
          </TableBody>
        </Table>
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
            <code className="text-sm">{user.username}</code>
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
