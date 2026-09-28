import { describe, expect, it } from "vitest";

import {
  appFullName,
  classifyVersions,
  draftFromPolicy,
  draftOutcome,
  isTenantSlug,
  parseRoutingPolicy,
  parseWeight,
  routingPolicyBlock,
  routingPolicyFromResponse,
  type VersionFacts,
} from "./routing-policy";

function version(
  versionName: string,
  overrides: Partial<VersionFacts> = {},
): VersionFacts {
  return {
    name: "shop",
    origin: "user",
    status: "loaded",
    version: versionName,
    visibility: "public",
    ...overrides,
  };
}

const versions = [version("2.0.0"), version("1.0.0")];

describe("routing policy names and eligibility", () => {
  it("uses the manifest name without prefixing namespace again", () => {
    expect(appFullName({ name: "@acme/shop" })).toBe("@acme/shop");
  });

  it("blocks core, reserved, and conflicting names", () => {
    expect(routingPolicyBlock([version("1.0.0")])).toBe("ok");
    expect(routingPolicyBlock([version("1.0.0", { name: "cpanel" })])).toBe(
      "core",
    );
    expect(routingPolicyBlock([version("1.0.0", { name: "webide" })])).toBe(
      "core",
    );
    expect(
      routingPolicyBlock([version("1.0.0", { origin: "core_bundled" })]),
    ).toBe("core");
    expect(
      routingPolicyBlock([
        version("1.0.0", { name: "shop" }),
        version("2.0.0", { name: "@acme/shop" }),
      ]),
    ).toBe("conflict");
    expect(routingPolicyBlock([version("1.0.0", { name: "@scope/cpanel" })])).toBe(
      "ok",
    );
  });

  it("offers only public enabled user versions", () => {
    const classified = classifyVersions([
      version("2.0.0"),
      version("1.9.0", { staged: true }),
      version("1.8.0", { visibility: "internal" }),
      version("1.7.0", { status: "disabled" }),
      version("1.6.0", { origin: "core_overlay" }),
      version("1.5.0", { origin: undefined }),
    ]);
    expect(classified.eligible.map((worker) => worker.version)).toEqual([
      "2.0.0",
    ]);
    expect(classified.excluded.map((item) => item.reason)).toEqual([
      "staged",
      "internal",
      "disabled",
      "core",
      "core",
    ]);
  });
});

describe("routing policy draft", () => {
  it("accepts a tenant slug and a positive weight", () => {
    expect(isTenantSlug("acme")).toBe(true);
    expect(isTenantSlug("acme-west-2")).toBe(true);
    expect(isTenantSlug("Acme")).toBe(false);
    expect(isTenantSlug(" acme")).toBe(false);
    expect(isTenantSlug("-acme")).toBe(false);
    expect(isTenantSlug("a".repeat(64))).toBe(false);
    expect(parseWeight("80")).toBe(80);
    expect(parseWeight("100")).toBe(100);
    expect(parseWeight("0")).toBeNull();
    expect(parseWeight("01")).toBeNull();
    expect(parseWeight("80.5")).toBeNull();
  });

  it("treats public without traffic as no policy", () => {
    expect(draftOutcome("shop", draftFromPolicy(null, versions)).kind).toBe(
      "absent",
    );
  });

  it("builds an allowlist without traffic and a public split", () => {
    const allowlist = draftOutcome("shop", {
      ...draftFromPolicy(null, versions),
      mode: "allowlist",
      tenants: ["acme", ""],
    });
    expect(allowlist).toEqual({
      kind: "ready",
      policy: {
        name: "shop",
        tenantAccess: { mode: "allowlist", tenants: ["acme"] },
      },
    });

    const split = draftOutcome("@acme/shop", {
      mode: "public",
      split: true,
      tenants: ["ignored"],
      weights: [
        { included: true, reason: null, version: "2.0.0", weight: "80" },
        { included: true, reason: null, version: "1.0.0", weight: "20" },
      ],
    });
    expect(split).toEqual({
      kind: "ready",
      policy: {
        name: "@acme/shop",
        tenantAccess: { mode: "public" },
        traffic: {
          versions: [
            { version: "2.0.0", weight: 80 },
            { version: "1.0.0", weight: 20 },
          ],
        },
      },
    });
    expect(JSON.stringify(split.kind === "ready" ? split.policy : null)).not.toContain(
      "ignored",
    );
  });

  it("rejects an empty allowlist, bad slugs, and weights that do not total 100", () => {
    const empty = draftOutcome("shop", {
      ...draftFromPolicy(null, versions),
      mode: "allowlist",
      tenants: ["", ""],
    });
    expect(empty.kind).toBe("invalid");
    const weights = draftOutcome("shop", {
      mode: "public",
      split: true,
      tenants: [""],
      weights: [
        { included: true, reason: null, version: "2.0.0", weight: "50" },
        { included: true, reason: null, version: "1.0.0", weight: "40" },
      ],
    });
    expect(weights).toMatchObject({
      kind: "invalid",
      issues: [{ code: "weight-sum", sum: 90 }],
    });
  });

  it("keeps a saved ineligible version visible so it is not dropped silently", () => {
    const draft = draftFromPolicy(
      {
        name: "shop",
        tenantAccess: { mode: "public" },
        traffic: {
          versions: [
            { version: "2.0.0", weight: 80 },
            { version: "9.9.9", weight: 20 },
          ],
        },
      },
      [version("2.0.0"), version("1.0.0", { staged: true })],
    );
    expect(draft.weights.map((row) => [row.version, row.reason])).toEqual([
      ["2.0.0", null],
      ["9.9.9", "missing"],
    ]);
    expect(draftOutcome("shop", draft).kind).toBe("invalid");
  });
});

describe("routing policy response", () => {
  it("reads a null policy and a document", () => {
    expect(routingPolicyFromResponse({ policy: null })).toBeNull();
    expect(
      parseRoutingPolicy({
        name: "shop",
        tenantAccess: { mode: "public", tenants: ["nope"] },
        traffic: { versions: [{ version: "1.0.0", weight: 100 }] },
      }),
    ).toEqual({
      name: "shop",
      tenantAccess: { mode: "public" },
      traffic: { versions: [{ version: "1.0.0", weight: 100 }] },
    });
  });

  it("rejects an envelope without policy", () => {
    expect(() => routingPolicyFromResponse({})).toThrow(
      /missing policy/,
    );
  });
});
