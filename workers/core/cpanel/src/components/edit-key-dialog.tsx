import { useMutation } from "@tanstack/react-query";
import { Button } from "@edger/ui/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@edger/ui/components/ui/dialog";
import {
  apiJson,
  can,
  PERMISSION_CATALOG,
  type ApiKey,
  type Principal,
  type UpdateKeyPermissionsRequest,
} from "../lib/api";
import { useI18n } from "../lib/i18n";
import * as React from "react";

export function EditKeyDialog({
  apiKey,
  keyInfo,
  onClose,
  onSaved,
  principal,
}: {
  apiKey: string;
  keyInfo: ApiKey;
  onClose(): void;
  onSaved(): void;
  principal: Principal;
}) {
  const { t } = useI18n();
  const [permissions, setPermissions] = React.useState<string[]>([
    ...keyInfo.permissions,
  ]);
  const save = useMutation({
    mutationFn: () => {
      const request: UpdateKeyPermissionsRequest = { permissions };
      return apiJson<ApiKey>(apiKey, `/api/admin/keys/${keyInfo.id}`, {
        body: JSON.stringify(request),
        headers: { "content-type": "application/json" },
        method: "PATCH",
      });
    },
    onSuccess: () => {
      onSaved();
      onClose();
    },
  });

  React.useEffect(() => {
    setPermissions([...keyInfo.permissions]);
    save.reset();
  }, [keyInfo.id, keyInfo.permissions, save.reset]);

  return (
    <Dialog onOpenChange={(open) => !open && onClose()} open>
      <DialogContent className="sm:max-w-xl">
        <DialogHeader>
          <DialogTitle>{t("keys.edit.title")}</DialogTitle>
          <DialogDescription>
            {t("keys.edit.description").replace("{name}", keyInfo.name)}
          </DialogDescription>
        </DialogHeader>
        <fieldset className="space-y-1.5">
          <legend className="text-sm font-medium">{t("keys.permissions")}</legend>
          <div className="grid grid-cols-2 gap-1.5">
            {PERMISSION_CATALOG.map((permission) => {
              const allowed = can(principal, permission);
              return (
                <label
                  className={`flex items-center gap-2 text-sm ${allowed ? "" : "opacity-40"}`}
                  key={permission}
                >
                  <input
                    checked={permissions.includes(permission)}
                    disabled={(!allowed && !permissions.includes(permission)) || save.isPending}
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
        {save.error && (
          <p className="text-sm text-destructive" role="alert">
            {t("keys.edit.error")}
          </p>
        )}
        <DialogFooter>
          <Button
            disabled={save.isPending}
            onClick={onClose}
            variant="outline"
          >
            {t("keys.cancel")}
          </Button>
          <Button
            disabled={save.isPending || permissions.length === 0}
            onClick={() => save.mutate()}
          >
            {save.isPending ? t("keys.edit.saving") : t("keys.edit.save")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
