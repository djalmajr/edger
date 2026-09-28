import { Button } from "@edger/ui/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuTrigger,
} from "@edger/ui/components/ui/dropdown-menu";
import { type ThemePreference, useTheme } from "@edger/ui/lib/theme";

import { type Locale, useI18n } from "../lib/i18n";
import { MonitorGlyph, MoonGlyph, SunGlyph } from "./glyphs";

const LOCALE_OPTIONS: Array<{
  flag: string;
  label: string;
  value: Locale;
}> = [
  { flag: "🇧🇷", label: "Português", value: "pt-BR" },
  { flag: "🇺🇸", label: "English", value: "en-US" },
  { flag: "🇪🇸", label: "Español", value: "es-ES" },
];

// Compact language/theme controls shared by the authenticated shell header
// and the login screen (top-right of the card, must not overflow on mobile).
export function LanguageMenu() {
  const { locale, setLocale, t } = useI18n();
  const selected =
    LOCALE_OPTIONS.find((option) => option.value === locale) ??
    LOCALE_OPTIONS[0];
  return (
    <DropdownMenu>
      <DropdownMenuTrigger
        render={
          <Button
            aria-label={t("preferences.language")}
            className="size-8"
            size="icon-sm"
            title={t("preferences.language")}
            variant="ghost"
          />
        }
      >
        <span aria-hidden className="text-xl leading-none">
          {selected.flag}
        </span>
      </DropdownMenuTrigger>
      <DropdownMenuContent className="min-w-40" side="bottom">
        <DropdownMenuRadioGroup
          onValueChange={(value) => setLocale(value as Locale)}
          value={locale}
        >
          {LOCALE_OPTIONS.map((option) => (
            <DropdownMenuRadioItem key={option.value} value={option.value}>
              <span aria-hidden className="text-base leading-none">
                {option.flag}
              </span>
              {option.label}
            </DropdownMenuRadioItem>
          ))}
        </DropdownMenuRadioGroup>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

export function ThemeMenu() {
  const { t } = useI18n();
  const { resolvedTheme, setTheme, theme } = useTheme();
  const options: Array<{ label: string; value: ThemePreference }> = [
    { label: t("preferences.theme.light"), value: "light" },
    { label: t("preferences.theme.dark"), value: "dark" },
    { label: t("preferences.theme.system"), value: "system" },
  ];
  const currentLabel =
    options.find((option) => option.value === theme)?.label ?? options[2].label;
  const ThemeIcon =
    theme === "system"
      ? MonitorGlyph
      : resolvedTheme === "dark"
        ? MoonGlyph
        : SunGlyph;
  const label = `${t("preferences.theme")}: ${currentLabel}`;
  return (
    <DropdownMenu>
      <DropdownMenuTrigger
        render={
          <Button
            aria-label={label}
            className="size-8"
            size="icon-sm"
            title={label}
            variant="ghost"
          />
        }
      >
        <ThemeIcon className="size-[1.125rem]" />
      </DropdownMenuTrigger>
      <DropdownMenuContent className="min-w-32" side="bottom">
        <DropdownMenuRadioGroup
          onValueChange={(value) => setTheme(value as ThemePreference)}
          value={theme}
        >
          {options.map((option) => (
            <DropdownMenuRadioItem key={option.value} value={option.value}>
              {option.label}
            </DropdownMenuRadioItem>
          ))}
        </DropdownMenuRadioGroup>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
