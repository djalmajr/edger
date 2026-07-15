import { describe, expect, it } from "vitest";

import { normalizeLocale, translate } from "./i18n";

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
});
