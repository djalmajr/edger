import {
  routingPolicyFromResponse,
  type RoutingPolicy,
} from "./routing-policy";

export type { RoutingPolicy };

// Transport error that carries the HTTP status. The cPanel maps statuses to
// fixed, translated copy instead of echoing server bodies — credential values
// must never surface in error text, URLs, or logs.
export class ApiError extends Error {
  readonly status: number;

  constructor(status: number, message: string) {
    super(message);
    this.name = "ApiError";
    this.status = status;
  }
}

// Session storage for the active credential: a `ses-` session token from
// password login or the legacy root/egk_/OIDC token entered directly.
export const SESSION_KEY = "edger.cpanel.apiKey";
export const SESSION_TOKEN_PREFIX = "ses-";

export function isSessionToken(token: string): boolean {
  return token.startsWith(SESSION_TOKEN_PREFIX);
}

export type LoginOptions = {
  passwordEnabled: boolean;
  rootSeeded: boolean;
};

export type Principal = {
  name?: string;
  namespaces?: string[];
  role?: string;
  isRoot?: boolean;
  permissions?: string[];
  workers?: string[];
};

// Espelho do catálogo do servidor (edger-core PERMISSION_CATALOG) para a UI
// de criação de keys — o servidor é a fonte da verdade e recusa o que não
// conhece; isto aqui só desenha os checkboxes.
export const PERMISSION_CATALOG = [
  "workers:read",
  "workers:install",
  "workers:delete",
  "workers:promote",
  "workers:toggle",
  "workers:invoke",
  "files:read",
  "files:write",
  "files:delete",
  "observability:read",
  "keys:manage",
] as const;

// Mirror of the server's principal_has_permission: the root principal holds
// everything, "*" grants everything, and any other permission must be
// present literally. The UI hides every action whose permission the logged
// principal lacks; the server still enforces each route.
export function can(principal: Principal, permission: string): boolean {
  if (principal.isRoot) return true;
  const permissions = principal.permissions ?? [];
  return permissions.includes("*") || permissions.includes(permission);
}

export function canManageKeys(principal: Principal): boolean {
  return can(principal, "keys:manage");
}

export type ApiKey = {
  id: number;
  name: string;
  keyPrefix: string;
  role: string;
  permissions: string[];
  namespaces: string[];
  workers: string[];
  createdAt: number;
  lastUsedAt?: number | null;
  expiresAt?: number | null;
  revokedAt?: number | null;
};

export type CreateKeyRequest = {
  name: string;
  permissions: string[];
  namespaces: string[];
  workers: string[];
  expiresAt?: number;
};

export type UpdateKeyPermissionsRequest = {
  permissions: string[];
};

export type CreatedKey = { key: ApiKey; rawKey: string };
export type Worker = {
  defaultVersion?: string | null;
  healthCheck?: {
    method?: string;
    mode?: string;
    path?: string;
    timeoutMs?: number;
  } | null;
  kind: unknown;
  name: string;
  namespace?: string | null;
  origin?: string;
  source?: string;
  staged?: boolean;
  status: string;
  visibility?: "public" | "internal";
  version: string;
};
export type RuntimeWorker = {
  activeProcesses?: number;
  health?: {
    failureCount?: number;
    observedAtMs?: number | null;
    sampleCount?: number;
    status?: string;
    successCount?: number;
    windowMs?: number;
  };
  idleProcesses?: number;
  maxProcesses?: number;
  name: string;
  queued?: number;
  rejectedTotal?: number;
  requestDurationMsLast?: number;
  requestDurationMsP95?: number;
  requestTotal?: number;
  state?: string;
  terminatingProcesses?: number;
  timeoutTotal?: number;
  totalProcesses?: number;
  uptimeSeconds?: number;
  version: string;
  waitMs?: number;
  waitMsP95?: number;
};
export type RuntimePool = {
  activeRequests?: number;
  activeWorkers?: number;
  cacheHits?: number;
  cacheMisses?: number;
  ephemeralInflight?: number;
  ephemeralQueued?: number;
  ephemeralRejected?: number;
  idleWorkers?: number;
  requestDurationMsLast?: number;
  spawnLatencyMsLast?: number;
  spawnLatencyMsP50?: number;
  terminatedTotal?: number;
  totalWorkers?: number;
};
export type RuntimeData = {
  metricsStats: {
    pool?: RuntimePool;
    workers?: RuntimeWorker[];
  } | null;
  principal: Principal;
  tenantRoutingEnabled?: boolean;
  workerErrors: Record<string, { count?: number; latest?: { code?: string } }>;
  weightedRoutingEnabled?: boolean;
  workers: Worker[];
};

