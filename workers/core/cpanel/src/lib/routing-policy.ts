// Admin `name` is already the manifest name (`app` or `@scope/name`).
// `namespace` is a separate field (`@scope`) and must not be prefixed again.
export function appFullName(worker: { name: string }): string {
  return worker.name;
}

const RESERVED_APP_NAMES = new Set(["cpanel", "webide"]);
const MAX_TRAFFIC_VERSIONS = 8;
const MAX_TENANT_SLUG_CHARS = 63;
const TENANT_SLUG = /^[a-z0-9]+(?:-[a-z0-9]+)*$/;

export type TenantAccess =
  | { mode: "public" }
  | { mode: "allowlist"; tenants: string[] };

export type RoutingPolicy = {
  name: string;
  tenantAccess: TenantAccess;
  traffic?: { versions: { version: string; weight: number }[] };
};

export type VersionFacts = {
  name: string;
  origin?: string;
  staged?: boolean;
  status: string;
  version: string;
  visibility?: "internal" | "public";
};

export type ExclusionReason = "core" | "disabled" | "internal" | "staged";
export type RowReason = ExclusionReason | "missing";

export type WeightRow = {
  included: boolean;
  reason: RowReason | null;
  version: string;
  weight: string;
};

export type RoutingDraft = {
  mode: "public" | "allowlist";
  split: boolean;
  tenants: string[];
  weights: WeightRow[];
};

export type DraftIssue =
  | { code: "no-eligible" }
  | { code: "tenant-duplicate"; slug: string }
  | { code: "tenant-invalid"; slug: string }
  | { code: "tenant-required" }
  | { code: "version-ineligible"; version: string }
  | { code: "weight-count" }
  | { code: "weight-invalid"; version: string }
  | { code: "weight-sum"; sum: number };

export type DraftOutcome =
  | { kind: "absent" }
  | { issues: DraftIssue[]; kind: "invalid" }
  | { kind: "ready"; policy: RoutingPolicy };

export function isTenantSlug(slug: string): boolean {
  return [...slug].length <= MAX_TENANT_SLUG_CHARS && TENANT_SLUG.test(slug);
}

export function parseWeight(raw: string): number | null {
  if (!/^(?:[1-9]|[1-9][0-9]|100)$/.test(raw)) return null;
  return Number(raw);
}

// Only an explicit user origin is eligible. Missing origin is not offered.
export function versionExclusion(worker: VersionFacts): ExclusionReason | null {
  if (worker.origin !== "user") return "core";
  if (worker.staged === true) return "staged";
  if (worker.visibility === "internal") return "internal";
  if (worker.status === "disabled") return "disabled";
  return null;
}

export function routingPolicyBlock(
  versions: readonly { name: string; origin?: string }[],
): "conflict" | "core" | "ok" {
  if (versions.length === 0) return "conflict";
  const name = versions[0].name;
  if (versions.some((version) => version.name !== name)) return "conflict";
  if (RESERVED_APP_NAMES.has(name)) return "core";
  if (!versions.some((version) => version.origin === "user")) return "core";
  return "ok";
}

export function classifyVersions(versions: readonly VersionFacts[]): {
  eligible: VersionFacts[];
  excluded: { reason: ExclusionReason; version: string }[];
} {
  const eligible: VersionFacts[] = [];
  const excluded: { reason: ExclusionReason; version: string }[] = [];
  const seen = new Set<string>();
  for (const worker of versions) {
    if (seen.has(worker.version)) continue;
    seen.add(worker.version);
    const reason = versionExclusion(worker);
    if (reason) excluded.push({ reason, version: worker.version });
    else eligible.push(worker);
  }
  return { eligible, excluded };
}

export function weightRows(
  versions: readonly VersionFacts[],
  policy: RoutingPolicy | null,
): WeightRow[] {
  const selected = new Map<string, number>();
  for (const entry of policy?.traffic?.versions ?? []) {
    if (!selected.has(entry.version)) selected.set(entry.version, entry.weight);
  }
  const rows: WeightRow[] = [];
  const seen = new Set<string>();
  for (const worker of versions) {
    if (seen.has(worker.version)) continue;
    seen.add(worker.version);
    const reason = versionExclusion(worker);
    if (reason && !selected.has(worker.version)) continue;
    rows.push({
      included: selected.has(worker.version),
      reason,
      version: worker.version,
      weight: selected.has(worker.version)
        ? String(selected.get(worker.version))
        : "",
    });
  }
  for (const [version, weight] of selected) {
    if (seen.has(version)) continue;
    rows.push({
      included: true,
      reason: "missing",
      version,
      weight: String(weight),
    });
  }
  return rows;
}

export function draftFromPolicy(
  policy: RoutingPolicy | null,
  versions: readonly VersionFacts[],
): RoutingDraft {
  const tenants =
    policy?.tenantAccess.mode === "allowlist" &&
    policy.tenantAccess.tenants.length > 0
      ? [...policy.tenantAccess.tenants]
      : [""];
  return {
    mode: policy?.tenantAccess.mode ?? "public",
    split: policy?.traffic !== undefined,
    tenants,
    weights: weightRows(versions, policy),
  };
}

