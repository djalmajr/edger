import { Button } from "@edger/ui/components/ui/button";
import {
  Card,
  CardAction,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@edger/ui/components/ui/card";
import { Input } from "@edger/ui/components/ui/input";
import { Label } from "@edger/ui/components/ui/label";
import { Separator } from "@edger/ui/components/ui/separator";
import * as React from "react";

import { ApiError, changeMyPassword, passwordPolicyIssues } from "../lib/api";
import { useI18n } from "../lib/i18n";
import { UsersGlyph } from "./glyphs";
import { LanguageMenu, ThemeMenu } from "./preference-menus";

// Full-screen, non-dismissible password change for a session whose password
// is temporary (created or reset by an operator). It replaces the Shell for
// every entry path — password login, a pasted session token and a session
// restored on reload — and is styled like the login card. There is no close
// button and Escape does nothing: the only exits are the logout action and a
// successful change (which stores the rotated token exactly like the
// account-menu dialog does, or ends the local session when the server hands
// back no token). The policy rules and the copy come from the shared
// passwordPolicyIssues and the change-password dialog's i18n keys — nothing
// is duplicated here; only the confirm field and its message are new.
export function PasswordChangeScreen({
  apiKey,
  onLogout,
  onNewSession,
  onRequireLogin,
}: {
  apiKey: string;
  onLogout(): void;
  onNewSession(token: string): void;
  onRequireLogin(): void;
}) {
  const { t } = useI18n();
  const [current, setCurrent] = React.useState("");
  const [next, setNext] = React.useState("");
  const [confirm, setConfirm] = React.useState("");
  const [error, setError] = React.useState("");
  const [submitting, setSubmitting] = React.useState(false);

  // The same policy check as the account-menu dialog: an empty new password
  // is allowed to be typed and rejected at submit time; anything non-empty
  // must satisfy the whole policy before the submit button enables.
  const nextOk =
    next === "" || passwordPolicyIssues(next).length === 0;
  const confirmOk = next === "" || confirm === next;

  function describeChangeError(reason: unknown): string {
    if (reason instanceof ApiError) {
      if (reason.status === 401) return t("account.changePassword.invalidCurrent");
      if (reason.status === 429) return t("auth.rateLimited");
      // A 400 carries the server's fixed validation copy (for example: the
      // new password must differ from the current one) — shown verbatim.
      if (reason.status === 400) return reason.message;
    }
    return t("auth.networkError");
  }

  async function submit(
    event: React.FormEvent<HTMLFormElement>,
  ): Promise<void> {
    event.preventDefault();
    if (submitting || !current || !nextOk || !confirmOk || next === "") return;
    setSubmitting(true);
    setError("");
    try {
      const token = await changeMyPassword(apiKey, current, next);
      if (token) onNewSession(token);
      else onRequireLogin();
    } catch (reason) {
      setError(describeChangeError(reason));
    } finally {
      setSubmitting(false);
    }
  }

  return (
    <main className="grid min-h-screen place-items-center bg-sidebar p-4">
      <Card className="w-full max-w-md rounded-md border border-border shadow-sm ring-0">
        <CardHeader>
          <div className="flex min-w-0 items-center gap-3">
            <span className="grid size-10 shrink-0 place-items-center rounded-lg bg-primary/15 text-primary">
              <UsersGlyph className="size-5" />
            </span>
            <div className="min-w-0">
              <CardTitle>{t("auth.passwordChange.title")}</CardTitle>
              <CardDescription>
                {t("auth.passwordChange.description")}
              </CardDescription>
            </div>
          </div>
          <CardAction>
            <div className="flex shrink-0 items-center gap-1">
              <LanguageMenu />
              <ThemeMenu />
            </div>
          </CardAction>
        </CardHeader>
        <CardContent>
          <div className="grid gap-4">
            <form className="grid gap-4" onSubmit={submit}>
              <div className="grid gap-2">
                <Label htmlFor="forced-current-password">
                  {t("account.changePassword.current")}
                </Label>
                <Input
                  autoComplete="current-password"
                  id="forced-current-password"
                  type="password"
                  value={current}
                  onChange={(event) => setCurrent(event.target.value)}
                />
              </div>
              <div className="grid gap-2">
                <Label htmlFor="forced-new-password">
                  {t("account.changePassword.new")}
                </Label>
                <Input
                  autoComplete="new-password"
                  id="forced-new-password"
                  type="password"
                  value={next}
                  onChange={(event) => setNext(event.target.value)}
                />
                <p className="text-xs text-muted-foreground">
                  {t("users.passwordHint")}
                </p>
                {!nextOk && (
                  <p className="text-sm text-destructive" role="alert">
                    {t("users.invalidPassword")}
                  </p>
                )}
              </div>
              <div className="grid gap-2">
                <Label htmlFor="forced-confirm-password">
                  {t("auth.passwordChange.confirm")}
                </Label>
                <Input
                  autoComplete="new-password"
                  id="forced-confirm-password"
                  type="password"
                  value={confirm}
                  onChange={(event) => setConfirm(event.target.value)}
                />
                {!confirmOk && (
                  <p className="text-sm text-destructive" role="alert">
                    {t("auth.passwordChange.mismatch")}
                  </p>
                )}
              </div>
              {error && (
                <p className="rounded-lg border border-destructive/30 bg-destructive/10 px-3 py-2 text-sm text-destructive">
                  {error}
                </p>
              )}
              <Button
                disabled={
                  submitting || !current || next === "" || !nextOk || !confirmOk
                }
                type="submit"
              >
                {submitting
                  ? t("account.changePassword.changing")
                  : t("account.changePassword.submit")}
              </Button>
            </form>
            <Separator />
            <Button
              className="justify-self-start"
              variant="outline"
              onClick={onLogout}
            >
              {t("account.logout")}
            </Button>
          </div>
        </CardContent>
      </Card>
    </main>
  );
}
