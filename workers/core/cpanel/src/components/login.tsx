import { Button } from "@edger/ui/components/ui/button";
import {
  Card,
  CardAction,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@edger/ui/components/ui/card";
import {
  InputGroup,
  InputGroupButton,
  InputGroupInput,
} from "@edger/ui/components/ui/input-group";
import { Input } from "@edger/ui/components/ui/input";
import { Label } from "@edger/ui/components/ui/label";
import { Separator } from "@edger/ui/components/ui/separator";
import * as React from "react";

import {
  ApiError,
  login,
  loadAll,
  loginOptions,
  type LoginOptions,
} from "../lib/api";
import { type TranslationKey, useI18n } from "../lib/i18n";
import {
  EyeGlyph,
  EyeOffGlyph,
  LogInGlyph,
  UsersGlyph,
} from "./glyphs";
import { LanguageMenu, ThemeMenu } from "./preference-menus";

export function AdminLogin({
  onAuthenticated,
}: {
  onAuthenticated(token: string): void;
}) {
  const { t } = useI18n();
  const [options, setOptions] = React.useState<LoginOptions | null>(null);
  const [username, setUsername] = React.useState("");
  const [password, setPassword] = React.useState("");
  const [showPassword, setShowPassword] = React.useState(false);
  const [token, setToken] = React.useState("");
  const [showToken, setShowToken] = React.useState(false);
  const [tokenOpen, setTokenOpen] = React.useState(false);
  const [submittingForm, setSubmittingForm] = React.useState<
    "password" | "token" | null
  >(null);
  const [error, setError] = React.useState("");

  // The seed claim is only shown when the runtime confirms it; an unknown or
  // partial /login-options answer must never present a fact it did not give.
  React.useEffect(() => {
    let cancelled = false;
    void loginOptions().then((data) => {
      if (!cancelled) setOptions(data);
    });
    return () => {
      cancelled = true;
    };
  }, []);

  // The form stays visible unless the runtime explicitly says password login
  // is off; an unknown answer keeps the form.
  const passwordEnabled = options?.passwordEnabled !== false;
  // The seed claim is only shown when the runtime confirms it.
  const rootSeeded =
    options?.passwordEnabled === true && options?.rootSeeded === true;
  // Copy states: a confirmed seed, an explicit "passwords off", or the
  // neutral token sentence. The neutral sentence also covers an unknown or
  // malformed /login-options answer — the screen never asserts a fact the
  // runtime did not confirm, and never promises actions the token cannot do.
  const copyKey: TranslationKey = rootSeeded
    ? "auth.rootSeeded"
    : options?.passwordEnabled === false
      ? "auth.passwordUnavailable"
      : "auth.alsoWithToken";

  function describeLoginError(reason: unknown): string {
    if (reason instanceof ApiError) {
      if (reason.status === 401) return t("auth.invalidCredentials");
      if (reason.status === 429) return t("auth.rateLimited");
      if (reason.status === 503) return t("auth.unavailable");
      return t("auth.loginFailed");
    }
    return t("auth.networkError");
  }

  async function submitPassword(
    event: React.FormEvent<HTMLFormElement>,
  ): Promise<void> {
    event.preventDefault();
    if (submittingForm !== null) return;
    const name = username.trim();
    if (!name || !password) return;
    setSubmittingForm("password");
    setError("");
    try {
      const token = await login(name, password);
      // Validate before storing: a token that cannot load the runtime is not
      // a session.
      await loadAll(token);
      onAuthenticated(token);
    } catch (reason) {
      // Fixed copy per status: the password never reaches the error surface.
      setError(describeLoginError(reason));
    } finally {
      setSubmittingForm(null);
    }
  }

  // The legacy root/egk_/OIDC path: the token itself is the credential,
  // validated with loadAll exactly like before.
  async function submitToken(
    event: React.FormEvent<HTMLFormElement>,
  ): Promise<void> {
    event.preventDefault();
    if (submittingForm !== null) return;
    const value = token.trim();
    if (!value) return;
    setSubmittingForm("token");
    setError("");
    try {
      await loadAll(value);
      onAuthenticated(value);
    } catch (reason) {
      setError(
        reason instanceof ApiError && reason.status === 401
          ? t("auth.tokenInvalid")
          : reason instanceof Error
            ? reason.message
            : t("auth.loginFailed"),
      );
    } finally {
      setSubmittingForm(null);
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
              <CardTitle>{t("auth.title")}</CardTitle>
              <CardDescription>{t("auth.description")}</CardDescription>
              <p className="text-sm text-muted-foreground">{t(copyKey)}</p>
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
            {passwordEnabled && (
              <form className="grid gap-4" onSubmit={submitPassword}>
                <div className="grid gap-2">
                  <Label htmlFor="login-username">
                    {t("auth.username")}
                  </Label>
                  <Input
                    autoComplete="username"
                    autoFocus
                    autoCapitalize="off"
                    autoCorrect="off"
                    id="login-username"
                    onChange={(event) => setUsername(event.target.value)}
                    spellCheck={false}
                    value={username}
                  />
                </div>
                <div className="grid gap-2">
                  <Label htmlFor="login-password">
                    {t("auth.password")}
                  </Label>
                  <InputGroup>
                    <InputGroupInput
                      autoComplete="current-password"
                      id="login-password"
                      onChange={(event) => setPassword(event.target.value)}
                      type={showPassword ? "text" : "password"}
                      value={password}
                    />
                    <InputGroupButton
                      aria-label={
                        showPassword
                          ? t("auth.hidePassword")
                          : t("auth.showPassword")
                      }
                      onClick={() => setShowPassword((value) => !value)}
                      size="icon-sm"
                      type="button"
                    >
                      {showPassword ? <EyeOffGlyph /> : <EyeGlyph />}
                    </InputGroupButton>
                  </InputGroup>
                </div>
                <Button
                  disabled={
                    submittingForm !== null || !username.trim() || !password
                  }
                  type="submit"
                >
                  <LogInGlyph />
                  {submittingForm === "password"
                    ? t("auth.entering")
                    : t("auth.enter")}
                </Button>
              </form>
            )}
            {error && (
              <p className="rounded-lg border border-destructive/30 bg-destructive/10 px-3 py-2 text-sm text-destructive">
                {error}
              </p>
            )}
            <Separator />
            <button
              className="justify-self-start text-xs text-muted-foreground underline-offset-2 hover:underline"
              onClick={() => setTokenOpen((value) => !value)}
              type="button"
            >
              {t("auth.enterWithToken")}
            </button>
            {tokenOpen && (
              <form className="grid gap-2" onSubmit={submitToken}>
                <Label htmlFor="login-token">{t("auth.token")}</Label>
                <InputGroup>
                  <InputGroupInput
                    autoComplete="off"
                    id="login-token"
                    onChange={(event) => setToken(event.target.value)}
                    type={showToken ? "text" : "password"}
                    value={token}
                  />
                  <InputGroupButton
                    aria-label={
                      showToken ? t("auth.hideToken") : t("auth.showToken")
                    }
                    onClick={() => setShowToken((value) => !value)}
                    size="icon-sm"
                    type="button"
                  >
                    {showToken ? <EyeOffGlyph /> : <EyeGlyph />}
                  </InputGroupButton>
                </InputGroup>
                <Button
                  disabled={submittingForm !== null || !token.trim()}
                  type="submit"
                  variant="outline"
                >
                  {submittingForm === "token"
                    ? t("auth.entering")
                    : t("auth.enter")}
                </Button>
              </form>
            )}
          </div>
        </CardContent>
      </Card>
    </main>
  );
}
