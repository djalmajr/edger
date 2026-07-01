const state = {
  apiKey: "",
  gateway: null,
  keys: [],
  modules: [],
  principal: null,
  workers: [],
};

const els = {
  alert: document.querySelector("#alert"),
  createdKey: document.querySelector("#createdKey"),
  gatewayContent: document.querySelector("#gatewayContent"),
  gatewayRequests: document.querySelector("#gatewayRequests"),
  keyForm: document.querySelector("#keyForm"),
  keysCount: document.querySelector("#keysCount"),
  keysTable: document.querySelector("#keysTable"),
  loginForm: document.querySelector("#loginForm"),
  modulesCount: document.querySelector("#modulesCount"),
  modulesTable: document.querySelector("#modulesTable"),
  overviewContent: document.querySelector("#overviewContent"),
  principalBadge: document.querySelector("#principalBadge"),
  refreshButton: document.querySelector("#refreshButton"),
  sessionLine: document.querySelector("#sessionLine"),
  workersCount: document.querySelector("#workersCount"),
  workersTable: document.querySelector("#workersTable"),
};

document.querySelectorAll(".nav-item").forEach((button) => {
  button.addEventListener("click", () => activateView(button.dataset.view));
});

els.loginForm.addEventListener("submit", async (event) => {
  event.preventDefault();
  const form = new FormData(els.loginForm);
  state.apiKey = String(form.get("apiKey") || "").trim();
  await refreshAll();
});

els.refreshButton.addEventListener("click", refreshAll);

els.keyForm.addEventListener("submit", async (event) => {
  event.preventDefault();
  const form = new FormData(els.keyForm);
  const payload = {
    expiresAt: null,
    name: String(form.get("name") || "").trim(),
    namespaces: splitCsv(String(form.get("namespaces") || "*")),
    permissions: splitCsv(String(form.get("permissions") || "")),
    role: String(form.get("role") || "viewer").trim(),
  };
  const result = await apiJson("/api/admin/keys", {
    body: JSON.stringify(payload),
    headers: { "content-type": "application/json" },
    method: "POST",
  });
  els.createdKey.hidden = false;
  els.createdKey.textContent = `Created ${result.key.name}: ${result.rawKey}`;
  els.keyForm.reset();
  await loadKeys();
  render();
});

function activateView(id) {
  document.querySelectorAll(".nav-item").forEach((button) => {
    button.classList.toggle("active", button.dataset.view === id);
  });
  document.querySelectorAll(".view").forEach((view) => {
    view.classList.toggle("active", view.id === id);
  });
}

async function refreshAll() {
  hideAlert();
  if (!state.apiKey) {
    showAlert("Enter a root API key to load operations data.");
    return;
  }
  try {
    await loadSession();
    await Promise.all([loadWorkers(), loadModules(), loadGateway(), loadKeys()]);
    els.refreshButton.disabled = false;
    render();
  } catch (error) {
    showAlert(error.message);
    render();
  }
}

async function loadSession() {
  const data = await apiJson("/api/admin/session");
  state.principal = data.principal;
}

async function loadWorkers() {
  const data = await apiJson("/api/admin/workers");
  state.workers = data.workers || [];
}

async function loadModules() {
  const data = await apiJson("/api/admin/extensions");
  state.modules = data.extensions || [];
}

async function loadGateway() {
  try {
    state.gateway = await apiJson("/api/admin/gateway/stats");
  } catch (error) {
    state.gateway = { error: error.message };
  }
}

async function loadKeys() {
  const data = await apiJson("/api/admin/keys");
  state.keys = data.keys || [];
}

async function apiJson(path, init = {}) {
  const headers = new Headers(init.headers || {});
  headers.set("x-api-key", state.apiKey);
  const response = await fetch(path, {
    ...init,
    headers,
  });
  const text = await response.text();
  const data = text ? JSON.parse(text) : {};
  if (!response.ok) {
    throw new Error(data.message || `${response.status} ${response.statusText}`);
  }
  return data;
}

function render() {
  renderSummary();
  renderOverview();
  renderWorkers();
  renderModules();
  renderGateway();
  renderKeys();
}

function renderSummary() {
  els.workersCount.textContent = state.workers.length || "-";
  els.modulesCount.textContent = state.modules.length || "-";
  els.keysCount.textContent = state.keys.length || "-";
  els.gatewayRequests.textContent = state.gateway?.requests?.total ?? "-";
  if (state.principal) {
    els.sessionLine.textContent = `${state.principal.name} · ${state.principal.role}`;
    els.principalBadge.textContent = state.principal.isRoot ? "root" : state.principal.role;
    els.principalBadge.className = `badge ${state.principal.isRoot ? "status-root" : ""}`;
  } else {
    els.sessionLine.textContent = "Waiting for operator key";
    els.principalBadge.textContent = "offline";
    els.principalBadge.className = "badge";
  }
}

