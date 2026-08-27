#!/usr/bin/env python3
"""Ciclo de vida de uma api-key contra uma instância viva, ponta a ponta.

Os testes de integração cobrem cada peça com o router montado em memória; este
script é o contrário — fala HTTP de verdade com um binário rodando, que é onde
aparecem os defeitos de montagem: rota não registrada, guarda ausente na
porta, credencial que atravessa o transporte sem ser reavaliada.

    ROOT_API_KEY=test-root cargo run --bin edger   # noutro terminal
    python3 planning/edger/scripts/api-keys-mcp-e2e.py

Sai com 1 na primeira falha de contrato, então serve de gate. Cria e apaga a
própria key; não deixa resíduo no store.
"""

import json
import os
import subprocess
import sys

BASE = os.environ.get("EDGER_URL", "http://localhost:19080")
ROOT = os.environ.get("EDGER_ROOT_KEY", "test-root")

ok: list[str] = []
bad: list[str] = []


def curl(path, *, key=ROOT, method=None, data=None):
    cmd = ["curl", "-s", "-w", "\n%{http_code}", "-H", f"authorization: Bearer {key}"]
    if method:
        cmd += ["-X", method]
    if data is not None:
        cmd += ["-H", "content-type: application/json", "-d", json.dumps(data)]
    cmd.append(BASE + path)
    out = subprocess.run(cmd, capture_output=True, text=True).stdout
    body, _, code = out.rpartition("\n")
    try:
        parsed = json.loads(body) if body.strip() else None
    except json.JSONDecodeError:
        parsed = body
    return int(code), parsed


def check(nome, esperado, obtido):
    if esperado == obtido:
        ok.append(nome)
        print(f"  PASS  {nome} ({obtido})")
    else:
        bad.append(nome)
        print(f"  FAIL  {nome} — esperado {esperado!r}, obtido {obtido!r}")


def rpc(key, method, params=None, rid=1):
    payload = {"jsonrpc": "2.0", "id": rid, "method": method}
    if params:
        payload["params"] = params
    return curl("/api/mcp", key=key, data=payload)


def tool(key, name, args, rid=1):
    return rpc(key, "tools/call", {"name": name, "arguments": args}, rid)


print("0) o transporte existe e é POST-only")
code, _ = curl("/api/mcp")
check("GET /api/mcp", 405, code)

print("1) create de key escopada (root)")
code, body = curl(
    "/api/admin/keys",
    data={
        "name": "e2e-local",
        "permissions": ["workers:read", "workers:invoke"],
        "workers": ["p-*"],
    },
)
check("POST /api/admin/keys", 201, code)
raw = body["rawKey"]
kid = body["key"]["id"]
check("rawKey tem prefixo egk_", True, raw.startswith("egk_"))
check("rawKey tem 64 hex após o prefixo", 64, len(raw) - 4)
check("escopo por worker gravado", ["p-*"], body["key"]["workers"])
check("permissions gravadas", ["workers:read", "workers:invoke"], body["key"]["permissions"])

print("2) tools/list é o subset HTTP")
code, body = rpc(raw, "tools/list", rid=2)
names = [t["name"] for t in body["result"]["tools"]]
locais = [
    n
    for n in names
    if n
    in (
        "edger.write_worker_file",
        "edger.list_workers",
        "edger.validate_local",
        "edger.prepare_commit",
        "edger.inspect_worker",
    )
]
check("nenhuma tool local de filesystem", [], locais)
check("install_worker exposto", True, "edger.install_worker" in names)
check("tools de gestão de keys expostas", True, "edger.create_api_key" in names)

print("3) tool dentro da permissão")
code, body = tool(raw, "edger.list_deployed_workers", {}, 3)
check("isError", False, body["result"]["isError"])
print(f"  {len(body['result']['structuredContent']['workers'])} workers dentro do escopo")

print("4) tool fora da permissão vira RESULT, não erro de protocolo")
code, body = tool(raw, "edger.promote_worker", {"name": "x", "version": "1.0.0"}, 4)
check("HTTP continua 200", 200, code)
check("isError", True, body["result"]["isError"])
check("_meta.status", 403, body["result"]["_meta"]["status"])
check("não virou erro JSON-RPC", True, "error" not in body)

print("5) anti-escalada: a key não gerencia keys")
code, _ = curl("/api/admin/keys", key=raw)
check("GET /api/admin/keys sem keys:manage", 403, code)
code, body = tool(raw, "edger.list_api_keys", {}, 5)
check("mesma recusa pela tool", True, body["result"]["isError"])

print("6) escopo por worker: rota por nome não entrega worker alheio")
code, inventario = curl("/api/admin/workers", key=ROOT)
todos = [w["name"] for w in inventario["workers"]]
dentro = next((n for n in todos if n), None)
fora = next((n for n in todos if n != dentro), None)
if dentro and fora:
    code, body = curl(
        "/api/admin/keys",
        data={"name": "e2e-escopo", "permissions": ["workers:read"], "workers": [dentro]},
    )
    escopada, eid = body["rawKey"], body["key"]["id"]
    code, _ = curl(f"/api/admin/workers/{dentro}/errors", key=escopada)
    check("erros do worker DENTRO do escopo", 200, code)
    # Erro recente carrega mensagem e stack. Esta rota lia pelo nome cru e
    # entregava `200` para worker alheio — o único ponto por onde o escopo
    # vazava.
    code, _ = curl(f"/api/admin/workers/{fora}/errors", key=escopada)
    check("erros do worker FORA do escopo", 404, code)
    curl(f"/api/admin/keys/{eid}/revoke", method="POST")
    curl(f"/api/admin/keys/{eid}", method="DELETE")

    # A observabilidade não recebe nome no path: ela filtra na fonte. Sem esse
    # filtro, observability:read — que deixou de ser root-only — bastava para
    # ler o evento de qualquer worker.
    code, body = curl(
        "/api/admin/keys",
        data={
            "name": "e2e-observador",
            "permissions": ["observability:read"],
            "workers": [dentro],
        },
    )
    observador, oid = body["rawKey"], body["key"]["id"]
    code, eventos = curl("/api/admin/observability/events?limit=200", key=observador)
    check("listagem de eventos autorizada", 200, code)
    alheios = sorted(
        {
            e["worker"]
            for e in (eventos or {}).get("events", [])
            if e.get("worker") and e["worker"] != dentro
        }
    )
    check("nenhum evento de worker alheio", [], alheios)
    curl(f"/api/admin/keys/{oid}/revoke", method="POST")
    curl(f"/api/admin/keys/{oid}", method="DELETE")
else:
    print("  (pulado: a instância precisa de dois workers para comparar)")

print("7) delete antes do revoke é recusado")
code, body = curl(f"/api/admin/keys/{kid}", method="DELETE")
check("DELETE de key viva", 409, code)
check("código", "KEY_NOT_REVOKED", (body or {}).get("code"))

print("8) revoke mata a credencial na porta")
code, _ = curl(f"/api/admin/keys/{kid}/revoke", method="POST")
check("POST revoke", 200, code)
code, _ = rpc(raw, "ping", rid=7)
check("MCP com key revogada", 401, code)

print("9) delete depois do revoke")
code, _ = curl(f"/api/admin/keys/{kid}", method="DELETE")
check("DELETE de key revogada", 200, code)
code, body = curl("/api/admin/keys")
check("key sumiu da listagem", [], [k for k in body["keys"] if k["id"] == kid])

print(f"\nTOTAL: {len(ok)} passaram, {len(bad)} falharam")
sys.exit(1 if bad else 0)