export function draftOutcome(name: string, draft: RoutingDraft): DraftOutcome {
  if (draft.mode === "public" && !draft.split) return { kind: "absent" };
  const issues = collectIssues(draft);
  if (issues.length > 0) return { issues, kind: "invalid" };
  return { kind: "ready", policy: toPolicy(name, draft) };
}

export function includedWeightSum(draft: RoutingDraft): number | null {
  const included = draft.weights.filter((row) => row.included);
  if (included.length === 0) return 0;
  let sum = 0;
  for (const row of included) {
    const weight = parseWeight(row.weight);
    if (weight === null) return null;
    sum += weight;
  }
  return sum;
}

function collectIssues(draft: RoutingDraft): DraftIssue[] {
  const issues: DraftIssue[] = [];
  if (draft.mode === "allowlist") {
    const filled = draft.tenants.filter((slug) => slug !== "");
    if (filled.length === 0) issues.push({ code: "tenant-required" });
    const counts = new Map<string, number>();
    for (const slug of filled) counts.set(slug, (counts.get(slug) ?? 0) + 1);
    for (const [slug, count] of counts) {
      if (!isTenantSlug(slug)) issues.push({ code: "tenant-invalid", slug });
      if (count > 1) issues.push({ code: "tenant-duplicate", slug });
    }
  }
  if (!draft.split) return issues;
  const hasEligible = draft.weights.some((row) => row.reason === null);
  const included = draft.weights.filter((row) => row.included);
  if (!hasEligible) issues.push({ code: "no-eligible" });
  else if (
    included.length < 1 ||
    included.length > MAX_TRAFFIC_VERSIONS
  ) {
    issues.push({ code: "weight-count" });
  }
  const parsed: number[] = [];
  let weightsValid = true;
  for (const row of included) {
    if (row.reason) {
      issues.push({ code: "version-ineligible", version: row.version });
    }
    const weight = parseWeight(row.weight);
    if (weight === null) {
      weightsValid = false;
      issues.push({ code: "weight-invalid", version: row.version });
    } else parsed.push(weight);
  }
  if (weightsValid && included.length > 0) {
    const sum = parsed.reduce((total, weight) => total + weight, 0);
    if (sum !== 100) issues.push({ code: "weight-sum", sum });
  }
  return issues;
}

function toPolicy(name: string, draft: RoutingDraft): RoutingPolicy {
  const tenantAccess: TenantAccess =
    draft.mode === "allowlist"
      ? {
          mode: "allowlist",
          tenants: draft.tenants.filter((slug) => slug !== ""),
        }
      : { mode: "public" };
  const policy: RoutingPolicy = { name, tenantAccess };
  if (draft.split) {
    policy.traffic = {
      versions: draft.weights
        .filter((row) => row.included)
        .map((row) => {
          const weight = parseWeight(row.weight);
          if (weight === null) throw new Error("invalid routing draft");
          return { version: row.version, weight };
        }),
    };
  }
  return policy;
}

export function parseRoutingPolicy(value: unknown): RoutingPolicy | null {
  if (typeof value !== "object" || value === null) return null;
  const record = value as Record<string, unknown>;
  if (typeof record.name !== "string" || record.name.length === 0) return null;
  const tenantAccess = parseTenantAccess(record.tenantAccess);
  if (!tenantAccess) return null;
  const policy: RoutingPolicy = { name: record.name, tenantAccess };
  if (record.traffic === undefined) return policy;
  const traffic = parseTraffic(record.traffic);
  if (!traffic) return null;
  policy.traffic = traffic;
  return policy;
}

export function routingPolicyFromResponse(data: unknown): RoutingPolicy | null {
  if (typeof data !== "object" || data === null || !("policy" in data)) {
    throw new Error("routing policy response is missing policy");
  }
  const policy = (data as { policy: unknown }).policy;
  if (policy === null) return null;
  const parsed = parseRoutingPolicy(policy);
  if (!parsed) throw new Error("routing policy response is invalid");
  return parsed;
}

function parseTenantAccess(value: unknown): TenantAccess | null {
  if (typeof value !== "object" || value === null) return null;
  const record = value as Record<string, unknown>;
  if (record.mode === "public") return { mode: "public" };
  if (
    record.mode === "allowlist" &&
    Array.isArray(record.tenants) &&
    record.tenants.every((slug) => typeof slug === "string")
  ) {
    return { mode: "allowlist", tenants: record.tenants };
  }
  return null;
}

function parseTraffic(
  value: unknown,
): { versions: { version: string; weight: number }[] } | null {
  if (typeof value !== "object" || value === null) return null;
  const versions = (value as { versions?: unknown }).versions;
  if (!Array.isArray(versions)) return null;
  const parsed: { version: string; weight: number }[] = [];
  for (const entry of versions) {
    if (typeof entry !== "object" || entry === null) return null;
    const version = (entry as { version?: unknown }).version;
    const weight = (entry as { weight?: unknown }).weight;
    if (
      typeof version !== "string" ||
      typeof weight !== "number" ||
      !Number.isFinite(weight)
    ) {
      return null;
    }
    parsed.push({ version, weight });
  }
  return { versions: parsed };
}