function renderOverview() {
  const principal = state.principal || {};
  els.overviewContent.innerHTML = detailItems([
    ["Principal", principal.name || "-"],
    ["Role", principal.role || "-"],
    ["Namespaces", listText(principal.namespaces)],
    ["Permissions", listText(principal.permissions)],
    ["Gateway history", state.gateway?.history?.persistent?.enabled ? "persistent" : "local"],
    ["Remote deploy", "not included"],
  ]);
}

function renderWorkers() {
  els.workersTable.innerHTML = table(
    ["Name", "Version", "Kind", "Visibility", "Status", "Source"],
    state.workers.map((worker) => [
      code(worker.name),
      text(worker.version),
      text(kindLabel(worker.kind)),
      status(worker.visibility),
      status(worker.status),
      text(worker.source),
    ]),
  );
}

function renderModules() {
  els.modulesTable.innerHTML = table(
    ["Name", "Kind", "Status", "Capabilities", "Priority"],
    state.modules.map((mod) => [
      code(mod.name),
      text(kindLabel(mod.kind)),
      status(mod.status),
      listText(mod.capabilities),
      text(mod.priority),
    ]),
  );
}

function renderGateway() {
  if (state.gateway?.error) {
    els.gatewayContent.innerHTML = detailItems([["Status", state.gateway.error]]);
    return;
  }
  const requests = state.gateway?.requests || {};
  const rateLimit = state.gateway?.rateLimit || {};
  els.gatewayContent.innerHTML = detailItems([
    ["Total", requests.total ?? "-"],
    ["Continued", requests.continued ?? "-"],
    ["Redirected", requests.redirected ?? "-"],
    ["Rate limited", requests.rateLimited ?? "-"],
    ["Buckets", rateLimit.activeBuckets ?? "-"],
    ["History", state.gateway?.history?.persistent?.enabled ? "persistent" : "local"],
  ]);
}

function renderKeys() {
  els.keysTable.innerHTML = table(
    ["Name", "Role", "Prefix", "Namespaces", "Status", "Action"],
    state.keys.map((key) => [
      code(key.name),
      text(key.role),
      text(key.keyPrefix),
      listText(key.namespaces),
      status(key.isRoot ? "root" : key.revoked ? "revoked" : "active"),
      key.isRoot
        ? ""
        : `<div class="row-actions"><button data-revoke="${escapeHtml(String(key.id || ""))}" type="button">Revoke</button></div>`,
    ]),
  );
  els.keysTable.querySelectorAll("[data-revoke]").forEach((button) => {
    button.addEventListener("click", async () => {
      await apiJson(`/api/admin/keys/${button.dataset.revoke}/revoke`, { method: "POST" });
      await loadKeys();
      render();
    });
  });
}

function table(headers, rows) {
  if (!rows.length) {
    return '<div class="empty-state">No data loaded.</div>';
  }
  return `
    <table>
      <thead><tr>${headers.map((header) => `<th>${escapeHtml(header)}</th>`).join("")}</tr></thead>
      <tbody>${rows
        .map((row) => `<tr>${row.map((cell) => `<td>${cell}</td>`).join("")}</tr>`)
        .join("")}</tbody>
    </table>
  `;
}

function detailItems(items) {
  return items
    .map(([label, value]) => `<div class="detail-item"><span>${escapeHtml(label)}</span><strong>${escapeHtml(String(value))}</strong></div>`)
    .join("");
}

function status(value) {
  const text = String(value || "-");
  const className = text.toLowerCase().replace(/[^a-z0-9_-]/g, "-");
  return `<span class="status-${className}">${escapeHtml(text)}</span>`;
}

function code(value) {
  return `<code>${escapeHtml(String(value || "-"))}</code>`;
}

function text(value) {
  return escapeHtml(String(value || "-"));
}

function listText(value) {
  return text(Array.isArray(value) && value.length ? value.join(", ") : "-");
}

// ExecutionKind serializes unit variants as strings ("FetchHandler") and
// data-carrying variants as objects ({ StaticSpa: {...} }); surface the variant name.
function kindLabel(kind) {
  if (kind == null) return "-";
  if (typeof kind === "string") return kind;
  if (typeof kind === "object") {
    const keys = Object.keys(kind);
    return keys.length ? keys[0] : "-";
  }
  return String(kind);
}

function splitCsv(value) {
  return value
    .split(",")
    .map((item) => item.trim())
    .filter(Boolean);
}

function showAlert(message) {
  els.alert.hidden = false;
  els.alert.textContent = message;
}

function hideAlert() {
  els.alert.hidden = true;
  els.alert.textContent = "";
}

function escapeHtml(value) {
  return value
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;");
}
