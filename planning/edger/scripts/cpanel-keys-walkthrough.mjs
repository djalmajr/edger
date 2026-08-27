// Walkthrough da tela API Keys contra uma instância viva.
//
// O `cpanel-ui-gate.sh` é estático — garante que os markers certos estão no
// bundle. Este aqui abre a tela de verdade e exercita o fluxo que o operador
// faz: entrar, criar com escopo, ver o segredo uma única vez, usar a
// credencial resultante e apagá-la. É o que pega tela que compila e não
// funciona.
//
// Exige Playwright com chromium instalado (o repo não o traz; rode a partir de
// um projeto que já o tenha, ou `npx playwright install chromium`):
//
//     EDGER_BASE_URL=http://localhost:19080 node cpanel-keys-walkthrough.mjs
//
// Sai com 1 na primeira quebra de contrato. Cria e apaga a própria key.
import { chromium } from "@playwright/test";

const BASE = process.env.EDGER_BASE_URL ?? "http://localhost:19080";
const ROOT = process.env.ROOT_API_KEY ?? "test-root";
const ok = [];
const bad = [];
const check = (nome, cond, detalhe = "") => {
  (cond ? ok : bad).push(nome);
  console.log(`  ${cond ? "PASS" : "FAIL"}  ${nome}${detalhe ? " — " + detalhe : ""}`);
};

const browser = await chromium.launch();
const page = await browser.newPage({ viewport: { width: 1400, height: 900 } });
const erros = [];
page.on("pageerror", (e) => erros.push(String(e)));
page.on("console", (m) => m.type() === "error" && erros.push(m.text()));

console.log("1) login com a root key");
await page.goto(`${BASE}/cpanel/`, { waitUntil: "networkidle" });
await page.locator('input[name="apiKey"]').fill(ROOT);
await page.getByRole("button", { name: "Connect" }).click();
await page.waitForTimeout(1500);
check("entrou no painel", (await page.locator('input[name="apiKey"]').count()) === 0);

console.log("2) a aba API Keys existe e navega");
const aba = page.getByRole("button", { name: "API Keys" });
check("item de navegação visível", (await aba.count()) === 1);
await aba.click();
await page.waitForTimeout(1200);
check("rota /cpanel/keys", page.url().endsWith("/cpanel/keys"), page.url());

const linhasAgora = () => page.locator("table tbody tr").count();
const antes = await linhasAgora();
const viaApi = await fetch(`${BASE}/api/admin/keys`, {
  headers: { authorization: `Bearer ${ROOT}` },
}).then((r) => r.json());
check("tabela reflete a API", antes === viaApi.keys.length, `ui=${antes} api=${viaApi.keys.length}`);

console.log("3) o diálogo oferece o catálogo inteiro e se protege");
await page.getByRole("button", { name: "New key" }).click();
await page.waitForTimeout(800);
const d = page.locator('[role="dialog"]');
const criar = d.getByRole("button", { name: "Create" });
const caixas = d.locator('input[type="checkbox"]');
check("catálogo com as 7 permissions", (await caixas.count()) === 7);
check("workers:read vem marcado por default", await caixas.first().isChecked());
check("Create bloqueado sem nome", await criar.isDisabled());

console.log("4) a guarda do formulário responde ao estado");
await d.locator("#key-name").fill("walkthrough-ui");
await page.waitForTimeout(300);
check("Create libera com nome + permission default", !(await criar.isDisabled()));
// Clicar no rótulo alterna a caixa — é assim que a permission sai.
await d.getByText("workers:read", { exact: true }).click();
await page.waitForTimeout(300);
check("rótulo alterna a caixa", !(await caixas.first().isChecked()));
check("Create volta a bloquear sem nenhuma permission", await criar.isDisabled());
await d.getByText("workers:read", { exact: true }).click();
await page.waitForTimeout(300);
check("Create libera de novo", !(await criar.isDisabled()));

console.log("5) criar com escopo restrito");
await d.locator("#key-workers").fill("chunked-*");
await criar.click();
await page.waitForTimeout(1800);

console.log("6) o segredo aparece UMA vez");
const corpo = await page.locator("body").innerText();
const casou = corpo.match(/egk_[0-9a-f]{64}/);
check("raw key no painel one-time", !!casou);
check("avisa que não será mostrada de novo", /never be shown|only time|uma única vez/i.test(corpo));

let idCriada = null;
if (casou) {
  console.log("7) a credencial vale de verdade, e só até onde deve");
  const raw = casou[0];
  const inv = await fetch(`${BASE}/api/admin/workers`, {
    headers: { authorization: `Bearer ${raw}` },
  });
  check("autentica no admin", inv.status === 200, `status=${inv.status}`);
  const nomes = (await inv.json()).workers.map((w) => w.name);
  check(
    "enxerga só o escopo chunked-*",
    nomes.length > 0 && nomes.every((n) => n.startsWith("chunked-")),
    nomes.join(","),
  );
  const escalada = await fetch(`${BASE}/api/admin/keys`, {
    headers: { authorization: `Bearer ${raw}` },
  });
  check("não gerencia keys (sem keys:manage)", escalada.status === 403, `status=${escalada.status}`);
  const todas = await fetch(`${BASE}/api/admin/keys`, {
    headers: { authorization: `Bearer ${ROOT}` },
  }).then((r) => r.json());
  idCriada = todas.keys.find((k) => k.name === "walkthrough-ui")?.id ?? null;
}

console.log("8) fechar o painel e ver a linha nova");
await page.getByRole("button", { name: "Close" }).last().click();
await page.waitForTimeout(1500);
const depois = await linhasAgora();
check("linha nova na tabela", depois === antes + 1, `antes=${antes} depois=${depois}`);
const textoTabela = await page.locator("table").innerText();
check("mostra a permission concedida", textoTabela.includes("workers:read"));
check("mostra o escopo de worker", textoTabela.includes("chunked-*"));

console.log("9) apagar confirma e some da lista");
const linha = page.locator("table tbody tr", { hasText: "walkthrough-ui" });
check("achou a linha criada", (await linha.count()) === 1);
await linha.getByRole("button").last().click();
await page.waitForTimeout(800);
const confirma = page.locator('[role="alertdialog"], [role="dialog"]').last();
check("pede confirmação", await confirma.isVisible());
const txt = await confirma.innerText();
check(
  "diz que é irreversível",
  /cannot be undone|immediately|irrevers/i.test(txt),
  txt.slice(0, 70).replace(/\n/g, " "),
);
await confirma.getByRole("button", { name: /^Delete$/i }).last().click();
await page.waitForTimeout(1800);
const final = await linhasAgora();
check("linha sumiu da tela", final === antes, `voltou para ${final}`);

if (idCriada) {
  const sobrou = await fetch(`${BASE}/api/admin/keys`, {
    headers: { authorization: `Bearer ${ROOT}` },
  }).then((r) => r.json());
  check("apagada no SERVIDOR, não só na tela", !sobrou.keys.some((k) => k.id === idCriada));
}

check("sem erro de JS no console", erros.length === 0, erros.slice(0, 2).join(" | "));

await page.screenshot({ path: "/tmp/cpanel-keys-final.png", fullPage: true });
await browser.close();
console.log(`\nTOTAL: ${ok.length} passaram, ${bad.length} falharam`);
process.exit(bad.length ? 1 : 0);
