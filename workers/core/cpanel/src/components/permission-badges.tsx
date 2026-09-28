import { Badge } from "@edger/ui/components/ui/badge";
import * as React from "react";
import { useI18n } from "../lib/i18n";

type PermissionLayout = {
  lines: number[];
  overflowLine?: number;
  visibleCount: number;
};

function pack(
  widths: number[],
  width: number,
  gap: number,
): { fits: boolean; lines: number[] } {
  let line = 1;
  let used = 0;
  const lines: number[] = [];
  for (const itemWidth of widths) {
    if (used > 0 && used + gap + itemWidth > width) {
      line += 1;
      used = 0;
    }
    lines.push(line);
    used += (used > 0 ? gap : 0) + itemWidth;
  }
  return { fits: line <= 2, lines };
}

function chooseLayout(
  permissionWidths: number[],
  overflowWidths: number[],
  width: number,
  gap: number,
): PermissionLayout {
  for (let visibleCount = permissionWidths.length; visibleCount >= 0; visibleCount -= 1) {
    const hiddenCount = permissionWidths.length - visibleCount;
    const widths = permissionWidths.slice(0, visibleCount);
    if (hiddenCount > 0) widths.push(overflowWidths[hiddenCount - 1] ?? 0);
    const result = pack(widths, width, gap);
    if (result.fits) {
      return {
        lines: result.lines.slice(0, visibleCount),
        overflowLine: hiddenCount > 0 ? result.lines.at(-1) : undefined,
        visibleCount,
      };
    }
  }
  return { lines: [], visibleCount: 0 };
}

export function PermissionBadges({
  permissions,
}: {
  permissions: string[];
}) {
  const { t } = useI18n();
  const containerRef = React.useRef<HTMLDivElement>(null);
  const probeRef = React.useRef<HTMLDivElement>(null);
  const [layout, setLayout] = React.useState<PermissionLayout | null>(null);

  const measure = React.useCallback(() => {
    const container = containerRef.current;
    const probe = probeRef.current;
    if (!container || !probe) return;

    const width = container.getBoundingClientRect().width;
    if (width <= 0) {
      setLayout(null);
      return;
    }
    const permissionWidths = permissions.map((_, index) =>
      probe
        .querySelector<HTMLElement>(`[data-permission-measure="${index}"]`)
        ?.getBoundingClientRect().width ?? 0,
    );
    const overflowWidths = permissions.map((_, index) =>
      probe
        .querySelector<HTMLElement>(`[data-overflow-measure="${index + 1}"]`)
        ?.getBoundingClientRect().width ?? 0,
    );
    const gap =
      Number.parseFloat(getComputedStyle(container).columnGap) || 0;
    setLayout(chooseLayout(permissionWidths, overflowWidths, width, gap));
  }, [permissions]);

  React.useLayoutEffect(() => {
    measure();
  }, [measure]);

  React.useEffect(() => {
    const container = containerRef.current;
    if (!container) return;
    const observer =
      typeof ResizeObserver === "undefined"
        ? null
        : new ResizeObserver(measure);
    observer?.observe(container);
    window.addEventListener("resize", measure);
    return () => {
      observer?.disconnect();
      window.removeEventListener("resize", measure);
    };
  }, [measure]);

  const visibleCount = layout?.visibleCount ?? 0;
  const hiddenPermissions = permissions.slice(visibleCount);
  const accessibleNames = permissions.join(", ");
  const ariaLabel = t("keys.permissions.aria").replace(
    "{permissions}",
    accessibleNames,
  );

  return (
    <>
      <div
        aria-label={ariaLabel}
        className="flex w-full min-w-0 max-w-64 flex-wrap content-start gap-1"
        data-permission-badges
        role="list"
        ref={containerRef}
        style={{ visibility: layout ? "visible" : "hidden" }}
      >
        {permissions.slice(0, visibleCount).map((permission, index) => (
          <Badge
            data-line={layout?.lines[index]}
            key={permission}
            role="listitem"
            variant="secondary"
          >
            {permission}
          </Badge>
        ))}
        {hiddenPermissions.length > 0 && (
          <Badge
            aria-label={t("keys.permissions.hidden")
              .replace("{count}", String(hiddenPermissions.length))
              .replace("{permissions}", hiddenPermissions.join(", "))}
            data-line={layout?.overflowLine}
            role="listitem"
            title={hiddenPermissions.join(", ")}
            variant="secondary"
          >
            +{hiddenPermissions.length}
          </Badge>
        )}
      </div>
      <span className="sr-only">
        {t("keys.permissions.all").replace("{permissions}", accessibleNames)}
      </span>
      <div
        aria-hidden="true"
        className="pointer-events-none invisible absolute -left-[10000px] flex w-max flex-col gap-1"
        ref={probeRef}
      >
        {permissions.map((permission, index) => (
          <Badge
            data-permission-measure={index}
            key={permission}
            variant="secondary"
          >
            {permission}
          </Badge>
        ))}
        {permissions.map((_, index) => (
          <Badge
            data-overflow-measure={index + 1}
            key={`overflow-${index + 1}`}
            variant="secondary"
          >
            +{index + 1}
          </Badge>
        ))}
      </div>
    </>
  );
}
