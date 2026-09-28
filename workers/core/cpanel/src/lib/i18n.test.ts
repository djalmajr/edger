import { describe, expect, it } from "vitest";

import {
  messages,
  normalizeLocale,
  type TranslationKey,
  translate,
} from "./i18n";

describe("cPanel i18n", () => {
  it("normalizes supported browser locale aliases", () => {
    expect(normalizeLocale("pt-BR")).toBe("pt-BR");
    expect(normalizeLocale("pt")).toBe("pt-BR");
    expect(normalizeLocale("en-GB")).toBe("en-US");
    expect(normalizeLocale("es-MX")).toBe("es-ES");
  });

  it("falls back to Portuguese for unsupported locales", () => {
    expect(normalizeLocale("fr-FR")).toBe("pt-BR");
  });

  it("translates the authenticated shell", () => {
    expect(translate("pt-BR", "nav.workers")).toBe("Workers");
    expect(translate("en-US", "account.logout")).toBe("Log out");
    expect(translate("es-ES", "preferences.theme.dark")).toBe("Oscuro");
  });

  it("keeps every key present in all three locales", () => {
    const locales: Array<"pt-BR" | "en-US" | "es-ES"> = [
      "pt-BR",
      "en-US",
      "es-ES",
    ];
    const pt = Object.keys(messages["pt-BR"]).sort();
    for (const locale of locales) {
      expect(Object.keys(messages[locale]).sort()).toEqual(pt);
    }
    // A missing key must never surface as an undefined string.
    for (const locale of locales) {
      for (const key of pt as TranslationKey[]) {
        expect(translate(locale, key)).toBeTruthy();
      }
    }
  });

  it("translates the admin login screen in each locale", () => {
    expect(translate("pt-BR", "auth.title")).toBe("Acesso administrativo");
    expect(translate("en-US", "auth.title")).toBe("Admin access");
    expect(translate("es-ES", "auth.title")).toBe("Acceso administrativo");
    expect(translate("pt-BR", "auth.description")).toBe(
      "Entre com usuário e senha do console.",
    );
    expect(translate("pt-BR", "auth.rootSeeded")).toBe(
      "Root é seedado no primeiro boot.",
    );
    expect(translate("en-US", "auth.rootSeeded")).toBe(
      "Root is seeded on first boot.",
    );
    expect(translate("pt-BR", "auth.username")).toBe("Usuário");
    expect(translate("pt-BR", "auth.password")).toBe("Senha");
    expect(translate("pt-BR", "auth.enter")).toBe("Entrar");
    expect(translate("en-US", "auth.enter")).toBe("Sign in");
    expect(translate("es-ES", "auth.enter")).toBe("Entrar");
    expect(translate("pt-BR", "auth.enterWithToken")).toBe("Entrar com token");
    expect(translate("en-US", "auth.enterWithToken")).toBe(
      "Sign in with token",
    );
    expect(translate("pt-BR", "auth.invalidCredentials")).toBe(
      "Usuário ou senha inválidos.",
    );
    expect(translate("en-US", "auth.invalidCredentials")).toBe(
      "Invalid username or password.",
    );
    expect(translate("pt-BR", "auth.rateLimited")).toContain("Muitas tentativas");
    expect(translate("pt-BR", "auth.unavailable")).toContain("indisponível");
    expect(translate("pt-BR", "auth.networkError")).toContain("conexão");
    expect(translate("pt-BR", "auth.loginFailed")).toContain("entrar");
    expect(translate("pt-BR", "auth.tokenInvalid")).toContain("Token");
    // The three copy states: confirmed seed, explicit passwords-off, and the
    // neutral token sentence (also the unknown/malformed fallback).
    expect(translate("pt-BR", "auth.passwordUnavailable")).toBe(
      "O login por senha ainda não está disponível. Entre com token.",
    );
    expect(translate("en-US", "auth.passwordUnavailable")).toBe(
      "Password sign-in is not available yet. Sign in with a token.",
    );
    expect(translate("es-ES", "auth.passwordUnavailable")).toBe(
      "El inicio de sesión con contraseña aún no está disponible. Entre con token.",
    );
    expect(translate("pt-BR", "auth.alsoWithToken")).toBe(
      "Você também pode entrar com token.",
    );
    expect(translate("en-US", "auth.alsoWithToken")).toBe(
      "You can also sign in with a token.",
    );
    expect(translate("es-ES", "auth.alsoWithToken")).toBe(
      "También puede entrar con token.",
    );
    // The copy must never promise password setup through the token.
    expect(translate("pt-BR", "auth.passwordUnavailable")).not.toContain(
      "configurar",
    );
    expect(translate("en-US", "auth.passwordUnavailable")).not.toContain(
      "set a password",
    );
    expect(translate("es-ES", "auth.passwordUnavailable")).not.toContain(
      "configurar",
    );
    expect(translate("pt-BR", "auth.alsoWithToken")).not.toContain(
      "configurar",
    );
    expect(translate("es-ES", "auth.rootSeeded")).toBe(
      "La cuenta root se crea en el primer arranque.",
    );
  });

  it("explains the console user management and own-password copy", () => {
    expect(translate("pt-BR", "nav.users")).toBe("Usuários");
    expect(translate("en-US", "nav.users")).toBe("Users");
    expect(translate("es-ES", "nav.users")).toBe("Usuarios");
    expect(translate("en-US", "users.noManagement")).toContain("root principal");
    expect(translate("pt-BR", "users.noManagement")).toContain("principal root");
    expect(translate("es-ES", "users.noManagement")).toContain("principal root");
    expect(translate("en-US", "users.delete.description")).toContain(
      "cannot be undone",
    );
    expect(translate("pt-BR", "users.disable.description")).toContain(
      "revogadas",
    );
    expect(translate("en-US", "users.reset.description")).toContain("revokes");
    expect(translate("en-US", "users.invalidUsername")).toContain(
      "no leading punctuation",
    );
    expect(translate("en-US", "users.invalidPassword")).toContain("12 to 128");
    expect(translate("pt-BR", "account.changePassword")).toBe("Alterar senha");
    expect(translate("en-US", "account.changePassword")).toBe("Change password");
    expect(translate("es-ES", "account.changePassword")).toBe(
      "Cambiar contraseña",
    );
    expect(
      translate("en-US", "account.changePassword.invalidCurrent"),
    ).toContain("not valid");
    // The own-password copy must say a fresh login can be required.
    expect(translate("en-US", "account.changePassword.description")).toContain(
      "new login",
    );
  });

  it("explains host availability and session cohorts in each locale", () => {
    expect(translate("pt-BR", "routing.lead")).toContain("não autentica");
    expect(translate("en-US", "routing.lead")).toContain("does not authenticate");
    expect(translate("es-ES", "routing.lead")).toContain("No autentica");
    expect(translate("pt-BR", "routing.splitHint")).toContain("80/20");
    expect(translate("en-US", "routing.splitHint")).toContain("80/20");
    expect(translate("es-ES", "routing.splitHint")).toContain("80/20");
    expect(translate("en-US", "routing.confirmDeleteBody")).toContain(
      "not an empty tenant list",
    );
    const routingFlagsPt = translate("pt-BR", "routing.flags");
    expect(routingFlagsPt).toContain("setup Rancher");
    expect(routingFlagsPt).toContain("restrição por tenant");
    expect(routingFlagsPt).toContain("divisão por peso");
    expect(routingFlagsPt).toContain("só afeta requisições");
    expect(routingFlagsPt).toContain("opção de roteamento ponderado está ligada");

    const routingFlagsEn = translate("en-US", "routing.flags");
    expect(routingFlagsEn).toContain("Rancher or installation setup");
    expect(routingFlagsEn).toContain("Tenant restrictions affect requests only");
    expect(routingFlagsEn).toContain("Weighted routing affects requests only");
    expect(routingFlagsEn).toContain("option is enabled");

    const routingFlagsEs = translate("es-ES", "routing.flags");
    expect(routingFlagsEs).toContain("setup Rancher");
    expect(routingFlagsEs).toContain("restricción por tenant");
    expect(routingFlagsEs).toContain("enrutamiento ponderado");
    expect(routingFlagsEs).toContain("solo afecta las peticiones");
    expect(routingFlagsEs).toContain("está activada");
    expect(translate("en-US", "routing.confirmApply")).toBe("Save configuration");
    expect(translate("en-US", "routing.saved")).toContain("when the matching options are on");
    expect(translate("en-US", "routing.saved")).not.toContain("not active");
    expect(translate("pt-BR", "routing.effective")).toBe("Política configurada");
    expect(translate("en-US", "routing.effective")).toBe("Configured policy");
    expect(translate("es-ES", "routing.effective")).toBe("Política configurada");
  });
});