export type OperationalEvent = {
  atMs?: number;
  code?: string;
  droppedCount?: number;
  durationMs?: number | null;
  id?: number | string;
  kind?: string;
  level?: string;
  message?: string;
  namespace?: string;
  outcome?: string;
  processId?: string;
  requestId?: string;
  source?: string;
  status?: number | string;
  traceId?: string;
  truncated?: boolean;
  version?: string;
  worker?: string;
};

// The runtime serves this worker under the injected <base href> — behind a
// stripping proxy that is e.g. "/apps/cpanel/", not "/cpanel/". Absolute
// paths would escape the proxy prefix and land on whatever owns "/" out
// there, so the SPA derives everything from the base: its own mount for the
// router, and the base's PARENT as the runtime root for admin/metrics calls.
// Guarded for non-DOM test runs — the SPA always has a document.
const baseURI = typeof document === "undefined" ? "http://localhost/" : document.baseURI;
export const workerBasePath = new URL(baseURI).pathname.replace(/\/+$/, "");
const runtimeRoot = new URL("..", baseURI);

export function runtimeUrl(path: string): string {
  return new URL(path.replace(/^\/+/, ""), runtimeRoot).toString();
}

export async function apiJson<T>(
  apiKey: string,
  path: string,
  init: RequestInit = {},
): Promise<T> {
  const headers = new Headers(init.headers);
  headers.set("x-api-key", apiKey);
  const response = await fetch(runtimeUrl(path), { ...init, headers });
  const text = await response.text();
  const data = text ? (JSON.parse(text) as unknown) : {};
  if (!response.ok) {
    const message =
      typeof data === "object" &&
      data !== null &&
      "message" in data &&
      typeof data.message === "string"
        ? data.message
        : `${response.status} ${response.statusText}`;
    throw new ApiError(response.status, message);
  }
  return data as T;
}

export async function apiDownload(
  apiKey: string,
  path: string,
): Promise<{ blob: Blob; filename: string }> {
  const response = await fetch(runtimeUrl(path), { headers: { "x-api-key": apiKey } });
  if (!response.ok) {
    const data = (await response.json().catch(() => ({}))) as {
      message?: string;
    };
    throw new ApiError(
      response.status,
      data.message ?? `${response.status} ${response.statusText}`,
    );
  }
  const disposition = response.headers.get("content-disposition") ?? "";
  const filename = disposition.match(/filename="([^"]+)"/)?.[1] ?? "download";
  return { blob: await response.blob(), filename };
}

export async function loadAll(apiKey: string): Promise<RuntimeData> {
  const session = await apiJson<{
    principal: Principal;
    tenantRoutingEnabled?: unknown;
    weightedRoutingEnabled?: unknown;
  }>(
    apiKey,
    "/api/admin/session",
  );
  const [workers, workerErrors, metricsStats] = await Promise.all([
    apiJson<{ workers: Worker[] }>(apiKey, "/api/admin/workers").then(
      (data) => data.workers ?? [],
    ),
    apiJson<{ summary: RuntimeData["workerErrors"] }>(
      apiKey,
      "/api/admin/workers/error-summary",
    )
      .then((data) => data.summary ?? {})
      .catch(() => ({})),
    apiJson<NonNullable<RuntimeData["metricsStats"]>>(
      apiKey,
      "/metrics/stats",
    ).catch(() => null),
  ]);
  return {
    metricsStats,
    principal: session.principal,
    tenantRoutingEnabled: session.tenantRoutingEnabled === true,
    workerErrors,
    weightedRoutingEnabled: session.weightedRoutingEnabled === true,
    workers,
  };
}

