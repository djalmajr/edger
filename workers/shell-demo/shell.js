const state = {
  apiKey: "",
  items: [],
  selectedId: "",
};

const els = {
  alert: document.querySelector("#alert"),
  catalogCount: document.querySelector("#catalogCount"),
  catalogForm: document.querySelector("#catalogForm"),
  catalogList: document.querySelector("#catalogList"),
  detailPanel: document.querySelector("#detailPanel"),
  refreshButton: document.querySelector("#refreshButton"),
  sessionLine: document.querySelector("#sessionLine"),
};

els.catalogForm.addEventListener("submit", async (event) => {
  event.preventDefault();
  const form = new FormData(els.catalogForm);
  state.apiKey = String(form.get("apiKey") || "").trim();
  await refreshCatalog();
});

els.refreshButton.addEventListener("click", refreshCatalog);

render();

async function refreshCatalog() {
  hideAlert();
  if (!state.apiKey) {
    showAlert("Root key required to load the runtime catalog.");
    return;
  }
  try {
    const data = await apiJson("/api/admin/catalog");
    state.items = data.items || [];
    state.selectedId = state.items[0]?.id || "";
    els.refreshButton.disabled = false;
    els.sessionLine.textContent = `${state.items.length} catalog items loaded`;
    render();
  } catch (error) {
    showAlert(error.message);
    state.items = [];
    state.selectedId = "";
    render();
  }
}

async function apiJson(path) {
  const response = await fetch(path, {
    headers: {
      "x-api-key": state.apiKey,
    },
  });
  const text = await response.text();
  const data = text ? JSON.parse(text) : {};
  if (!response.ok) {
    throw new Error(data.message || `${response.status} ${response.statusText}`);
  }
  return data;
}

function render() {
  renderCatalog();
  renderDetail();
}

function renderCatalog() {
  els.catalogCount.textContent = `${state.items.length} items`;
  if (!state.items.length) {
    els.catalogList.innerHTML = `<div class="catalog-item" aria-disabled="true">
      <span class="catalog-title">No catalog loaded</span>
      <span class="catalog-meta">Connect with a root key</span>
    </div>`;
    return;
  }
  els.catalogList.innerHTML = state.items.map(catalogItem).join("");
  els.catalogList.querySelectorAll("[data-item-id]").forEach((link) => {
    link.addEventListener("click", (event) => {
      const item = state.items.find((candidate) => candidate.id === link.dataset.itemId);
      if (!item || isDisabled(item)) {
        event.preventDefault();
        return;
      }
      state.selectedId = item.id;
      render();
    });
  });
}

function catalogItem(item) {
  const disabled = isDisabled(item);
  return `<a class="catalog-item" href="${disabled ? "#" : escapeHtml(item.route)}"
      data-item-id="${escapeHtml(item.id)}" aria-disabled="${disabled ? "true" : "false"}">
    <span class="catalog-title">${escapeHtml(item.title)}</span>
    <span class="badge ${disabled ? "disabled" : ""}">${escapeHtml(item.status)}</span>
    <span class="catalog-meta">${escapeHtml(item.kind)} · ${escapeHtml(item.owner)} · ${escapeHtml(item.visibility)}</span>
  </a>`;
}

function renderDetail() {
  const item = state.items.find((candidate) => candidate.id === state.selectedId);
  if (!item) {
    els.detailPanel.innerHTML = detailCells([
      ["Selection", "No module selected"],
      ["Access", "Root key required"],
    ]);
    return;
  }
  els.detailPanel.innerHTML = detailCells([
    ["Title", item.title],
    ["Route", item.route],
    ["Owner", item.owner],
    ["Owner kind", item.ownerKind],
    ["Visibility", item.visibility],
    ["Status", item.status],
    ["Source", item.source],
  ]);
}

function detailCells(items) {
  return items
    .map(
      ([label, value]) =>
        `<div class="detail-cell"><span>${escapeHtml(label)}</span><strong>${escapeHtml(String(value || "-"))}</strong></div>`,
    )
    .join("");
}

function isDisabled(item) {
  return item.status === "disabled" || item.visibility === "denied";
}

function showAlert(message) {
  els.alert.hidden = false;
  els.alert.textContent = message;
  els.sessionLine.textContent = "Catalog unavailable";
}

function hideAlert() {
  els.alert.hidden = true;
  els.alert.textContent = "";
}

function escapeHtml(value) {
  return String(value)
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;");
}
