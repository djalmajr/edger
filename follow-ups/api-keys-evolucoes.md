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
- **`role` ainda autoriza uma coisa**: `run_health_check` exige
  `principal.role == "admin"` além de `workers:read`, e é o único uso de
  `role` como autorização — o resto do modelo é permission. Ou o gate vira
  uma permission (`workers:health`?), ou o `role` some do vocabulário de
  autorização. Hoje uma key criada com o default `operator` leva `403` no
  health-check mesmo com a permission certa, o que surpreende.
- **Health-check resolve o worker no índice inteiro**: diferente das outras
  rotas por nome, ele não passa por `require_visible_worker`. Só não vaza
  porque o gate de `role: admin` barra antes; se aquele gate virar permission,
  o filtro de escopo precisa entrar junto.
- **Limite do nome da key é em BYTES**: a validação usa `name.len()` e a
  mensagem diz "1-80 characters" — um nome acentuado estoura antes de 80
  caracteres. Ou conta `chars()`, ou a mensagem passa a dizer bytes.
- **`security_operational.rs` é flaky (~1 em 8), e é anterior a esta rodada**:
  os testes que capturam log usam `tracing::subscriber::set_default`, que
  instala o subscriber na THREAD, enquanto o `cargo test` roda os testes em
  paralelo. Quando duas capturas convivem, a asserção enxerga o buffer errado
  — em geral vazio. Com `--test-threads=1` são 6 de 6 verdes, o que confirma o
  diagnóstico. Serializar só a instalação com um mutex NÃO resolve: tentei, e
  a taxa piorou para 7 em 10, com o buffer recebendo o log de outra requisição
  em vez de ficar vazio — sinal de que o evento nasce fora da thread do teste,
  e não apenas de que duas capturas competem. O caminho provável é um
  subscriber por processo (`set_global_default` uma vez, com um layer que
  roteia por span/request_id) em vez de um por teste.