export function kindLabel(kind: unknown) {
  if (kind == null) return "-";
  if (typeof kind === "string") return kind;
  if (typeof kind === "object") return Object.keys(kind)[0] ?? "-";
  return String(kind);
}

export function workerUrl(worker: Worker, latest = false) {
  const scoped = worker.namespace
    ? `@${worker.namespace}/${worker.name}`
    : worker.name;
  // Workers live at the runtime root — which behind a stripping proxy is
  // the base's parent, not "/".
  return runtimeUrl(latest ? scoped : `${scoped}@${worker.version}`);
}

export function routingPolicyPath(name: string): string {
  return `/api/admin/routing-policy?name=${encodeURIComponent(name)}`;
}

export async function getRoutingPolicy(
  apiKey: string,
  name: string,
): Promise<RoutingPolicy | null> {
  return routingPolicyFromResponse(
    await apiJson<unknown>(apiKey, routingPolicyPath(name)),
  );
}

export async function putRoutingPolicy(
  apiKey: string,
  policy: RoutingPolicy,
): Promise<RoutingPolicy> {
  const saved = routingPolicyFromResponse(
    await apiJson<unknown>(apiKey, routingPolicyPath(policy.name), {
      body: JSON.stringify(policy),
      headers: { "content-type": "application/json" },
      method: "PUT",
    }),
  );
  if (!saved) throw new Error("routing policy response is missing policy");
  return saved;
}

export async function deleteRoutingPolicy(
  apiKey: string,
  name: string,
): Promise<{ deleted: true }> {
  const data = await apiJson<unknown>(apiKey, routingPolicyPath(name), {
    method: "DELETE",
  });
  if (
    typeof data !== "object" ||
    data === null ||
    !("deleted" in data) ||
    (data as { deleted?: unknown }).deleted !== true
  ) {
    throw new Error("routing policy response is missing deleted");
  }
  return { deleted: true };
}

// Console credential endpoints. They follow the same <base> discipline as
// the rest of the admin API (runtimeUrl): resolved against the base's parent,
// never against the SPA prefix behind the stripping proxy.

// Public on purpose: the login screen has no credential yet. The server must
// answer without one; a missing, failing, or malformed response means "the
// UI cannot confirm what is enabled" and falls back to the honest token copy
// instead of claiming a seeded root.
export async function loginOptions(): Promise<LoginOptions | null> {
  let response: Response;
  try {
    response = await fetch(runtimeUrl("/api/admin/login-options"));
  } catch {
    return null;
  }
  if (!response.ok) return null;
  try {
    const data = (await response.json()) as unknown;
    // The whole response must be well-formed; anything partial or odd-shaped
    // is rejected as unknown rather than partially trusted.
    if (
      typeof data === "object" &&
      data !== null &&
      typeof (data as LoginOptions).passwordEnabled === "boolean" &&
      typeof (data as LoginOptions).rootSeeded === "boolean"
    ) {
      return data as LoginOptions;
    }
  } catch {
    // non-JSON body: unknown
  }
  return null;
}

// POST /api/admin/login {username,password} -> {token:"ses-..."}. Failure is
// a bare ApiError with the status; the request body and any server echo are
// never kept in the error, so nothing credential-shaped reaches the UI.
export async function login(
  username: string,
  password: string,
): Promise<string> {
  const response = await fetch(runtimeUrl("/api/admin/login"), {
    body: JSON.stringify({ username, password }),
    headers: { "content-type": "application/json" },
    method: "POST",
  });
  if (!response.ok) throw new ApiError(response.status, "login failed");
  const data = (await response.json().catch(() => null)) as {
    token?: unknown;
  } | null;
  if (typeof data?.token !== "string" || data.token.length === 0) {
    throw new Error("login response is missing token");
  }
  return data.token;
}

