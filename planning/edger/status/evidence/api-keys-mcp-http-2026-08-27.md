# E2E de api-keys + MCP HTTP — 2026-08-27

Instância local do binário da branch `feat/mcp-http-api-keys` (0.3.0) na porta 19080,
com `ROOT_API_KEY=test-root`. Cobre as regressões de escopo corrigidas nesta rodada:
a rota de erros por worker e a listagem de eventos de observabilidade.

    python3 planning/edger/scripts/api-keys-mcp-e2e.py

```
0) o transporte existe e é POST-only
  PASS  GET /api/mcp (405)
1) create de key escopada (root)
  PASS  POST /api/admin/keys (201)
  PASS  rawKey tem prefixo egk_ (True)
  PASS  rawKey tem 64 hex após o prefixo (64)
  PASS  escopo por worker gravado (['p-*'])
  PASS  permissions gravadas (['workers:read', 'workers:invoke'])
2) tools/list é o subset HTTP
  PASS  nenhuma tool local de filesystem ([])
  PASS  install_worker exposto (True)
  PASS  tools de gestão de keys expostas (True)
3) tool dentro da permissão
  PASS  isError (False)
  10 workers dentro do escopo
4) tool fora da permissão vira RESULT, não erro de protocolo
  PASS  HTTP continua 200 (200)
  PASS  isError (True)
  PASS  _meta.status (403)
  PASS  não virou erro JSON-RPC (True)
5) anti-escalada: a key não gerencia keys
  PASS  GET /api/admin/keys sem keys:manage (403)
  PASS  mesma recusa pela tool (True)
6) escopo por worker: rota por nome não entrega worker alheio
  PASS  erros do worker DENTRO do escopo (200)
  PASS  erros do worker FORA do escopo (404)
  PASS  listagem de eventos autorizada (200)
  PASS  nenhum evento de worker alheio ([])
7) delete antes do revoke é recusado
  PASS  DELETE de key viva (409)
  PASS  código (KEY_NOT_REVOKED)
8) revoke mata a credencial na porta
  PASS  POST revoke (200)
  PASS  MCP com key revogada (401)
9) delete depois do revoke
  PASS  DELETE de key revogada (200)
  PASS  key sumiu da listagem ([])

TOTAL: 26 passaram, 0 falharam
```
