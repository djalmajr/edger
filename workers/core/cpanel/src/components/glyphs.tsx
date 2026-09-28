import * as React from "react";

// Inline lucide glyphs, geometry copied 1:1 from the @iconify-json/lucide
// 1.2.117 data the shared @edger/ui barrel consumes.
//
// The barrel itself cannot be imported from files that join the login
// screen's test graph: the bun test runner has no Vite icon plugin, and the
// barrel's export surface is frozen process-wide by whichever sibling test
// file loads it first (module cache). Keeping these glyphs local means the
// login screen and the preference menus never touch the barrel, so their
// DOM tests run under both vitest and bun.
type GlyphProps = { className?: string };

function createGlyph(children: React.ReactNode) {
  return function Glyph({ className }: GlyphProps) {
    return (
      <svg
        aria-hidden
        className={className}
        fill="none"
        height={24}
        stroke="currentColor"
        strokeLinecap="round"
        strokeLinejoin="round"
        strokeWidth={2}
        viewBox="0 0 24 24"
        width={24}
      >
        {children}
      </svg>
    );
  };
}

export const UsersGlyph = createGlyph(
  <>
    <path d="M16 21v-2a4 4 0 0 0-4-4H6a4 4 0 0 0-4 4v2" />
    <path d="M16 3.128a4 4 0 0 1 0 7.744" />
    <path d="M22 21v-2a4 4 0 0 0-3-3.87" />
    <circle cx={9} cy={7} r={4} />
  </>,
);

export const EyeGlyph = createGlyph(
  <>
    <path d="M2.062 12.348a1 1 0 0 1 0-.696a10.75 10.75 0 0 1 19.876 0a1 1 0 0 1 0 .696a10.75 10.75 0 0 1-19.876 0" />
    <circle cx={12} cy={12} r={3} />
  </>,
);

export const EyeOffGlyph = createGlyph(
  <>
    <path d="M10.733 5.076a10.744 10.744 0 0 1 11.205 6.575a1 1 0 0 1 0 .696a10.8 10.8 0 0 1-1.444 2.49m-6.41-.679a3 3 0 0 1-4.242-4.242" />
    <path d="M17.479 17.499a10.75 10.75 0 0 1-15.417-5.151a1 1 0 0 1 0-.696a10.75 10.75 0 0 1 4.446-5.143M2 2l20 20" />
  </>,
);

export const LogInGlyph = createGlyph(
  <path d="m10 17l5-5l-5-5m5 5H3m12-9h4a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2h-4" />,
);

export const MonitorGlyph = createGlyph(
  <>
    <rect height={14} width={20} x={2} y={3} rx={2} />
    <path d="M8 21h8m-4-4v4" />
  </>,
);

export const MoonGlyph = createGlyph(
  <path d="M20.985 12.486a9 9 0 1 1-9.473-9.472c.405-.022.617.46.402.803a6 6 0 0 0 8.268 8.268c.344-.215.825-.004.803.401" />,
);

export const SunGlyph = createGlyph(
  <>
    <circle cx={12} cy={12} r={4} />
    <path d="M12 2v2m0 16v2M4.93 4.93l1.41 1.41m11.32 11.32l1.41 1.41M2 12h2m16 0h2M6.34 17.66l-1.41 1.41M19.07 4.93l1.41 1.41" />
  </>,
);