// POST /api/admin/logout revokes only `ses-` sessions; the endpoint accepts
// the session via the x-api-key header this SPA already uses.
export async function adminLogout(token: string, signal?: AbortSignal): Promise<void> {
  const response = await fetch(runtimeUrl("/api/admin/logout"), {
    headers: { "x-api-key": token },
    method: "POST",
    ...(signal ? { signal } : {}),
  });
  if (!response.ok) throw new ApiError(response.status, "logout failed");
}

// Hard bound on the best-effort logout revocation: the local session is gone
// before any network I/O, so a network that never answers must still end the
// logout. Short on purpose — one small request, already signed out locally.
export const LOGOUT_TIMEOUT_MS = 2000;

// Logout is best-effort revocation: the local session is removed FIRST, and
// only `ses-` sessions attempt the endpoint afterwards, raced against a short
// timer — a logout that never answers still ends (the timer resolves and
// aborts the fetch), and a healthy network still revokes remotely. Non-session
// credentials (root/egk_/OIDC) are never revoked through this endpoint, only
// cleared locally.
export function clearSession(token: string): Promise<void> {
  sessionStorage.removeItem(SESSION_KEY);
  if (!isSessionToken(token)) return Promise.resolve();
  return new Promise<void>((resolve) => {
    const controller = new AbortController();
    const timer = setTimeout(() => {
      controller.abort();
      resolve();
    }, LOGOUT_TIMEOUT_MS);
    adminLogout(token, controller.signal)
      .catch(() => undefined)
      .finally(() => {
        clearTimeout(timer);
        resolve();
      });
  });
}

export function compareSemver(a: string, b: string) {
  const left = a.split(".").map((part) => Number.parseInt(part, 10) || 0);
  const right = b.split(".").map((part) => Number.parseInt(part, 10) || 0);
  for (let index = 0; index < 3; index += 1)
    if ((left[index] ?? 0) !== (right[index] ?? 0))
      return (left[index] ?? 0) - (right[index] ?? 0);
  return 0;
}

// Console user management (story 26.04). Every route is root-only on the
// server; the UI just hides what the principal cannot do. Passwords ride the
// request body only — never URLs, errors, or logs.

export type AdminUser = {
  createdAt: number;
  disabled: boolean;
  id: number;
  isRoot: boolean;
  namespaces: string[];
  permissions: string[];
  role: string;
  username: string;
  workers: string[];
};

export type CreateUserRequest = {
  namespaces: string[];
  password: string;
  permissions: string[];
  username: string;
  workers: string[];
};

export type UpdateUserRequest = {
  disabled?: boolean;
  namespaces?: string[];
  permissions?: string[];
  workers?: string[];
};

function isStringArray(value: unknown): value is string[] {
  return Array.isArray(value) && value.every((entry) => typeof entry === "string");
}

// A user record must be fully well-formed before the UI draws from it — the
// same reject-the-whole policy as loginOptions: a partial principal must
// never drive an action.
function parseAdminUser(value: unknown): AdminUser {
  if (typeof value !== "object" || value === null)
    throw new Error("user record is malformed");
  const user = value as Record<string, unknown>;
  if (
    typeof user.id !== "number" ||
    typeof user.username !== "string" ||
    typeof user.role !== "string" ||
    typeof user.isRoot !== "boolean" ||
    typeof user.disabled !== "boolean" ||
    typeof user.createdAt !== "number" ||
    !isStringArray(user.permissions) ||
    !isStringArray(user.namespaces) ||
    !isStringArray(user.workers)
  )
    throw new Error("user record is malformed");
  return {
    createdAt: user.createdAt,
    disabled: user.disabled,
    id: user.id,
    isRoot: user.isRoot,
    namespaces: user.namespaces,
    permissions: user.permissions,
    role: user.role,
    username: user.username,
    workers: user.workers,
  };
}

