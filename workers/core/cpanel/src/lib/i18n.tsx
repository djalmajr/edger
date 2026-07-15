import * as React from "react";

const LOCALE_STORAGE_KEY = "edger.cpanel.locale";

export type Locale = "en-US" | "es-ES" | "pt-BR";
export type TranslationKey = keyof typeof messages["pt-BR"];

const messages = {
  "pt-BR": {
    "account.label": "Conta",
    "account.logout": "Sair",
    "account.namespaces": "Namespaces",
    "account.role": "Perfil",
    "nav.observability": "Observabilidade",
    "nav.observability.description": "Sinais e logs locais do runtime",
    "nav.overview": "Visão geral",
    "nav.overview.description": "Postura do runtime em um relance",
    "nav.workers": "Workers",
    "nav.workers.description": "Inventário de workers do runtime",
    "preferences.language": "Idioma",
    "preferences.theme": "Tema",
    "preferences.theme.dark": "Escuro",
    "preferences.theme.light": "Claro",
    "preferences.theme.system": "Sistema",
  },
  "en-US": {
    "account.label": "Account",
    "account.logout": "Log out",
    "account.namespaces": "Namespaces",
    "account.role": "Role",
    "nav.observability": "Observability",
    "nav.observability.description": "Local runtime signals and logs",
    "nav.overview": "Overview",
    "nav.overview.description": "Runtime posture at a glance",
    "nav.workers": "Workers",
    "nav.workers.description": "Runtime worker inventory",
    "preferences.language": "Language",
    "preferences.theme": "Theme",
    "preferences.theme.dark": "Dark",
    "preferences.theme.light": "Light",
    "preferences.theme.system": "System",
  },
  "es-ES": {
    "account.label": "Cuenta",
    "account.logout": "Cerrar sesión",
    "account.namespaces": "Namespaces",
    "account.role": "Perfil",
    "nav.observability": "Observabilidad",
    "nav.observability.description": "Señales y logs locales del runtime",
    "nav.overview": "Resumen",
    "nav.overview.description": "Estado del runtime de un vistazo",
    "nav.workers": "Workers",
    "nav.workers.description": "Inventario de workers del runtime",
    "preferences.language": "Idioma",
    "preferences.theme": "Tema",
    "preferences.theme.dark": "Oscuro",
    "preferences.theme.light": "Claro",
    "preferences.theme.system": "Sistema",
  },
} as const;

type I18nContextValue = {
  locale: Locale;
  setLocale(locale: Locale): void;
  t(key: TranslationKey): string;
};

const I18nContext = React.createContext<I18nContextValue | null>(null);

export function I18nProvider({ children }: { children: React.ReactNode }) {
  const [locale, setLocaleState] = React.useState(readLocale);
  const setLocale = React.useCallback((next: Locale) => {
    localStorage.setItem(LOCALE_STORAGE_KEY, next);
    document.documentElement.lang = next;
    setLocaleState(next);
  }, []);
  React.useEffect(() => {
    document.documentElement.lang = locale;
  }, [locale]);
  const value = React.useMemo(
    () => ({ locale, setLocale, t: (key: TranslationKey) => translate(locale, key) }),
    [locale, setLocale],
  );
  return <I18nContext.Provider value={value}>{children}</I18nContext.Provider>;
}

export function normalizeLocale(value: string | null | undefined): Locale {
  const normalized = value?.toLowerCase();
  if (normalized?.startsWith("en")) return "en-US";
  if (normalized?.startsWith("es")) return "es-ES";
  return "pt-BR";
}

export function translate(locale: Locale, key: TranslationKey): string {
  return messages[locale][key];
}

export function useI18n() {
  const context = React.useContext(I18nContext);
  if (!context) throw new Error("useI18n must be used within <I18nProvider>");
  return context;
}

function readLocale(): Locale {
  if (typeof window === "undefined") return "pt-BR";
  return normalizeLocale(
    localStorage.getItem(LOCALE_STORAGE_KEY) ?? navigator.language,
  );
}
