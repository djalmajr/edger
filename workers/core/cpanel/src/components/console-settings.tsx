import { useQuery } from "@tanstack/react-query";
import { Button } from "@edger/ui/components/ui/button";
import {
  Card,
  CardContent,
  CardHeader,
  CardTitle,
} from "@edger/ui/components/ui/card";
import { RefreshCwIcon } from "@edger/ui/icons/lucide";
import { apiJson, type Principal } from "../lib/api";
import { useI18n } from "../lib/i18n";

// Read-only view over /api/admin/settings: the values are fixed at
// installation startup, so the page renders values only — no inputs,
// switches or save action that would pretend to write.
export type InstallationSettings = {
  loginMaxFailures: number;
  loginWindowSeconds: number;
  passwordEnabled: boolean;
  passwordMaxLength: number;
  passwordMinLength: number;
  runtimeVersion: string;
  sessionTtlSeconds: number;
  tenantRoutingEnabled: boolean;
  weightedRoutingEnabled: boolean;
};

export function ConsoleSettings({
  apiKey,
  principal,
}: {
  apiKey: string;
  principal: Principal;
}) {
  const { t } = useI18n();
  // The gate runs before the child, so a non-root principal never mounts
  // the query hooks and never issues a single request.
  if (!principal.isRoot) {
    return (
      <p className="text-sm text-muted-foreground">
        {t("settings.noManagement")}
      </p>
    );
  }
  return <SettingsPanel apiKey={apiKey} />;
}

function SettingsPanel({ apiKey }: { apiKey: string }) {
  const { t } = useI18n();
  const settingsQuery = useQuery({
    queryFn: () =>
      apiJson<InstallationSettings>(apiKey, "/api/admin/settings"),
    queryKey: ["cpanel", "settings"],
  });

  const days = (seconds: number) => {
    const value = Math.round((seconds / 86_400) * 10) / 10;
    return value === 1
      ? t("settings.daysOne")
      : t("settings.days").replace("{count}", String(value));
  };
  const minutes = (seconds: number) => {
    const value = Math.round((seconds / 60) * 10) / 10;
    return value === 1
      ? t("settings.minutesOne")
      : t("settings.minutes").replace("{count}", String(value));
  };
  const flag = (enabled: boolean) =>
    enabled ? t("settings.enabled") : t("settings.disabled");

  if (settingsQuery.isLoading) {
    return (
      <p className="text-sm text-foreground" role="status">
        {t("settings.loading")}
      </p>
    );
  }
  if (settingsQuery.error || !settingsQuery.data) {
    return (
      <div className="flex items-center justify-between gap-3">
        <p className="text-sm text-destructive" role="alert">
          {t("settings.loadError")}
        </p>
        <Button
          onClick={() => void settingsQuery.refetch()}
          size="sm"
          variant="outline"
        >
          {t("settings.retry")}
        </Button>
      </div>
    );
  }
  const data = settingsQuery.data;
  const row = (label: string, value: string) => (
    <div className="flex justify-between gap-3 border-b pb-2 last:border-0">
      <span className="text-muted-foreground">{label}</span>
      <strong className="truncate">{value}</strong>
    </div>
  );
  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <p className="text-sm text-muted-foreground">
          {t("settings.readOnly")}
        </p>
        <Button
          onClick={() => void settingsQuery.refetch()}
          variant="outline"
        >
          <RefreshCwIcon />
          {t("overview.refresh")}
        </Button>
      </div>
      <div className="grid gap-4 lg:grid-cols-3">
        <Card>
          <CardHeader>
            <CardTitle>{t("settings.runtime")}</CardTitle>
          </CardHeader>
          <CardContent className="grid gap-3 text-sm">
            {row(t("settings.runtimeVersion"), data.runtimeVersion)}
          </CardContent>
        </Card>
        <Card>
          <CardHeader>
            <CardTitle>{t("settings.routing")}</CardTitle>
          </CardHeader>
          <CardContent className="grid gap-3 text-sm">
            {row(t("settings.tenantRouting"), flag(data.tenantRoutingEnabled))}
            {row(
              t("settings.weightedRouting"),
              flag(data.weightedRoutingEnabled),
            )}
          </CardContent>
        </Card>
        <Card>
          <CardHeader>
            <CardTitle>{t("settings.security")}</CardTitle>
          </CardHeader>
          <CardContent className="grid gap-3 text-sm">
            {row(t("settings.passwordEnabled"), flag(data.passwordEnabled))}
            {row(t("settings.sessionDuration"), days(data.sessionTtlSeconds))}
            {row(
              t("settings.passwordLength"),
              `${data.passwordMinLength}–${data.passwordMaxLength}`,
            )}
            {row(
              t("settings.loginWindow"),
              minutes(data.loginWindowSeconds),
            )}
            {row(t("settings.maxFailures"), String(data.loginMaxFailures))}
          </CardContent>
        </Card>
      </div>
    </div>
  );
}