// Accepts the user record either wrapped as `{ user }` or bare, and rejects
// anything else — the list and the mutation responses must stay trustworthy.
function extractUser(data: unknown): AdminUser {
  if (typeof data !== "object" || data === null)
    throw new Error("user response is malformed");
  const record = data as Record<string, unknown>;
  const candidate =
    typeof record.user === "object" && record.user !== null ? record.user : record;
  return parseAdminUser(candidate);
}

export async function listUsers(apiKey: string): Promise<AdminUser[]> {
  const data = await apiJson<unknown>(apiKey, "/api/admin/users");
  if (
    typeof data !== "object" ||
    data === null ||
    !Array.isArray((data as { users?: unknown }).users)
  )
    throw new Error("users response is malformed");
  return (data as { users: unknown[] }).users.map(parseAdminUser);
}

export async function createUser(
  apiKey: string,
  request: CreateUserRequest,
): Promise<AdminUser> {
  const data = await apiJson<unknown>(apiKey, "/api/admin/users", {
    body: JSON.stringify(request),
    headers: { "content-type": "application/json" },
    method: "POST",
  });
  return extractUser(data);
}

export async function updateUser(
  apiKey: string,
  id: number,
  request: UpdateUserRequest,
): Promise<AdminUser> {
  const data = await apiJson<unknown>(apiKey, `/api/admin/users/${id}`, {
    body: JSON.stringify(request),
    headers: { "content-type": "application/json" },
    method: "PATCH",
  });
  return extractUser(data);
}

export async function resetUserPassword(
  apiKey: string,
  id: number,
  password: string,
): Promise<void> {
  await apiJson(apiKey, `/api/admin/users/${id}/reset-password`, {
    body: JSON.stringify({ password }),
    headers: { "content-type": "application/json" },
    method: "POST",
  });
}

export async function deleteUser(apiKey: string, id: number): Promise<void> {
  await apiJson(apiKey, `/api/admin/users/${id}`, { method: "DELETE" });
}

// POST /api/admin/me/password — {current, new} for `ses-` session
// credentials only. A success answers either with the rotated session token
// ({"token":"ses-…"}) or without one (the runtime requires a fresh login);
// both are honest outcomes and never leak either password. Failure is a bare
// ApiError with the status: the request body and any server echo are not kept
// in the error.
export async function changeMyPassword(
  token: string,
  current: string,
  next: string,
): Promise<string | null> {
  const response = await fetch(runtimeUrl("/api/admin/me/password"), {
    body: JSON.stringify({ current, new: next }),
    headers: {
      "content-type": "application/json",
      "x-api-key": token,
    },
    method: "POST",
  });
  if (!response.ok) throw new ApiError(response.status, "password change failed");
  const data = (await response.json().catch(() => null)) as {
    token?: unknown;
  } | null;
  if (data === null) return null;
  if (typeof data.token === "string" && data.token.length > 0) return data.token;
  return null;
}

// Mirror of the server's strong-password policy (console_auth
// validate_password_policy): 12–128 characters with at least one letter, one
// digit and one symbol. The server validates; this only keeps the form honest
// before submit, so the copy and the rule stay aligned.
export type PasswordPolicyIssue = "length" | "letter" | "digit" | "symbol";

export function passwordPolicyIssues(password: string): PasswordPolicyIssue[] {
  const chars = [...password];
  const issues: PasswordPolicyIssue[] = [];
  if (chars.length < 12 || chars.length > 128) issues.push("length");
  if (!chars.some((char) => /[\p{L}]/u.test(char))) issues.push("letter");
  if (!chars.some((char) => /\d/.test(char))) issues.push("digit");
  if (!chars.some((char) => /[^\p{L}\p{N}\s]/u.test(char))) issues.push("symbol");
  return issues;
}

// Mirror of the story's username rule: [a-z0-9._-], 2–32 characters, no
// leading punctuation (no uppercase: the slug screen does not fold it either,
// and the server normalizes to lowercase). The server is the real barrier.
export function isValidUsername(username: string): boolean {
  return /^[a-z0-9][a-z0-9._-]{1,31}$/.test(username);
}
