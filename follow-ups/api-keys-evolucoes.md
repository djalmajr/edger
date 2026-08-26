# Follow-ups: api-keys e MCP HTTP (0.3.0)

- **Rotate com grace period** (padrão tenancit): tabela de previous tokens
  com `valid_until` para automação que não pode parar durante a troca. A v1
  fica em create/revoke/delete de propósito.
- **Rate limit por key** (padrão apigate): token bucket por id com RPM na
  própria key; hoje não há limite.
- **tools/list filtrado por permission**: o /api/mcp lista o subset inteiro
  para qualquer key; anunciar só o que a key pode chamar reduz ruído para o
  agente (a chamada já é negada hoje — é só apresentação).
- **Migração do Studio para key escopada**: trocar EDGER_ROOT_KEY do chart
  do lowcode-studio por uma key `workers:*` + `keys` do namespace/prefixo
  `p-*` dele (decisão do usuário: rodada separada, deploy próprio).
